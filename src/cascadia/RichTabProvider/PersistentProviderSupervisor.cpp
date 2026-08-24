// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "PersistentProviderSupervisor.h"

#include <windows.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <limits>
#include <vector>

#include <wil/resource.h>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr size_t MaximumQueuedControlFrames{ 4 };
        constexpr size_t MaximumQueuedControlBytes{ MaximumQueuedControlFrames * MaximumControlFrameSize };
        constexpr size_t MaximumPersistentStandardOutputSize{ 64 * 1024 };
        constexpr size_t MaximumPersistentStandardErrorSize{ 64 * 1024 };
        constexpr auto StableProcessLifetime = std::chrono::seconds{ 30 };
        constexpr auto GracefulStopTimeout = std::chrono::seconds{ 2 };
        constexpr auto ProcessPollInterval = std::chrono::milliseconds{ 100 };
        constexpr SIZE_T MaximumPersistentProcessMemory = 256ull * 1024ull * 1024ull;
        constexpr DWORD MaximumPersistentActiveProcesses = 8;

        struct Pipe
        {
            wil::unique_handle read;
            wil::unique_handle write;
        };

        bool _CreatePipe(Pipe& pipe, const bool childReads)
        {
            SECURITY_ATTRIBUTES attributes{ sizeof(attributes), nullptr, TRUE };
            HANDLE read = nullptr;
            HANDLE write = nullptr;
            if (!CreatePipe(&read, &write, &attributes, 0))
            {
                return false;
            }
            pipe.read.reset(read);
            pipe.write.reset(write);
            return SetHandleInformation(
                       childReads ? pipe.write.get() : pipe.read.get(),
                       HANDLE_FLAG_INHERIT,
                       0) != FALSE;
        }

        class Win32PersistentProviderProcess final : public IPersistentProviderProcess
        {
        public:
            Win32PersistentProviderProcess(
                wil::unique_handle job,
                wil::unique_process_handle process,
                wil::unique_handle input,
                wil::unique_handle output,
                wil::unique_handle error) :
                _job{ std::move(job) },
                _process{ std::move(process) },
                _input{ std::move(input) },
                _output{ std::move(output) },
                _error{ std::move(error) }
            {
                _standardOutput.reserve(MaximumPersistentStandardOutputSize);
                _standardError.reserve(MaximumPersistentStandardErrorSize);
            }

            void StartThreads()
            {
                _writer = std::thread{ [this]() { _WriteInput(); } };
                _stdoutReader = std::thread{ [this]() {
                    _ReadOutput(_output.get(), MaximumPersistentStandardOutputSize, _standardOutput);
                } };
                _stderrReader = std::thread{ [this]() {
                    _ReadOutput(_error.get(), MaximumPersistentStandardErrorSize, _standardError);
                } };
            }

            ~Win32PersistentProviderProcess() override
            {
                Stop({}, std::chrono::milliseconds{ 0 });
            }

            bool Send(std::string frame) override
            {
                if (frame.empty() || frame.size() > MaximumControlFrameSize)
                {
                    return false;
                }

                std::lock_guard lock{ _inputMutex };
                if (!_acceptingInput ||
                    _queuedInputBytes + frame.size() > MaximumQueuedControlBytes ||
                    _inputFrames.size() >= MaximumQueuedControlFrames)
                {
                    return false;
                }
                _queuedInputBytes += frame.size();
                _inputFrames.emplace_back(std::move(frame));
                _inputCondition.notify_one();
                return true;
            }

            bool IsRunning() const noexcept override
            {
                if (_writeFailed.load(std::memory_order_acquire) || !_process)
                {
                    return false;
                }
                return WaitForSingleObject(_process.get(), 0) == WAIT_TIMEOUT;
            }

            void Stop(std::string stopFrame, const std::chrono::milliseconds timeout) noexcept override
            {
                {
                    std::lock_guard lock{ _stopMutex };
                    if (_stopped)
                    {
                        return;
                    }
                    _stopped = true;
                }

                {
                    std::lock_guard lock{ _inputMutex };
                    _inputFrames.clear();
                    _queuedInputBytes = 0;
                    if (!stopFrame.empty() &&
                        stopFrame.size() <= MaximumControlFrameSize)
                    {
                        _queuedInputBytes += stopFrame.size();
                        _inputFrames.emplace_back(std::move(stopFrame));
                    }
                    _acceptingInput = false;
                    _closeInputWhenDrained = true;
                }
                _inputCondition.notify_one();

                const auto waitMilliseconds = timeout.count() <= 0 ?
                                                  DWORD{ 0 } :
                                                  static_cast<DWORD>((std::min)(
                                                      timeout.count(),
                                                      static_cast<int64_t>((std::numeric_limits<DWORD>::max)())));
                if (_process &&
                    WaitForSingleObject(_process.get(), waitMilliseconds) == WAIT_TIMEOUT)
                {
                    TerminateJobObject(_job.get(), ERROR_PROCESS_ABORTED);
                    WaitForSingleObject(_process.get(), 1000);
                }
                else if (_job)
                {
                    TerminateJobObject(_job.get(), ERROR_PROCESS_ABORTED);
                }

                if (_writer.joinable())
                {
                    CancelSynchronousIo(_writer.native_handle());
                }
                if (_stdoutReader.joinable())
                {
                    CancelSynchronousIo(_stdoutReader.native_handle());
                }
                if (_stderrReader.joinable())
                {
                    CancelSynchronousIo(_stderrReader.native_handle());
                }
                {
                    std::lock_guard lock{ _inputMutex };
                    _terminateInput = true;
                }
                _inputCondition.notify_one();
                if (_writer.joinable())
                {
                    _writer.join();
                }
                _input.reset();
                if (_stdoutReader.joinable())
                {
                    _stdoutReader.join();
                }
                if (_stderrReader.joinable())
                {
                    _stderrReader.join();
                }
                _output.reset();
                _error.reset();
            }

            std::string StandardError() const override
            {
                std::lock_guard lock{ _outputMutex };
                return _standardError;
            }

        private:
            void _WriteInput()
            {
                for (;;)
                {
                    std::string frame;
                    {
                        std::unique_lock lock{ _inputMutex };
                        _inputCondition.wait(lock, [&]() {
                            return _terminateInput || !_inputFrames.empty() || _closeInputWhenDrained;
                        });
                        if (_terminateInput)
                        {
                            return;
                        }
                        if (_inputFrames.empty())
                        {
                            if (_closeInputWhenDrained)
                            {
                                _input.reset();
                                return;
                            }
                            continue;
                        }
                        frame = std::move(_inputFrames.front());
                        _inputFrames.pop_front();
                        _queuedInputBytes -= frame.size();
                    }

                    size_t offset = 0;
                    while (offset < frame.size())
                    {
                        DWORD written = 0;
                        const auto chunk = static_cast<DWORD>((std::min)(
                            frame.size() - offset,
                            static_cast<size_t>((std::numeric_limits<DWORD>::max)())));
                        if (!WriteFile(
                                _input.get(),
                                frame.data() + offset,
                                chunk,
                                &written,
                                nullptr))
                        {
                            _writeFailed.store(true, std::memory_order_release);
                            _input.reset();
                            return;
                        }
                        offset += written;
                    }
                }
            }

            void _ReadOutput(HANDLE pipe, const size_t limit, std::string& destination)
            {
                std::array<char, 4096> buffer{};
                DWORD read = 0;
                while (ReadFile(pipe, buffer.data(), static_cast<DWORD>(buffer.size()), &read, nullptr) && read != 0)
                {
                    std::lock_guard lock{ _outputMutex };
                    const auto remaining = destination.size() < limit ? limit - destination.size() : 0;
                    destination.append(buffer.data(), (std::min)(remaining, static_cast<size_t>(read)));
                }
            }

            wil::unique_handle _job;
            wil::unique_process_handle _process;
            wil::unique_handle _input;
            wil::unique_handle _output;
            wil::unique_handle _error;

            mutable std::mutex _stopMutex;
            bool _stopped{ false };

            std::mutex _inputMutex;
            std::condition_variable _inputCondition;
            std::deque<std::string> _inputFrames;
            size_t _queuedInputBytes{ 0 };
            bool _acceptingInput{ true };
            bool _closeInputWhenDrained{ false };
            bool _terminateInput{ false };
            std::atomic<bool> _writeFailed{ false };
            std::thread _writer;

            mutable std::mutex _outputMutex;
            std::string _standardOutput;
            std::string _standardError;
            std::thread _stdoutReader;
            std::thread _stderrReader;
        };

        class Win32PersistentProviderProcessFactory final : public IPersistentProviderProcessFactory
        {
        public:
            PersistentProviderLaunchResult Launch(
                const Manifest& manifest,
                const CommandRunner::Environment& environment) override
            {
                PersistentProviderLaunchResult result;
                CommandRunner::LaunchInfo launch;
                if (!CommandRunner::BuildLaunchInfo(manifest, launch, result.win32Error))
                {
                    return result;
                }

                Pipe input;
                Pipe output;
                Pipe error;
                if (!_CreatePipe(input, true) || !_CreatePipe(output, false) || !_CreatePipe(error, false))
                {
                    result.win32Error = GetLastError();
                    return result;
                }

                wil::unique_handle job{ CreateJobObjectW(nullptr, nullptr) };
                if (!job)
                {
                    result.win32Error = GetLastError();
                    return result;
                }
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
                limits.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE |
                    JOB_OBJECT_LIMIT_ACTIVE_PROCESS |
                    JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                limits.BasicLimitInformation.ActiveProcessLimit = MaximumPersistentActiveProcesses;
                limits.ProcessMemoryLimit = MaximumPersistentProcessMemory;
                if (!SetInformationJobObject(
                        job.get(),
                        JobObjectExtendedLimitInformation,
                        &limits,
                        sizeof(limits)))
                {
                    result.win32Error = GetLastError();
                    return result;
                }

                STARTUPINFOEXW startup{};
                startup.StartupInfo.cb = sizeof(startup);
                startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
                startup.StartupInfo.hStdInput = input.read.get();
                startup.StartupInfo.hStdOutput = output.write.get();
                startup.StartupInfo.hStdError = error.write.get();

                SIZE_T attributeSize = 0;
                InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeSize);
                std::vector<std::byte> attributeStorage(attributeSize);
                startup.lpAttributeList = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(attributeStorage.data());
                if (!InitializeProcThreadAttributeList(startup.lpAttributeList, 1, 0, &attributeSize))
                {
                    result.win32Error = GetLastError();
                    return result;
                }
                const auto deleteAttributes = wil::scope_exit([&]() {
                    DeleteProcThreadAttributeList(startup.lpAttributeList);
                });
                const std::array inheritedHandles{
                    input.read.get(),
                    output.write.get(),
                    error.write.get(),
                };
                if (!UpdateProcThreadAttribute(
                        startup.lpAttributeList,
                        0,
                        PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                        const_cast<HANDLE*>(inheritedHandles.data()),
                        sizeof(inheritedHandles),
                        nullptr,
                        nullptr))
                {
                    result.win32Error = GetLastError();
                    return result;
                }

                auto environmentBlock = CommandRunner::BuildEnvironment(environment);
                PROCESS_INFORMATION processInfo{};
                if (!CreateProcessW(
                        launch.executable.c_str(),
                        launch.commandLine.data(),
                        nullptr,
                        nullptr,
                        TRUE,
                        CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                        environmentBlock.data(),
                        manifest.extensionRoot.c_str(),
                        &startup.StartupInfo,
                        &processInfo))
                {
                    result.win32Error = GetLastError();
                    return result;
                }
                wil::unique_process_handle process{ processInfo.hProcess };
                wil::unique_handle thread{ processInfo.hThread };
                if (!AssignProcessToJobObject(job.get(), process.get()))
                {
                    result.win32Error = GetLastError();
                    TerminateProcess(process.get(), result.win32Error);
                    return result;
                }

                input.read.reset();
                output.write.reset();
                error.write.reset();
                try
                {
                    result.process = std::make_shared<Win32PersistentProviderProcess>(
                        std::move(job),
                        std::move(process),
                        std::move(input.write),
                        std::move(output.read),
                        std::move(error.read));
                    static_cast<Win32PersistentProviderProcess*>(result.process.get())->StartThreads();
                }
                catch (const std::system_error& error)
                {
                    result.win32Error = static_cast<uint32_t>(error.code().value());
                    if (result.process)
                    {
                        result.process->Stop({}, std::chrono::milliseconds{ 0 });
                        result.process.reset();
                    }
                    return result;
                }
                catch (const std::bad_alloc&)
                {
                    result.win32Error = ERROR_NOT_ENOUGH_MEMORY;
                    if (result.process)
                    {
                        result.process->Stop({}, std::chrono::milliseconds{ 0 });
                        result.process.reset();
                    }
                    return result;
                }

                if (ResumeThread(thread.get()) == static_cast<DWORD>(-1))
                {
                    result.win32Error = GetLastError();
                    result.process->Stop({}, std::chrono::milliseconds{ 0 });
                    result.process.reset();
                }
                return result;
            }
        };
    }

    PersistentProviderSupervisor::PersistentProviderSupervisor(
        std::shared_ptr<IPersistentProviderProcessFactory> processFactory) :
        _processFactory{ processFactory ? std::move(processFactory) :
                                          std::make_shared<Win32PersistentProviderProcessFactory>() }
    {
        _worker = std::thread{ [this]() { _Worker(); } };
    }

    PersistentProviderSupervisor::~PersistentProviderSupervisor()
    {
        Shutdown();
    }

    size_t PersistentProviderSupervisor::KeyHash::operator()(const PersistentProviderKey& key) const noexcept
    {
        const auto first = std::hash<std::string>{}(key.providerId);
        const auto second = std::hash<std::string>{}(key.sessionId);
        constexpr auto hashCombineConstant = static_cast<size_t>(
            sizeof(size_t) == sizeof(uint64_t) ?
                0x9e3779b97f4a7c15ull :
                0x9e3779b9ull);
        return first ^ (second + hashCombineConstant + (first << 6) + (first >> 2));
    }

    std::chrono::milliseconds PersistentProviderSupervisor::BackoffForFailure(
        const size_t consecutiveFailures) noexcept
    {
        constexpr std::array delays{
            std::chrono::seconds{ 1 },
            std::chrono::seconds{ 2 },
            std::chrono::seconds{ 4 },
            std::chrono::seconds{ 8 },
            std::chrono::seconds{ 16 },
            std::chrono::seconds{ 30 },
        };
        if (consecutiveFailures == 0)
        {
            return std::chrono::milliseconds{ 0 };
        }
        return delays[(std::min)(consecutiveFailures - 1, delays.size() - 1)];
    }

    void PersistentProviderSupervisor::SetEventCallback(EventCallback callback)
    {
        std::lock_guard lock{ _callbackMutex };
        _eventCallback = std::move(callback);
    }

    void PersistentProviderSupervisor::StartOrRefresh(
        PersistentProviderKey key,
        Manifest manifest,
        CommandRunner::Environment environment,
        FrameFactory frameFactory,
        const uint64_t requestGeneration,
        const bool forceRestart,
        const bool resetBackoff)
    {
        _Queue(
            [this,
             key = std::move(key),
             manifest = std::move(manifest),
             environment = std::move(environment),
             frameFactory = std::move(frameFactory),
             requestGeneration,
             forceRestart,
             resetBackoff]() mutable {
                _StartOrRefresh(
                    key,
                    std::move(manifest),
                    std::move(environment),
                    std::move(frameFactory),
                    requestGeneration,
                    forceRestart,
                    resetBackoff);
            });
    }

    void PersistentProviderSupervisor::SendLease(
        PersistentProviderKey key,
        const uint64_t instanceGeneration,
        const uint64_t requestGeneration,
        std::string frame)
    {
        _Queue(
            [this,
             key = std::move(key),
             instanceGeneration,
             requestGeneration,
             frame = std::move(frame)]() mutable {
                _SendLease(key, instanceGeneration, requestGeneration, std::move(frame));
            });
    }

    void PersistentProviderSupervisor::Stop(PersistentProviderKey key)
    {
        _Queue([this, key = std::move(key)]() {
            _StopMatching([&](const auto& current) { return current == key; });
        });
    }

    void PersistentProviderSupervisor::StopProvider(std::string providerId)
    {
        _Queue([this, providerId = std::move(providerId)]() {
            _StopMatching([&](const auto& key) { return key.providerId == providerId; });
        });
    }

    void PersistentProviderSupervisor::StopSession(std::string sessionId)
    {
        _Queue([this, sessionId = std::move(sessionId)]() {
            _StopMatching([&](const auto& key) { return key.sessionId == sessionId; });
        });
    }

    void PersistentProviderSupervisor::Shutdown() noexcept
    {
        {
            std::lock_guard lock{ _queueMutex };
            if (_stopping)
            {
                return;
            }
            _stopping = true;
        }
        _queueCondition.notify_one();
        if (_worker.joinable())
        {
            _worker.join();
        }
    }

    void PersistentProviderSupervisor::_Queue(std::function<void()> action)
    {
        {
            std::lock_guard lock{ _queueMutex };
            if (_stopping)
            {
                return;
            }
            _actions.emplace_back(std::move(action));
        }
        _queueCondition.notify_one();
    }

    void PersistentProviderSupervisor::_Worker()
    {
        for (;;)
        {
            std::function<void()> action;
            {
                std::unique_lock lock{ _queueMutex };
                _queueCondition.wait_for(lock, ProcessPollInterval, [&]() {
                    return _stopping || !_actions.empty();
                });
                if (_stopping)
                {
                    break;
                }
                if (!_actions.empty())
                {
                    action = std::move(_actions.front());
                    _actions.pop_front();
                }
            }
            if (action)
            {
                try
                {
                    action();
                }
                catch (...)
                {
                    OutputDebugStringW(L"Rich Tabs persistent provider action failed.\n");
                }
            }
            _CheckProcesses();
        }

        _StopMatching([](const auto&) { return true; });
    }

    void PersistentProviderSupervisor::_StartOrRefresh(
        const PersistentProviderKey& key,
        Manifest manifest,
        CommandRunner::Environment environment,
        FrameFactory frameFactory,
        const uint64_t requestGeneration,
        const bool forceRestart,
        const bool resetBackoff)
    {
        auto& instance = _instances[key];
        if (forceRestart && instance.process)
        {
            _StopInstance(key, instance);
        }
        instance.manifest = std::move(manifest);
        instance.environment = std::move(environment);
        instance.requestGeneration = requestGeneration;
        if (resetBackoff)
        {
            instance.consecutiveFailures = 0;
            instance.restartAt.reset();
        }
        if (instance.process && !instance.process->IsRunning())
        {
            _ScheduleRestart(
                key,
                instance,
                PersistentProviderEventKind::ProcessExited,
                ERROR_PROCESS_ABORTED,
                instance.process->StandardError());
            instance.process->Stop({}, std::chrono::milliseconds{ 0 });
            instance.process.reset();
            instance.controlStarted = false;
        }

        auto started = false;
        if (!instance.process)
        {
            if (instance.restartAt &&
                std::chrono::steady_clock::now() < *instance.restartAt &&
                !resetBackoff)
            {
                return;
            }

            instance.restartAt.reset();
            PersistentProviderLaunchResult launch;
            try
            {
                launch = _processFactory->Launch(instance.manifest, instance.environment);
            }
            catch (const std::bad_alloc&)
            {
                launch.win32Error = ERROR_NOT_ENOUGH_MEMORY;
            }
            catch (const std::system_error& error)
            {
                launch.win32Error = static_cast<uint32_t>(error.code().value());
            }
            if (!launch.process)
            {
                _ScheduleRestart(
                    key,
                    instance,
                    PersistentProviderEventKind::LaunchFailed,
                    launch.win32Error,
                    {});
                return;
            }
            instance.process = std::move(launch.process);
            instance.instanceGeneration = _nextInstanceGeneration++;
            instance.startedAt = std::chrono::steady_clock::now();
            instance.controlStarted = false;
        }

        started = !instance.controlStarted;
        const auto frame = frameFactory(instance.instanceGeneration, started);
        if (!frame)
        {
            instance.process->Stop({}, std::chrono::milliseconds{ 0 });
            instance.process.reset();
            instance.controlStarted = false;
            return;
        }
        if (!instance.process->Send(*frame))
        {
            _ScheduleRestart(
                key,
                instance,
                PersistentProviderEventKind::ControlWriteFailed,
                ERROR_WRITE_FAULT,
                instance.process->StandardError());
            instance.process->Stop({}, std::chrono::milliseconds{ 0 });
            instance.process.reset();
            instance.controlStarted = false;
        }
        else
        {
            instance.controlStarted = true;
            if (started)
            {
                _Emit(PersistentProviderEvent{
                    PersistentProviderEventKind::Started,
                    key,
                    instance.instanceGeneration,
                    instance.requestGeneration,
                });
            }
        }
    }

    void PersistentProviderSupervisor::_SendLease(
        const PersistentProviderKey& key,
        const uint64_t instanceGeneration,
        const uint64_t requestGeneration,
        std::string frame)
    {
        const auto found = _instances.find(key);
        if (found == _instances.end())
        {
            _Emit(PersistentProviderEvent{
                PersistentProviderEventKind::LeaseDropped,
                key,
                instanceGeneration,
                requestGeneration,
                std::chrono::milliseconds{ 0 },
                ERROR_NOT_FOUND,
            });
            return;
        }
        if (found->second.instanceGeneration != instanceGeneration)
        {
            _Emit(PersistentProviderEvent{
                PersistentProviderEventKind::LeaseDropped,
                key,
                instanceGeneration,
                requestGeneration,
                std::chrono::milliseconds{ 0 },
                ERROR_REVISION_MISMATCH,
            });
            return;
        }
        if (!found->second.process || !found->second.process->IsRunning())
        {
            _Emit(PersistentProviderEvent{
                PersistentProviderEventKind::LeaseDropped,
                key,
                instanceGeneration,
                requestGeneration,
                std::chrono::milliseconds{ 0 },
                ERROR_PROCESS_ABORTED,
            });
            return;
        }
        if (!found->second.process->Send(std::move(frame)))
        {
            _ScheduleRestart(
                key,
                found->second,
                PersistentProviderEventKind::ControlWriteFailed,
                ERROR_WRITE_FAULT,
                found->second.process->StandardError());
            found->second.process->Stop({}, std::chrono::milliseconds{ 0 });
            found->second.process.reset();
            found->second.controlStarted = false;
        }
    }

    void PersistentProviderSupervisor::_StopMatching(
        const std::function<bool(const PersistentProviderKey&)>& predicate)
    {
        for (auto current = _instances.begin(); current != _instances.end();)
        {
            if (predicate(current->first))
            {
                _StopInstance(current->first, current->second);
                current = _instances.erase(current);
            }
            else
            {
                ++current;
            }
        }
    }

    void PersistentProviderSupervisor::_StopInstance(
        const PersistentProviderKey& key,
        Instance& instance) noexcept
    {
        if (!instance.process)
        {
            return;
        }
        std::string stopFrame;
        if (const auto serialized = SerializeControlFrame(
                ControlMessageKind::Stop,
                std::nullopt,
                {},
                instance.manifest))
        {
            stopFrame = *serialized.value;
        }
        else
        {
            _Emit(PersistentProviderEvent{
                PersistentProviderEventKind::StopFrameFailed,
                key,
                instance.instanceGeneration,
                instance.requestGeneration,
                std::chrono::milliseconds{ 0 },
                ERROR_INVALID_DATA,
            });
        }
        instance.process->Stop(std::move(stopFrame), GracefulStopTimeout);
        instance.process.reset();
        instance.controlStarted = false;
        instance.restartAt.reset();
        _Emit(PersistentProviderEvent{
            PersistentProviderEventKind::Stopped,
            key,
            instance.instanceGeneration,
            instance.requestGeneration,
        });
    }

    void PersistentProviderSupervisor::_ScheduleRestart(
        const PersistentProviderKey& key,
        Instance& instance,
        const PersistentProviderEventKind eventKind,
        const uint32_t win32Error,
        std::string standardError)
    {
        if (instance.startedAt != std::chrono::steady_clock::time_point{} &&
            std::chrono::steady_clock::now() - instance.startedAt >= StableProcessLifetime)
        {
            instance.consecutiveFailures = 0;
        }
        instance.startedAt = {};
        ++instance.consecutiveFailures;
        const auto retryAfter = BackoffForFailure(instance.consecutiveFailures);
        instance.restartAt = std::chrono::steady_clock::now() + retryAfter;
        _Emit(PersistentProviderEvent{
            eventKind,
            key,
            instance.instanceGeneration,
            instance.requestGeneration,
            retryAfter,
            win32Error,
            std::move(standardError),
        });
    }

    void PersistentProviderSupervisor::_CheckProcesses()
    {
        const auto now = std::chrono::steady_clock::now();
        for (auto& [key, instance] : _instances)
        {
            if (instance.process && !instance.process->IsRunning())
            {
                const auto standardError = instance.process->StandardError();
                instance.process->Stop({}, std::chrono::milliseconds{ 0 });
                instance.process.reset();
                instance.controlStarted = false;
                _ScheduleRestart(
                    key,
                    instance,
                    PersistentProviderEventKind::ProcessExited,
                    ERROR_PROCESS_ABORTED,
                    standardError);
            }
            if (!instance.process &&
                instance.restartAt &&
                now >= *instance.restartAt)
            {
                instance.restartAt.reset();
                _Emit(PersistentProviderEvent{
                    PersistentProviderEventKind::RestartRequested,
                    key,
                    instance.instanceGeneration,
                    instance.requestGeneration,
                });
            }
        }
    }

    void PersistentProviderSupervisor::_Emit(PersistentProviderEvent event)
    {
        EventCallback callback;
        {
            std::lock_guard lock{ _callbackMutex };
            callback = _eventCallback;
        }
        if (callback)
        {
            try
            {
                callback(event);
            }
            catch (...)
            {
                OutputDebugStringW(L"Rich Tabs persistent provider callback failed.\n");
            }
        }
    }
}
