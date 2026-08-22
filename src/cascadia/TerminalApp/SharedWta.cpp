// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "SharedWta.h"

#include <mutex>
#include <string>

#include "../WinRTUtils/inc/WtExeUtils.h"
#include "../inc/WtaProcess.h"
#include "AgentPaneLog.h"

namespace
{
    // Must remain strictly greater than WTA's 15-second
    // SESSION_CLOSE_TIMEOUT in tools/wta/src/master/mod.rs.
    constexpr auto WtaSessionCloseGracePeriod{ std::chrono::seconds{ 16 } };
    constexpr auto WtaMasterExitTimeout{ std::chrono::seconds{ 3 } };
    constexpr auto WtaMasterForcedExitTimeout{ std::chrono::seconds{ 5 } };
}

namespace winrt::TerminalApp::implementation::details
{
    uint64_t LiveObjectGenerationTracker::Get(const winrt::Windows::Foundation::IInspectable& object)
    {
        std::lock_guard lock{ _mutex };
        for (auto entry = _entries.begin(); entry != _entries.end();)
        {
            if (const auto live = entry->object.get())
            {
                if (live == object)
                {
                    return entry->generation;
                }
                ++entry;
            }
            else
            {
                entry = _entries.erase(entry);
            }
        }

        const auto generation = ++_nextGeneration;
        _entries.emplace_back(winrt::make_weak(object), generation);
        return generation;
    }

    std::string RetirementCoordinator::_CreateIdLocked(const std::string_view kind)
    {
        auto id = std::to_string(GetCurrentProcessId());
        id.push_back('-');
        id.append(kind);
        id.push_back('-');
        id.append(std::to_string(++_nextOperationId));
        return id;
    }

    std::string RetirementCoordinator::CreateRequestId()
    {
        std::lock_guard lock{ _mutex };
        return _CreateIdLocked("request");
    }

    RetirementRegistration RetirementCoordinator::Register(
        const bool scopeAll,
        const std::string_view /*reason*/,
        const std::string_view requestId)
    {
        std::lock_guard lock{ _mutex };

        if (scopeAll && !requestId.empty())
        {
            if (const auto existing = _allOperationsByRequest.find(std::string{ requestId });
                existing != _allOperationsByRequest.end())
            {
                if (const auto operation = _operations.find(existing->second);
                    operation != _operations.end())
                {
                    if (operation->second.recordedInHistory)
                    {
                        const auto completed = std::find(
                            _completedOperations.begin(),
                            _completedOperations.end(),
                            operation->first);
                        if (completed != _completedOperations.end())
                        {
                            _completedOperations.erase(completed);
                        }
                        operation->second.recordedInHistory = false;
                    }
                    ++operation->second.continuationCount;
                    return {
                        operation->first,
                        false,
                        operation->second.completed,
                    };
                }
            }
        }

        auto operationId = _CreateIdLocked("operation");
        Operation operation;
        if (scopeAll && !requestId.empty())
        {
            operation.requestId = requestId;
            _allOperationsByRequest[*operation.requestId] = operationId;
        }
        operation.continuationCount = 1;
        _operations.emplace(operationId, std::move(operation));
        return { std::move(operationId), true, false };
    }

    void RetirementCoordinator::_EraseLocked(const std::string& operationId)
    {
        if (const auto operation = _operations.find(operationId);
            operation != _operations.end())
        {
            if (operation->second.requestId)
            {
                if (const auto request = _allOperationsByRequest.find(*operation->second.requestId);
                    request != _allOperationsByRequest.end() && request->second == operationId)
                {
                    _allOperationsByRequest.erase(request);
                }
            }
            _operations.erase(operation);
        }
    }

    void RetirementCoordinator::_FinalizeCompletedLocked(
        const std::unordered_map<std::string, Operation>::iterator operation)
    {
        if (!operation->second.completed || operation->second.continuationCount != 0)
        {
            return;
        }

        if (operation->second.expireAfterContinuations)
        {
            const auto operationId = operation->first;
            _EraseLocked(operationId);
        }
        else if (!operation->second.recordedInHistory)
        {
            operation->second.recordedInHistory = true;
            _completedOperations.emplace_back(operation->first);
            _PruneCompletedLocked();
        }
    }

