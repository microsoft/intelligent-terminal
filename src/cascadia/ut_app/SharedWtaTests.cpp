// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/SharedWta.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace winrt::TerminalApp::implementation;

namespace TerminalAppUnitTests
{
    namespace
    {
        struct SuspendedProcessCallState
        {
            HANDLE resumedThread{ nullptr };
            HANDLE unregisteredWait{ nullptr };
            HANDLE unregisterCompletionEvent{ nullptr };
            HANDLE terminatedProcess{ nullptr };
            UINT exitCode{ 0 };
        };

        thread_local SuspendedProcessCallState* suspendedProcessCallState{ nullptr };

        struct ProcessRetirementCallState
        {
            HANDLE process{ nullptr };
            std::array<DWORD, 2> waitResults{};
            size_t waitResultCount{ 0 };
            size_t waitResultIndex{ 0 };
            std::vector<std::string> calls;
            std::vector<DWORD> waitTimeouts;
            BOOL terminateResult{ TRUE };
            UINT exitCode{ 0 };
        };

        thread_local ProcessRetirementCallState* processRetirementCallState{ nullptr };

        DWORD WINAPI ResumeThreadFailure(const HANDLE thread)
        {
            suspendedProcessCallState->resumedThread = thread;
            return static_cast<DWORD>(-1);
        }

        BOOL WINAPI RecordUnregisterWait(const HANDLE wait, const HANDLE completionEvent)
        {
            suspendedProcessCallState->unregisteredWait = wait;
            suspendedProcessCallState->unregisterCompletionEvent = completionEvent;
            return TRUE;
        }

        BOOL WINAPI RecordTerminateProcess(const HANDLE process, const UINT exitCode)
        {
            suspendedProcessCallState->terminatedProcess = process;
            suspendedProcessCallState->exitCode = exitCode;
            return TRUE;
        }

        DWORD WINAPI ScriptProcessWait(const HANDLE process, const DWORD timeout)
        {
            auto& state = *processRetirementCallState;
            state.calls.emplace_back("wait");
            state.process = process;
            state.waitTimeouts.emplace_back(timeout);
            if (state.waitResultIndex >= state.waitResultCount)
            {
                return WAIT_FAILED;
            }
            const auto result = state.waitResults[state.waitResultIndex++];
            if (result == WAIT_FAILED)
            {
                SetLastError(ERROR_INVALID_HANDLE);
            }
            return result;
        }

        BOOL WINAPI ScriptProcessTerminate(const HANDLE process, const UINT exitCode)
        {
            auto& state = *processRetirementCallState;
            state.calls.emplace_back("terminate");
            state.process = process;
            state.exitCode = exitCode;
            if (!state.terminateResult)
            {
                SetLastError(ERROR_ACCESS_DENIED);
            }
            return state.terminateResult;
        }
    }

    struct GenerationTestObject :
        winrt::implements<GenerationTestObject, winrt::Windows::Foundation::IStringable>
    {
        explicit GenerationTestObject(winrt::hstring value) :
            _value{ std::move(value) }
        {
        }

        winrt::hstring ToString() const
        {
            return _value;
        }

    private:
        winrt::hstring _value;
    };

    class SharedWtaTests
    {
        TEST_CLASS(SharedWtaTests);

