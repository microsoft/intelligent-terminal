// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/CommandPaletteTelemetry.h"

namespace TerminalAppUnitTests
{
    class CommandPaletteTelemetryTests
    {
        TEST_CLASS(CommandPaletteTelemetryTests);
        TEST_METHOD(VisibleModeEntry);
        TEST_METHOD(HiddenModeSelectionAndReopen);
        TEST_METHOD(LeavingAndReenteringMode);
    };

    void CommandPaletteTelemetryTests::VisibleModeEntry()
    {
        ::TerminalApp::CommandPaletteTelemetry::AgentPromptEntry entry;
        VERIFY_IS_FALSE(entry.Update(true, false));
        VERIFY_IS_TRUE(entry.Update(true, true));
        VERIFY_IS_FALSE(entry.Update(true, true));
    }

    void CommandPaletteTelemetryTests::HiddenModeSelectionAndReopen()
    {
        ::TerminalApp::CommandPaletteTelemetry::AgentPromptEntry entry;
        VERIFY_IS_FALSE(entry.Update(false, true));
        VERIFY_IS_FALSE(entry.Update(false, true));
        VERIFY_IS_TRUE(entry.Update(true, true));
        VERIFY_IS_FALSE(entry.Update(false, true));
        VERIFY_IS_TRUE(entry.Update(true, true));
        VERIFY_IS_FALSE(entry.Update(true, true));
    }

    void CommandPaletteTelemetryTests::LeavingAndReenteringMode()
    {
        ::TerminalApp::CommandPaletteTelemetry::AgentPromptEntry entry;
        VERIFY_IS_TRUE(entry.Update(true, true));
        VERIFY_IS_FALSE(entry.Update(true, false));
        VERIFY_IS_FALSE(entry.Update(true, false));
        VERIFY_IS_TRUE(entry.Update(true, true));
        VERIFY_IS_FALSE(entry.Update(false, false));
        VERIFY_IS_FALSE(entry.Update(true, false));
    }
}