    void RetirementCoordinator::_PruneCompletedLocked()
    {
        while (_completedOperations.size() > CompletedHistoryLimit)
        {
            auto operationId = std::move(_completedOperations.front());
            _completedOperations.pop_front();
            _EraseLocked(operationId);
        }
    }

    bool RetirementCoordinator::Complete(
        const std::string_view operationId,
        const bool expireAfterContinuations)
    {
        std::lock_guard lock{ _mutex };
        if (const auto operation = _operations.find(std::string{ operationId });
            operation != _operations.end())
        {
            operation->second.completed = true;
            operation->second.expireAfterContinuations =
                operation->second.expireAfterContinuations || expireAfterContinuations;
            _FinalizeCompletedLocked(operation);
            return true;
        }
        return false;
    }

    void RetirementCoordinator::ReleaseContinuation(const std::string_view operationId)
    {
        std::lock_guard lock{ _mutex };
        if (const auto operation = _operations.find(std::string{ operationId });
            operation != _operations.end())
        {
            if (operation->second.continuationCount != 0)
            {
                --operation->second.continuationCount;
            }
            if (operation->second.continuationCount == 0 && !operation->second.completed)
            {
                const auto id = operation->first;
                _EraseLocked(id);
            }
            else
            {
                _FinalizeCompletedLocked(operation);
            }
        }
    }

    void RetirementCoordinator::Expire(const std::string_view operationId)
    {
        std::lock_guard lock{ _mutex };
        const std::string id{ operationId };
        _EraseLocked(id);
    }

    bool RetirementCoordinator::ClaimAction(const std::string_view operationId, const std::string_view action)
    {
        std::lock_guard lock{ _mutex };
        if (const auto operation = _operations.find(std::string{ operationId });
            operation != _operations.end())
        {
            return operation->second.claimedActions.emplace(action).second;
        }
        return false;
    }

    bool TabRetirementTracker::BeginRebuild(const std::string_view tabId)
    {
        return _closeRequested.emplace(tabId, false).second;
    }

    bool TabRetirementTracker::RequestClose(const std::string_view tabId)
    {
        const auto [entry, inserted] = _closeRequested.emplace(tabId, true);
        entry->second = true;
        return inserted;
    }

    bool TabRetirementTracker::Complete(const std::string_view tabId)
    {
        const auto entry = _closeRequested.find(std::string{ tabId });
        if (entry == _closeRequested.end())
        {
            return false;
        }
        const bool shouldReopen = !entry->second;
        _closeRequested.erase(entry);
        return shouldReopen;
    }

    void RestartSuppressionTracker::Mark(const std::string_view tabId)
    {
        _marks[std::string{ tabId }] = std::chrono::steady_clock::now();
    }

    void RestartSuppressionTracker::Clear(const std::string_view tabId)
    {
        _marks.erase(std::string{ tabId });
    }

    bool RestartSuppressionTracker::Consume(const std::string_view tabId)
    {
        const auto mark = _marks.find(std::string{ tabId });
        if (mark == _marks.end())
        {
            return false;
        }
        const auto age = std::chrono::steady_clock::now() - mark->second;
        _marks.erase(mark);
        return age < std::chrono::seconds{ 5 };
    }

    void CoalescedRequest::Queue(std::string requestId)
    {
        _requestId = std::move(requestId);
    }

    std::optional<std::string> CoalescedRequest::Take()
    {
        return std::exchange(_requestId, std::nullopt);
    }

    void CoalescedRequest::Clear()
    {
        _requestId.reset();
    }

    bool CoalescedRequest::Pending() const noexcept
    {
        return _requestId.has_value();
    }

