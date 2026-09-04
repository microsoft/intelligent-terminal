// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalApp/TerminalPage.h"
#include "../inc/AgentAvailability.h"
#include "../inc/FreAgentSetup.h"
#include "../inc/AgentPolicy.h"
#include "../UnitTests_SettingsModel/TestUtils.h"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"

#include <atomic>
#include <future>

using namespace Microsoft::Console;
using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace winrt::TerminalApp;
using namespace winrt::Microsoft::Terminal::Settings;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Microsoft::Terminal::Control;

namespace TerminalAppLocalTests
{
    static constexpr std::wstring_view inboxSettings{ LR"({
        "schemes": [{
            "name": "Campbell",
            "foreground": "#CCCCCC",
            "background": "#0C0C0C",
            "cursorColor": "#FFFFFF",
            "black": "#0C0C0C",
            "red": "#C50F1F",
            "green": "#13A10E",
            "yellow": "#C19C00",
            "blue": "#0037DA",
            "purple": "#881798",
            "cyan": "#3A96DD",
            "white": "#CCCCCC",
            "brightBlack": "#767676",
            "brightRed": "#E74856",
            "brightGreen": "#16C60C",
            "brightYellow": "#F9F1A5",
            "brightBlue": "#3B78FF",
            "brightPurple": "#B4009E",
            "brightCyan": "#61D6D6",
            "brightWhite": "#F2F2F2"
        }]
    })" };

    // TODO:microsoft/terminal#3838:
    // Unfortunately, these tests _WILL NOT_ work in our CI. We're waiting for
    // an updated TAEF that will let us install framework packages when the test
    // package is deployed. Until then, these tests won't deploy in CI.

    class SettingsTests
    {
        // Use a custom AppxManifest to ensure that we can activate winrt types
        // from our test. This property will tell taef to manually use this as
        // the AppxManifest for this test class.
        // This does not yet work for anything XAML-y. See TabTests.cpp for more
        // details on that.
        BEGIN_TEST_CLASS(SettingsTests)
            TEST_CLASS_PROPERTY(L"RunAs", L"UAP")
            TEST_CLASS_PROPERTY(L"UAP:AppXManifest", L"TestHostAppXManifest.xml")
        END_TEST_CLASS()

        TEST_METHOD(TestIterateCommands);
        TEST_METHOD(TestIterateOnGeneratedNamedCommands);
        TEST_METHOD(TestIterateOnBadJson);
        TEST_METHOD(TestNestedCommands);
        TEST_METHOD(TestNestedInNestedCommand);
        TEST_METHOD(TestNestedInIterableCommand);
        TEST_METHOD(TestIterableInNestedCommand);
        TEST_METHOD(TestMixedNestedAndIterableCommand);

        TEST_METHOD(TestIterableColorSchemeCommands);

        TEST_METHOD(TestElevateArg);
        TEST_METHOD(TestAgentSettingsChangeClassification);
        TEST_METHOD(TestAgentSettingsFocusGate);
        TEST_METHOD(TestAgentPaneRebindCapability);
        TEST_METHOD(TestAgentPaneSwitchCapability);
        TEST_METHOD(TestAgentPaneSettingsRebindRouting);
        TEST_METHOD(TestAgentPaneModelHotUpdateRouting);
        TEST_METHOD(TestAgentPanePrewarmSchedulesOnlyActiveTab);
        TEST_METHOD(TestAgentPanePrewarmRechecksSelection);
        TEST_METHOD(TestRestoredAgentPaneSuppressesBlankPrewarm);
        TEST_METHOD(TestAgentPanePrewarmSuppressesDuplicates);
        TEST_METHOD(TestExplicitlyClosedAgentPaneStaysClosedUntilRequested);
        TEST_METHOD(TestExplicitCloseWinsMaterializedObservationRace);
        TEST_METHOD(TestSpeculativePrewarmSelectionBudget);
        TEST_METHOD(TestOpenedAgentPaneIsNotSpeculative);
        TEST_METHOD(TestRestoredAndTransferredPanesAreNotSpeculative);
        TEST_METHOD(TestExplicitCloseDuringSpeculativeMaterializationWins);
        TEST_METHOD(TestRepeatedSelectionRetainsOneSpeculativePane);
        TEST_METHOD(TestSettingsRecreationPreservesSpeculativeBudget);
        TEST_METHOD(TestStartupPrewarmIgnoresSlowEmptyAvailabilityCache);
        TEST_METHOD(TestStartupPrewarmRejectsNoAllowedAgents);
        TEST_METHOD(TestStartupPrewarmBlockedCopilotUsesExplicitFallback);
        TEST_METHOD(TestStartupPrewarmCustomRequiresTrustedCommand);
        TEST_METHOD(TestStartupPrewarmAvailabilityDoesNotDuplicate);
        TEST_METHOD(TestAgentAvailabilityUiSnapshotIsImmediateAndRefreshIsSingleFlight);
        TEST_METHOD(TestAgentAvailabilityFailureReachesProbingAndJoinedCallersThenRetries);
        TEST_METHOD(TestAgentAvailabilityInvalidationDuringProbeRetriesJoinedCallers);
        TEST_METHOD(TestAgentAvailabilityInvalidationThenFailureReachesAllCallers);
        TEST_METHOD(TestAgentAvailabilityRepeatedInvalidationIsBounded);
        TEST_METHOD(TestTerminalPageAgentAvailabilityFailureAllowsRetry);
        TEST_METHOD(TestAgentPickerRefreshResolvesPolicyDefaultAndPreservesSelection);
        TEST_METHOD(TestFreAgentSetupRequiresSelectionAndGuardsHooks);
        TEST_METHOD(TestFrePostInstallAvailabilityFailureStopsSetupAndAllowsRetry);

        TEST_CLASS_SETUP(ClassSetup)
        {
            return true;
        }

    private:
        void _logCommandNames(winrt::Windows::Foundation::Collections::IMapView<winrt::hstring, Command> commands, const int indentation = 1)
        {
            if (indentation == 1)
            {
                Log::Comment((commands.Size() == 0) ? L"Commands:\n  <none>" : L"Commands:");
            }
            for (const auto& nameAndCommand : commands)
            {
                Log::Comment(fmt::format(L"{0:>{1}}* {2}->{3}",
                                         L"",
                                         indentation,
                                         nameAndCommand.Key(),
                                         nameAndCommand.Value().Name())
                                 .c_str());

                if (nameAndCommand.Value().HasNestedCommands())
                {
                    _logCommandNames(nameAndCommand.Value().NestedCommands(), indentation + 2);
                }
            }
        }
        void _logCommands(winrt::Windows::Foundation::Collections::IVector<Command> commands, const int indentation = 1)
        {
            if (indentation == 1)
            {
                Log::Comment((commands.Size() == 0) ? L"Commands:\n  <none>" : L"Commands:");
            }
            for (const auto& cmd : commands)
            {
                Log::Comment(fmt::format(L"{0:>{1}}* {2}",
                                         L"",
                                         indentation,
                                         cmd.Name())
                                 .c_str());

                if (cmd.HasNestedCommands())
                {
                    _logCommandNames(cmd.NestedCommands(), indentation + 2);
                }
            }
        }
    };

    void SettingsTests::TestIterateCommands()
    {
        // For this test, put an iterable command with a given `name`,
        // containing a ${profile.name} to replace. When we expand it, it should
        // have created one command for each profile.

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "name": "iterable command ${profile.name}",
                    "iterateOn": "profiles",
                    "command": { "action": "splitPane", "profile": "${profile.name}" }
                },
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());

        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        auto nameMap{ settings.ActionMap().NameMap() };
        VERIFY_ARE_EQUAL(1u, nameMap.Size());

        {
            auto command = nameMap.TryLookup(L"iterable command ${profile.name}");
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"${profile.name}", terminalArgs.Profile());
        }

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, expandedCommands.Size());

        {
            auto command = expandedCommands.GetAt(0);
            VERIFY_ARE_EQUAL(L"iterable command profile0", command.Name());
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile0", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(1);
            VERIFY_ARE_EQUAL(L"iterable command profile1", command.Name());
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile1", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(2);
            VERIFY_ARE_EQUAL(L"iterable command profile2", command.Name());
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile2", terminalArgs.Profile());
        }
    }

    void SettingsTests::TestIterateOnGeneratedNamedCommands()
    {
        // For this test, put an iterable command without a given `name` to
        // replace. When we expand it, it should still work.

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "iterateOn": "profiles",
                    "command": { "action": "splitPane", "profile": "${profile.name}" }
                },
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());

        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        auto nameMap{ settings.ActionMap().NameMap() };
        VERIFY_ARE_EQUAL(1u, nameMap.Size());

        {
            auto command = nameMap.TryLookup(L"Split pane, profile: ${profile.name}");
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"${profile.name}", terminalArgs.Profile());
        }

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, expandedCommands.Size());

        {
            auto command = expandedCommands.GetAt(0);
            VERIFY_ARE_EQUAL(L"Split pane, profile: profile0", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile0", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(1);
            VERIFY_ARE_EQUAL(L"Split pane, profile: profile1", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile1", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(2);
            VERIFY_ARE_EQUAL(L"Split pane, profile: profile2", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile2", terminalArgs.Profile());
        }
    }

    void SettingsTests::TestIterateOnBadJson()
    {
        // For this test, put an iterable command with a profile name that would
        // cause bad json to be filled in. Something like a profile with a name
        // of "Foo\"", so the trailing '"' might break the json parsing.

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1\"",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "name": "iterable command ${profile.name}",
                    "iterateOn": "profiles",
                    "command": { "action": "splitPane", "profile": "${profile.name}" }
                },
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());

        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        auto nameMap{ settings.ActionMap().NameMap() };
        VERIFY_ARE_EQUAL(1u, nameMap.Size());

        {
            auto command = nameMap.TryLookup(L"iterable command ${profile.name}");
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"${profile.name}", terminalArgs.Profile());
        }

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, expandedCommands.Size());

        {
            auto command = expandedCommands.GetAt(0);
            VERIFY_ARE_EQUAL(L"iterable command profile0", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile0", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(1);
            VERIFY_ARE_EQUAL(L"iterable command profile1\"", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile1\"", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(2);
            VERIFY_ARE_EQUAL(L"iterable command profile2", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile2", terminalArgs.Profile());
        }
    }

    void SettingsTests::TestNestedCommands()
    {
        // This test checks a nested command.
        // The commands should look like:
        //
        // <Command Palette>
        // └─ Connect to ssh...
        //    ├─ first.com
        //    └─ second.com

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "name": "Connect to ssh...",
                    "commands": [
                        {
                            "name": "first.com",
                            "command": { "action": "newTab", "commandline": "ssh me@first.com" }
                        },
                        {
                            "name": "second.com",
                            "command": { "action": "newTab", "commandline": "ssh me@second.com" }
                        }
                    ]
                },
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(1u, expandedCommands.Size());

        auto rootCommand = expandedCommands.GetAt(0);
        VERIFY_IS_NOT_NULL(rootCommand);
        VERIFY_ARE_EQUAL(L"Connect to ssh...", rootCommand.Name());
        auto rootActionAndArgs = rootCommand.ActionAndArgs();
        VERIFY_IS_NOT_NULL(rootActionAndArgs);
        VERIFY_ARE_EQUAL(ShortcutAction::Invalid, rootActionAndArgs.Action());

        VERIFY_ARE_EQUAL(2u, rootCommand.NestedCommands().Size());

        {
            winrt::hstring commandName{ L"first.com" };
            auto command = rootCommand.NestedCommands().Lookup(commandName);
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);

            VERIFY_IS_FALSE(command.HasNestedCommands());
        }
        {
            winrt::hstring commandName{ L"second.com" };
            auto command = rootCommand.NestedCommands().Lookup(commandName);
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);

            VERIFY_IS_FALSE(command.HasNestedCommands());
        }
    }

    void SettingsTests::TestNestedInNestedCommand()
    {
        // This test checks a nested command that includes nested commands.
        // The commands should look like:
        //
        // <Command Palette>
        // └─ grandparent
        //    └─ parent
        //       ├─ child1
        //       └─ child2

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "name": "grandparent",
                    "commands": [
                        {
                            "name": "parent",
                            "commands": [
                                {
                                    "name": "child1",
                                    "command": { "action": "newTab", "commandline": "ssh me@first.com" }
                                },
                                {
                                    "name": "child2",
                                    "command": { "action": "newTab", "commandline": "ssh me@second.com" }
                                }
                            ]
                        },
                    ]
                },
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(1u, expandedCommands.Size());

        auto grandparentCommand = expandedCommands.GetAt(0);
        VERIFY_IS_NOT_NULL(grandparentCommand);
        VERIFY_ARE_EQUAL(L"grandparent", grandparentCommand.Name());

        auto grandparentActionAndArgs = grandparentCommand.ActionAndArgs();
        VERIFY_IS_NOT_NULL(grandparentActionAndArgs);
        VERIFY_ARE_EQUAL(ShortcutAction::Invalid, grandparentActionAndArgs.Action());

        VERIFY_ARE_EQUAL(1u, grandparentCommand.NestedCommands().Size());

        winrt::hstring parentName{ L"parent" };
        auto parent = grandparentCommand.NestedCommands().Lookup(parentName);
        VERIFY_IS_NOT_NULL(parent);
        auto parentActionAndArgs = parent.ActionAndArgs();
        VERIFY_IS_NOT_NULL(parentActionAndArgs);
        VERIFY_ARE_EQUAL(ShortcutAction::Invalid, parentActionAndArgs.Action());

        VERIFY_ARE_EQUAL(2u, parent.NestedCommands().Size());
        {
            winrt::hstring childName{ L"child1" };
            auto child = parent.NestedCommands().Lookup(childName);
            VERIFY_IS_NOT_NULL(child);
            auto childActionAndArgs = child.ActionAndArgs();
            VERIFY_IS_NOT_NULL(childActionAndArgs);

            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, childActionAndArgs.Action());
            const auto& realArgs = childActionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_FALSE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_TRUE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"ssh me@first.com", terminalArgs.Commandline());

            VERIFY_IS_FALSE(child.HasNestedCommands());
        }
        {
            winrt::hstring childName{ L"child2" };
            auto child = parent.NestedCommands().Lookup(childName);
            VERIFY_IS_NOT_NULL(child);
            auto childActionAndArgs = child.ActionAndArgs();
            VERIFY_IS_NOT_NULL(childActionAndArgs);

            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, childActionAndArgs.Action());
            const auto& realArgs = childActionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_FALSE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_TRUE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"ssh me@second.com", terminalArgs.Commandline());

            VERIFY_IS_FALSE(child.HasNestedCommands());
        }
    }

    void SettingsTests::TestNestedInIterableCommand()
    {
        // This test checks an iterable command that includes a nested command.
        // The commands should look like:
        //
        // <Command Palette>
        //  ├─ profile0...
        //  |  ├─ Split pane, profile: profile0
        //  |  ├─ Split pane, direction: right, profile: profile0
        //  |  └─ Split pane, direction: down, profile: profile0
        //  ├─ profile1...
        //  |  ├─Split pane, profile: profile1
        //  |  ├─Split pane, direction: right, profile: profile1
        //  |  └─Split pane, direction: down, profile: profile1
        //  └─ profile2...
        //     ├─ Split pane, profile: profile2
        //     ├─ Split pane, direction: right, profile: profile2
        //     └─ Split pane, direction: down, profile: profile2

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "iterateOn": "profiles",
                    "name": "${profile.name}...",
                    "commands": [
                        { "command": { "action": "splitPane", "profile": "${profile.name}", "split": "auto" } },
                        { "command": { "action": "splitPane", "profile": "${profile.name}", "split": "right" } },
                        { "command": { "action": "splitPane", "profile": "${profile.name}", "split": "down" } }
                    ]
                }
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());

        VERIFY_ARE_EQUAL(3u, expandedCommands.Size());

        const std::vector<std::wstring> profileNames{ L"profile0", L"profile1", L"profile2" };
        for (auto i = 0u; i < profileNames.size(); i++)
        {
            const auto& name{ profileNames[i] };
            winrt::hstring commandName{ profileNames[i] + L"..." };

            auto command = expandedCommands.GetAt(i);
            VERIFY_ARE_EQUAL(commandName, command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::Invalid, actionAndArgs.Action());

            VERIFY_IS_TRUE(command.HasNestedCommands());
            VERIFY_ARE_EQUAL(3u, command.NestedCommands().Size());
            _logCommandNames(command.NestedCommands());
            {
                const auto childCommandName = fmt::format(FMT_COMPILE(L"Split pane, profile: {}"), name);
                auto childCommand = command.NestedCommands().Lookup(childCommandName);
                VERIFY_IS_NOT_NULL(childCommand);
                auto childActionAndArgs = childCommand.ActionAndArgs();
                VERIFY_IS_NOT_NULL(childActionAndArgs);

                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, childActionAndArgs.Action());
                const auto& realArgs = childActionAndArgs.Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(realArgs);
                // Verify the args have the expected value
                VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
                auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
                VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
                VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
                VERIFY_IS_FALSE(terminalArgs.Profile().empty());
                VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

                VERIFY_IS_FALSE(childCommand.HasNestedCommands());
            }
            {
                const auto childCommandName = fmt::format(FMT_COMPILE(L"Split pane, split: down, profile: {}"), name);
                auto childCommand = command.NestedCommands().Lookup(childCommandName);
                VERIFY_IS_NOT_NULL(childCommand);
                auto childActionAndArgs = childCommand.ActionAndArgs();
                VERIFY_IS_NOT_NULL(childActionAndArgs);

                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, childActionAndArgs.Action());
                const auto& realArgs = childActionAndArgs.Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(realArgs);
                // Verify the args have the expected value
                VERIFY_ARE_EQUAL(SplitDirection::Down, realArgs.SplitDirection());
                auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
                VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
                VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
                VERIFY_IS_FALSE(terminalArgs.Profile().empty());
                VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

                VERIFY_IS_FALSE(childCommand.HasNestedCommands());
            }
            {
                const auto childCommandName = fmt::format(FMT_COMPILE(L"Split pane, split: right, profile: {}"), name);
                auto childCommand = command.NestedCommands().Lookup(childCommandName);
                VERIFY_IS_NOT_NULL(childCommand);
                auto childActionAndArgs = childCommand.ActionAndArgs();
                VERIFY_IS_NOT_NULL(childActionAndArgs);

                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, childActionAndArgs.Action());
                const auto& realArgs = childActionAndArgs.Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(realArgs);
                // Verify the args have the expected value
                VERIFY_ARE_EQUAL(SplitDirection::Right, realArgs.SplitDirection());
                auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
                VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
                VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
                VERIFY_IS_FALSE(terminalArgs.Profile().empty());
                VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

                VERIFY_IS_FALSE(childCommand.HasNestedCommands());
            }
        }
    }

    void SettingsTests::TestIterableInNestedCommand()
    {
        // This test checks a nested command that includes an iterable command.
        // The commands should look like:
        //
        // <Command Palette>
        // └─ New Tab With Profile...
        //    ├─ Profile 1
        //    ├─ Profile 2
        //    └─ Profile 3

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "name": "New Tab With Profile...",
                    "commands": [
                        {
                            "iterateOn": "profiles",
                            "command": { "action": "newTab", "profile": "${profile.name}" }
                        }
                    ]
                }
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(1u, expandedCommands.Size());

        auto rootCommand = expandedCommands.GetAt(0);
        VERIFY_IS_NOT_NULL(rootCommand);
        VERIFY_ARE_EQUAL(L"New Tab With Profile...", rootCommand.Name());

        auto rootActionAndArgs = rootCommand.ActionAndArgs();
        VERIFY_IS_NOT_NULL(rootActionAndArgs);
        VERIFY_ARE_EQUAL(ShortcutAction::Invalid, rootActionAndArgs.Action());

        VERIFY_ARE_EQUAL(3u, rootCommand.NestedCommands().Size());

        for (auto name : std::vector<std::wstring>({ L"profile0", L"profile1", L"profile2" }))
        {
            const auto childCommandName = fmt::format(FMT_COMPILE(L"New tab, profile: {}"), name);
            auto command = rootCommand.NestedCommands().Lookup(childCommandName);
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);

            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

            VERIFY_IS_FALSE(command.HasNestedCommands());
        }
    }
    void SettingsTests::TestMixedNestedAndIterableCommand()
    {
        // This test checks a nested commands that includes an iterable command
        // that includes a nested command.
        // The commands should look like:
        //
        // <Command Palette>
        // └─ New Pane...
        //    ├─ profile0...
        //    |  ├─ Split automatically
        //    |  ├─ Split right
        //    |  └─ Split down
        //    ├─ profile1...
        //    |  ├─ Split automatically
        //    |  ├─ Split right
        //    |  └─ Split down
        //    └─ profile2...
        //       ├─ Split automatically
        //       ├─ Split right
        //       └─ Split down

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "actions": [
                {
                    "name": "New Pane...",
                    "commands": [
                        {
                            "iterateOn": "profiles",
                            "name": "${profile.name}...",
                            "commands": [
                                { "command": { "action": "splitPane", "profile": "${profile.name}", "split": "auto" } },
                                { "command": { "action": "splitPane", "profile": "${profile.name}", "split": "right" } },
                                { "command": { "action": "splitPane", "profile": "${profile.name}", "split": "down" } }
                            ]
                        }
                    ]
                }
            ]
        })" };

        CascadiaSettings settings{ settingsJson, inboxSettings };

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(0u, settings.Warnings().Size());
        VERIFY_ARE_EQUAL(1u, expandedCommands.Size());

        auto rootCommand = expandedCommands.GetAt(0);
        VERIFY_IS_NOT_NULL(rootCommand);
        VERIFY_ARE_EQUAL(L"New Pane...", rootCommand.Name());

        VERIFY_IS_NOT_NULL(rootCommand);
        auto rootActionAndArgs = rootCommand.ActionAndArgs();
        VERIFY_IS_NOT_NULL(rootActionAndArgs);
        VERIFY_ARE_EQUAL(ShortcutAction::Invalid, rootActionAndArgs.Action());

        VERIFY_ARE_EQUAL(3u, rootCommand.NestedCommands().Size());

        for (auto name : std::vector<std::wstring>({ L"profile0", L"profile1", L"profile2" }))
        {
            winrt::hstring commandName{ name + L"..." };
            auto command = rootCommand.NestedCommands().Lookup(commandName);
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::Invalid, actionAndArgs.Action());

            VERIFY_IS_TRUE(command.HasNestedCommands());
            VERIFY_ARE_EQUAL(3u, command.NestedCommands().Size());

            _logCommandNames(command.NestedCommands());
            {
                const auto childCommandName = fmt::format(FMT_COMPILE(L"Split pane, profile: {}"), name);
                auto childCommand = command.NestedCommands().Lookup(childCommandName);
                VERIFY_IS_NOT_NULL(childCommand);
                auto childActionAndArgs = childCommand.ActionAndArgs();
                VERIFY_IS_NOT_NULL(childActionAndArgs);

                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, childActionAndArgs.Action());
                const auto& realArgs = childActionAndArgs.Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(realArgs);
                // Verify the args have the expected value
                VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
                auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
                VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
                VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
                VERIFY_IS_FALSE(terminalArgs.Profile().empty());
                VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

                VERIFY_IS_FALSE(childCommand.HasNestedCommands());
            }
            {
                const auto childCommandName = fmt::format(FMT_COMPILE(L"Split pane, split: down, profile: {}"), name);
                auto childCommand = command.NestedCommands().Lookup(childCommandName);
                VERIFY_IS_NOT_NULL(childCommand);
                auto childActionAndArgs = childCommand.ActionAndArgs();
                VERIFY_IS_NOT_NULL(childActionAndArgs);

                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, childActionAndArgs.Action());
                const auto& realArgs = childActionAndArgs.Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(realArgs);
                // Verify the args have the expected value
                VERIFY_ARE_EQUAL(SplitDirection::Down, realArgs.SplitDirection());
                auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
                VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
                VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
                VERIFY_IS_FALSE(terminalArgs.Profile().empty());
                VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

                VERIFY_IS_FALSE(childCommand.HasNestedCommands());
            }
            {
                const auto childCommandName = fmt::format(FMT_COMPILE(L"Split pane, split: right, profile: {}"), name);
                auto childCommand = command.NestedCommands().Lookup(childCommandName);
                VERIFY_IS_NOT_NULL(childCommand);
                auto childActionAndArgs = childCommand.ActionAndArgs();
                VERIFY_IS_NOT_NULL(childActionAndArgs);

                VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, childActionAndArgs.Action());
                const auto& realArgs = childActionAndArgs.Args().try_as<SplitPaneArgs>();
                VERIFY_IS_NOT_NULL(realArgs);
                // Verify the args have the expected value
                VERIFY_ARE_EQUAL(SplitDirection::Right, realArgs.SplitDirection());
                auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
                VERIFY_IS_NOT_NULL(terminalArgs);
                VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
                VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
                VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
                VERIFY_IS_FALSE(terminalArgs.Profile().empty());
                VERIFY_ARE_EQUAL(name, terminalArgs.Profile());

                VERIFY_IS_FALSE(childCommand.HasNestedCommands());
            }
        }
    }

    void SettingsTests::TestIterableColorSchemeCommands()
    {
        // For this test, put an iterable command with a given `name`,
        // containing a ${profile.name} to replace. When we expand it, it should
        // have created one command for each profile.

        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "historySize": 1,
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "historySize": 2,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "historySize": 3,
                    "commandline": "wsl.exe"
                }
            ],
            "schemes": [
                {
                    "name": "Campbell",
                    "foreground": "#CCCCCC",
                    "background": "#0C0C0C",
                    "cursorColor": "#FFFFFF",
                    "black": "#0C0C0C",
                    "red": "#C50F1F",
                    "green": "#13A10E",
                    "yellow": "#C19C00",
                    "blue": "#0037DA",
                    "purple": "#881798",
                    "cyan": "#3A96DD",
                    "white": "#CCCCCC",
                    "brightBlack": "#767676",
                    "brightRed": "#E74856",
                    "brightGreen": "#16C60C",
                    "brightYellow": "#F9F1A5",
                    "brightBlue": "#3B78FF",
                    "brightPurple": "#B4009E",
                    "brightCyan": "#61D6D6",
                    "brightWhite": "#F2F2F2"
                },
                {
                    "name": "Campbell PowerShell",
                    "foreground": "#CCCCCC",
                    "background": "#012456",
                    "cursorColor": "#FFFFFF",
                    "black": "#0C0C0C",
                    "red": "#C50F1F",
                    "green": "#13A10E",
                    "yellow": "#C19C00",
                    "blue": "#0037DA",
                    "purple": "#881798",
                    "cyan": "#3A96DD",
                    "white": "#CCCCCC",
                    "brightBlack": "#767676",
                    "brightRed": "#E74856",
                    "brightGreen": "#16C60C",
                    "brightYellow": "#F9F1A5",
                    "brightBlue": "#3B78FF",
                    "brightPurple": "#B4009E",
                    "brightCyan": "#61D6D6",
                    "brightWhite": "#F2F2F2"
                },
                {
                    "name": "Vintage",
                    "foreground": "#C0C0C0",
                    "background": "#000000",
                    "cursorColor": "#FFFFFF",
                    "black": "#000000",
                    "red": "#800000",
                    "green": "#008000",
                    "yellow": "#808000",
                    "blue": "#000080",
                    "purple": "#800080",
                    "cyan": "#008080",
                    "white": "#C0C0C0",
                    "brightBlack": "#808080",
                    "brightRed": "#FF0000",
                    "brightGreen": "#00FF00",
                    "brightYellow": "#FFFF00",
                    "brightBlue": "#0000FF",
                    "brightPurple": "#FF00FF",
                    "brightCyan": "#00FFFF",
                    "brightWhite": "#FFFFFF"
                }
            ],
            "actions": [
                {
                    "name": "iterable command ${scheme.name}",
                    "iterateOn": "schemes",
                    "command": { "action": "splitPane", "profile": "${scheme.name}" }
                },
            ]
        })" };

        CascadiaSettings settings{ settingsJson, {} };

        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        auto nameMap{ settings.ActionMap().NameMap() };
        VERIFY_ARE_EQUAL(1u, nameMap.Size());

        {
            auto command = nameMap.TryLookup(L"iterable command ${scheme.name}");
            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"${scheme.name}", terminalArgs.Profile());
        }

        const auto& expandedCommands{ settings.GlobalSettings().ActionMap().ExpandedCommands() };
        _logCommands(expandedCommands);

        VERIFY_ARE_EQUAL(3u, expandedCommands.Size());

        // Yes, this test is testing splitPane with profiles named after each
        // color scheme. These would obviously not work in real life, they're
        // just easy tests to write.

        {
            auto command = expandedCommands.GetAt(0);
            VERIFY_ARE_EQUAL(L"iterable command Campbell", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"Campbell", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(1);
            VERIFY_ARE_EQUAL(L"iterable command Campbell PowerShell", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"Campbell PowerShell", terminalArgs.Profile());
        }

        {
            auto command = expandedCommands.GetAt(2);
            VERIFY_ARE_EQUAL(L"iterable command Vintage", command.Name());

            VERIFY_IS_NOT_NULL(command);
            auto actionAndArgs = command.ActionAndArgs();
            VERIFY_IS_NOT_NULL(actionAndArgs);
            VERIFY_ARE_EQUAL(ShortcutAction::SplitPane, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<SplitPaneArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            VERIFY_ARE_EQUAL(SplitDirection::Automatic, realArgs.SplitDirection());
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"Vintage", terminalArgs.Profile());
        }
    }

    void SettingsTests::TestElevateArg()
    {
        static constexpr std::wstring_view settingsJson{ LR"(
        {
            "defaultProfile": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
            "profiles": [
                {
                    "name": "profile0",
                    "guid": "{6239a42c-0000-49a3-80bd-e8fdd045185c}",
                    "commandline": "cmd.exe"
                },
                {
                    "name": "profile1",
                    "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                    "elevate": true,
                    "commandline": "pwsh.exe"
                },
                {
                    "name": "profile2",
                    "guid": "{6239a42c-2222-49a3-80bd-e8fdd045185c}",
                    "elevate": false,
                    "commandline": "wsl.exe"
                }
            ],
            "keybindings": [
                { "keys": ["ctrl+a"], "command": { "action": "newTab", "profile": "profile0" } },
                { "keys": ["ctrl+b"], "command": { "action": "newTab", "profile": "profile1" } },
                { "keys": ["ctrl+c"], "command": { "action": "newTab", "profile": "profile2" } },

                { "keys": ["ctrl+d"], "command": { "action": "newTab", "profile": "profile0", "elevate": false } },
                { "keys": ["ctrl+e"], "command": { "action": "newTab", "profile": "profile1", "elevate": false } },
                { "keys": ["ctrl+f"], "command": { "action": "newTab", "profile": "profile2", "elevate": false } },

                { "keys": ["ctrl+g"], "command": { "action": "newTab", "profile": "profile0", "elevate": true } },
                { "keys": ["ctrl+h"], "command": { "action": "newTab", "profile": "profile1", "elevate": true } },
                { "keys": ["ctrl+i"], "command": { "action": "newTab", "profile": "profile2", "elevate": true } },
            ]
        })" };

        const winrt::guid guid0{ ::Microsoft::Console::Utils::GuidFromString(L"{6239a42c-0000-49a3-80bd-e8fdd045185c}") };
        const winrt::guid guid1{ ::Microsoft::Console::Utils::GuidFromString(L"{6239a42c-1111-49a3-80bd-e8fdd045185c}") };
        const winrt::guid guid2{ ::Microsoft::Console::Utils::GuidFromString(L"{6239a42c-2222-49a3-80bd-e8fdd045185c}") };

        CascadiaSettings settings{ settingsJson, {} };

        auto keymap = settings.GlobalSettings().ActionMap();
        VERIFY_ARE_EQUAL(3u, settings.ActiveProfiles().Size());

        const auto profile2Guid = settings.ActiveProfiles().GetAt(2).Guid();
        VERIFY_ARE_NOT_EQUAL(winrt::guid{}, profile2Guid);

        VERIFY_ARE_EQUAL(9u, keymap.KeyBindings().Size());

        {
            Log::Comment(L"profile.elevate=omitted, action.elevate=nullopt: don't auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('A'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile0", terminalArgs.Profile());
            VERIFY_IS_NULL(terminalArgs.Elevate());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"cmd.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(false, termSettings->Elevate());
        }
        {
            Log::Comment(L"profile.elevate=true, action.elevate=nullopt: DO auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('B'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile1", terminalArgs.Profile());
            VERIFY_IS_NULL(terminalArgs.Elevate());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"pwsh.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(true, termSettings->Elevate());
        }
        {
            Log::Comment(L"profile.elevate=false, action.elevate=nullopt: don't auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('C'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile2", terminalArgs.Profile());
            VERIFY_IS_NULL(terminalArgs.Elevate());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"wsl.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(false, termSettings->Elevate());
        }

        {
            Log::Comment(L"profile.elevate=omitted, action.elevate=false: don't auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('D'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile0", terminalArgs.Profile());
            VERIFY_IS_NOT_NULL(terminalArgs.Elevate());
            VERIFY_IS_FALSE(terminalArgs.Elevate().Value());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"cmd.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(false, termSettings->Elevate());
        }
        {
            Log::Comment(L"profile.elevate=true, action.elevate=false: don't auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('E'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile1", terminalArgs.Profile());
            VERIFY_IS_NOT_NULL(terminalArgs.Elevate());
            VERIFY_IS_FALSE(terminalArgs.Elevate().Value());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"pwsh.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(false, termSettings->Elevate());
        }
        {
            Log::Comment(L"profile.elevate=false, action.elevate=false: don't auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('F'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile2", terminalArgs.Profile());
            VERIFY_IS_NOT_NULL(terminalArgs.Elevate());
            VERIFY_IS_FALSE(terminalArgs.Elevate().Value());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"wsl.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(false, termSettings->Elevate());
        }

        {
            Log::Comment(L"profile.elevate=omitted, action.elevate=true: DO auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('G'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile0", terminalArgs.Profile());
            VERIFY_IS_NOT_NULL(terminalArgs.Elevate());
            VERIFY_IS_TRUE(terminalArgs.Elevate().Value());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"cmd.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(true, termSettings->Elevate());
        }
        {
            Log::Comment(L"profile.elevate=true, action.elevate=true: DO auto elevate");
            KeyChord kc{ true, false, false, false, static_cast<int32_t>('H'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile1", terminalArgs.Profile());
            VERIFY_IS_NOT_NULL(terminalArgs.Elevate());
            VERIFY_IS_TRUE(terminalArgs.Elevate().Value());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"pwsh.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(true, termSettings->Elevate());
        }
        {
            Log::Comment(L"profile.elevate=false, action.elevate=true: DO auto elevate");

            KeyChord kc{ true, false, false, false, static_cast<int32_t>('I'), 0 };
            auto actionAndArgs = TestUtils::GetActionAndArgs(keymap, kc);
            VERIFY_ARE_EQUAL(ShortcutAction::NewTab, actionAndArgs.Action());
            const auto& realArgs = actionAndArgs.Args().try_as<NewTabArgs>();
            VERIFY_IS_NOT_NULL(realArgs);
            // Verify the args have the expected value
            auto terminalArgs{ realArgs.ContentArgs().try_as<NewTerminalArgs>() };
            VERIFY_IS_NOT_NULL(terminalArgs);
            VERIFY_IS_TRUE(terminalArgs.Commandline().empty());
            VERIFY_IS_TRUE(terminalArgs.StartingDirectory().empty());
            VERIFY_IS_TRUE(terminalArgs.TabTitle().empty());
            VERIFY_IS_FALSE(terminalArgs.Profile().empty());
            VERIFY_ARE_EQUAL(L"profile2", terminalArgs.Profile());
            VERIFY_IS_NOT_NULL(terminalArgs.Elevate());
            VERIFY_IS_TRUE(terminalArgs.Elevate().Value());

            const auto termSettingsResult = TerminalSettings::CreateWithNewTerminalArgs(settings, terminalArgs);
            const auto termSettings = termSettingsResult.DefaultSettings();
            VERIFY_ARE_EQUAL(L"wsl.exe", termSettings->Commandline());
            VERIFY_ARE_EQUAL(true, termSettings->Elevate());
        }
    }

    void SettingsTests::TestAgentPanePrewarmSchedulesOnlyActiveTab()
    {
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker active;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker background;

        VERIFY_IS_TRUE(active.Schedule(true));
        VERIFY_IS_FALSE(background.Schedule(false));
    }

    void SettingsTests::TestAgentPanePrewarmRechecksSelection()
    {
        using State = winrt::TerminalApp::implementation::details::AgentPanePrewarmState;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;

        VERIFY_IS_TRUE(tracker.Schedule(true));
        VERIFY_IS_FALSE(tracker.BeginScheduled(false));
        VERIFY_ARE_EQUAL(State::Dormant, tracker.State());
        VERIFY_IS_TRUE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.BeginScheduled(true));
        tracker.Complete(true);
        VERIFY_ARE_EQUAL(State::Materialized, tracker.State());
    }

    void SettingsTests::TestRestoredAgentPaneSuppressesBlankPrewarm()
    {
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker restored;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker transferred;

        restored.MarkMaterialized();
        transferred.MarkMaterialized();
        VERIFY_IS_FALSE(restored.Schedule(true));
        VERIFY_IS_FALSE(transferred.Schedule(true));
    }

    void SettingsTests::TestAgentPanePrewarmSuppressesDuplicates()
    {
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;

        VERIFY_IS_TRUE(tracker.Schedule(true));
        VERIFY_IS_FALSE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.BeginScheduled(true));
        VERIFY_IS_FALSE(tracker.BeginImmediate());
    }

    void SettingsTests::TestExplicitlyClosedAgentPaneStaysClosedUntilRequested()
    {
        using State = winrt::TerminalApp::implementation::details::AgentPanePrewarmState;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;

        tracker.MarkExplicitlyClosed();
        VERIFY_IS_FALSE(tracker.Schedule(true));
        VERIFY_ARE_EQUAL(State::ExplicitlyClosed, tracker.State());
        VERIFY_IS_TRUE(tracker.BeginImmediate());
    }

    void SettingsTests::TestExplicitCloseWinsMaterializedObservationRace()
    {
        using State = winrt::TerminalApp::implementation::details::AgentPanePrewarmState;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;

        tracker.MarkMaterialized();
        tracker.MarkExplicitlyClosed();
        tracker.ObserveMaterialized();
        tracker.MarkAbsent();

        VERIFY_ARE_EQUAL(State::ExplicitlyClosed, tracker.State());
        VERIFY_IS_FALSE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.BeginImmediate());
        tracker.Complete(true);
        VERIFY_ARE_EQUAL(State::Materialized, tracker.State());
    }

    void SettingsTests::TestSpeculativePrewarmSelectionBudget()
    {
        using Tracker = winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker;
        Tracker tabs[3];

        for (auto selected = 0; selected < 3; ++selected)
        {
            for (auto index = 0; index < 3; ++index)
            {
                if (index != selected)
                {
                    tabs[index].EvictUnusedSpeculative();
                }
            }
            VERIFY_IS_TRUE(tabs[selected].Schedule(true));
            VERIFY_IS_TRUE(tabs[selected].BeginScheduled(true));
            tabs[selected].Complete(true);

            const auto retained = std::count_if(std::begin(tabs), std::end(tabs), [](const auto& tracker) {
                return tracker.IsUnusedSpeculative();
            });
            VERIFY_ARE_EQUAL(decltype(retained){ 1 }, retained);
        }
    }

    void SettingsTests::TestOpenedAgentPaneIsNotSpeculative()
    {
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;
        VERIFY_IS_TRUE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.BeginScheduled(true));
        tracker.Complete(true);
        VERIFY_IS_TRUE(tracker.IsUnusedSpeculative());

        tracker.MarkUsed();
        VERIFY_IS_FALSE(tracker.IsUnusedSpeculative());
        VERIFY_IS_FALSE(tracker.EvictUnusedSpeculative());
    }

    void SettingsTests::TestRestoredAndTransferredPanesAreNotSpeculative()
    {
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker restored;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker transferred;

        restored.MarkMaterialized();
        transferred.MarkMaterialized();
        transferred.MarkUsed();

        VERIFY_IS_FALSE(restored.EvictUnusedSpeculative());
        VERIFY_IS_FALSE(transferred.EvictUnusedSpeculative());
    }

    void SettingsTests::TestExplicitCloseDuringSpeculativeMaterializationWins()
    {
        using State = winrt::TerminalApp::implementation::details::AgentPanePrewarmState;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;

        VERIFY_IS_TRUE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.BeginScheduled(true));
        tracker.MarkExplicitlyClosed();
        tracker.Complete(true);

        VERIFY_ARE_EQUAL(State::ExplicitlyClosed, tracker.State());
        VERIFY_IS_FALSE(tracker.IsUnusedSpeculative());
        VERIFY_IS_FALSE(tracker.Schedule(true));
    }

    void SettingsTests::TestRepeatedSelectionRetainsOneSpeculativePane()
    {
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker tracker;
        VERIFY_IS_TRUE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.BeginScheduled(true));
        tracker.Complete(true);

        VERIFY_IS_FALSE(tracker.Schedule(true));
        VERIFY_IS_TRUE(tracker.IsUnusedSpeculative());
    }

    void SettingsTests::TestSettingsRecreationPreservesSpeculativeBudget()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using Tracker = winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker;
        Tracker tabs[3];

        VERIFY_IS_TRUE(tabs[0].Schedule(true));
        VERIFY_IS_TRUE(tabs[0].BeginScheduled(true));
        tabs[0].Complete(true);
        const auto speculativeRecreation =
            Page::_GetAgentPaneRecreationOptions(true, true, tabs[0].IsUnusedSpeculative());
        VERIFY_IS_TRUE(speculativeRecreation.speculative);
        tabs[0].MarkAbsent();
        tabs[0].MarkMaterialized(speculativeRecreation.speculative);

        tabs[0].EvictUnusedSpeculative();
        VERIFY_IS_TRUE(tabs[1].Schedule(true));
        VERIFY_IS_TRUE(tabs[1].BeginScheduled(true));
        tabs[1].Complete(true);

        const auto retained = std::count_if(std::begin(tabs), std::end(tabs), [](const auto& tracker) {
            return tracker.IsUnusedSpeculative();
        });
        VERIFY_ARE_EQUAL(decltype(retained){ 1 }, retained);

        const auto openedRecreation = Page::_GetAgentPaneRecreationOptions(true, false, false);
        VERIFY_IS_FALSE(openedRecreation.speculative);
    }

    void SettingsTests::TestStartupPrewarmIgnoresSlowEmptyAvailabilityCache()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        const std::vector<std::wstring> allowed{ L"copilot", L"codex" };

        Availability::ResetCacheForTests();
        auto reset = wil::scope_exit([]() {
            Availability::ResetCacheForTests();
        });
        std::promise<void> probeStarted;
        std::promise<void> releaseProbe;
        const auto release = releaseProbe.get_future().share();
        Availability::SetProbeFunctionForTests([&]() {
            probeStarted.set_value();
            release.wait();
            return Availability::AgentIds{};
        });
        auto refresh = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        probeStarted.get_future().wait();

        const auto selected = Page::_ResolvePolicyApprovedDefaultAgent(
            L"copilot",
            {},
            true,
            allowed,
            {});
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker helper;
        const auto helperScheduled = helper.Schedule(true);
        releaseProbe.set_value();
        refresh.get();

        VERIFY_IS_TRUE(selected.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot" }, selected->id);
        VERIFY_IS_FALSE(selected->command.empty());
        VERIFY_IS_TRUE(helperScheduled);
    }

    void SettingsTests::TestStartupPrewarmRejectsNoAllowedAgents()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        const std::vector<std::wstring> allowed;

        VERIFY_IS_FALSE(Page::_ResolvePolicyApprovedDefaultAgent(
                                 L"copilot",
                                 {},
                                 true,
                                 allowed,
                                 {})
                            .has_value());
    }

    void SettingsTests::TestStartupPrewarmBlockedCopilotUsesExplicitFallback()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        const std::vector<std::wstring> allowed{ L"codex" };

        const auto selected = Page::_ResolvePolicyApprovedDefaultAgent(
            L"copilot",
            {},
            true,
            allowed,
            {});
        VERIFY_IS_TRUE(selected.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"codex" }, selected->id);
        VERIFY_IS_FALSE(selected->command.empty());
        VERIFY_IS_TRUE(selected->command.find(L"copilot") == std::wstring::npos);
    }

    void SettingsTests::TestStartupPrewarmCustomRequiresTrustedCommand()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        const std::vector<std::wstring> noBuiltins;

        const auto valid = Page::_ResolvePolicyApprovedDefaultAgent(
            L"custom:local",
            L"agent.exe --acp",
            true,
            noBuiltins,
            {});
        VERIFY_IS_TRUE(valid.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"custom:local" }, valid->id);
        VERIFY_ARE_EQUAL(std::wstring{ L"agent.exe --acp" }, valid->command);

        VERIFY_IS_FALSE(Page::_ResolvePolicyApprovedDefaultAgent(
                                 L"custom:local",
                                 L" \t ",
                                 true,
                                 noBuiltins,
                                 {})
                            .has_value());
        VERIFY_IS_FALSE(Page::_ResolvePolicyApprovedDefaultAgent(
                                 L"custom:local",
                                 L"agent.exe --acp",
                                 false,
                                 noBuiltins,
                                 {})
                            .has_value());
        VERIFY_IS_FALSE(Page::_ResolvePolicyApprovedDefaultAgent(
                                 L"custom:",
                                 L"agent.exe --acp",
                                 true,
                                 noBuiltins,
                                 {})
                            .has_value());
    }

    void SettingsTests::TestStartupPrewarmAvailabilityDoesNotDuplicate()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        winrt::TerminalApp::implementation::details::MasterOwnershipTracker provider;
        winrt::TerminalApp::implementation::details::AgentPanePrewarmTracker helper;

        VERIFY_IS_TRUE(provider.AcquireLease());
        VERIFY_IS_TRUE(helper.Schedule(true));
        VERIFY_IS_TRUE(helper.BeginScheduled(true));
        helper.Complete(true);

        bool refreshScheduled = true;
        VERIFY_IS_FALSE(Page::_CompleteAgentAvailabilityRefresh(refreshScheduled, 1, 1));
        VERIFY_IS_FALSE(refreshScheduled);
        VERIFY_IS_FALSE(provider.AcquireLease());
        VERIFY_IS_FALSE(helper.Schedule(true));
    }

    void SettingsTests::TestAgentAvailabilityUiSnapshotIsImmediateAndRefreshIsSingleFlight()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;
        Availability::ResetCacheForTests();
        auto reset = wil::scope_exit([]() {
            Availability::ResetCacheForTests();
        });

        std::atomic<uint32_t> probeCalls{ 0 };
        std::promise<void> probeStarted;
        std::promise<void> releaseProbe;
        const auto release = releaseProbe.get_future().share();
        Availability::SetProbeFunctionForTests([&]() {
            if (probeCalls.fetch_add(1) == 0)
            {
                probeStarted.set_value();
            }
            release.wait();
            return Availability::AgentIds{ L"codex" };
        });

        auto firstRefresh = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        probeStarted.get_future().wait();
        auto secondRefresh = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        auto uiRead = std::async(std::launch::async, []() {
            return Availability::GetCachedHostAgentIds();
        });

        const auto uiReadStatus = uiRead.wait_for(std::chrono::milliseconds{ 100 });
        releaseProbe.set_value();
        VERIFY_IS_TRUE(uiReadStatus == std::future_status::ready);
        VERIFY_IS_TRUE(uiRead.get().empty());

        VERIFY_IS_TRUE(firstRefresh.get().contains(L"codex"));
        VERIFY_IS_TRUE(secondRefresh.get().contains(L"codex"));
        VERIFY_ARE_EQUAL(1u, probeCalls.load());
    }

    void SettingsTests::TestAgentAvailabilityInvalidationDuringProbeRetriesJoinedCallers()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;
        Availability::ResetCacheForTests();
        auto reset = wil::scope_exit([]() {
            Availability::ResetCacheForTests();
        });

        std::atomic<uint32_t> probeCalls{ 0 };
        std::promise<void> staleProbeStarted;
        std::promise<void> freshProbeStarted;
        std::promise<void> releaseStaleProbe;
        std::promise<void> releaseFreshProbe;
        const auto releaseStale = releaseStaleProbe.get_future().share();
        const auto releaseFresh = releaseFreshProbe.get_future().share();
        Availability::SetProbeFunctionForTests([&]() {
            const auto call = probeCalls.fetch_add(1);
            if (call == 0)
            {
                staleProbeStarted.set_value();
                releaseStale.wait();
                return Availability::AgentIds{ L"codex" };
            }

            VERIFY_ARE_EQUAL(1u, call);
            freshProbeStarted.set_value();
            releaseFresh.wait();
            return Availability::AgentIds{ L"claude" };
        });

        auto originalCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        staleProbeStarted.get_future().wait();

        Availability::InvalidateHostAgentIds();
        auto followUpCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        releaseStaleProbe.set_value();
        freshProbeStarted.get_future().wait();

        auto joinedCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        VERIFY_IS_TRUE(Availability::WaitForRefreshCallersForTests(
            2,
            std::chrono::seconds{ 2 }));
        VERIFY_IS_TRUE(joinedCaller.wait_for(std::chrono::milliseconds{ 0 }) == std::future_status::timeout);
        VERIFY_IS_TRUE(Availability::GetCachedHostAgentIds().empty());

        releaseFreshProbe.set_value();
        VERIFY_IS_TRUE(originalCaller.get().contains(L"claude"));
        VERIFY_IS_TRUE(followUpCaller.get().contains(L"claude"));
        VERIFY_IS_TRUE(joinedCaller.get().contains(L"claude"));
        VERIFY_IS_FALSE(Availability::GetCachedHostAgentIds().contains(L"codex"));
        VERIFY_ARE_EQUAL(2u, probeCalls.load());
    }

    void SettingsTests::TestAgentAvailabilityFailureReachesProbingAndJoinedCallersThenRetries()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;
        Availability::ResetCacheForTests();
        auto reset = wil::scope_exit([]() {
            Availability::ResetCacheForTests();
        });

        std::atomic<uint32_t> probeCalls{ 0 };
        std::promise<void> failedProbeStarted;
        std::promise<void> releaseFailedProbe;
        const auto release = releaseFailedProbe.get_future().share();
        Availability::SetProbeFunctionForTests([&]() {
            if (probeCalls.fetch_add(1) == 0)
            {
                failedProbeStarted.set_value();
                release.wait();
                throw std::runtime_error{ "deterministic probe failure" };
            }
            return Availability::AgentIds{ L"codex" };
        });

        auto probingCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        failedProbeStarted.get_future().wait();
        auto joinedCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        VERIFY_IS_TRUE(Availability::WaitForRefreshCallersForTests(
            1,
            std::chrono::seconds{ 2 }));
        releaseFailedProbe.set_value();

        bool probingCallerFailed = false;
        try
        {
            static_cast<void>(probingCaller.get());
        }
        catch (const std::runtime_error& e)
        {
            probingCallerFailed = std::string_view{ e.what() } == "deterministic probe failure";
        }
        bool joinedCallerFailed = false;
        try
        {
            static_cast<void>(joinedCaller.get());
        }
        catch (const std::runtime_error& e)
        {
            joinedCallerFailed = std::string_view{ e.what() } == "deterministic probe failure";
        }

        VERIFY_IS_TRUE(probingCallerFailed);
        VERIFY_IS_TRUE(joinedCallerFailed);
        VERIFY_IS_FALSE(Availability::IsHostAgentCacheValidForTests());
        VERIFY_IS_TRUE(Availability::GetCachedHostAgentIds().empty());
        VERIFY_ARE_EQUAL(1u, probeCalls.load());

        const auto retried = Availability::RefreshHostAgentIds();
        VERIFY_IS_TRUE(retried.contains(L"codex"));
        VERIFY_IS_TRUE(Availability::IsHostAgentCacheValidForTests());
        VERIFY_ARE_EQUAL(2u, probeCalls.load());
    }

    void SettingsTests::TestAgentAvailabilityInvalidationThenFailureReachesAllCallers()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;
        Availability::ResetCacheForTests();
        auto reset = wil::scope_exit([]() {
            Availability::ResetCacheForTests();
        });

        std::atomic<uint32_t> probeCalls{ 0 };
        std::promise<void> staleProbeStarted;
        std::promise<void> failedProbeStarted;
        std::promise<void> releaseStaleProbe;
        std::promise<void> releaseFailedProbe;
        const auto releaseStale = releaseStaleProbe.get_future().share();
        const auto releaseFailed = releaseFailedProbe.get_future().share();
        Availability::SetProbeFunctionForTests([&]() -> Availability::AgentIds {
            const auto call = probeCalls.fetch_add(1);
            if (call == 0)
            {
                staleProbeStarted.set_value();
                releaseStale.wait();
                throw std::runtime_error{ "stale failure" };
            }

            VERIFY_ARE_EQUAL(1u, call);
            failedProbeStarted.set_value();
            releaseFailed.wait();
            throw std::runtime_error{ "current failure" };
        });

        auto originalCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        staleProbeStarted.get_future().wait();
        Availability::InvalidateHostAgentIds();
        auto joinedCaller = std::async(std::launch::async, []() {
            return Availability::RefreshHostAgentIds();
        });
        releaseStaleProbe.set_value();
        failedProbeStarted.get_future().wait();
        VERIFY_IS_TRUE(Availability::WaitForRefreshCallersForTests(
            1,
            std::chrono::seconds{ 2 }));
        releaseFailedProbe.set_value();

        for (auto* caller : { &originalCaller, &joinedCaller })
        {
            bool failedWithCurrentGeneration = false;
            try
            {
                static_cast<void>(caller->get());
            }
            catch (const std::runtime_error& e)
            {
                failedWithCurrentGeneration = std::string_view{ e.what() } == "current failure";
            }
            VERIFY_IS_TRUE(failedWithCurrentGeneration);
        }
        VERIFY_IS_FALSE(Availability::IsHostAgentCacheValidForTests());
        VERIFY_ARE_EQUAL(2u, probeCalls.load());
    }

    void SettingsTests::TestAgentAvailabilityRepeatedInvalidationIsBounded()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;
        Availability::ResetCacheForTests();
        auto reset = wil::scope_exit([]() {
            Availability::ResetCacheForTests();
        });

        std::atomic<uint32_t> probeCalls{ 0 };
        Availability::SetProbeFunctionForTests([&]() {
            ++probeCalls;
            Availability::InvalidateHostAgentIds();
            return Availability::AgentIds{ L"codex" };
        });

        bool boundedFailure = false;
        try
        {
            static_cast<void>(Availability::RefreshHostAgentIds());
        }
        catch (const std::runtime_error& e)
        {
            boundedFailure = std::string_view{ e.what() } == "Agent availability refresh was repeatedly invalidated";
        }

        VERIFY_IS_TRUE(boundedFailure);
        VERIFY_ARE_EQUAL(8u, probeCalls.load());
        VERIFY_IS_FALSE(Availability::IsHostAgentCacheValidForTests());
    }

    void SettingsTests::TestTerminalPageAgentAvailabilityFailureAllowsRetry()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;

        bool scheduled = false;
        uint64_t generation = 0;
        const auto first = Page::_TryScheduleAgentAvailabilityRefresh(scheduled, generation);
        VERIFY_IS_TRUE(first.has_value());
        VERIFY_IS_TRUE(scheduled);

        const auto joined = Page::_TryScheduleAgentAvailabilityRefresh(scheduled, generation);
        VERIFY_IS_FALSE(joined.has_value());
        VERIFY_ARE_EQUAL(2ull, generation);

        const bool refreshAgain =
            Page::_CompleteAgentAvailabilityRefresh(scheduled, *first, generation);
        VERIFY_IS_TRUE(refreshAgain);
        VERIFY_IS_FALSE(scheduled);

        const auto retry = Page::_TryScheduleAgentAvailabilityRefresh(scheduled, generation);
        VERIFY_IS_TRUE(retry.has_value());
        VERIFY_ARE_EQUAL(3ull, *retry);
        VERIFY_IS_TRUE(scheduled);
    }

    void SettingsTests::TestFreAgentSetupRequiresSelectionAndGuardsHooks()
    {
        namespace Setup = ::Microsoft::Terminal::FreAgentSetup;

        VERIFY_ARE_EQUAL(
            Setup::AvailabilityState::Unavailable,
            Setup::ClassifyAvailability(false, true, true));
        VERIFY_ARE_EQUAL(
            Setup::AvailabilityState::BlockedByPolicy,
            Setup::ClassifyAvailability(false, false, true));
        VERIFY_ARE_EQUAL(
            Setup::AvailabilityState::Available,
            Setup::ClassifyAvailability(true, true, true));

        VERIFY_IS_FALSE(Setup::CanSave(L""));
        VERIFY_IS_FALSE(Setup::ShouldInstallHooks(L"", true, false));

        VERIFY_IS_TRUE(Setup::CanSave(L"codex"));
        VERIFY_IS_FALSE(Setup::ShouldInstallHooks(L"codex", false, false));
        VERIFY_IS_FALSE(Setup::ShouldInstallHooks(L"codex", true, true));
        VERIFY_IS_TRUE(Setup::ShouldInstallHooks(L"codex", true, false));
    }

    void SettingsTests::TestFrePostInstallAvailabilityFailureStopsSetupAndAllowsRetry()
    {
        namespace Setup = ::Microsoft::Terminal::FreAgentSetup;

        VERIFY_IS_FALSE(Setup::CanContinueAfterPostInstallRefresh(false, L"codex", false));
        VERIFY_IS_FALSE(Setup::CanContinueAfterPostInstallRefresh(true, L"", true));
        VERIFY_IS_FALSE(Setup::CanContinueAfterPostInstallRefresh(true, L"codex", false));
        VERIFY_IS_TRUE(Setup::CanContinueAfterPostInstallRefresh(true, L"codex", true));
    }

    void SettingsTests::TestAgentPickerRefreshResolvesPolicyDefaultAndPreservesSelection()
    {
        namespace Availability = ::Microsoft::Terminal::AgentAvailability;

        const std::vector<std::wstring> codexOnly{ L"codex" };
        const Availability::AgentIds initiallyEmpty;
        VERIFY_IS_FALSE(
            Availability::SelectAvailableAllowedAgentId(L"copilot", codexOnly, initiallyEmpty).has_value());

        const Availability::AgentIds completed{ L"copilot", L"codex" };
        const auto policyDefault =
            Availability::SelectAvailableAllowedAgentId(L"copilot", codexOnly, completed);
        VERIFY_IS_TRUE(policyDefault.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"codex" }, *policyDefault);

        const std::vector<std::wstring> bothAllowed{ L"copilot", L"codex" };
        const auto preserved =
            Availability::SelectAvailableAllowedAgentId(L"codex", bothAllowed, completed);
        VERIFY_IS_TRUE(preserved.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"codex" }, *preserved);
    }

    void SettingsTests::TestAgentSettingsChangeClassification()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using ChangeKind = Page::AgentSettingsChangeKind;

        const Page::AgentSettingsSnapshot native{
            L"copilot", L"", L"gpt-5.4", std::nullopt, {}
        };
        VERIFY_ARE_EQUAL(ChangeKind::None, Page::_ClassifyAgentSettingsChange(native, native));

        auto nativeHotUpdate = native;
        nativeHotUpdate.acpModel = L"gpt-5.5";
        VERIFY_ARE_EQUAL(
            ChangeKind::ModelHotUpdate,
            Page::_ClassifyAgentSettingsChange(native, nativeHotUpdate));

        auto nativeReset = native;
        nativeReset.acpModel.clear();
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(native, nativeReset));

        auto builtInAgentChange = native;
        builtInAgentChange.acpAgent = L"codex";
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(native, builtInAgentChange));

        namespace AgentPolicy = ::Microsoft::Terminal::Settings::Model::AgentPolicy;
        AgentPolicy::PolicySnapshot unrestrictedSnapshot;
        auto unrestrictedPolicy = native;
        unrestrictedPolicy.effectiveAcpAgent = L"copilot";
        unrestrictedPolicy.allowedAcpAgents = Page::_CaptureAllowedAcpAgentIds(unrestrictedSnapshot);
        AgentPolicy::PolicySnapshot partialSnapshot;
        partialSnapshot.allowedAgents =
            std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"copilot", L"codex" };
        auto partialPolicy = unrestrictedPolicy;
        partialPolicy.allowedAcpAgents = Page::_CaptureAllowedAcpAgentIds(partialSnapshot);
        VERIFY_ARE_EQUAL(
            ChangeKind::RecreatePane,
            Page::_ClassifyAgentSettingsChange(unrestrictedPolicy, partialPolicy));
        VERIFY_ARE_EQUAL(
            ChangeKind::None,
            Page::_ClassifyAgentSettingsChange(partialPolicy, partialPolicy));

        auto blockedDefaultPolicy = partialPolicy;
        blockedDefaultPolicy.effectiveAcpAgent = L"codex";
        VERIFY_ARE_EQUAL(
            ChangeKind::RecreatePane,
            Page::_ClassifyAgentSettingsChange(partialPolicy, blockedDefaultPolicy));

        auto gemini = native;
        gemini.acpAgent = L"gemini";
        auto geminiModelChange = gemini;
        geminiModelChange.acpModel = L"gemini-2.5-pro";
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(gemini, geminiModelChange));

        auto byok = native;
        byok.acpModel.clear();
        byok.customModelLaunch =
            ::Microsoft::Terminal::CustomModels::MakeLaunchConfiguration(
                L"custom:provider:model-a",
                L"https://example.invalid/v1",
                L"model-a",
                L"credential-a",
                true);
        auto changedByok = byok;
        changedByok.customModelLaunch->modelId = L"model-b";
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(byok, changedByok));

        auto changedByokEndpoint = byok;
        changedByokEndpoint.customModelLaunch->endpoint = L"https://new.example.invalid/v1";
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(byok, changedByokEndpoint));

        auto changedByokCredential = byok;
        changedByokCredential.customModelLaunch->credentialId = L"credential-b";
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(byok, changedByokCredential));

        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(native, byok));
        VERIFY_ARE_EQUAL(
            ChangeKind::AgentRebind,
            Page::_ClassifyAgentSettingsChange(byok, native));

        auto customCommand = native;
        customCommand.acpAgent = L"custom:local";
        customCommand.acpCustomCommand = L"agent.exe --acp";
        VERIFY_ARE_EQUAL(
            ChangeKind::RecreatePane,
            Page::_ClassifyAgentSettingsChange(native, customCommand));

        auto customModelChange = customCommand;
        customModelChange.acpAgent = L"custom:test";
        customModelChange.acpModel = L"model-a";
        auto changedCustomModel = customModelChange;
        changedCustomModel.acpModel = L"model-b";
        VERIFY_ARE_EQUAL(
            ChangeKind::RecreatePane,
            Page::_ClassifyAgentSettingsChange(customModelChange, changedCustomModel));
    }

    void SettingsTests::TestAgentSettingsFocusGate()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using ChangeKind = Page::AgentSettingsChangeKind;

        VERIFY_IS_FALSE(Page::_ShouldDeferAgentSettingsChange(ChangeKind::None, false, false));
        VERIFY_IS_FALSE(Page::_ShouldDeferAgentSettingsChange(ChangeKind::ModelHotUpdate, false, false));
        VERIFY_IS_FALSE(Page::_ShouldDeferAgentSettingsChange(ChangeKind::AgentRebind, false, false));
        VERIFY_IS_TRUE(Page::_ShouldDeferAgentSettingsChange(ChangeKind::RecreatePane, false, false));
        VERIFY_IS_FALSE(Page::_ShouldDeferAgentSettingsChange(ChangeKind::RecreatePane, true, false));
        VERIFY_IS_FALSE(Page::_ShouldDeferAgentSettingsChange(ChangeKind::RecreatePane, false, true));
    }

    void SettingsTests::TestAgentPaneRebindCapability()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;

        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::NotConnected, false));
        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::NotConnected, true));
        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::Connecting, false));
        VERIFY_IS_TRUE(Page::_CanRebindAgentPane(State::Connecting, true));
        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::Connected, false));
        VERIFY_IS_TRUE(Page::_CanRebindAgentPane(State::Connected, true));
        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::Closing, true));
        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::Closed, true));
        VERIFY_IS_FALSE(Page::_CanRebindAgentPane(State::Failed, true));

        const auto visibleActive = Page::_GetAgentPaneRecreationOptions(false, true, false);
        VERIFY_IS_FALSE(visibleActive.autoStash);
        VERIFY_IS_TRUE(visibleActive.focusPane);
        VERIFY_IS_FALSE(visibleActive.speculative);

        const auto visibleBackground = Page::_GetAgentPaneRecreationOptions(false, false, false);
        VERIFY_IS_FALSE(visibleBackground.autoStash);
        VERIFY_IS_FALSE(visibleBackground.focusPane);
        VERIFY_IS_FALSE(visibleBackground.speculative);

        const auto stashedActive = Page::_GetAgentPaneRecreationOptions(true, true, false);
        VERIFY_IS_TRUE(stashedActive.autoStash);
        VERIFY_IS_FALSE(stashedActive.focusPane);
        VERIFY_IS_FALSE(stashedActive.speculative);

        const auto stashedBackground = Page::_GetAgentPaneRecreationOptions(true, false, true);
        VERIFY_IS_TRUE(stashedBackground.autoStash);
        VERIFY_IS_FALSE(stashedBackground.focusPane);
        VERIFY_IS_TRUE(stashedBackground.speculative);
    }

    void SettingsTests::TestAgentPaneSettingsRebindRouting()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using Request = Page::AgentPaneSettingsBindingRequest;

        struct TestCase
        {
            const wchar_t* name;
            Request request;
            bool globalAgentChanged;
            bool cloudModelChanged;
            bool customModelLaunchChanged;
            bool expectedAffected;
            bool expectedLaunchable;
            const wchar_t* expectedAgent;
            const wchar_t* expectedSource;
            const wchar_t* expectedModel;
            const wchar_t* expectedCustomSelection;
        };

        const Request byokGlobal{
            .globalAgentId = L"copilot",
            .globalModel = L"gpt-cloud",
            .globalAgentCliPath = L"copilot --acp --stdio",
            .customModelSelection = L"custom:provider:model-b",
        };
        const Request cloudGlobal{
            .globalAgentId = L"copilot",
            .globalModel = L"gpt-5.6",
            .globalAgentCliPath = L"copilot --acp --stdio --model gpt-5.6",
        };

        std::vector<TestCase> cases;
        cases.push_back({
            L"global Host Copilot inherits BYOK",
            byokGlobal,
            false,
            false,
            true,
            true,
            true,
            L"copilot",
            L"host",
            L"",
            L"custom:provider:model-b",
        });

        auto hostOpenCodeOverride = byokGlobal;
        hostOpenCodeOverride.hasAgentOverride = true;
        hostOpenCodeOverride.agentIdOverride = L"opencode";
        hostOpenCodeOverride.agentModelOverride = L"override-model";
        hostOpenCodeOverride.agentSourceOverride = L"host";
        cases.push_back({
            L"Host OpenCode override inherits BYOK",
            hostOpenCodeOverride,
            false,
            false,
            true,
            true,
            true,
            L"opencode",
            L"host",
            L"",
            L"custom:provider:model-b",
        });

        auto hostCopilotProfile = byokGlobal;
        hostCopilotProfile.profileBackend = L"host:copilot";
        cases.push_back({
            L"host Copilot profile inherits BYOK",
            hostCopilotProfile,
            false,
            false,
            true,
            true,
            true,
            L"copilot",
            L"host",
            L"",
            L"custom:provider:model-b",
        });

        auto wslOpenCodeOverride = hostOpenCodeOverride;
        wslOpenCodeOverride.agentSourceOverride = L"wsl";
        wslOpenCodeOverride.agentWslDistroOverride = L"Ubuntu";
        cases.push_back({
            L"WSL OpenCode override does not inherit Host BYOK",
            wslOpenCodeOverride,
            false,
            false,
            true,
            false,
            true,
            L"opencode",
            L"wsl",
            L"override-model",
            L"",
        });

        auto wslCopilotProfile = byokGlobal;
        wslCopilotProfile.profileBackend = L"wsl:Ubuntu:copilot";
        wslCopilotProfile.profileActiveShell = L"wsl:Ubuntu";
        cases.push_back({
            L"WSL Copilot profile does not inherit Host BYOK",
            wslCopilotProfile,
            false,
            false,
            true,
            false,
            true,
            L"copilot",
            L"wsl",
            L"",
            L"",
        });

        auto unsupportedOverride = hostOpenCodeOverride;
        unsupportedOverride.agentIdOverride = L"claude";
        cases.push_back({
            L"unsupported Host agent is excluded",
            unsupportedOverride,
            false,
            false,
            true,
            false,
            true,
            L"claude",
            L"host",
            L"override-model",
            L"",
        });

        auto customOverride = hostOpenCodeOverride;
        customOverride.agentIdOverride = L"custom:local";
        customOverride.agentCustomCommandOverride = L"agent.exe --acp";
        cases.push_back({
            L"custom Host agent is excluded",
            customOverride,
            false,
            false,
            true,
            false,
            true,
            L"custom:local",
            L"host",
            L"override-model",
            L"",
        });

        auto nonLaunchableCustomOverride = customOverride;
        nonLaunchableCustomOverride.agentCustomCommandOverride.clear();
        cases.push_back({
            L"non-launchable custom binding is excluded",
            nonLaunchableCustomOverride,
            false,
            false,
            true,
            false,
            false,
            L"custom:local",
            L"host",
            L"override-model",
            L"",
        });

        auto unknownProfile = byokGlobal;
        unknownProfile.profileBackend = L"host:unknown";
        cases.push_back({
            L"unknown profile agent is excluded",
            unknownProfile,
            false,
            false,
            true,
            false,
            false,
            L"unknown",
            L"host",
            L"",
            L"",
        });

        auto invalidProfile = byokGlobal;
        invalidProfile.profileBackend = L"host:";
        cases.push_back({
            L"invalid profile backend is excluded",
            invalidProfile,
            false,
            false,
            true,
            false,
            false,
            L"",
            L"host",
            L"",
            L"",
        });

        cases.push_back({
            L"native cloud model updates global follower",
            cloudGlobal,
            false,
            true,
            false,
            true,
            true,
            L"copilot",
            L"host",
            L"gpt-5.6",
            L"",
        });

        auto cloudOverride = cloudGlobal;
        cloudOverride.hasAgentOverride = true;
        cloudOverride.agentIdOverride = L"opencode";
        cloudOverride.agentModelOverride = L"override-model";
        cloudOverride.agentSourceOverride = L"host";
        cases.push_back({
            L"native cloud model excludes override",
            cloudOverride,
            false,
            true,
            false,
            false,
            true,
            L"opencode",
            L"host",
            L"override-model",
            L"",
        });

        for (const auto& test : cases)
        {
            Log::Comment(test.name);
            const auto binding = Page::_ResolveAgentPaneSettingsBinding(test.request);
            VERIFY_ARE_EQUAL(test.expectedLaunchable, binding.launchable);
            VERIFY_ARE_EQUAL(std::wstring{ test.expectedAgent }, binding.agentId);
            VERIFY_ARE_EQUAL(std::wstring{ test.expectedSource }, binding.agentSource);
            VERIFY_ARE_EQUAL(std::wstring{ test.expectedModel }, binding.acpModel);
            VERIFY_ARE_EQUAL(std::wstring{ test.expectedCustomSelection }, binding.customModelSelection);
            VERIFY_ARE_EQUAL(
                test.expectedAffected,
                Page::_IsAgentPaneSettingsRebindAffected(
                    binding,
                    test.globalAgentChanged,
                    test.cloudModelChanged,
                    test.customModelLaunchChanged));

            if (test.expectedAffected)
            {
                const auto payload = Page::_BuildAgentPaneSettingsRebindPayload(binding);
                VERIFY_ARE_EQUAL(
                    winrt::to_string(winrt::hstring{ test.expectedAgent }),
                    payload["agent_id"].asString());
                VERIFY_ARE_EQUAL(
                    winrt::to_string(winrt::hstring{ test.expectedSource }),
                    payload["agent_source"].asString());
                VERIFY_ARE_EQUAL(
                    winrt::to_string(winrt::hstring{ test.expectedModel }),
                    payload["acp_model"].asString());
                VERIFY_ARE_EQUAL(
                    winrt::to_string(winrt::hstring{ test.expectedCustomSelection }),
                    payload["custom_model_selection"].asString());
            }
        }
    }

    void SettingsTests::TestAgentPaneSwitchCapability()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using Binding = Page::AgentPaneSettingsBinding;
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;

        Binding hostCopilot{
            .agentId = L"copilot",
            .agentSource = L"host",
            .launchable = true,
        };
        auto hostCodex = hostCopilot;
        hostCodex.agentId = L"codex";
        VERIFY_IS_TRUE(Page::_CanSwitchAgentInPlace(hostCopilot, hostCodex, State::Connecting, true));
        VERIFY_IS_TRUE(Page::_CanSwitchAgentInPlace(hostCopilot, hostCodex, State::Connected, true));
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(hostCopilot, hostCodex, State::Connected, false));
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(hostCopilot, hostCodex, State::Closed, true));

        auto wslCopilot = hostCopilot;
        wslCopilot.agentSource = L"wsl";
        wslCopilot.agentWslDistro = L"Ubuntu";
        auto wslCodex = wslCopilot;
        wslCodex.agentId = L"codex";
        VERIFY_IS_TRUE(Page::_CanSwitchAgentInPlace(wslCopilot, wslCodex, State::Connected, true));
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(hostCopilot, wslCodex, State::Connected, true));

        auto otherDistro = wslCodex;
        otherDistro.agentWslDistro = L"Debian";
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(wslCopilot, otherDistro, State::Connected, true));

        auto customAgent = hostCopilot;
        customAgent.agentId = L"custom:local";
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(customAgent, hostCodex, State::Connected, true));
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(hostCopilot, customAgent, State::Connected, true));

        auto unavailableTarget = hostCodex;
        unavailableTarget.launchable = false;
        VERIFY_IS_FALSE(Page::_CanSwitchAgentInPlace(hostCopilot, unavailableTarget, State::Connected, true));

        VERIFY_IS_TRUE(Page::_CanRetainAgentPaneForMasterRestart(State::Connecting));
        VERIFY_IS_TRUE(Page::_CanRetainAgentPaneForMasterRestart(State::Connected));
        VERIFY_IS_FALSE(Page::_CanRetainAgentPaneForMasterRestart(State::Closed));

        const auto payload = Page::_BuildAgentPaneSettingsRebindPayload(wslCodex);
        VERIFY_ARE_EQUAL(std::string{ "wsl" }, payload["agent_source"].asString());
        VERIFY_ARE_EQUAL(std::string{ "Ubuntu" }, payload["wsl_distro"].asString());
    }

    void SettingsTests::TestAgentPaneModelHotUpdateRouting()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using Request = Page::AgentPaneSettingsBindingRequest;

        const auto globalFollower = Page::_ResolveAgentPaneSettingsBinding(Request{
            .globalAgentId = L"copilot",
            .globalModel = L"gpt-5.6",
            .globalAgentCliPath = L"copilot --acp --stdio --model gpt-5.6",
        });
        const auto perTabModelOverride = Page::_ResolveAgentPaneSettingsBinding(Request{
            .hasAgentOverride = true,
            .agentIdOverride = L"copilot",
            .agentModelOverride = L"gpt-5.5",
            .agentSourceOverride = L"host",
        });
        const auto nonLaunchableGlobalFollower = Page::_ResolveAgentPaneSettingsBinding(Request{
            .globalAgentId = L"copilot",
            .globalModel = L"gpt-5.6",
        });
        using State = winrt::Microsoft::Terminal::TerminalConnection::ConnectionState;

        VERIFY_IS_TRUE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, State::Connected, true, true));
        VERIFY_IS_FALSE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, State::Connected, true, true));
        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, State::Connecting, true, false));
        VERIFY_IS_TRUE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, State::Connecting, true, false));
        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, State::Connected, true, false));
        VERIFY_IS_TRUE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, State::Connected, true, false));
        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, State::Connected, false, true));
        VERIFY_IS_TRUE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, State::Connected, false, true));
        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, State::Failed, true, true));
        VERIFY_IS_TRUE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, State::Failed, true, true));
        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, State::NotConnected, true, true));
        VERIFY_IS_TRUE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, State::NotConnected, true, true));
        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(globalFollower, std::nullopt, true, true));
        VERIFY_IS_TRUE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(globalFollower, std::nullopt, true, true));

        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(std::nullopt, State::Connected, true, true));
        VERIFY_IS_FALSE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(std::nullopt, State::Connected, false, false));

        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(perTabModelOverride, State::Connected, true, true));
        VERIFY_IS_FALSE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(perTabModelOverride, State::Connected, false, false));

        VERIFY_IS_FALSE(Page::_IsAgentPaneModelHotUpdateTarget(nonLaunchableGlobalFollower, State::Connected, true, true));
        VERIFY_IS_FALSE(Page::_ShouldRecreateAgentPaneForModelHotUpdate(nonLaunchableGlobalFollower, State::Connected, false, false));
    }
}
