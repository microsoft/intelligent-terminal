// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/CommandPaletteTelemetry.h"
#include "../TerminalApp/SidebarTelemetry.h"
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
        TEST_METHOD(SidebarKeyboardEntryRequiresCompletedExternalTransition);
        TEST_METHOD(SidebarHistoryAgeUsesPreviousActivity);
    };

    void CommandPaletteTelemetryTests::SidebarKeyboardEntryRequiresCompletedExternalTransition()
    {
        ::TerminalApp::SidebarTelemetry::KeyboardEntry entry;
        entry.Preparing(false, true, true);
        VERIFY_IS_TRUE(entry.Focused(true));
        VERIFY_IS_FALSE(entry.Focused(true));
        entry.Preparing(true, true, true);
        VERIFY_IS_FALSE(entry.Focused(true));
        entry.Preparing(false, true, false);
        VERIFY_IS_FALSE(entry.Focused(true));
        entry.Preparing(false, true, true);
        VERIFY_IS_FALSE(entry.Focused(false));
        entry.Preparing(false, true, true);
        entry.Preparing(true, true, true);
        VERIFY_IS_FALSE(entry.Focused(true));
        entry.Preparing(false, true, true);
        VERIFY_IS_TRUE(entry.Focused(true));
    }

    void CommandPaletteTelemetryTests::SidebarHistoryAgeUsesPreviousActivity()
    {
        using ::TerminalApp::SidebarTelemetry::AgeDays;
        VERIFY_ARE_EQUAL(int64_t{ 2 }, AgeDays(1, 2 * 86'400'000ULL + 1));
        VERIFY_ARE_EQUAL(int64_t{ 0 }, AgeDays(100, 101));
        VERIFY_ARE_EQUAL(int64_t{ -1 }, AgeDays(std::nullopt, 100));
        VERIFY_ARE_EQUAL(int64_t{ -1 }, AgeDays(101, 100));
    }

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