    std::optional<std::wstring> BuildEnvironmentBlock(
        const std::span<const std::pair<std::wstring, std::wstring>> overrides) noexcept
    {
        try
        {
            if (overrides.empty())
            {
                return std::wstring{};
            }

            for (const auto& override : overrides)
            {
                if (!IsValidEnvironmentOverride(override.first, override.second))
                {
                    _agentPaneLog(
                        "rejecting invalid wta-master environment override name_length=" + std::to_string(override.first.size()));
                    return std::nullopt;
                }
            }

            const auto isOverridden = [&](const std::wstring_view name) {
                return std::ranges::any_of(overrides, [&](const auto& item) {
                    return _wcsicmp(std::wstring{ name }.c_str(), item.first.c_str()) == 0;
                });
            };

            std::vector<std::wstring> entries;
            const auto environment = GetEnvironmentStringsW();
            THROW_LAST_ERROR_IF_NULL(environment);
            const auto freeEnvironment = wil::scope_exit([&]() noexcept { FreeEnvironmentStringsW(environment); });

            for (const wchar_t* current = environment; *current;)
            {
                const std::wstring_view entry{ current };
                const auto separator = entry.find(L'=', entry.starts_with(L'=') ? 1 : 0);
                const auto name = separator == std::wstring_view::npos ? entry : entry.substr(0, separator);
                if (!isOverridden(name))
                {
                    entries.emplace_back(entry);
                }
                current += entry.size() + 1;
            }

            for (const auto& [name, value] : overrides)
            {
                entries.emplace_back(name + L'=' + value);
            }
            std::ranges::sort(entries, [](const auto& left, const auto& right) {
                return _wcsicmp(left.c_str(), right.c_str()) < 0;
            });

            std::wstring block;
            for (const auto& entry : entries)
            {
                block.append(entry);
                block.push_back(L'\0');
            }
            block.push_back(L'\0');
            return block;
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            return std::nullopt;
        }
    }

    bool ResumeSuspendedProcess(
        const HANDLE thread,
        const HANDLE process,
        HANDLE& waitHandle,
        const SuspendedProcessOperations& operations) noexcept
    {
        const auto resumeResult = operations.resumeThread(thread);
        if (resumeResult != static_cast<DWORD>(-1))
        {
            return true;
        }

        if (waitHandle)
        {
            operations.unregisterWait(waitHandle, nullptr);
            waitHandle = nullptr;
        }
        operations.terminateProcess(process, 1);
        return false;
    }

    bool EnsureProcessExitedBeforeRestart(
        const HANDLE process,
        const DWORD pid,
        const DWORD exitTimeoutMs,
        const DWORD forcedExitTimeoutMs,
        const ProcessRetirementOperations& operations)
    {
        const auto waitResult = operations.waitForSingleObject(process, exitTimeoutMs);
        if (waitResult == WAIT_OBJECT_0)
        {
            return true;
        }

        if (waitResult == WAIT_TIMEOUT)
        {
            _agentPaneLog(
                "wta-master did not exit before restart pid=" + std::to_string(pid) +
                " timeout_ms=" + std::to_string(exitTimeoutMs) + "; forcing termination");
        }
        else
        {
            const auto error = GetLastError();
            _agentPaneLog(
                "waiting for wta-master before restart failed pid=" + std::to_string(pid) +
                " result=" + std::to_string(waitResult) +
                " error=" + std::to_string(error) + "; forcing termination");
        }

        if (!operations.terminateProcess(process, 1))
        {
            const auto error = GetLastError();
            _agentPaneLog(
                "failed to terminate wta-master before restart pid=" + std::to_string(pid) +
                " error=" + std::to_string(error) + "; verifying process exit");
        }

        const auto reapResult = operations.waitForSingleObject(process, forcedExitTimeoutMs);
        if (reapResult != WAIT_OBJECT_0)
        {
            const auto error = reapResult == WAIT_FAILED ? GetLastError() : ERROR_SUCCESS;
            _agentPaneLog(
                "failed to reap wta-master before restart pid=" + std::to_string(pid) +
                " result=" + std::to_string(reapResult) +
                " error=" + std::to_string(error) + "; replacement suppressed");
            return false;
        }

        return true;
    }

    ProcessWaitGenerationTracker::Generation ProcessWaitGenerationTracker::Register(const DWORD pid) noexcept
    {
        do
        {
            ++_nextGeneration;
        } while (_nextGeneration == 0);

        _currentGeneration = _nextGeneration;
        _pid = pid;
        return _currentGeneration;
    }

    void ProcessWaitGenerationTracker::Retire() noexcept
    {
        _currentGeneration = 0;
        _pid = 0;
    }