        TEST_METHOD(EmptyEnvironmentOverridesInheritParent);
        TEST_METHOD(ValidEnvironmentOverridesCloneAndReplace);
        TEST_METHOD(MixedInvalidEnvironmentOverridesFail);
        TEST_METHOD(AcceptsValidEnvironmentOverride);
        TEST_METHOD(RejectsEmptyEnvironmentName);
        TEST_METHOD(RejectsEqualsInEnvironmentName);
        TEST_METHOD(RejectsEmbeddedNullInEnvironmentName);
        TEST_METHOD(RejectsEmbeddedNullInEnvironmentValue);
        TEST_METHOD(ResumeFailureCleansUpSuspendedProcess);
        TEST_METHOD(RestartWaitsForRetiredProcessBeforeContinuing);
        TEST_METHOD(RestartTimeoutTerminatesThenReapsRetiredProcess);
        TEST_METHOD(RestartWaitFailureTerminatesThenReapsRetiredProcess);
        TEST_METHOD(RestartFailsWhenForcedProcessReapTimesOut);
        TEST_METHOD(StaleWaitCallbackCannotClaimReplacementWithReusedPid);
        TEST_METHOD(RetiredWaitCallbackCannotClaimAfterCleanup);
        TEST_METHOD(CurrentUnexpectedExitWithRetainedRefsRespawnsOnce);
        TEST_METHOD(UnexpectedExitWithoutRecoveryConditionsDoesNotRespawn);
        TEST_METHOD(DeliberateOrStaleExitDoesNotRespawn);
        TEST_METHOD(RecoveryReplacementExitDoesNotLoop);
        TEST_METHOD(AllScopeRetirementJoinsSameRequest);
        TEST_METHOD(LiveSettingsObjectSharesGeneration);
        TEST_METHOD(LaterSettingsObjectGetsNewGenerationAfterValueReuse);
        TEST_METHOD(DistinctAllScopeRequestsRemainDistinct);
        TEST_METHOD(TabScopeRetirementIsNeverDeduplicated);
        TEST_METHOD(RetirementActionIsClaimedOnce);
        TEST_METHOD(TimedOutSettingsRetirementKeepsRestartClaimableAcrossPages);
        TEST_METHOD(TimedOutRestartRetirementKeepsRestartClaimableAcrossPages);
        TEST_METHOD(CompletedRetirementHistoryIsBounded);
        TEST_METHOD(ExpiredRetirementRequestCanRegisterAgain);
        TEST_METHOD(CloseSupersedesPendingRebuild);
        TEST_METHOD(RestartSuppressionClearsBeforeReopen);
        TEST_METHOD(RepeatedRestartRequestsAreCoalescedOnCompletion);
    };

    void SharedWtaTests::EmptyEnvironmentOverridesInheritParent()
    {
        const auto block = details::BuildEnvironmentBlock({});

        VERIFY_IS_TRUE(block.has_value());
        VERIFY_IS_TRUE(block->empty());
    }

    void SharedWtaTests::ValidEnvironmentOverridesCloneAndReplace()
    {
        const std::array overrides{
            std::pair{ std::wstring{ L"PATH" }, std::wstring{ L"WTA_SHARED_WTA_TEST_OVERRIDE=debug" } },
        };

        const auto block = details::BuildEnvironmentBlock(overrides);

        VERIFY_IS_TRUE(block.has_value());
        VERIFY_IS_FALSE(block->empty());
        VERIFY_IS_GREATER_THAN_OR_EQUAL(block->size(), size_t{ 2 });
        VERIFY_ARE_EQUAL(L'\0', (*block)[block->size() - 1]);
        VERIFY_ARE_EQUAL(L'\0', (*block)[block->size() - 2]);

        size_t pathEntries = 0;
        bool foundInheritedEntry = false;
        for (const wchar_t* current = block->data(); *current;)
        {
            const std::wstring_view entry{ current };
            const auto separator = entry.find(L'=', entry.starts_with(L'=') ? 1 : 0);
            const auto name = separator == std::wstring_view::npos ? entry : entry.substr(0, separator);
            if (_wcsicmp(std::wstring{ name }.c_str(), L"PATH") == 0)
            {
                ++pathEntries;
                VERIFY_ARE_EQUAL(std::wstring_view{ L"PATH=WTA_SHARED_WTA_TEST_OVERRIDE=debug" }, entry);
            }
            else
            {
                foundInheritedEntry = true;
            }
            current += entry.size() + 1;
        }
        VERIFY_ARE_EQUAL(size_t{ 1 }, pathEntries);
        VERIFY_IS_TRUE(foundInheritedEntry);
    }

