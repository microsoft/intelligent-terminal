// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "SharedWta.h"

#include <mutex>
#include <string>

#include "../WinRTUtils/inc/WtExeUtils.h"
#include "../inc/WtaProcess.h"

namespace winrt::TerminalApp::implementation
{
    SharedWta& SharedWta::Instance()
    {
        // Magic-static initialization is thread-safe in C++11+.
        static SharedWta s_instance;
        return s_instance;
    }

    SharedWta::~SharedWta()
    {
        // Process is exiting; tear wta down deterministically via
        // KILL_ON_JOB_CLOSE rather than letting handles leak.
        //
        // Wait callback synchronisation: cancel the wait WITH a
        // blocking unregister BEFORE we touch the fields it might
        // read. Without this, an in-flight callback could deref
        // `this` after the destructor finished — UAF.
        HANDLE waitToCancel = nullptr;
        {
            std::lock_guard lock{ _mtx };
            waitToCancel = _waitHandle;
            _waitHandle = nullptr;
        }
        if (waitToCancel)
        {
            UnregisterWaitEx(waitToCancel, INVALID_HANDLE_VALUE);
        }
        std::lock_guard lock{ _mtx };
        _job.reset();
        _process.reset();
        _pid = 0;
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

    std::wstring_view SharedWta::MasterPipeName() const noexcept
    {
        std::lock_guard lock{ _mtx };
        return _masterPipeName;
    }

    bool SharedWta::AcquirePane(const std::wstring_view wtaPath,
                                std::span<const std::wstring> extraArgs)
    {
        if (wtaPath.empty())
        {
            return false;
        }

        std::lock_guard lock{ _mtx };

        if (!_process.is_valid())
        {
            if (!_SpawnLocked(wtaPath, extraArgs))
            {
                return false;
            }
        }
        ++_refCount;
        return true;
    }

    void SharedWta::ReleasePane()
    {
        std::lock_guard lock{ _mtx };
        if (_refCount == 0)
        {
            return;
        }
        if (--_refCount == 0 && _process.is_valid())
        {
            _CleanupLocked();
        }
    }

    bool SharedWta::Restart()
    {
        std::lock_guard lock{ _mtx };

        // Nothing running → nothing to restart. Caller's surrounding
        // teardown+reopen path will trigger the usual lazy `AcquirePane`
        // spawn anyway, so this is a benign no-op (not an error).
        if (!_process.is_valid())
        {
            return true;
        }

        // Dedup the multi-window fan-out. `/restart` arrives via
        // `_dispatchRestartAgentStackToPage`, which calls
        // `OnRestartAgentStackRequested` (and thus `Restart()`) on EVERY
        // window's UI thread. Without this guard, window B's Restart kills
        // window A's just-spawned master, breaking the freshly-reopened
        // helper in window A. 500 ms is comfortably larger than the typical
        // UI-thread RunAsync hop (the 07:32 log showed a 240 ms gap between
        // windows) and tiny compared to any human-driven legitimate "two
        // restarts in a row".
        if (_lastRespawn &&
            std::chrono::steady_clock::now() - *_lastRespawn < std::chrono::milliseconds(500))
        {
            return true;
        }

        // No cached args means we've never successfully spawned in this
        // process, which contradicts `_process.is_valid()` — defensive
        // bail rather than spawning with empty wtaPath.
        if (_cachedWtaPath.empty())
        {
            return false;
        }

        // Drop the Job first so KILL_ON_JOB_CLOSE reaps the old master +
        // every agent CLI descendant, then respawn under the same
        // _masterPipeName. Any helper that's about to be torn down (the
        // /restart caller closes every agent pane) sees its pipe go EOF
        // and exits naturally; any helper that races a reconnect against
        // the respawn finds the new master listening on the same name.
        // Refcount is left untouched on purpose — the caller is still
        // holding refs for the panes it's about to close-and-reopen, and
        // the matching ReleasePane / AcquirePane pair will balance out.
        _CleanupLocked();
        return _SpawnLocked(std::wstring_view{ _cachedWtaPath }, _cachedExtraArgs);
    }

    bool SharedWta::Restart(const std::wstring_view wtaPath,
                            std::span<const std::wstring> extraArgs)
    {
        if (wtaPath.empty())
        {
            return false;
        }

        std::lock_guard lock{ _mtx };

        // Nothing live to replace (e.g. settings changed while no pane
        // was open in any window). The next AcquirePane will _SpawnLocked
        // with freshly-built args anyway, so we don't need to touch the
        // cache here.
        if (!_process.is_valid())
        {
            return true;
        }

        // Respawn the master with the *new* args so the running agent
        // CLI is replaced with whatever the new settings demand. The
        // surrounding `_RebuildAgentStack` flow has already torn down
        // every agent pane in this window and is about to reopen one;
        // refcount is left alone for the same reason as the cached-args
        // overload — outgoing ReleasePane / incoming AcquirePane balance.
        _CleanupLocked();
        return _SpawnLocked(wtaPath, extraArgs);
    }

    bool SharedWta::_SpawnLocked(const std::wstring_view wtaPath,
                                 std::span<const std::wstring> extraArgs)
    {
        // Lazily allocate the master pipe name once per process. We
        // intentionally keep it across master respawns: helpers
        // spawned earlier may still hold the original pipe path on
        // their cmdline, and the new master must listen on that same
        // name so the helpers reconnect cleanly.
        if (_masterPipeName.empty())
        {
            GUID guid{};
            if (FAILED(CoCreateGuid(&guid)))
            {
                return false;
            }
            wchar_t buf[64]{};
            const auto written = StringFromGUID2(guid, buf, ARRAYSIZE(buf));
            if (written <= 0)
            {
                return false;
            }
            // StringFromGUID2 returns `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}`
            // — strip the braces for a cleaner pipe name.
            std::wstring_view raw{ buf, static_cast<size_t>(written - 1) };
            if (raw.size() >= 2 && raw.front() == L'{' && raw.back() == L'}')
            {
                raw = raw.substr(1, raw.size() - 2);
            }
            _masterPipeName = L"\\\\.\\pipe\\wta-master-";
            _masterPipeName.append(raw);
        }

        // Build the command line. Master mode owns the agent CLI and
        // listens on the named pipe for helper connections (see
        // doc/specs/Multi-window-agent-pane.md, "Target architecture").
        // extraArgs carries per-process settings (--agent, --agent-id,
        // --acp-model, --no-autofix, --language, ...) so the master
        // can pass them through to the agent CLI it spawns. Each
        // element is escaped here via QuoteAndEscapeCommandlineArg
        // so callers don't have to think about quoting.
        size_t argsBudget = 0;
        for (const auto& a : extraArgs)
        {
            // +3 covers leading space and the two surrounding quotes
            // that QuoteAndEscapeCommandlineArg always emits.
            argsBudget += a.size() + 3;
        }
        std::wstring commandline;
        commandline.reserve(wtaPath.size() + 64 + _masterPipeName.size() + argsBudget);
        commandline.push_back(L'"');
        commandline.append(wtaPath);
        commandline.append(L"\" --master \"");
        commandline.append(_masterPipeName);
        commandline.append(L"\"");
        for (const auto& arg : extraArgs)
        {
            // Skip empty values defensively — callers shouldn't push
            // them, but if a settings string is empty we'd otherwise
            // emit a bare `""` arg which the agent CLI would see as a
            // junk positional.
            if (arg.empty())
            {
                continue;
            }
            commandline.push_back(L' ');
            QuoteAndEscapeCommandlineArg(arg, commandline);
        }

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        // No stdio inheritance — wta's bytes flow to/from per-pane
        // conpty HANDLEs, not the process's own stdio.

        PROCESS_INFORMATION pi{};

        // CREATE_SUSPENDED so the child can be placed inside the Job
        // Object before it executes a single instruction. Without
        // this, a Terminal crash in the microseconds between
        // CreateProcessW and AssignProcessToJobObject would leak wta
        // (no job → no KILL_ON_JOB_CLOSE containment).
        DWORD creationFlags = CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;

        // Refresh the current process's PATH from the Windows registry
        // so the master (which inherits our env) sees PATH entries added
        // after Terminal launched (e.g. WinGet\Links after FRE installs
        // copilot). Using RefreshProcessPath + lpEnvironment=nullptr
        // preserves all process-only variables (WT_COM_CLSID, etc.)
        // that regenerate() would drop.
        try
        {
            ::Microsoft::Terminal::WtaProcess::RefreshProcessPath();
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
        }

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
        // wta's lifetime to ours. When the last pane releases (or
        // Terminal exits and the destructor runs), the job handle
        // drops and the OS terminates wta + every descendant it
        // spawned. Any failure here MUST TerminateProcess to avoid
        // leaking a suspended-then-uncontained wta.
        wil::unique_handle job{ CreateJobObjectW(nullptr, nullptr) };
        if (!job)
        {
            TerminateProcess(process.get(), 1);
            return false;
        }
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(job.get(),
                                     JobObjectExtendedLimitInformation,
                                     &limits,
                                     sizeof(limits)))
        {
            TerminateProcess(process.get(), 1);
            return false;
        }
        if (!AssignProcessToJobObject(job.get(), process.get()))
        {
            TerminateProcess(process.get(), 1);
            return false;
        }

        // Crash detection: register a thread-pool wait that fires
        // when wta exits for any reason. The callback flips state
        // back to "no wta" so the next AcquirePane respawns. Set up
        // BEFORE ResumeThread so the wait is in place by the time
        // the child actually starts running.
        //
        // Context is the PID, not a `this` pointer. The callback
        // dispatches via `Instance()` and uses the captured PID to
        // detect a stale registration (see `_OnProcessExited`'s
        // mismatch bail). Casting via `uintptr_t` is the canonical
        // PVOID-as-integer round trip.
        HANDLE waitHandle = nullptr;
        if (!RegisterWaitForSingleObject(
                &waitHandle,
                process.get(),
                &SharedWta::_OnProcessExitedThunk,
                reinterpret_cast<PVOID>(static_cast<uintptr_t>(pid)),
                INFINITE,
                WT_EXECUTEONLYONCE))
        {
            // Couldn't set up the watcher — proceed without auto-recovery
            // rather than fail the spawn. wta still runs; the user just
            // won't get a transparent respawn if it crashes.
            waitHandle = nullptr;
        }

        // Hand wta the go-ahead. After this point, any failure has to
        // route through the normal Release path / external crash path.
        ResumeThread(thread.get());

        _process = std::move(process);
        _job = std::move(job);
        _pid = pid;
        _waitHandle = waitHandle;

        // Cache the spawn inputs so `Restart()` can replay them. Overwrites
        // any prior cache: if a respawn after crash used different
        // settings (none today, but the path is here), the most recent
        // wins. Done at the very end so partial-failure paths above
        // leave the previous cache intact.
        _cachedWtaPath.assign(wtaPath);
        _cachedExtraArgs.assign(extraArgs.begin(), extraArgs.end());
        _lastRespawn = std::chrono::steady_clock::now();
        return true;
    }

