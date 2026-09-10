// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../inc/AgentYoloPolicy.h"
#include "../TerminalApp/TerminalPage.h"
#include "../inc/AgentAvailability.h"
#include "../UnitTests_SettingsModel/TestUtils.h"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"

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
        TEST_METHOD(TestAgentHooksReconciliationClassification);
        TEST_METHOD(TestHostAgentSnapshotParsing);
        TEST_METHOD(TestHostAgentSnapshotRejectsInvalidPayload);
        TEST_METHOD(TestFreAgentCandidatesFromKnownSnapshot);
        TEST_METHOD(TestFreAgentCandidatesFromUnknownProbe);
        TEST_METHOD(TestFreAgentSelectionDecision);
        TEST_METHOD(TestAgentSettingsFocusGate);
        TEST_METHOD(TestAgentPaneRebindCapability);
        TEST_METHOD(TestAgentPaneSwitchCapability);
        TEST_METHOD(TestDefaultProviderYoloInheritance);
        TEST_METHOD(TestHotDefaultProviderYoloUsesOutgoingBinding);
        TEST_METHOD(TestAgentPaneSettingsRebindRouting);
        TEST_METHOD(TestAgentPaneModelHotUpdateRouting);

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

    void SettingsTests::TestAgentHooksReconciliationClassification()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;
        using Scope = Page::AgentHooksReconciliationScope;

        const Page::AgentSettingsSnapshot enabled{
            L"copilot", L"", L"", std::nullopt, {}, true
        };
        VERIFY_ARE_EQUAL(
            Scope::None,
            Page::_ClassifyAgentHooksReconciliation(enabled, enabled));

        auto turnedOff = enabled;
        turnedOff.agentSessionManagementEnabled = false;
        VERIFY_ARE_EQUAL(
            Scope::None,
            Page::_ClassifyAgentHooksReconciliation(enabled, turnedOff));
        VERIFY_ARE_EQUAL(
            Scope::All,
            Page::_ClassifyAgentHooksReconciliation(turnedOff, enabled));

        auto switchedAgent = enabled;
        switchedAgent.acpAgent = L"claude";
        VERIFY_ARE_EQUAL(
            Scope::SelectedAgent,
            Page::_ClassifyAgentHooksReconciliation(enabled, switchedAgent));

        auto switchedCustomAgent = enabled;
        switchedCustomAgent.acpAgent = L"custom:local";
        VERIFY_ARE_EQUAL(
            Scope::None,
            Page::_ClassifyAgentHooksReconciliation(enabled, switchedCustomAgent));

        auto switchedWhileDisabled = turnedOff;
        switchedWhileDisabled.acpAgent = L"claude";
        VERIFY_ARE_EQUAL(
            Scope::None,
            Page::_ClassifyAgentHooksReconciliation(turnedOff, switchedWhileDisabled));
    }

    void SettingsTests::TestHostAgentSnapshotParsing()
    {
        using namespace ::Microsoft::Terminal::AgentAvailability;

        constexpr std::string_view payload = R"({
            "agents": [
                { "id": "copilot", "display_name": "GitHub Copilot" }
            ],
            "availability": [
                {
                    "id": "copilot",
                    "display_name": "GitHub Copilot",
                    "native_cli_found": true,
                    "launch_ready": true,
                    "requires_npx": false
                },
                {
                    "id": "claude",
                    "display_name": "Claude",
                    "native_cli_found": true,
                    "launch_ready": false,
                    "requires_npx": true
                },
                {
                    "id": "codex",
                    "display_name": "Codex",
                    "native_cli_found": false,
                    "launch_ready": false,
                    "requires_npx": true
                },
                {
                    "id": "gemini",
                    "display_name": "Gemini",
                    "native_cli_found": false,
                    "launch_ready": false,
                    "requires_npx": false
                },
                {
                    "id": "opencode",
                    "display_name": "OpenCode",
                    "native_cli_found": false,
                    "launch_ready": false,
                    "requires_npx": false
                }
            ],
            "npx_found": false
        })";

        const auto snapshot = ParseHostAgentSnapshot(payload);
        VERIFY_IS_TRUE(snapshot.has_value());
        VERIFY_IS_FALSE(snapshot->npxFound);
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), snapshot->availability.size());

        const auto copilot = snapshot->availability.find(L"copilot");
        VERIFY_IS_TRUE(copilot != snapshot->availability.end());
        VERIFY_IS_TRUE(copilot->second.nativeCliFound);
        VERIFY_IS_TRUE(copilot->second.launchReady);
        VERIFY_IS_FALSE(copilot->second.requiresNpx);

        const auto claude = snapshot->availability.find(L"claude");
        VERIFY_IS_TRUE(claude != snapshot->availability.end());
        VERIFY_IS_TRUE(claude->second.nativeCliFound);
        VERIFY_IS_FALSE(claude->second.launchReady);
        VERIFY_IS_TRUE(claude->second.requiresNpx);

        const auto strictIds = ParseHostAgentIds(payload);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), strictIds.size());
        VERIFY_IS_TRUE(strictIds.contains(L"copilot"));
    }

    void SettingsTests::TestHostAgentSnapshotRejectsInvalidPayload()
    {
        using namespace ::Microsoft::Terminal::AgentAvailability;

        VERIFY_IS_FALSE(ParseHostAgentSnapshot("{}").has_value());
        VERIFY_IS_FALSE(ParseHostAgentSnapshot(R"({
            "availability": [{
                "id": "copilot",
                "native_cli_found": true,
                "launch_ready": true,
                "requires_npx": false
            }],
            "npx_found": false
        })")
                            .has_value());
        VERIFY_IS_FALSE(ParseHostAgentSnapshot(R"({
            "availability": [
                { "id": "copilot", "native_cli_found": true, "launch_ready": true, "requires_npx": false },
                { "id": "copilot", "native_cli_found": true, "launch_ready": true, "requires_npx": false },
                { "id": "claude", "native_cli_found": true, "launch_ready": false, "requires_npx": true },
                { "id": "codex", "native_cli_found": false, "launch_ready": false, "requires_npx": true },
                { "id": "gemini", "native_cli_found": false, "launch_ready": false, "requires_npx": false },
                { "id": "opencode", "native_cli_found": false, "launch_ready": false, "requires_npx": false }
            ],
            "npx_found": false
        })")
                            .has_value());
        VERIFY_IS_FALSE(ParseHostAgentSnapshot(R"({
            "availability": [
                { "id": "copilot", "native_cli_found": false, "launch_ready": true, "requires_npx": false },
                { "id": "claude", "native_cli_found": true, "launch_ready": false, "requires_npx": true },
                { "id": "codex", "native_cli_found": false, "launch_ready": false, "requires_npx": true },
                { "id": "gemini", "native_cli_found": false, "launch_ready": false, "requires_npx": false },
                { "id": "opencode", "native_cli_found": false, "launch_ready": false, "requires_npx": false }
            ],
            "npx_found": false
        })")
                            .has_value());
    }

    void SettingsTests::TestFreAgentCandidatesFromKnownSnapshot()
    {
        using namespace ::Microsoft::Terminal::AgentAvailability;
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;

        HostAgentSnapshot snapshot;
        snapshot.availability.emplace(L"copilot", HostAgentAvailability{ false, false, false });
        snapshot.availability.emplace(L"claude", HostAgentAvailability{ true, false, true });
        snapshot.availability.emplace(L"codex", HostAgentAvailability{ false, false, true });
        snapshot.availability.emplace(L"gemini", HostAgentAvailability{ true, true, false });
        snapshot.availability.emplace(L"opencode", HostAgentAvailability{ false, false, false });

        const std::vector<Reg::BuiltinAgent> allowed{ Reg::BuiltinAcpAgents.begin(), Reg::BuiltinAcpAgents.end() };
        const auto candidates = BuildFreAgentCandidates(allowed, snapshot, L"codex", L"codex");

        VERIFY_ARE_EQUAL(static_cast<size_t>(3), candidates.size());
        VERIFY_IS_TRUE(Reg::AgentIdEquals(candidates[0].id, L"copilot"));
        VERIFY_IS_TRUE(Reg::AgentIdEquals(candidates[1].id, L"claude"));
        VERIFY_IS_TRUE(Reg::AgentIdEquals(candidates[2].id, L"gemini"));
    }

    void SettingsTests::TestFreAgentCandidatesFromUnknownProbe()
    {
        using namespace ::Microsoft::Terminal::AgentAvailability;
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;

        const std::vector<Reg::BuiltinAgent> allowed{ Reg::BuiltinAcpAgents.begin(), Reg::BuiltinAcpAgents.end() };
        const auto preferred = BuildFreAgentCandidates(allowed, std::nullopt, L"CoDeX", L"claude");
        VERIFY_ARE_EQUAL(static_cast<size_t>(3), preferred.size());
        VERIFY_IS_TRUE(Reg::AgentIdEquals(preferred[0].id, L"codex"));
        VERIFY_IS_TRUE(Reg::AgentIdEquals(preferred[1].id, L"claude"));
        VERIFY_IS_TRUE(Reg::AgentIdEquals(preferred[2].id, L"copilot"));

        const std::vector<Reg::BuiltinAgent> geminiOnly{ Reg::BuiltinAcpAgents[3] };
        const auto policyFallback = BuildFreAgentCandidates(geminiOnly, std::nullopt, L"codex", L"custom:local");
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), policyFallback.size());
        VERIFY_IS_TRUE(Reg::AgentIdEquals(policyFallback[0].id, L"gemini"));

        const std::vector<Reg::BuiltinAgent> none;
        VERIFY_IS_TRUE(BuildFreAgentCandidates(none, std::nullopt, L"copilot", L"copilot").empty());
    }

    void SettingsTests::TestFreAgentSelectionDecision()
    {
        using namespace ::Microsoft::Terminal::AgentAvailability;
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;

        const std::vector<Reg::BuiltinAgent> allowed{ Reg::BuiltinAcpAgents.begin(), Reg::BuiltinAcpAgents.end() };

        const auto untouchedBuiltin = DecideFreAgentSelection(allowed, L"claude", L"CLAUDE", false);
        VERIFY_IS_FALSE(untouchedBuiltin.persistSelection);
        VERIFY_ARE_EQUAL(std::wstring{ L"claude" }, untouchedBuiltin.setupAgentId);

        const auto untouchedCustomFallback = DecideFreAgentSelection(allowed, L"copilot", L"custom:local", false);
        VERIFY_IS_FALSE(untouchedCustomFallback.persistSelection);
        VERIFY_IS_TRUE(untouchedCustomFallback.setupAgentId.empty());

        const auto explicitFallback = DecideFreAgentSelection(allowed, L"copilot", L"custom:local", true);
        VERIFY_IS_TRUE(explicitFallback.persistSelection);
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot" }, explicitFallback.setupAgentId);

        const std::vector<Reg::BuiltinAgent> geminiOnly{ Reg::BuiltinAcpAgents[3] };
        const auto blocked = DecideFreAgentSelection(geminiOnly, L"copilot", L"", true);
        VERIFY_IS_FALSE(blocked.persistSelection);
        VERIFY_IS_TRUE(blocked.setupAgentId.empty());

        VERIFY_IS_FALSE(NeedsFreNodeBootstrap(std::nullopt, L"claude"));
        HostAgentSnapshot snapshot;
        snapshot.npxFound = false;
        snapshot.availability.emplace(L"claude", HostAgentAvailability{ true, false, true });
        VERIFY_IS_TRUE(NeedsFreNodeBootstrap(snapshot, L"CLAUDE"));
        snapshot.npxFound = true;
        VERIFY_IS_FALSE(NeedsFreNodeBootstrap(snapshot, L"claude"));
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

        const auto visibleActive = Page::_GetAgentPaneRecreationOptions(false, true);
        VERIFY_IS_FALSE(visibleActive.autoStash);
        VERIFY_IS_TRUE(visibleActive.focusPane);

        const auto visibleBackground = Page::_GetAgentPaneRecreationOptions(false, false);
        VERIFY_IS_FALSE(visibleBackground.autoStash);
        VERIFY_IS_FALSE(visibleBackground.focusPane);

        const auto stashedActive = Page::_GetAgentPaneRecreationOptions(true, true);
        VERIFY_IS_TRUE(stashedActive.autoStash);
        VERIFY_IS_FALSE(stashedActive.focusPane);

        const auto stashedBackground = Page::_GetAgentPaneRecreationOptions(true, false);
        VERIFY_IS_TRUE(stashedBackground.autoStash);
        VERIFY_IS_FALSE(stashedBackground.focusPane);
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

    void SettingsTests::TestDefaultProviderYoloInheritance()
    {
        namespace Policy = ::Microsoft::Terminal::Settings::Model::AgentYoloPolicy;

        VERIFY_IS_TRUE(Policy::CanUserRequestEnable(false));
        VERIFY_IS_FALSE(Policy::CanUserRequestEnable(true));
        VERIFY_IS_TRUE(Policy::IsAutomaticProviderKnownUnsupported(L"opencode"));
        VERIFY_IS_FALSE(Policy::IsAutomaticProviderKnownUnsupported(L"copilot"));
        VERIFY_IS_TRUE(Policy::IsAutomaticEnableAvailable(false, L"copilot"));
        VERIFY_IS_FALSE(Policy::IsAutomaticEnableAvailable(true, L"copilot"));
        VERIFY_IS_FALSE(Policy::IsAutomaticEnableAvailable(false, L"opencode"));
        VERIFY_IS_FALSE(Policy::IsAutomaticEnableAvailable(false, L""));

        const auto automatic = [](const bool configuredEnabled,
                                  const bool policyBlocked,
                                  const std::wstring_view defaultAgentId,
                                  const std::wstring_view currentAgentId) {
            return Policy::ShouldRequestAutomaticEnable(
                configuredEnabled,
                policyBlocked,
                defaultAgentId,
                currentAgentId);
        };

        VERIFY_IS_TRUE(automatic(true, false, L"copilot", L"copilot"));
        VERIFY_IS_TRUE(automatic(true, false, L"CoPiLoT", L"copilot"));
        VERIFY_IS_FALSE(automatic(true, false, L"copilot", L"claude"));
        VERIFY_IS_FALSE(automatic(true, false, L"opencode", L"opencode"));
        VERIFY_IS_FALSE(automatic(true, false, L"", L""));
        VERIFY_IS_FALSE(automatic(false, false, L"copilot", L"copilot"));
        VERIFY_IS_FALSE(automatic(true, true, L"copilot", L"copilot"));
    }

    void SettingsTests::TestHotDefaultProviderYoloUsesOutgoingBinding()
    {
        using Page = winrt::TerminalApp::implementation::TerminalPage;

        Page::AgentRuntimeConfigSnapshot previous;
        previous.defaultAgentId = L"copilot";
        previous.yoloEnabled = false;

        Page::AgentRuntimeConfigSnapshot current;
        current.defaultAgentId = L"gemini";
        current.yoloEnabled = true;

        Page::AgentPaneSettingsBinding globalFollower;
        globalFollower.agentId = L"gemini";
        globalFollower.followsGlobalAcpModel = true;
        VERIFY_IS_FALSE(Page::_ResolveHotAutomaticYoloForAgentBinding(
            previous, current, globalFollower, L"copilot"));
        VERIFY_IS_TRUE(Page::_ResolveHotAutomaticYoloForAgentBinding(
            previous, current, globalFollower, L"gemini"));
        VERIFY_IS_FALSE(Page::_ResolveHotAutomaticYoloForAgentBinding(
            previous, current, globalFollower, L""));

        Page::AgentPaneSettingsBinding agentOverride;
        agentOverride.agentId = L"gemini";
        VERIFY_IS_TRUE(Page::_ResolveHotAutomaticYoloForAgentBinding(
            previous, current, agentOverride, L"gemini"));
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