    std::optional<DWORD> ProcessWaitGenerationTracker::Claim(const Generation generation) noexcept
    {
        if (generation == 0 || generation != _currentGeneration)
        {
            return std::nullopt;
        }

        const auto pid = _pid;
        Retire();
        return pid;
    }

    ProcessWaitGenerationTracker::Generation ProcessWaitGenerationTracker::Current() const noexcept
    {
        return _currentGeneration;
    }
}

namespace winrt::TerminalApp::implementation
{
    SharedWta& SharedWta::Instance()
    {
        // Initialization remains thread-safe, but this process singleton must
        // outlive delayed ReleasePaneAfterSessionClose coroutines. At process
        // exit Windows closes the Job handle, preserving KILL_ON_JOB_CLOSE
        // cleanup for the master and its descendants.
        static auto* const s_instance = new SharedWta;
        return *s_instance;
    }

    std::string SharedWta::CreateRetirementRequestId()
    {
        return _retirementCoordinator.CreateRequestId();
    }

    details::RetirementRegistration SharedWta::RegisterRetirement(
        const bool scopeAll,
        const std::string_view reason,
        const std::string_view requestId)
    {
        return _retirementCoordinator.Register(scopeAll, reason, requestId);
    }

    bool SharedWta::CompleteRetirement(
        const std::string_view operationId,
        const bool expireAfterContinuations)
    {
        return _retirementCoordinator.Complete(operationId, expireAfterContinuations);
    }

    void SharedWta::ReleaseRetirementContinuation(const std::string_view operationId)
    {
        _retirementCoordinator.ReleaseContinuation(operationId);
    }

    void SharedWta::ExpireRetirement(const std::string_view operationId)
    {
        _retirementCoordinator.Expire(operationId);
    }

    bool SharedWta::ClaimRetirementAction(const std::string_view operationId, const std::string_view action)
    {
        return _retirementCoordinator.ClaimAction(operationId, action);
    }

    uint64_t SharedWta::GetSettingsGeneration(const winrt::Windows::Foundation::IInspectable& settings)
    {
        return _settingsGenerations.Get(settings);
    }