    void SharedWta::_CleanupLocked()
    {
        // Order matters: drop the job FIRST so KILL_ON_JOB_CLOSE
        // terminates wta + descendants while we still hold a process
        // handle that lets us observe the termination if needed.
        _job.reset();
        _process.reset();
        if (_waitHandle)
        {
            // Non-blocking unregister. If the callback is in flight
            // it will take _mtx after we release it, observe an
            // invalid _process, and bail.
            UnregisterWaitEx(_waitHandle, nullptr);
            _waitHandle = nullptr;
        }
        _pid = 0;
    }

    void CALLBACK SharedWta::_OnProcessExitedThunk(PVOID context, BOOLEAN /*timedOut*/)
    {
        // `context` is the PID at registration time, packed via
        // `reinterpret_cast<PVOID>(static_cast<uintptr_t>(pid))`. Round
        // trip back and let `_OnProcessExited` compare against the
        // currently-registered PID to detect a stale callback.
        const auto observedPid = static_cast<DWORD>(reinterpret_cast<uintptr_t>(context));
        SharedWta::Instance()._OnProcessExited(observedPid);
    }

    void SharedWta::_OnProcessExited(DWORD observedPid)
    {
        // Runs on a Win32 thread-pool thread. wta has exited (crash,
        // OOM, manual kill). Clear our process record so the next
        // AcquirePane respawns. Existing panes that still hold refs
        // become zombies until their Closed handlers call
        // ReleasePane (which will then no-op the cleanup since
        // _process is already invalid).
        std::lock_guard lock{ _mtx };

        // Stale-callback bail. `_CleanupLocked` only does a non-blocking
        // `UnregisterWaitEx(nullptr)`, so a callback that was already
        // queued for the OLD master can still fire after `_SpawnLocked`
        // has installed a NEW master. The captured PID lets us tell:
        // when it doesn't match the live `_pid`, the callback is for
        // a previously-killed master and must not touch `_process` /
        // `_waitHandle` (which now belong to the new master).
        if (_pid != observedPid)
        {
            return;
        }

        if (!_process.is_valid())
        {
            // Race: Release already cleaned up before our callback
            // ran. Nothing to do.
            return;
        }
        _job.reset();
        _process.reset();
        if (_waitHandle)
        {
            // We're inside the wait callback — non-blocking
            // unregister is the documented pattern.
            UnregisterWaitEx(_waitHandle, nullptr);
            _waitHandle = nullptr;
        }
        _pid = 0;
    }
}
