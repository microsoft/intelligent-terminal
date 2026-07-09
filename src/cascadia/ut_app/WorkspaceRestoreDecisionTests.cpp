// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/WorkspaceRestoreDecision.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace winrt::TerminalApp::implementation;

namespace TerminalAppUnitTests
{
    class WorkspaceRestoreDecisionTests
    {
        TEST_CLASS(WorkspaceRestoreDecisionTests);

        TEST_METHOD(AllClosedRestoresInNewWindow);
        TEST_METHOD(OneAnchorRestoresOnlyMissingTabsIntoAnchorWindow);
        TEST_METHOD(DraggedTabCountsAsMissing);
        TEST_METHOD(EverythingPresentFocusesExistingWindow);
    };

    static void VerifyMissingIds(const std::vector<std::wstring>& expected, const std::vector<std::wstring>& actual)
    {
        VERIFY_ARE_EQUAL(expected.size(), actual.size());
        for (size_t i = 0; i < expected.size() && i < actual.size(); ++i)
        {
            VERIFY_ARE_EQUAL(expected[i], actual[i]);
        }
    }

    void WorkspaceRestoreDecisionTests::AllClosedRestoresInNewWindow()
    {
        const std::vector<std::wstring> saved{ L"A", L"B", L"C" };

        const auto plan = DecideWorkspaceRestore(saved, {});

        VERIFY_IS_TRUE(plan.newWindow);
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, plan.windowId);
        VERIFY_IS_TRUE(plan.missingTabIds.empty());
    }

    void WorkspaceRestoreDecisionTests::OneAnchorRestoresOnlyMissingTabsIntoAnchorWindow()
    {
        const std::vector<std::wstring> saved{ L"A", L"B", L"C" };
        const std::vector<WorkspaceLiveBinding> live{
            { L"A", 42, 42 },
        };

        const auto plan = DecideWorkspaceRestore(saved, live);

        VERIFY_IS_FALSE(plan.newWindow);
        VERIFY_ARE_EQUAL(uint64_t{ 42 }, plan.windowId);
        VerifyMissingIds({ L"B", L"C" }, plan.missingTabIds);
    }

    void WorkspaceRestoreDecisionTests::DraggedTabCountsAsMissing()
    {
        const std::vector<std::wstring> saved{ L"A", L"B", L"C" };
        const std::vector<WorkspaceLiveBinding> live{
            { L"A", 1, 1 },
            { L"C", 2, 1 },
        };

        const auto plan = DecideWorkspaceRestore(saved, live);

        VERIFY_IS_FALSE(plan.newWindow);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, plan.windowId);
        VerifyMissingIds({ L"B", L"C" }, plan.missingTabIds);
    }

    void WorkspaceRestoreDecisionTests::EverythingPresentFocusesExistingWindow()
    {
        const std::vector<std::wstring> saved{ L"A", L"B", L"C" };
        const std::vector<WorkspaceLiveBinding> live{
            { L"A", 7, 7 },
            { L"B", 7, 7 },
            { L"C", 7, 7 },
        };

        const auto plan = DecideWorkspaceRestore(saved, live);

        VERIFY_IS_FALSE(plan.newWindow);
        VERIFY_ARE_EQUAL(uint64_t{ 7 }, plan.windowId);
        VERIFY_IS_TRUE(plan.missingTabIds.empty());
    }
}
