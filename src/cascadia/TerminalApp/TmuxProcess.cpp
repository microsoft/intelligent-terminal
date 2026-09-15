// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxProcess.cpp
//
// Abstract:
// - Overlapped process stdio with bounded writes and owned-process-only shutdown.

#include "TmuxProcess.h"

#include <Windows.h>
#include <objbase.h>
#include <process.h>
#include <wil/resource.h>
#include <wil/result.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cerrno>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <stdexcept>
#include <system_error>
#include <utility>
#include <vector>

namespace Microsoft::Terminal::Tmux
{
    namespace
    {
        constexpr DWORD IoBufferBytes = 64 * 1024;

        struct Pipe
        {
            wil::unique_hfile parent;
            wil::unique_hfile child;
        };

        Pipe MakePipe(const bool parentReads)
        {
            GUID guid;
            THROW_IF_FAILED(CoCreateGuid(&guid));
            wchar_t guidText[40]{};
            THROW_HR_IF(E_UNEXPECTED, StringFromGUID2(guid, guidText, ARRAYSIZE(guidText)) == 0);
            const auto name = std::wstring{ LR"(\\.\pipe\IntelligentTerminal.Tmux.)" } + guidText;
            Pipe pipe;
            pipe.parent.reset(CreateNamedPipeW(name.c_str(),
                                               (parentReads ? PIPE_ACCESS_INBOUND : PIPE_ACCESS_OUTBOUND) |
                                                   FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                                               PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                                               1,
                                               IoBufferBytes,
                                               IoBufferBytes,
                                               0,
                                               nullptr));
            THROW_LAST_ERROR_IF(!pipe.parent);
            SECURITY_ATTRIBUTES security{ sizeof(security), nullptr, TRUE };
            pipe.child.reset(CreateFileW(name.c_str(), parentReads ? GENERIC_WRITE : GENERIC_READ, 0, &security, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr));
            THROW_LAST_ERROR_IF(!pipe.child);

            // Opening our client first is intentional: there is no blocking
            // connect and no opportunity to inherit an unconnected pipe.
            OVERLAPPED connected{};
            wil::unique_handle event{ CreateEventW(nullptr, TRUE, FALSE, nullptr) };
            THROW_LAST_ERROR_IF(!event);
            connected.hEvent = event.get();
            if (!ConnectNamedPipe(pipe.parent.get(), &connected))
            {
                THROW_LAST_ERROR_IF(GetLastError() != ERROR_PIPE_CONNECTED);
            }
            return pipe;
        }

        class IoOperation
        {
        public:
            explicit IoOperation(const HANDLE pipe) :
                pipe{ pipe },
                event{ CreateEventW(nullptr, TRUE, FALSE, nullptr) }
            {
                THROW_LAST_ERROR_IF(!event);
                overlapped.hEvent = event.get();
            }

            ~IoOperation()
            {
                Cancel();
            }

            IoOperation(const IoOperation&) = delete;
            IoOperation& operator=(const IoOperation&) = delete;

            void Cancel() noexcept
            {
                if (pending)
                {
                    CancelIoEx(pipe, &overlapped);
                    DWORD ignored{};
                    GetOverlappedResult(pipe, &overlapped, &ignored, TRUE);
                    pending = false;
                }
            }

            void Prepare()
            {
                THROW_IF_WIN32_BOOL_FALSE(ResetEvent(event.get()));
                overlapped = {};
                overlapped.hEvent = event.get();
            }

            HANDLE pipe;
            wil::unique_handle event;
            OVERLAPPED overlapped{};
            bool pending{};
        };

        class ReadOperation : public IoOperation
        {
        public:
            using IoOperation::IoOperation;

            ~ReadOperation()
            {
                Cancel();
            }

            bool Step(const std::function<void(std::string_view)>& callback, const bool processExited)
            {
                if (closed)
                {
                    return false;
                }

                DWORD count{};
                BOOL success{};
                if (pending)
                {
                    success = GetOverlappedResult(pipe, &overlapped, &count, FALSE);
                    if (!success && GetLastError() == ERROR_IO_INCOMPLETE)
                    {
                        if (!processExited)
                        {
                            return false;
                        }
                        // A descendant may have inherited stdout. Do not wait
                        // for it to close, but retain a read which won the race
                        // with cancellation and contains the backend's last bytes.
                        CancelIoEx(pipe, &overlapped);
                        success = GetOverlappedResult(pipe, &overlapped, &count, TRUE);
                    }
                    pending = false;
                }
                else
                {
                    Prepare();
                    success = ReadFile(pipe, buffer.data(), static_cast<DWORD>(buffer.size()), &count, &overlapped);
                    if (!success && GetLastError() == ERROR_IO_PENDING)
                    {
                        pending = true;
                        return processExited;
                    }
                }

                if (!success)
                {
                    const auto error = GetLastError();
                    if (error == ERROR_BROKEN_PIPE || error == ERROR_PIPE_NOT_CONNECTED ||
                        (processExited && error == ERROR_OPERATION_ABORTED))
                    {
                        closed = true;
                        return true;
                    }
                    THROW_WIN32(error);
                }
                if (count == 0)
                {
                    closed = true;
                }
                else if (callback)
                {
                    callback(std::string_view{ buffer.data(), count });
                }
                return true;
            }

