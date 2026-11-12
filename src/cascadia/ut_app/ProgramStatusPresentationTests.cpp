// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/ProgramStatusPresentation.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::ProgramStatusPresentation;

namespace TerminalAppUnitTests
{
    class ProgramStatusPresentationTests
    {
        TEST_CLASS(ProgramStatusPresentationTests);
        TEST_METHOD(DescendantsAndUnseenResultsSurviveRootCompletion);
        TEST_METHOD(CanonicalRequestsWinWithoutClearingProgramResults);
        TEST_METHOD(CanonicalActivitySurvivesCompletionWithoutInventingProgress);
        TEST_METHOD(TabAggregationCountsPanesAndNeverAveragesJobs);
        TEST_METHOD(AcknowledgmentRemovesAttentionWithoutInventingLiveness);
        TEST_METHOD(AttentionNavigationPrioritizesRequestsAndWrapsWithoutAcknowledging);
        TEST_METHOD(UntrustedTextCannotOverrideChromeDirection);
        TEST_METHOD(SingleStatusUsesTheWinningFactsDescription);
        TEST_METHOD(CurrentMessagesTakePrecedenceOverTitles);
        TEST_METHOD(StatusTextAppendsMessagesWithoutResultBadges);
        TEST_METHOD(HookAndProgramArrivalOrderPreservesIndependentFacts);
    };