    SharedWta::~SharedWta()
    {
        // Process is exiting, so a graceful per-session close can no longer
        // delay app shutdown. KILL_ON_JOB_CLOSE deterministically reclaims the
        // master, every agent CLI, and their MCP descendants without orphans.
        //
        // Wait callback synchronisation: cancel the wait WITH a
        // blocking unregister BEFORE we touch the fields it might
        // read. Without this, an in-flight callback could deref
        // `this` after the destructor finished — UAF.
        HANDLE waitToCancel = nullptr;
        {
            std::lock_guard lock{ _mtx };
            _waitGeneration.Retire();
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
                                std::span<const std::wstring> extraArgs,
                                std::span<const std::pair<std::wstring, std::wstring>> environment)
    {
        if (wtaPath.empty())
        {
            return false;
        }

        std::lock_guard lock{ _mtx };
        if (_spawnSuppressed)
        {
            return false;
        }

        // A new pane request after an unexpected master exit starts a fresh
        // master. Existing helpers do not reconnect; they exit when the old
        // pipe closes, so this cannot restore or cross-bind their sessions.
        if (!_process.is_valid())
        {
            if (!_SpawnLocked(wtaPath, extraArgs, environment))
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
        if (--_refCount == 0)
        {
            if (_process.is_valid())
            {
                _CleanupLocked();
            }
        }
    }

    winrt::fire_and_forget SharedWta::ReleasePaneAfterSessionClose()
    {
        co_await winrt::resume_after(WtaSessionCloseGracePeriod);
        Instance().ReleasePane();
    }

    bool SharedWta::Restart()
    {
        std::lock_guard lock{ _mtx };
        if (_spawnSuppressed)
        {
            return false;
        }

        // Nothing running → nothing to restart. Caller's surrounding
        // teardown+reopen path will trigger the usual lazy `AcquirePane`
        // spawn anyway, so this is a benign no-op (not an error).
        if (!_process.is_valid())
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

        return _RestartLocked(std::wstring_view{ _cachedWtaPath }, _cachedExtraArgs, _cachedEnvironment);
    }

    bool SharedWta::Restart(const std::wstring_view wtaPath,
                            std::span<const std::wstring> extraArgs,
                            std::span<const std::pair<std::wstring, std::wstring>> environment)
    {
        if (wtaPath.empty())
        {
            return false;
        }

        std::lock_guard lock{ _mtx };
        if (_spawnSuppressed)
        {
            return false;
        }

        // Nothing live to replace (e.g. settings changed while no pane
        // was open in any window). The next AcquirePane will _SpawnLocked
        // with freshly-built args anyway, so we don't need to touch the
        // cache here.
        if (!_process.is_valid())
        {
            return true;
        }

        // Settings reload is delivered to every window, and a page may defer
        // its rebuild until a terminal tab regains focus. If the live master
        // already has these exact trusted arguments, restarting it again is
        // both unnecessary and disruptive to helpers in other windows.
        const bool sameArgs = _cachedWtaPath == wtaPath &&
                              _cachedExtraArgs.size() == extraArgs.size() &&
                              std::equal(_cachedExtraArgs.begin(), _cachedExtraArgs.end(), extraArgs.begin()) &&
                              _cachedEnvironment.size() == environment.size() &&
                              std::equal(_cachedEnvironment.begin(), _cachedEnvironment.end(), environment.begin());
        if (sameArgs)
        {
            return true;
        }

        return _RestartLocked(wtaPath, extraArgs, environment);
    }

    bool SharedWta::_RestartLocked(
        const std::wstring_view wtaPath,
        std::span<const std::wstring> extraArgs,
        std::span<const std::pair<std::wstring, std::wstring>> environment)
    {
        // Own the next inputs independently of the cache. The cached-args
        // overload passes spans into member vectors that _SpawnLocked updates
        // after success.
        const std::wstring nextWtaPath{ wtaPath };
        const std::vector<std::wstring> nextExtraArgs{ extraArgs.begin(), extraArgs.end() };
        const std::vector<std::pair<std::wstring, std::wstring>> nextEnvironment{
            environment.begin(), environment.end()
        };

        // Closing the KILL_ON_JOB_CLOSE job requests termination, but process
        // teardown and named-pipe release are asynchronous. Keep the old
        // process handle until it is signaled so the replacement cannot race
        // the old master for the stable, process-wide pipe name. Holding _mtx
        // throughout also preserves cross-window restart deduplication.
        const auto retiredPid = _pid;
        auto retiredProcess = _CleanupLocked();
        if (retiredProcess &&
            !details::EnsureProcessExitedBeforeRestart(
                retiredProcess.get(),
                retiredPid,
                static_cast<DWORD>(std::chrono::duration_cast<std::chrono::milliseconds>(WtaMasterExitTimeout).count()),
                static_cast<DWORD>(std::chrono::duration_cast<std::chrono::milliseconds>(WtaMasterForcedExitTimeout).count())))
        {
            _spawnSuppressed = true;
            return false;
        }

        // Refcount is deliberately untouched. The surrounding pane teardown
        // and reopen balances its outgoing ReleasePane and incoming
        // AcquirePane after the replacement has claimed the shared pipe.
        return _SpawnLocked(nextWtaPath, nextExtraArgs, nextEnvironment);
    }

    bool SharedWta::_SpawnLocked(const std::wstring_view wtaPath,
                                 std::span<const std::wstring> extraArgs,
                                 std::span<const std::pair<std::wstring, std::wstring>> environment)
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
        // copilot). With no overrides, lpEnvironment=nullptr inherits it
        // directly; with overrides, BuildEnvironmentBlock clones it first.
        // Both preserve process-only variables (WT_COM_CLSID, etc.) that
        // regenerate() would drop.
        try
        {
            ::Microsoft::Terminal::WtaProcess::RefreshProcessPath();
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
        }

        auto environmentBlock = details::BuildEnvironmentBlock(environment);
        if (!environmentBlock)
        {
            return false;
        }

        std::wstring mutableCmdLine{ commandline };
        if (!CreateProcessW(
                /* lpApplicationName    */ nullptr,
                /* lpCommandLine        */ mutableCmdLine.data(),
                /* lpProcessAttributes  */ nullptr,
                /* lpThreadAttributes   */ nullptr,
                /* bInheritHandles      */ FALSE,
                /* dwCreationFlags      */ creationFlags,
                /* lpEnvironment        */ environmentBlock->empty() ? nullptr : environmentBlock->data(),
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
        // Terminal exits and Windows closes the final handle), the job handle
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
        // Context is a registration generation, not a PID or `this`
        // pointer. PID reuse cannot make a delayed callback match a
        // replacement wait, and the integer context has no lifetime to
        // manage while non-blocking unregister drains queued callbacks.
        HANDLE waitHandle = nullptr;
        const auto waitGeneration = _waitGeneration.Register(pid);
        if (!RegisterWaitForSingleObject(
                &waitHandle,
                process.get(),
                &SharedWta::_OnProcessExitedThunk,
                reinterpret_cast<PVOID>(waitGeneration),
                INFINITE,
                WT_EXECUTEONLYONCE))
        {
            // Couldn't set up the watcher — proceed without auto-recovery
            // rather than fail the spawn. wta still runs; the user just
            // won't get a transparent respawn if it crashes.
            waitHandle = nullptr;
            _waitGeneration.Retire();
        }

        // Hand wta the go-ahead. A resume failure leaves the child suspended,
        // so cancel its wait registration and terminate it before any process
        // state or spawn inputs are published.
        if (!details::ResumeSuspendedProcess(thread.get(), process.get(), waitHandle))
        {
            _waitGeneration.Retire();
            return false;
        }

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
        _cachedEnvironment.assign(environment.begin(), environment.end());
        return true;
    }

    wil::unique_handle SharedWta::_CleanupLocked()
    {
        // Order matters: drop the job FIRST so KILL_ON_JOB_CLOSE
        // terminates wta + descendants while we still hold a process
        // handle that lets restart observe termination and pipe release.
        // Deliberate teardown: the master is reaped silently (job close, no
        // console event), so it can't log its own death — record it here.
        _agentPaneLog("releasing wta-master pid=" + std::to_string(_pid) + " (deliberate teardown via KILL_ON_JOB_CLOSE)");
        auto process = std::move(_process);
        _job.reset();
        if (_waitHandle)
        {
            // Invalidate before the non-blocking unregister. A queued
            // callback can run after a replacement is spawned, but its
            // retired generation cannot claim the replacement state.
            _waitGeneration.Retire();
            UnregisterWaitEx(_waitHandle, nullptr);
            _waitHandle = nullptr;
        }
        else
        {
            _waitGeneration.Retire();
        }
        _pid = 0;
        return process;
    }

    void CALLBACK SharedWta::_OnProcessExitedThunk(PVOID context, BOOLEAN /*timedOut*/)
    {
        const auto generation = reinterpret_cast<details::ProcessWaitGenerationTracker::Generation>(context);
        SharedWta::Instance()._OnProcessExited(generation);
    }

    void SharedWta::_OnProcessExited(const details::ProcessWaitGenerationTracker::Generation generation)
    {
        // Runs on a Win32 thread-pool thread. wta has exited (crash,
        // OOM, manual kill). Clear our process record so the next
        // AcquirePane respawns. Existing panes that still hold refs
        // become zombies until their Closed handlers call
        // ReleasePane (which will then no-op the cleanup since
        // _process is already invalid).
        std::lock_guard lock{ _mtx };

        // Claiming also retires the current generation. A callback queued
        // before cleanup/restart cannot claim a later registration, even
        // when Windows assigns both process lifetimes the same PID.
        const auto observedPid = _waitGeneration.Claim(generation);
        if (!observedPid)
        {
            return;
        }

        if (!_process.is_valid())
        {
            // Race: Release already cleaned up before our callback
            // ran. Nothing to do.
            return;
        }
        // The master exited on its own — crash, OOM, or an external kill
        // (taskkill /F, Task Manager). It can't log its own hard death from
        // inside, but this wait callback (the parent observing it) can. This
        // is the external observer that makes otherwise-silent master deaths
        // diagnosable; deliberate teardowns never reach here (they reset
        // _process first, so the validity check above bails).
        _agentPaneLog("wta-master exited unexpectedly pid=" + std::to_string(*observedPid) + " (crash/OOM/external kill — observed by wait callback)");
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
