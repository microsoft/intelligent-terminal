// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/ProgressPresentation.h"

using namespace WEX::TestExecution;
using Microsoft::Terminal::ProgressPresentation::HasVisibleProgress;

namespace TerminalAppUnitTests
{
    class ProgressPresentationTests
    {
        TEST_CLASS(ProgressPresentationTests);

        TEST_METHOD(ZeroDeterminateProgressRetainsIdentity);
        TEST_METHOD(ReportedProgressAndWorkingSpinnerRemainVisible);
        TEST_METHOD(ClearedProgressRetainsIdentity);
    };

    void ProgressPresentationTests::ZeroDeterminateProgressRetainsIdentity()
    {
        VERIFY_IS_FALSE(HasVisibleProgress(4, 0), L"Native Blocked has paused progress at zero, not a drawable ring.");
        VERIFY_IS_FALSE(HasVisibleProgress(2, 0), L"Error without progress must not erase the identity icon.");
        VERIFY_IS_FALSE(HasVisibleProgress(1, 0), L"Normal progress at zero must not erase the identity icon.");
    }

    void ProgressPresentationTests::ReportedProgressAndWorkingSpinnerRemainVisible()
    {
        for (const auto state : { 1u, 2u, 4u })
        {
            for (const auto progress : { 1u, 37u, 100u })
            {
                VERIFY_IS_TRUE(HasVisibleProgress(state, progress));
            }
        }
        VERIFY_IS_TRUE(HasVisibleProgress(3, 0), L"Indeterminate Working has drawable animation without a percentage.");
        VERIFY_IS_TRUE(HasVisibleProgress(3, 37), L"A stale percentage must not hide an indeterminate ring.");
    }

    void ProgressPresentationTests::ClearedProgressRetainsIdentity()
    {
        VERIFY_IS_FALSE(HasVisibleProgress(0, 0));
        VERIFY_IS_FALSE(HasVisibleProgress(0, 37), L"Clearing progress must restore identity even if a percentage remains.");
    }
}