    void ProgramStatusPresentationTests::DescendantsAndUnseenResultsSurviveRootCompletion()
    {
        std::vector<PaneStatus> panes{ { .observed = true, .working = 2, .unseenDone = 1 } };
        auto result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::Working);
        panes[0].blocked = 1;
        result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::Blocked);
        VERIFY_ARE_EQUAL(1u, result.workingPanes);
        VERIFY_ARE_EQUAL(1u, result.blockedPanes);
        VERIFY_ARE_EQUAL(1u, result.completedPanes);
    }

    void ProgramStatusPresentationTests::CanonicalRequestsWinWithoutClearingProgramResults()
    {
        const std::vector<PaneStatus> panes{
            { .observed = true, .unseenError = 1 },
            { .observed = true, .working = 1 },
            { .canonicalInput = true },
        };
        const auto result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::InputRequest);
        VERIFY_ARE_EQUAL(1u, result.errorPanes);
        VERIFY_ARE_EQUAL(1u, result.workingPanes);
    }

    void ProgramStatusPresentationTests::TabAggregationCountsPanesAndNeverAveragesJobs()
    {
        std::vector<PaneStatus> panes{
            { .observed = true, .working = 1, .progress = 20 },
            { .observed = true, .working = 3, .progress = 80 },
        };
        VERIFY_ARE_EQUAL(2u, Reduce(panes).workingPanes);
        VERIFY_IS_FALSE(Reduce(panes).progress.has_value());
        panes.resize(1);
        VERIFY_ARE_EQUAL(20u, Reduce(panes).progress.value());
        panes[0].working = 2;
        VERIFY_IS_FALSE(Reduce(panes).progress.has_value());
        panes[0].working = 1;
        panes[0].blocked = 1;
        VERIFY_IS_FALSE(Reduce(panes).progress.has_value());
    }

    void ProgramStatusPresentationTests::CanonicalActivitySurvivesCompletionWithoutInventingProgress()
    {
        std::vector<PaneStatus> panes{
            { .observed = true, .unseenDone = 1 },
            { .canonicalWorking = true },
        };
        auto result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::Working);
        VERIFY_ARE_EQUAL(1u, result.workingPanes);
        VERIFY_ARE_EQUAL(1u, result.completedPanes);
        panes[0].working = 1;
        panes[0].progress = 20;
        VERIFY_IS_FALSE(Reduce(panes).progress.has_value());
        panes.resize(1);
        panes[0].canonicalWorking = true;
        VERIFY_ARE_EQUAL(1u, Reduce(panes).workingPanes);
        VERIFY_ARE_EQUAL(20u, Reduce(panes).progress.value());
        panes[0].connectionError = true;
        VERIFY_ARE_EQUAL(1u, Reduce(panes).errorPanes);
        panes[0].unseenError = 1;
        VERIFY_ARE_EQUAL(1u, Reduce(panes).errorPanes);
        VERIFY_IS_TRUE(Reduce(panes).category == Category::Error);
    }

    void ProgramStatusPresentationTests::AcknowledgmentRemovesAttentionWithoutInventingLiveness()
    {
        std::vector<PaneStatus> panes{ { .observed = true, .unseenDone = 1 } };
        VERIFY_IS_TRUE(Reduce(panes).category == Category::Done);
        panes[0].unseenDone = 0;
        VERIFY_IS_TRUE(Reduce(panes).category == Category::Idle);
        panes[0].unseenError = 1;
        VERIFY_IS_TRUE(Reduce(panes).category == Category::Error);
        panes[0].blocked = 1;
        panes[0].unseenError = 0;
        VERIFY_IS_TRUE(Reduce(panes).category == Category::Blocked);
    }

    void ProgramStatusPresentationTests::AttentionNavigationPrioritizesRequestsAndWrapsWithoutAcknowledging()
    {
        std::vector<AttentionTarget> panes{
            { 10, Category::Working },
            { 20, Category::Done },
            { 30, Category::Blocked },
            { 40, Category::InputRequest },
            { 50, Category::InputRequest },
        };
        VERIFY_ARE_EQUAL(40u, NextAttention(panes, 20).value());
        VERIFY_ARE_EQUAL(50u, NextAttention(panes, 40).value());
        VERIFY_ARE_EQUAL(40u, NextAttention(panes, 50).value());
        VERIFY_ARE_EQUAL(40u, NextAttention(panes, std::nullopt).value());
        panes.resize(3);
        VERIFY_ARE_EQUAL(30u, NextAttention(panes, 30).value());
        panes.resize(2);
        VERIFY_ARE_EQUAL(20u, NextAttention(panes, 10).value());
        panes.resize(1);
        VERIFY_IS_FALSE(NextAttention(panes, 10).has_value());
        VERIFY_IS_FALSE(NextAttention({}, std::nullopt).has_value());
        panes = { { 60, Category::Working, true }, { 70, Category::Working } };
        VERIFY_ARE_EQUAL(60u, NextAttention(panes, 70).value());
    }

    void ProgramStatusPresentationTests::UntrustedTextCannotOverrideChromeDirection()
    {
        VERIFY_ARE_EQUAL(std::wstring{ L"safe.exe" }, DisplayText(L"safe\u202e.exe\u202c"));
        VERIFY_ARE_EQUAL(std::wstring{ L"abc" }, DisplayText(L"\u061ca\u200eb\u200fc\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069"));
        VERIFY_ARE_EQUAL(std::wstring{ L"\u0645\u0631\u062d\u0628\u0627\u200c\U0001f642" }, DisplayText(L"\u0645\u0631\u062d\u0628\u0627\u200c\U0001f642"));
        VERIFY_ARE_EQUAL(std::wstring{ L"safe title" }, DisplayText(L"safe\r\n title\u2028\u2029"));
    }

    void ProgramStatusPresentationTests::SingleStatusUsesTheWinningFactsDescription()
    {
        std::vector<PaneStatus> panes{ {
            .observed = true,
            .working = 1,
            .unseenError = 1,
            .taskState = "working",
            .title = L"Running tests",
            .message = L"Detailed output",
        } };
        auto result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::Working);
        VERIFY_ARE_EQUAL(1u, result.errorPanes);
        VERIFY_ARE_EQUAL(std::wstring{ L"Detailed output" }, result.description);

        panes[0].canonicalInput = true;
        panes[0].inputMessage = L"Approve deployment";
        result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::InputRequest);
        VERIFY_ARE_EQUAL(std::wstring{ L"Approve deployment" }, result.description);
        panes[0].canonicalInput = false;
        panes[0].working = 0;
        panes[0].unseenError = 0;
        panes[0].canonicalWorking = true;
        panes[0].taskState = "done";
        result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::Working);
        VERIFY_IS_TRUE(result.description.empty());

        panes[0].blocked = 1;
        panes[0].taskState = "blocked";
        panes[0].blockKind = "permission";
        result = Reduce(panes);
        VERIFY_IS_TRUE(result.category == Category::Blocked);
        VERIFY_ARE_EQUAL(std::string{ "permission" }, result.blockKind);
        VERIFY_ARE_EQUAL(std::wstring{ L"Detailed output" }, result.description);
    }

    void ProgramStatusPresentationTests::CurrentMessagesTakePrecedenceOverTitles()
    {
        for (const auto& [state, category] : {
                 std::pair{ "idle", Category::Idle },
                 std::pair{ "working", Category::Working },
                 std::pair{ "blocked", Category::Blocked },
                 std::pair{ "done", Category::Done },
                 std::pair{ "error", Category::Error },
             })
        {
            PaneStatus pane{
                .observed = true,
                .working = category == Category::Working ? 1u : 0u,
                .blocked = category == Category::Blocked ? 1u : 0u,
                .unseenDone = category == Category::Done ? 1u : 0u,
                .unseenError = category == Category::Error ? 1u : 0u,
                .taskState = state,
                .title = L"Task title",
                .message = L"Current\u202e message",
            };
            const auto result = Reduce(std::span{ &pane, 1 });
            VERIFY_IS_TRUE(result.category == category);
            VERIFY_ARE_EQUAL(std::wstring{ L"Current message" }, result.description);
            pane.message.clear();
            VERIFY_ARE_EQUAL(std::wstring{ L"Task title" }, Reduce(std::span{ &pane, 1 }).description);
        }
    }

    void ProgramStatusPresentationTests::StatusTextAppendsMessagesWithoutResultBadges()
    {
        PaneStatus pane{
            .observed = true,
            .working = 1,
            .unseenDone = 1,
            .unseenError = 1,
            .taskState = "working",
            .title = L"Task title",
            .message = L"Running tests",
        };
        auto summary = Reduce(std::span{ &pane, 1 });
        VERIFY_ARE_EQUAL(std::wstring{ L"Active \u00b7 \u2068Running tests\u2069" }, FormatText(summary, L"Active"));
        pane.unseenError = 0;
        pane.progress = 40;
        summary = Reduce(std::span{ &pane, 1 });
        VERIFY_ARE_EQUAL(40u, summary.progress.value());
        VERIFY_ARE_EQUAL(std::wstring{ L"Active 40% \u00b7 \u2068Running tests\u2069" }, FormatText(summary, L"Active", L"40%"));
        pane.working = 0;
        pane.taskState = "done";
        pane.message = L"Tests passed";
        summary = Reduce(std::span{ &pane, 1 });
        VERIFY_IS_TRUE(summary.category == Category::Done);
        VERIFY_ARE_EQUAL(std::wstring{ L"Completed \u00b7 \u2068Tests passed\u2069" }, FormatText(summary, L"Completed"));
        pane.unseenDone = 0;
        summary = Reduce(std::span{ &pane, 1 });
        VERIFY_IS_TRUE(summary.category == Category::Idle);
        VERIFY_ARE_EQUAL(std::wstring{ L"Idle" }, FormatText(summary, L"Idle"));
    }

    void ProgramStatusPresentationTests::HookAndProgramArrivalOrderPreservesIndependentFacts()
    {
        for (const auto hookFirst : { false, true })
        {
            PaneStatus pane;
            const auto hook = [&]() { pane.canonicalWorking = true; };
            const auto program = [&]() {
                pane.observed = true;
                pane.unseenDone = 1;
                pane.taskState = "done";
                pane.title = L"Finished subtask";
            };
            if (hookFirst)
            {
                hook();
                program();
            }
            else
            {
                program();
                hook();
            }
            auto result = Reduce(std::span{ &pane, 1 });
            VERIFY_IS_TRUE(result.category == Category::Working);
            VERIFY_ARE_EQUAL(1u, result.completedPanes);
            VERIFY_IS_TRUE(result.description.empty());
            pane.canonicalWorking = false;
            VERIFY_IS_TRUE(Reduce(std::span{ &pane, 1 }).category == Category::Done);

            pane.working = 1;
            pane.taskState = "working";
            VERIFY_IS_TRUE(Reduce(std::span{ &pane, 1 }).category == Category::Working);
        }
        const std::vector<AttentionTarget> candidates{
            { 1, Category::Working, false, true },
            { 2, Category::Working, true },
        };
        VERIFY_ARE_EQUAL(1u, NextAttention(candidates, std::nullopt).value());
    }
}
