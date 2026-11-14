// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "WtaProcess.h"
#include <array>
#include <chrono>
#include <mutex>
#include <vector>

namespace Microsoft::Terminal::WtaProcess
{
    // A private per-window planner. Calls run on background threads; only one
    // bounded request owns the pipes at a time. A cancelled exchange poisons the
    // transport and kills its job so no late answer can satisfy another request.
    class OmniPlannerProcess
    {
    public:
        ~OmniPlannerProcess()
        {
            _Reset();
        }

        bool BeginWarmup() noexcept { return !_warming.exchange(true); }
        void EndWarmup() noexcept { _warming.store(false); }

        CaptureResult Exchange(const std::wstring& path, std::string_view request, const std::atomic<bool>* cancellation = nullptr)
        {
            CaptureResult result;
            const auto started = GetTickCount64();
            const auto cancelled = [&]() { return cancellation && cancellation->load(); };
            const auto expired = [&]() { return GetTickCount64() - started >= 100'000; };
            std::unique_lock lock{ _mutex, std::defer_lock };
            while (!lock.try_lock_for(std::chrono::milliseconds{ 25 }))
            {
                if (cancelled() || expired())
                {
                    result.cancelled = cancelled();
                    result.timedOut = !result.cancelled;
                    return result;
                }
            }
            try
            {
                THROW_HR_IF(E_INVALIDARG, request.size() > 24 * 1024 || request.find('\n') != std::string_view::npos);
                if (cancelled() || expired())
                {
                    result.cancelled = cancelled();
                    result.timedOut = !result.cancelled;
                    return result;
                }
                if (!_process || WaitForSingleObject(_process.get(), 0) != WAIT_TIMEOUT)
                {
                    _Reset();
                    _Start(path);
                }
                auto frame = std::string{ request } + '\n';
                DWORD written{};
                THROW_IF_WIN32_BOOL_FALSE(WriteFile(_input.get(), frame.data(), static_cast<DWORD>(frame.size()), &written, nullptr));
                THROW_HR_IF(E_FAIL, written != frame.size());
                std::string response;
                std::array<char, 4096> buffer;
                for (;;)
                {
                    if (cancelled() || expired())
                    {
                        result.cancelled = cancelled();
                        result.timedOut = !result.cancelled;
                        _Reset();
                        return result;
                    }
                    DWORD available{};
                    THROW_IF_WIN32_BOOL_FALSE(PeekNamedPipe(_output.get(), nullptr, 0, nullptr, &available, nullptr));
                    if (available)
                    {
                        DWORD read{};
                        THROW_IF_WIN32_BOOL_FALSE(ReadFile(_output.get(), buffer.data(), (std::min)(available, static_cast<DWORD>(buffer.size())), &read, nullptr));
                        THROW_HR_IF(E_FAIL, read == 0);
                        response.append(buffer.data(), read);
                        THROW_HR_IF(E_FAIL, response.size() > 64 * 1024);
                        if (const auto end = response.find('\n'); end != std::string::npos)
                        {
                            THROW_HR_IF(E_FAIL, end + 1 != response.size());
                            response.resize(end);
                            result.completed = true;
                            result.exitCode = response.starts_with("{\"error\":") ? 1 : 0;
                            result.output = std::move(response);
                            if (result.exitCode != 0)
                            {
                                _Reset();
                            }
                            return result;
                        }
                    }
                    THROW_HR_IF(E_FAIL, WaitForSingleObject(_process.get(), available ? 0 : 25) != WAIT_TIMEOUT);
                }
            }
            catch (...)
            {
                LOG_HR(wil::ResultFromCaughtException());
                _Reset();
                return result;
            }
        }

    private:
        void _Reset() noexcept
        {
            _job.reset();
            if (_process && WaitForSingleObject(_process.get(), 0) == WAIT_TIMEOUT)
            {
                // Also covers a suspended child whose job assignment failed.
                LOG_IF_WIN32_BOOL_FALSE(TerminateProcess(_process.get(), ERROR_CANCELLED));
            }
            _input.reset();
            _output.reset();
            _process.reset();
        }

        void _Start(const std::wstring& path)
        {
            SECURITY_ATTRIBUTES security{ sizeof(security), nullptr, TRUE };
            wil::unique_handle childInput;
            wil::unique_handle childOutput;
            // One outstanding request fits without waiting for the server to
            // finish disposing the previous session and read its next frame.
            THROW_IF_WIN32_BOOL_FALSE(CreatePipe(childInput.addressof(), _input.addressof(), &security, 64 * 1024));
            THROW_IF_WIN32_BOOL_FALSE(CreatePipe(_output.addressof(), childOutput.addressof(), &security, 64 * 1024));
            THROW_IF_WIN32_BOOL_FALSE(SetHandleInformation(_input.get(), HANDLE_FLAG_INHERIT, 0));
            THROW_IF_WIN32_BOOL_FALSE(SetHandleInformation(_output.get(), HANDLE_FLAG_INHERIT, 0));
            wil::unique_hfile error{ CreateFileW(L"NUL", GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, &security, OPEN_EXISTING, 0, nullptr) };
            THROW_LAST_ERROR_IF(!error);
            _job.reset(CreateJobObjectW(nullptr, nullptr));
            THROW_LAST_ERROR_IF(!_job);
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            THROW_IF_WIN32_BOOL_FALSE(SetInformationJobObject(_job.get(), JobObjectExtendedLimitInformation, &limits, sizeof(limits)));

            STARTUPINFOEXW startup{};
            startup.StartupInfo.cb = sizeof(startup);
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = childInput.get();
            startup.StartupInfo.hStdOutput = childOutput.get();
            startup.StartupInfo.hStdError = error.get();
            SIZE_T size{};
            InitializeProcThreadAttributeList(nullptr, 1, 0, &size);
            std::vector<std::byte> storage(size);
            startup.lpAttributeList = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(storage.data());
            THROW_IF_WIN32_BOOL_FALSE(InitializeProcThreadAttributeList(startup.lpAttributeList, 1, 0, &size));
            const auto deleteAttributes = wil::scope_exit([&]() { DeleteProcThreadAttributeList(startup.lpAttributeList); });
            std::array<HANDLE, 3> inherited{ childInput.get(), childOutput.get(), error.get() };
            THROW_IF_WIN32_BOOL_FALSE(UpdateProcThreadAttribute(startup.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherited.data(), sizeof(inherited), nullptr, nullptr));
            auto commandLine = L"\"" + path + L"\" omni-plan --serve";
            PROCESS_INFORMATION info{};
            THROW_IF_WIN32_BOOL_FALSE(CreateProcessW(path.c_str(), commandLine.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT, nullptr, nullptr, &startup.StartupInfo, &info));
            _process.reset(info.hProcess);
            wil::unique_handle thread{ info.hThread };
            THROW_IF_WIN32_BOOL_FALSE(AssignProcessToJobObject(_job.get(), _process.get()));
            THROW_LAST_ERROR_IF(ResumeThread(thread.get()) == static_cast<DWORD>(-1));
        }

        std::timed_mutex _mutex;
        std::atomic<bool> _warming{ false };
        wil::unique_handle _job;
        wil::unique_handle _process;
        wil::unique_handle _input;
        wil::unique_handle _output;
    };
}
