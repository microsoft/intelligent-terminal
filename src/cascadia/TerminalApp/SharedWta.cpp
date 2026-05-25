// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "SharedWta.h"

#include <mutex>
#include <string>

namespace winrt::TerminalApp::implementation
{
    SharedWta& SharedWta::Instance()
    {
        // Magic-static initialization is thread-safe in C++11+.
        static SharedWta s_instance;
        return s_instance;
    }

    bool SharedWta::IsRunning() const noexcept
    {
        std::lock_guard lock{ _mtx };
        return _process.is_valid();
    }

    HANDLE SharedWta::ProcessHandle() const noexcept
    {
        std::lock_guard lock{ _mtx };
        return _process.is_valid() ? _process.get() : INVALID_HANDLE_VALUE;
    }

    DWORD SharedWta::ProcessId() const noexcept
    {
        std::lock_guard lock{ _mtx };
        return _pid;
    }

    bool SharedWta::EnsureRunning(const std::wstring_view wtaPath)
    {
        if (wtaPath.empty())
        {
            return false;
        }

        std::lock_guard lock{ _mtx };
        if (_process.is_valid())
        {
            return true;
        }

        // Build the command line. The new singleton lives entirely
        // off the agent-pane attach path; it does not bind any conpty
        // to its own stdio (each per-tab pane brings its own pair of
        // HANDLEs via `_internal.attach_pane`), so we pass --headless
        // to tell wta to skip Ratatui-on-stdout binding.
        std::wstring commandline;
        commandline.reserve(wtaPath.size() + 32);
        commandline.push_back(L'"');
        commandline.append(wtaPath);
        commandline.append(L"\" --headless");

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        // No stdio inheritance — wta's bytes flow to/from per-pane
        // conpty HANDLEs, not the process's own stdio. Leaving the
        // si.hStd* fields null is exactly the "no conpty client"
        // shape we want.

        PROCESS_INFORMATION pi{};

        // CREATE_NO_WINDOW so wta doesn't pop a console window when
        // the parent (Terminal) was launched from Explorer. The
        // ComServer connection still works without one — wta uses
        // WT_COM_CLSID to find IProtocolServer, not the console.
        DWORD creationFlags = CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT;

        std::wstring mutableCmdLine{ commandline };
        if (!CreateProcessW(
                /* lpApplicationName    */ nullptr,
                /* lpCommandLine        */ mutableCmdLine.data(),
                /* lpProcessAttributes  */ nullptr,
                /* lpThreadAttributes   */ nullptr,
                /* bInheritHandles      */ FALSE,
                /* dwCreationFlags      */ creationFlags,
                /* lpEnvironment        */ nullptr,
                /* lpCurrentDirectory   */ nullptr,
                /* lpStartupInfo        */ &si,
                /* lpProcessInformation */ &pi))
        {
            return false;
        }

        wil::unique_handle process{ pi.hProcess };
        wil::unique_handle thread{ pi.hThread };
        const auto pid = pi.dwProcessId;

        // Containment: a Job Object with KILL_ON_JOB_CLOSE binds
        // wta's lifetime to ours. When Terminal exits (or this
        // singleton is destroyed), the job handle drops and the OS
        // terminates wta + every descendant it spawned (each
        // per-tab agent CLI child). Without this, a crashed
        // Terminal would leave orphan wta + claude/copilot/gemini
        // processes behind.
        wil::unique_handle job{ CreateJobObjectW(nullptr, nullptr) };
        if (!job)
        {
            // Spawn already succeeded; we'd leak wta on abnormal
            // Terminal exit, which is bad but not catastrophic.
            // Surface failure to the caller so they can log.
            return false;
        }
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(job.get(),
                                     JobObjectExtendedLimitInformation,
                                     &limits,
                                     sizeof(limits)))
        {
            return false;
        }
        if (!AssignProcessToJobObject(job.get(), process.get()))
        {
            return false;
        }

        // Commit only after all fallible operations have succeeded —
        // if we returned early between Assign and these moves, the
        // local `job` destructor would fire KILL_ON_JOB_CLOSE on the
        // freshly-spawned wta.
        _process = std::move(process);
        _job = std::move(job);
        _pid = pid;
        return true;
    }
}
