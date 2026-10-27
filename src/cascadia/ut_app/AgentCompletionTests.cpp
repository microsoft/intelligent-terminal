// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include <winrt/Windows.System.h>
#include <til/winrt.h>
#include "../TerminalApp/ContentManager.h"

using namespace WEX::TestExecution;
using CompletionState = winrt::TerminalApp::implementation::ContentManager::AgentCompletionState;

namespace TerminalAppUnitTests
{
    class AgentCompletionTests
    {
        TEST_CLASS(AgentCompletionTests);
        TEST_METHOD(CompletionRequiresObservedActivity);
        TEST_METHOD(ReadingAndRepeatedIdleDoNotRearmCompletion);
        TEST_METHOD(NewActivityAndIdentityResetCompletion);
    };

    void AgentCompletionTests::CompletionRequiresObservedActivity()
    {
        const winrt::guid pane{ L"64711111-2222-3333-4444-555555555555" };
        CompletionState state;
        VERIFY_IS_FALSE(state.Update(pane, "session", "copilot", "Idle", false));
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "session", "copilot", "Working", false);
        VERIFY_IS_TRUE(state.Update(pane, "session", "copilot", "Idle", false));
        VERIFY_IS_TRUE(state.unread);
        state.Update(pane, "session", "copilot", "Error", false);
        state.Update(pane, "session", "copilot", "Idle", false);
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "session", "copilot", "Attention", false);
        state.Update(pane, "session", "copilot", "Idle", false);
        VERIFY_IS_TRUE(state.unread);
    }

    void AgentCompletionTests::ReadingAndRepeatedIdleDoNotRearmCompletion()
    {
        const winrt::guid pane{ L"64721111-2222-3333-4444-555555555555" };
        CompletionState state;
        state.Update(pane, "session", "copilot", "Working", false);
        state.Update(pane, "session", "copilot", "Idle", false);
        VERIFY_IS_TRUE(state.unread);
        VERIFY_IS_TRUE(state.Update(pane, "session", "copilot", "Idle", true));
        VERIFY_IS_FALSE(state.unread);
        VERIFY_IS_FALSE(state.Update(pane, "session", "copilot", "Idle", false));
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "session", "copilot", "Working", true);
        state.Update(pane, "session", "copilot", "Idle", true);
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "session", "copilot", "Working", true);
        state.Update(pane, "session", "copilot", "Idle", false);
        VERIFY_IS_TRUE(state.unread);
    }

    void AgentCompletionTests::NewActivityAndIdentityResetCompletion()
    {
        const winrt::guid pane{ L"64731111-2222-3333-4444-555555555555" };
        const winrt::guid otherPane{ L"64741111-2222-3333-4444-555555555555" };
        CompletionState state;
        state.Update(pane, "first", "copilot", "Working", false);
        state.Update(pane, "first", "copilot", "Idle", false);
        VERIFY_IS_TRUE(state.unread);
        state.Update(pane, "first", "copilot", "Working", false);
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "first", "copilot", "Idle", false);
        state.Update(pane, "second", "copilot", "Idle", false);
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "second", "copilot", "Working", false);
        state.Update(pane, "second", "copilot", "Idle", false);
        state.Update(pane, "second", "claude", "Idle", false);
        VERIFY_IS_FALSE(state.unread);
        state.Update(pane, "second", "claude", "Working", false);
        state.Update(pane, "second", "claude", "Idle", false);
        state.Update(otherPane, "second", "claude", "Idle", false);
        VERIFY_IS_FALSE(state.unread);
    }
}