    void SharedWtaTests::MixedInvalidEnvironmentOverridesFail()
    {
        const std::array invalidNameOverrides{
            std::pair{ std::wstring{ L"WTA_LOG" }, std::wstring{ L"debug" } },
            std::pair{ std::wstring{}, std::wstring{ L"value" } },
        };
        VERIFY_IS_FALSE(details::BuildEnvironmentBlock(invalidNameOverrides).has_value());

        constexpr wchar_t invalidValue[]{ L't', L'r', L'a', L'c', L'e', L'\0', L'j', L'u', L'n', L'k' };
        const std::array invalidValueOverrides{
            std::pair{ std::wstring{ L"WTA_LOG" }, std::wstring{ L"debug" } },
            std::pair{ std::wstring{ L"RUST_LOG" }, std::wstring{ invalidValue, std::size(invalidValue) } },
        };
        VERIFY_IS_FALSE(details::BuildEnvironmentBlock(invalidValueOverrides).has_value());
    }

    void SharedWtaTests::AcceptsValidEnvironmentOverride()
    {
        VERIFY_IS_TRUE(details::IsValidEnvironmentOverride(L"WTA_LOG", L"debug=verbose"));
        VERIFY_IS_TRUE(details::IsValidEnvironmentOverride(L"WTA_LOG", L""));
    }