            bool closed{};
            std::array<char, IoBufferBytes> buffer{};
        };
    }

    struct TmuxProcess::State : std::enable_shared_from_this<State>
    {
        explicit State(Callbacks callbacks) :
            callbacks{ std::move(callbacks) },
            stopEvent{ CreateEventW(nullptr, TRUE, FALSE, nullptr) },
            writeEvent{ CreateEventW(nullptr, FALSE, FALSE, nullptr) },
            startEvent{ CreateEventW(nullptr, TRUE, FALSE, nullptr) }
        {
            THROW_LAST_ERROR_IF(!stopEvent || !writeEvent || !startEvent);
        }

        static unsigned int __stdcall WorkerMain(void* raw) noexcept
        {
            const std::unique_ptr<std::shared_ptr<State>> context{ static_cast<std::shared_ptr<State>*>(raw) };
            const auto state = *context;
            WaitForSingleObject(state->startEvent.get(), INFINITE);
            state->Run();
            return 0;
        }

        void Start(std::wstring commandLine, const std::wstring& workingDirectory)
        {
            {
                std::lock_guard lock{ mutex };
                if (startAttempted || closing)
                {
                    throw std::logic_error{ "Tmux backend transport can only be started once and cannot start after Close" };
                }
                startAttempted = true;
                launching = true;
            }
            const auto launchFinished = wil::scope_exit([&]() noexcept {
                {
                    std::lock_guard lock{ mutex };
                    launching = false;
                }
                launched.notify_all();
            });
            try
            {
                Launch(std::move(commandLine), workingDirectory);
            }
            catch (...)
            {
                RecordFailure(std::current_exception());
                RequestStop();
                throw;
            }
        }

        void Launch(std::wstring commandLine, const std::wstring& workingDirectory)
        {
            if (commandLine.empty() || commandLine.size() > 32766 ||
                commandLine.find(L'\0') != std::wstring::npos ||
                workingDirectory.empty() || workingDirectory.find(L'\0') != std::wstring::npos)
            {
                throw std::invalid_argument{ "Tmux backend requires a valid command line and explicit working directory" };
            }

            auto input = MakePipe(false);
            auto output = MakePipe(true);
            auto error = MakePipe(true);
            SIZE_T attributeBytes{};
            InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeBytes);
            THROW_LAST_ERROR_IF(attributeBytes == 0);
            std::vector<unsigned char> storage(attributeBytes);
            const auto attributes = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(storage.data());
            THROW_IF_WIN32_BOOL_FALSE(InitializeProcThreadAttributeList(attributes, 1, 0, &attributeBytes));
            const auto deleteAttributes = wil::scope_exit([&]() noexcept {
                DeleteProcThreadAttributeList(attributes);
            });
            std::array<HANDLE, 3> inherited{ input.child.get(), output.child.get(), error.child.get() };
            THROW_IF_WIN32_BOOL_FALSE(UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherited.data(), sizeof(inherited), nullptr, nullptr));

            STARTUPINFOEXW startup{};
            startup.StartupInfo.cb = sizeof(startup);
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = input.child.get();
            startup.StartupInfo.hStdOutput = output.child.get();
            startup.StartupInfo.hStdError = error.child.get();
            startup.lpAttributeList = attributes;
            PROCESS_INFORMATION information{};
            THROW_IF_WIN32_BOOL_FALSE(CreateProcessW(nullptr, commandLine.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT, nullptr, workingDirectory.c_str(), &startup.StartupInfo, &information));
            process.reset(information.hProcess);
            wil::unique_handle mainThread{ information.hThread };
            stdinPipe = std::move(input.parent);
            stdoutPipe = std::move(output.parent);
            stderrPipe = std::move(error.parent);
            input.child.reset();
            output.child.reset();
            error.child.reset();

