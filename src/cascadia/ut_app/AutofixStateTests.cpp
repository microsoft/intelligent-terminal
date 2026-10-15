// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/AutofixState.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class AutofixStateTests
    {
        TEST_CLASS(AutofixStateTests);

        TEST_METHOD(IdleHasNoDiagnostics);
        TEST_METHOD(ActionableStatesHaveDiagnostics);
        TEST_METHOD(DetectionVisibilityDoesNotRequireConnectionState);
        TEST_METHOD(DisabledDetectionHidesAllDiagnosticStates);
    };

    void AutofixStateTests::IdleHasNoDiagnostics()
    {
        VERIFY_IS_FALSE(TerminalApp::Autofix::HasDiagnostics(TerminalApp::Autofix::State::Idle));
    }

    void AutofixStateTests::ActionableStatesHaveDiagnostics()
    {
        using State = TerminalApp::Autofix::State;

        VERIFY_IS_TRUE(TerminalApp::Autofix::HasDiagnostics(State::Detected));
        VERIFY_IS_TRUE(TerminalApp::Autofix::HasDiagnostics(State::Pending));
        VERIFY_IS_TRUE(TerminalApp::Autofix::HasDiagnostics(State::Review));
    }

    void AutofixStateTests::DetectionVisibilityDoesNotRequireConnectionState()
    {
        using State = TerminalApp::Autofix::State;

        VERIFY_IS_FALSE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Idle, true));
        VERIFY_IS_TRUE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Detected, true));
        VERIFY_IS_TRUE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Pending, true));
        VERIFY_IS_TRUE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Review, true));
    }

    void AutofixStateTests::DisabledDetectionHidesAllDiagnosticStates()
    {
        using State = TerminalApp::Autofix::State;

        VERIFY_IS_FALSE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Idle, false));
        VERIFY_IS_FALSE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Detected, false));
        VERIFY_IS_FALSE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Pending, false));
        VERIFY_IS_FALSE(TerminalApp::Autofix::ShouldShowDiagnostics(State::Review, false));
    }
}
