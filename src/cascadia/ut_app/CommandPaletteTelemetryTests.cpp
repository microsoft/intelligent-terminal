// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/CommandPaletteTelemetry.h"
#include "../inc/InteractionTelemetry.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class CommandPaletteTelemetryTests
    {
        TEST_CLASS(CommandPaletteTelemetryTests);
        TEST_METHOD(VisibleModeEntry);
        TEST_METHOD(HiddenModeSelectionAndReopen);
        TEST_METHOD(LeavingAndReenteringMode);
        TEST_METHOD(ClosingBeforeModePreparation);
        TEST_METHOD(DailyInteraction);
    };

    void CommandPaletteTelemetryTests::DailyInteraction()
    {
        ::Microsoft::Terminal::Telemetry::DailyInteraction interaction;
        VERIFY_IS_TRUE(interaction.Observe(100));
        VERIFY_IS_FALSE(interaction.Observe(100));
        VERIFY_IS_TRUE(interaction.Observe(101));
        VERIFY_IS_FALSE(interaction.Observe(100));
        VERIFY_IS_FALSE(interaction.Observe(101));
        VERIFY_IS_TRUE(interaction.Observe(107));
        ::Microsoft::Terminal::Telemetry::DailyInteraction otherProcess;
        VERIFY_IS_TRUE(otherProcess.Observe(107));
    }

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

    void CommandPaletteTelemetryTests::ClosingBeforeModePreparation()
    {
        ::TerminalApp::CommandPaletteTelemetry::AgentPromptEntry entry;
        VERIFY_IS_FALSE(entry.Update(true, false));
        VERIFY_IS_FALSE(entry.Update(false, false));
        VERIFY_IS_FALSE(entry.Update(false, true));
        VERIFY_IS_TRUE(entry.Update(true, true));
        VERIFY_IS_FALSE(entry.Update(false, true));
        VERIFY_IS_FALSE(entry.Update(false, true));
        VERIFY_IS_TRUE(entry.Update(true, true));
    }
}