            // The suspended backend cannot spawn anything before ownership and
            // the worker's shutdown path have been fully established.
            try
            {
                auto context = std::make_unique<std::shared_ptr<State>>(shared_from_this());
                unsigned int threadId{};
                worker.reset(reinterpret_cast<HANDLE>(_beginthreadex(nullptr, 0, &State::WorkerMain, context.get(), 0, &threadId)));
                if (!worker)
                {
                    throw std::system_error{ errno, std::generic_category(), "Starting tmux transport worker" };
                }
                workerId.store(threadId, std::memory_order_release);
                context.release();
                if (WaitForSingleObject(stopEvent.get(), 0) == WAIT_OBJECT_0)
                {
                    THROW_IF_WIN32_BOOL_FALSE(TerminateProcess(process.get(), ERROR_PROCESS_ABORTED));
                }
                else
                {
                    THROW_LAST_ERROR_IF(ResumeThread(mainThread.get()) == static_cast<DWORD>(-1));
                }
                {
                    std::lock_guard lock{ mutex };
                    accepting = !closing;
                }
                THROW_IF_WIN32_BOOL_FALSE(SetEvent(startEvent.get()));
            }
            catch (...)
            {
                RequestStop();
                if (worker)
                {
                    SetEvent(startEvent.get());
                    WaitForSingleObject(worker.get(), INFINITE);
                }
                else
                {
                    TerminateProcess(process.get(), ERROR_PROCESS_ABORTED);
                    WaitForSingleObject(process.get(), INFINITE);
                }
                throw;
            }
        }

        void Enqueue(std::string data)
        {
            std::lock_guard lock{ mutex };
            if (failure)
            {
                std::rethrow_exception(failure);
            }
            if (!accepting)
            {
                throw std::runtime_error{ "Tmux backend transport is not running" };
            }
            if (data.size() > MaxQueuedBytes - queuedBytes)
            {
                throw std::length_error{ "Tmux backend write queue is full" };
            }
            if (!data.empty())
            {
                const auto size = data.size();
                queue.emplace_back(std::move(data));
                queuedBytes += size;
                THROW_IF_WIN32_BOOL_FALSE(SetEvent(writeEvent.get()));
            }
        }

        void RequestStop() noexcept
        {
            {
                std::lock_guard lock{ mutex };
                accepting = false;
                closing = true;
            }
            SetEvent(stopEvent.get());
        }

        void RecordFailure(const std::exception_ptr& error) noexcept
        {
            std::lock_guard lock{ mutex };
            if (!failure)
            {
                failure = error;
                OutputDebugStringW(L"Tmux process transport failed; inspect RethrowFailure or the failed callback.\n");
            }
        }

        std::exception_ptr Failure() const
        {
            std::lock_guard lock{ mutex };
            return failure;
        }

        bool WriteStep(IoOperation& operation, std::string& current, size_t& offset)
        {
            if (!operation.pending && current.empty())
            {
                std::lock_guard lock{ mutex };
                if (queue.empty())
                {
                    return false;
                }
                current = std::move(queue.front());
                queue.pop_front();
                offset = 0;
            }
            DWORD count{};
            BOOL success{};
            if (operation.pending)
            {
                success = GetOverlappedResult(operation.pipe, &operation.overlapped, &count, FALSE);
                if (!success && GetLastError() == ERROR_IO_INCOMPLETE)
                {
                    return false;
                }
                operation.pending = false;
            }
            else
            {
                operation.Prepare();
                const auto size = static_cast<DWORD>((std::min)(current.size() - offset, size_t{ IoBufferBytes }));
                success = WriteFile(operation.pipe, current.data() + offset, size, &count, &operation.overlapped);
                if (!success && GetLastError() == ERROR_IO_PENDING)
                {
                    operation.pending = true;
                    return false;
                }
            }
            THROW_LAST_ERROR_IF(!success);
            THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_WRITE_FAULT), count == 0);
            offset += count;
            {
                std::lock_guard lock{ mutex };
                queuedBytes -= count;
            }
            if (offset == current.size())
            {
                current.clear();
            }
            return true;
        }

        void Pump()
        {
            // Declare buffers before operations so cancellation always completes
            // before memory passed to overlapped I/O can be released.
            std::string current;
            size_t offset{};
            ReadOperation output{ stdoutPipe.get() };
            ReadOperation error{ stderrPipe.get() };
            IoOperation input{ stdinPipe.get() };
            bool processExited{};
            ULONGLONG drainDeadline{};

            while (WaitForSingleObject(stopEvent.get(), 0) != WAIT_OBJECT_0)
            {
                if (!processExited && WaitForSingleObject(process.get(), 0) == WAIT_OBJECT_0)
                {
                    processExited = true;
                    drainDeadline = GetTickCount64() + 1000;
                    std::lock_guard lock{ mutex };
                    accepting = false;
                }

                auto progress = output.Step(callbacks.output, processExited);
                if (WaitForSingleObject(stopEvent.get(), 0) == WAIT_OBJECT_0)
                {
                    return;
                }
                progress = error.Step(callbacks.error, processExited) || progress;
                if (processExited)
                {
                    if (output.closed && error.closed)
                    {
                        return;
                    }
                    if (GetTickCount64() >= drainDeadline)
                    {
                        throw std::runtime_error{ "Tmux backend output remained active after the backend exited" };
                    }
                }
                else
                {
                    progress = WriteStep(input, current, offset) || progress;
                }
                if (progress)
                {
                    continue;
                }

                std::array<HANDLE, 6> handles{ stopEvent.get(), process.get(), writeEvent.get() };
                DWORD count = 3;
                for (auto* operation : { static_cast<IoOperation*>(&output), static_cast<IoOperation*>(&error), &input })
                {
                    if (operation->pending)
                    {
                        handles[count++] = operation->event.get();
                    }
                }
                THROW_LAST_ERROR_IF(WaitForMultipleObjects(count, handles.data(), FALSE, INFINITE) == WAIT_FAILED);
            }
        }

        void Run() noexcept
        {
            try
            {
                Pump();
            }
            catch (...)
            {
                RecordFailure(std::current_exception());
            }

            RequestStop();
            stdinPipe.reset();
            stdoutPipe.reset();
            stderrPipe.reset();
            DWORD exitCode = ERROR_PROCESS_ABORTED;
            try
            {
                const auto wait = WaitForSingleObject(process.get(), 500);
                THROW_LAST_ERROR_IF(wait == WAIT_FAILED);
                if (wait == WAIT_TIMEOUT)
                {
                    // No job object: a durable tmux server or other independently
                    // useful descendant must survive closing this frontend.
                    if (!TerminateProcess(process.get(), ERROR_PROCESS_ABORTED))
                    {
                        const auto error = GetLastError();
                        if (WaitForSingleObject(process.get(), 0) != WAIT_OBJECT_0)
                        {
                            THROW_WIN32(error);
                        }
                    }
                }
                THROW_LAST_ERROR_IF(WaitForSingleObject(process.get(), INFINITE) == WAIT_FAILED);
                THROW_IF_WIN32_BOOL_FALSE(GetExitCodeProcess(process.get(), &exitCode));
                process.reset();
            }
            catch (...)
            {
                RecordFailure(std::current_exception());
            }
            {
                std::lock_guard lock{ mutex };
                queue.clear();
                queuedBytes = 0;
            }

            bool reported{};
            const auto report = [&]() noexcept {
                if (const auto error = Failure(); error && callbacks.failed && !reported)
                {
                    reported = true;
                    try
                    {
                        callbacks.failed(error);
                    }
                    catch (...)
                    {
                        RecordFailure(std::current_exception());
                    }
                }
            };
            report();
            if (callbacks.exited)
            {
                try
                {
                    callbacks.exited(exitCode);
                }
                catch (...)
                {
                    RecordFailure(std::current_exception());
                }
            }
            report();
        }

        Callbacks callbacks;
        wil::unique_handle stopEvent;
        wil::unique_handle writeEvent;
        wil::unique_handle startEvent;
        wil::unique_handle process;
        wil::unique_hfile stdinPipe;
        wil::unique_hfile stdoutPipe;
        wil::unique_hfile stderrPipe;
        wil::unique_handle worker;
        std::atomic<unsigned int> workerId{};
        mutable std::mutex mutex;
        std::condition_variable launched;
        std::deque<std::string> queue;
        size_t queuedBytes{};
        bool accepting{};
        bool startAttempted{};
        bool launching{};
        bool closing{};
        std::exception_ptr failure;
    };

    TmuxProcess::TmuxProcess(Callbacks callbacks) :
        _state{ std::make_shared<State>(std::move(callbacks)) }
    {
    }

    TmuxProcess::~TmuxProcess() noexcept
    {
        Close();
    }

    void TmuxProcess::Start(std::wstring commandLine, std::wstring workingDirectory)
    {
        const auto state = _state;
        state->Start(std::move(commandLine), workingDirectory);
    }

    void TmuxProcess::Write(std::string data)
    {
        _state->Enqueue(std::move(data));
    }

    void TmuxProcess::Close() noexcept
    {
        const auto state = _state;
        state->RequestStop();
        if (GetCurrentThreadId() == state->workerId.load(std::memory_order_acquire))
        {
            return;
        }
        {
            std::unique_lock lock{ state->mutex };
            state->launched.wait(lock, [&]() { return !state->launching; });
        }
        if (state->worker)
        {
            WaitForSingleObject(state->worker.get(), INFINITE);
        }
    }

    void TmuxProcess::RethrowFailure() const
    {
        if (const auto failure = _state->Failure())
        {
            std::rethrow_exception(failure);
        }
    }
}