    void SharedWtaTests::RejectsEmptyEnvironmentName()
    {
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"", L"value"));
    }

    void SharedWtaTests::RejectsEqualsInEnvironmentName()
    {
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"WTA=LOG", L"value"));
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"=C:", L"value"));
    }

    void SharedWtaTests::RejectsEmbeddedNullInEnvironmentName()
    {
        constexpr wchar_t name[]{ L'W', L'T', L'A', L'\0', L'L', L'O', L'G' };
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(std::wstring_view{ name, std::size(name) }, L"value"));
    }

    void SharedWtaTests::RejectsEmbeddedNullInEnvironmentValue()
    {
        constexpr wchar_t value[]{ L'd', L'e', L'b', L'u', L'g', L'\0', L't', L'r', L'a', L'c', L'e' };
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"WTA_LOG", std::wstring_view{ value, std::size(value) }));
    }

    void SharedWtaTests::ResumeFailureCleansUpSuspendedProcess()
    {
        const auto thread = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(1));
        const auto process = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(2));
        HANDLE wait = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(3));
        SuspendedProcessCallState callState;
        suspendedProcessCallState = &callState;
        const auto resetCallState = wil::scope_exit([]() noexcept { suspendedProcessCallState = nullptr; });

        const details::SuspendedProcessOperations operations{
            &ResumeThreadFailure,
            &RecordUnregisterWait,
            &RecordTerminateProcess,
        };

        VERIFY_IS_FALSE(details::ResumeSuspendedProcess(thread, process, wait, operations));
        VERIFY_IS_NULL(wait);
        VERIFY_ARE_EQUAL(thread, callState.resumedThread);
        VERIFY_ARE_EQUAL(reinterpret_cast<HANDLE>(static_cast<uintptr_t>(3)), callState.unregisteredWait);
        VERIFY_IS_NULL(callState.unregisterCompletionEvent);
        VERIFY_ARE_EQUAL(process, callState.terminatedProcess);
        VERIFY_ARE_EQUAL(UINT{ 1 }, callState.exitCode);
    }

    void SharedWtaTests::RestartWaitsForRetiredProcessBeforeContinuing()
    {
        const auto process = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(4));
        ProcessRetirementCallState callState{
            .waitResults = { WAIT_OBJECT_0 },
            .waitResultCount = 1,
        };
        processRetirementCallState = &callState;
        const auto resetCallState = wil::scope_exit([]() noexcept { processRetirementCallState = nullptr; });
        const details::ProcessRetirementOperations operations{
            &ScriptProcessWait,
            &ScriptProcessTerminate,
        };

        VERIFY_IS_TRUE(details::EnsureProcessExitedBeforeRestart(process, 42, 3000, 5000, operations));
        VERIFY_ARE_EQUAL(process, callState.process);
        VERIFY_ARE_EQUAL(size_t{ 1 }, callState.calls.size());
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[0]);
        VERIFY_ARE_EQUAL(size_t{ 1 }, callState.waitTimeouts.size());
        VERIFY_ARE_EQUAL(DWORD{ 3000 }, callState.waitTimeouts[0]);
    }

    void SharedWtaTests::RestartTimeoutTerminatesThenReapsRetiredProcess()
    {
        const auto process = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(5));
        ProcessRetirementCallState callState{
            .waitResults = { WAIT_TIMEOUT, WAIT_OBJECT_0 },
            .waitResultCount = 2,
        };
        processRetirementCallState = &callState;
        const auto resetCallState = wil::scope_exit([]() noexcept { processRetirementCallState = nullptr; });
        const details::ProcessRetirementOperations operations{
            &ScriptProcessWait,
            &ScriptProcessTerminate,
        };

        VERIFY_IS_TRUE(details::EnsureProcessExitedBeforeRestart(process, 43, 3000, 5000, operations));
        VERIFY_ARE_EQUAL(size_t{ 3 }, callState.calls.size());
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[0]);
        VERIFY_ARE_EQUAL(std::string{ "terminate" }, callState.calls[1]);
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[2]);
        VERIFY_ARE_EQUAL(size_t{ 2 }, callState.waitTimeouts.size());
        VERIFY_ARE_EQUAL(DWORD{ 3000 }, callState.waitTimeouts[0]);
        VERIFY_ARE_EQUAL(DWORD{ 5000 }, callState.waitTimeouts[1]);
        VERIFY_ARE_EQUAL(UINT{ 1 }, callState.exitCode);
    }

    void SharedWtaTests::RestartWaitFailureTerminatesThenReapsRetiredProcess()
    {
        const auto process = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(6));
        ProcessRetirementCallState callState{
            .waitResults = { WAIT_FAILED, WAIT_OBJECT_0 },
            .waitResultCount = 2,
        };
        processRetirementCallState = &callState;
        const auto resetCallState = wil::scope_exit([]() noexcept { processRetirementCallState = nullptr; });
        const details::ProcessRetirementOperations operations{
            &ScriptProcessWait,
            &ScriptProcessTerminate,
        };

        VERIFY_IS_TRUE(details::EnsureProcessExitedBeforeRestart(process, 44, 3000, 5000, operations));
        VERIFY_ARE_EQUAL(size_t{ 3 }, callState.calls.size());
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[0]);
        VERIFY_ARE_EQUAL(std::string{ "terminate" }, callState.calls[1]);
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[2]);
    }

    void SharedWtaTests::RestartFailsWhenForcedProcessReapTimesOut()
    {
        const auto process = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(7));
        ProcessRetirementCallState callState{
            .waitResults = { WAIT_TIMEOUT, WAIT_TIMEOUT },
            .waitResultCount = 2,
        };
        processRetirementCallState = &callState;
        const auto resetCallState = wil::scope_exit([]() noexcept { processRetirementCallState = nullptr; });
        const details::ProcessRetirementOperations operations{
            &ScriptProcessWait,
            &ScriptProcessTerminate,
        };

        VERIFY_IS_FALSE(details::EnsureProcessExitedBeforeRestart(process, 45, 3000, 5000, operations));
        VERIFY_ARE_EQUAL(size_t{ 3 }, callState.calls.size());
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[0]);
        VERIFY_ARE_EQUAL(std::string{ "terminate" }, callState.calls[1]);
        VERIFY_ARE_EQUAL(std::string{ "wait" }, callState.calls[2]);
    }

    void SharedWtaTests::StaleWaitCallbackCannotClaimReplacementWithReusedPid()
    {
        details::ProcessWaitGenerationTracker tracker;
        constexpr DWORD reusedPid{ 46 };

        const auto staleGeneration = tracker.Register(reusedPid);
        tracker.Retire();
        const auto replacementGeneration = tracker.Register(reusedPid);

        VERIFY_ARE_NOT_EQUAL(staleGeneration, replacementGeneration);
        VERIFY_IS_FALSE(tracker.Claim(staleGeneration).has_value());
        VERIFY_ARE_EQUAL(replacementGeneration, tracker.Current());

        const auto replacementPid = tracker.Claim(replacementGeneration);
        VERIFY_IS_TRUE(replacementPid.has_value());
        VERIFY_ARE_EQUAL(reusedPid, *replacementPid);
        VERIFY_ARE_EQUAL(details::ProcessWaitGenerationTracker::Generation{ 0 }, tracker.Current());
    }

    void SharedWtaTests::RetiredWaitCallbackCannotClaimAfterCleanup()
    {
        details::ProcessWaitGenerationTracker tracker;
        const auto retiredGeneration = tracker.Register(47);

        tracker.Retire();

        VERIFY_IS_FALSE(tracker.Claim(retiredGeneration).has_value());
        VERIFY_ARE_EQUAL(details::ProcessWaitGenerationTracker::Generation{ 0 }, tracker.Current());
    }

    void SharedWtaTests::CurrentUnexpectedExitWithRetainedRefsRespawnsOnce()
    {
        details::UnexpectedExitRecoveryPolicy policy;
        policy.Arm(7);

        VERIFY_IS_TRUE(policy.ShouldRespawn(7, 2, false, true));
        VERIFY_IS_FALSE(policy.ShouldRespawn(7, 2, false, true));
    }

    void SharedWtaTests::UnexpectedExitWithoutRecoveryConditionsDoesNotRespawn()
    {
        details::UnexpectedExitRecoveryPolicy policy;
        policy.Arm(7);
        VERIFY_IS_FALSE(policy.ShouldRespawn(7, 0, false, true));

        policy.Arm(8);
        VERIFY_IS_FALSE(policy.ShouldRespawn(8, 1, true, true));

        policy.Arm(9);
        VERIFY_IS_FALSE(policy.ShouldRespawn(9, 1, false, false));
    }

    void SharedWtaTests::DeliberateOrStaleExitDoesNotRespawn()
    {
        details::UnexpectedExitRecoveryPolicy policy;
        policy.Arm(7);

        VERIFY_IS_FALSE(policy.ShouldRespawn(6, 1, false, true));
        VERIFY_IS_TRUE(policy.ShouldRespawn(7, 1, false, true));

        policy.Arm(8);
        policy.Retire();
        VERIFY_IS_FALSE(policy.ShouldRespawn(8, 1, false, true));
    }

    void SharedWtaTests::RecoveryReplacementExitDoesNotLoop()
    {
        details::UnexpectedExitRecoveryPolicy policy;
        policy.Arm(7);

        VERIFY_IS_TRUE(policy.ShouldRespawn(7, 1, false, true));
        VERIFY_IS_FALSE(policy.ShouldRespawn(8, 1, false, true));

        policy.Arm(9);
        VERIFY_IS_TRUE(policy.ShouldRespawn(9, 1, false, true));
    }

    void SharedWtaTests::AllScopeRetirementJoinsSameRequest()
    {
        details::RetirementCoordinator coordinator;

        const auto first = coordinator.Register(true, "restart_agent_stack", "request-1");
        const auto second = coordinator.Register(true, "restart_agent_stack", "request-1");

        VERIFY_IS_FALSE(first.operationId.empty());
        VERIFY_IS_TRUE(first.shouldPublish);
        VERIFY_IS_FALSE(second.shouldPublish);
        VERIFY_ARE_EQUAL(first.operationId, second.operationId);

        VERIFY_IS_TRUE(coordinator.Complete(first.operationId));
        coordinator.ReleaseContinuation(first.operationId);
        const auto lateJoin = coordinator.Register(true, "restart_agent_stack", "request-1");
        VERIFY_ARE_EQUAL(first.operationId, lateJoin.operationId);
        VERIFY_IS_TRUE(lateJoin.alreadyCompleted);
        coordinator.ReleaseContinuation(lateJoin.operationId);
    }

    void SharedWtaTests::LiveSettingsObjectSharesGeneration()
    {
        details::LiveObjectGenerationTracker tracker;
        const auto settings = winrt::make<GenerationTestObject>(L"A");
        const auto sameSettings = settings;

        VERIFY_ARE_EQUAL(tracker.Get(settings), tracker.Get(sameSettings));
    }

    void SharedWtaTests::LaterSettingsObjectGetsNewGenerationAfterValueReuse()
    {
        details::LiveObjectGenerationTracker tracker;

        auto settings = winrt::make<GenerationTestObject>(L"A");
        const auto firstGeneration = tracker.Get(settings);
        settings = nullptr;

        auto replacement = winrt::make<GenerationTestObject>(L"B");
        const auto secondGeneration = tracker.Get(replacement);
        replacement = nullptr;

        const auto sameValueReplacement = winrt::make<GenerationTestObject>(L"A");
        const auto thirdGeneration = tracker.Get(sameValueReplacement);

        VERIFY_IS_LESS_THAN(firstGeneration, secondGeneration);
        VERIFY_IS_LESS_THAN(secondGeneration, thirdGeneration);
    }

    void SharedWtaTests::DistinctAllScopeRequestsRemainDistinct()
    {
        details::RetirementCoordinator coordinator;

        const auto first = coordinator.Register(true, "settings_master_configuration_changed", "settings-a");
        const auto second = coordinator.Register(true, "settings_master_configuration_changed", "settings-b");

        VERIFY_ARE_NOT_EQUAL(first.operationId, second.operationId);
        VERIFY_IS_TRUE(coordinator.ClaimAction(first.operationId, "restart_master"));
        VERIFY_IS_TRUE(coordinator.ClaimAction(second.operationId, "restart_master"));
    }

    void SharedWtaTests::TabScopeRetirementIsNeverDeduplicated()
    {
        details::RetirementCoordinator coordinator;

        const auto first = coordinator.Register(false, "agent_switch");
        const auto second = coordinator.Register(false, "agent_switch");

        VERIFY_IS_TRUE(first.shouldPublish);
        VERIFY_IS_TRUE(second.shouldPublish);
        VERIFY_ARE_NOT_EQUAL(first.operationId, second.operationId);
    }

    void SharedWtaTests::RetirementActionIsClaimedOnce()
    {
        details::RetirementCoordinator coordinator;
        const auto operation = coordinator.Register(true, "restart_agent_stack", "request-1");

        VERIFY_IS_TRUE(coordinator.ClaimAction(operation.operationId, "restart_master"));
        VERIFY_IS_FALSE(coordinator.ClaimAction(operation.operationId, "restart_master"));
    }

    void SharedWtaTests::TimedOutSettingsRetirementKeepsRestartClaimableAcrossPages()
    {
        details::RetirementCoordinator coordinator;
        const auto firstPage = coordinator.Register(
            true,
            "settings_master_configuration_changed",
            "settings-timeout");
        const auto secondPage = coordinator.Register(
            true,
            "settings_master_configuration_changed",
            "settings-timeout");

        VERIFY_ARE_EQUAL(firstPage.operationId, secondPage.operationId);
        VERIFY_IS_TRUE(coordinator.Complete(firstPage.operationId, true));
        coordinator.ReleaseContinuation(firstPage.operationId);

        VERIFY_IS_TRUE(coordinator.ClaimAction(secondPage.operationId, "restart_master"));
        VERIFY_IS_FALSE(coordinator.ClaimAction(firstPage.operationId, "restart_master"));
        coordinator.ReleaseContinuation(secondPage.operationId);

        VERIFY_IS_FALSE(coordinator.ClaimAction(firstPage.operationId, "restart_master"));
        VERIFY_IS_FALSE(coordinator.Complete(firstPage.operationId));
    }

    void SharedWtaTests::TimedOutRestartRetirementKeepsRestartClaimableAcrossPages()
    {
        details::RetirementCoordinator coordinator;
        const auto firstPage = coordinator.Register(true, "restart_agent_stack", "restart-timeout");
        const auto secondPage = coordinator.Register(true, "restart_agent_stack", "restart-timeout");

        VERIFY_ARE_EQUAL(firstPage.operationId, secondPage.operationId);
        VERIFY_IS_TRUE(coordinator.Complete(firstPage.operationId, true));
        VERIFY_IS_TRUE(coordinator.ClaimAction(firstPage.operationId, "restart_master"));
        coordinator.ReleaseContinuation(firstPage.operationId);

        VERIFY_IS_FALSE(coordinator.ClaimAction(secondPage.operationId, "restart_master"));
        coordinator.ReleaseContinuation(secondPage.operationId);

        const auto replacement = coordinator.Register(true, "restart_agent_stack", "restart-timeout");
        VERIFY_ARE_NOT_EQUAL(firstPage.operationId, replacement.operationId);
        VERIFY_IS_TRUE(replacement.shouldPublish);
    }

    void SharedWtaTests::CompletedRetirementHistoryIsBounded()
    {
        details::RetirementCoordinator coordinator;
        const auto inFlight = coordinator.Register(true, "restart_agent_stack", "in-flight");
        std::string firstCompletedId;
        std::string lastCompletedId;

        for (size_t i = 0; i <= details::RetirementCoordinator::CompletedHistoryLimit; ++i)
        {
            const auto requestId = "completed-" + std::to_string(i);
            const auto operation = coordinator.Register(true, "restart_agent_stack", requestId);
            if (i == 0)
            {
                firstCompletedId = operation.operationId;
            }
            lastCompletedId = operation.operationId;
            VERIFY_IS_TRUE(coordinator.Complete(operation.operationId));
            coordinator.ReleaseContinuation(operation.operationId);
        }

        const auto evicted = coordinator.Register(true, "restart_agent_stack", "completed-0");
        VERIFY_ARE_NOT_EQUAL(firstCompletedId, evicted.operationId);
        VERIFY_IS_TRUE(evicted.shouldPublish);

        const auto retained = coordinator.Register(
            true,
            "restart_agent_stack",
            "completed-" + std::to_string(details::RetirementCoordinator::CompletedHistoryLimit));
        VERIFY_ARE_EQUAL(lastCompletedId, retained.operationId);
        VERIFY_IS_TRUE(retained.alreadyCompleted);
        coordinator.ReleaseContinuation(retained.operationId);

        VERIFY_IS_TRUE(coordinator.ClaimAction(inFlight.operationId, "restart_master"));
        VERIFY_IS_TRUE(coordinator.Complete(inFlight.operationId));
        coordinator.ReleaseContinuation(inFlight.operationId);
    }

    void SharedWtaTests::ExpiredRetirementRequestCanRegisterAgain()
    {
        details::RetirementCoordinator coordinator;
        const auto first = coordinator.Register(true, "restart_agent_stack", "request-1");

        coordinator.Expire(first.operationId);
        const auto replacement = coordinator.Register(true, "restart_agent_stack", "request-1");

        VERIFY_ARE_NOT_EQUAL(first.operationId, replacement.operationId);
        VERIFY_IS_TRUE(replacement.shouldPublish);
    }

    void SharedWtaTests::CloseSupersedesPendingRebuild()
    {
        details::TabRetirementTracker tracker;

        VERIFY_IS_TRUE(tracker.BeginRecreation("tab-1"));
        VERIFY_IS_FALSE(tracker.RequestClose("tab-1"));
        VERIFY_IS_FALSE(tracker.Complete("tab-1"));
    }

    void SharedWtaTests::RestartSuppressionClearsBeforeReopen()
    {
        details::RestartSuppressionTracker tracker;

        tracker.Mark("tab-1");
        tracker.Clear("tab-1");
        VERIFY_IS_FALSE(tracker.Consume("tab-1"));

        tracker.Mark("tab-1");
        VERIFY_IS_TRUE(tracker.Consume("tab-1"));
        VERIFY_IS_FALSE(tracker.Consume("tab-1"));
    }

    void SharedWtaTests::RepeatedRestartRequestsAreCoalescedOnCompletion()
    {
        details::CoalescedRequest pending;

        pending.Queue("restart-1");
        pending.Queue("restart-2");
        VERIFY_IS_TRUE(pending.Pending());

        std::vector<std::string> restarted;
        if (const auto pendingRestart = pending.Take())
        {
            restarted.emplace_back(*pendingRestart);
        }

        VERIFY_ARE_EQUAL(static_cast<size_t>(1), restarted.size());
        VERIFY_ARE_EQUAL(std::string{ "restart-2" }, restarted.front());
        VERIFY_IS_FALSE(pending.Pending());
        VERIFY_IS_FALSE(pending.Take().has_value());
    }
}
