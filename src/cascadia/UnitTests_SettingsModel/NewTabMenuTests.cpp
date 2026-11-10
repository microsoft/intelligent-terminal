// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "../TerminalSettingsModel/NewTabMenuEntry.h"
#include "../TerminalSettingsModel/FolderEntry.h"
#include "../TerminalSettingsModel/CascadiaSettings.h"
#include "../TerminalSettingsModel/resource.h"
#include "../inc/NewTabMenuUtils.h"
#include "../types/inc/colorTable.hpp"
#include "JsonTestClass.h"

using namespace Microsoft::Console;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Settings::Model::implementation;
using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;

namespace SettingsModelUnitTests
{
    namespace Model = winrt::Microsoft::Terminal::Settings::Model;
    namespace Menu = ::Microsoft::Terminal::NewTabMenu;

    static constexpr std::string_view profileDefaults{ R"json({
        "defaultProfile": "{00000000-0000-0000-0000-000000000002}",
        "profiles": [
            {
                "guid": "{00000000-0000-0000-0000-000000000001}",
                "name": "Renamed agent",
                "source": "IntelligentTerminal.AgentProfiles",
                "agentProfile.id": "claude"
            },
            {
                "guid": "{00000000-0000-0000-0000-000000000002}",
                "name": "Claude",
                "commandline": "cmd.exe"
            },
            {
                "guid": "{00000000-0000-0000-0000-000000000003}",
                "name": "Copilot",
                "source": "IntelligentTerminal.AgentProfiles",
                "agentProfile.id": "copilot"
            },
            {
                "guid": "{00000000-0000-0000-0000-000000000004}",
                "name": "Agent copy",
                "agentProfile.id": "claude"
            }
        ]
    })json" };

    class NewTabMenuTests : public JsonTestClass
    {
        TEST_CLASS(NewTabMenuTests);

        TEST_METHOD(DefaultsToRemainingProfiles);
        TEST_METHOD(ParseEmptyFolder);
        TEST_METHOD(DefaultMenuGroupsAgentProfiles);
        TEST_METHOD(DefaultMenuOmitsEmptyGroups);
        TEST_METHOD(DefaultMenuExcludesHiddenAndOrphanedAgents);
        TEST_METHOD(CustomMenuPreservesLayout);
    };

    void NewTabMenuTests::DefaultMenuGroupsAgentProfiles()
    {
        for (const std::string_view user : { R"({})", R"({"newTabMenu":[{"type":"remainingProfiles"}]})" })
        {
            const auto settings = winrt::make_self<CascadiaSettings>(user, profileDefaults);
            const auto entries = settings->GlobalSettings().NewTabMenu();
            const auto before = settings->ToJson();
            const auto grouped = Menu::GroupDefaultProfiles(entries);

            VERIFY_ARE_EQUAL(3u, grouped.Size());
            VERIFY_ARE_EQUAL(Model::NewTabMenuEntryType::RemainingProfiles, grouped.GetAt(0).Type());
            VERIFY_ARE_EQUAL(Model::NewTabMenuEntryType::Separator, grouped.GetAt(1).Type());
            VERIFY_ARE_EQUAL(Model::NewTabMenuEntryType::RemainingProfiles, grouped.GetAt(2).Type());

            const auto ordinary = grouped.GetAt(0).as<Model::RemainingProfilesEntry>().Profiles();
            const auto agents = grouped.GetAt(2).as<Model::RemainingProfilesEntry>().Profiles();
            VERIFY_ARE_EQUAL(2u, ordinary.Size());
            VERIFY_ARE_EQUAL(2u, agents.Size());
            auto ordinaryIterator = ordinary.First();
            VERIFY_ARE_EQUAL(1, ordinaryIterator.Current().Key());
            VERIFY_IS_TRUE(ordinaryIterator.MoveNext());
            VERIFY_ARE_EQUAL(3, ordinaryIterator.Current().Key());
            auto agentIterator = agents.First();
            VERIFY_ARE_EQUAL(0, agentIterator.Current().Key());
            VERIFY_IS_TRUE(agentIterator.MoveNext());
            VERIFY_ARE_EQUAL(2, agentIterator.Current().Key());
            for (const auto& [index, profile] : ordinary)
            {
                VERIFY_IS_TRUE(profile == settings->ActiveProfiles().GetAt(index));
            }
            for (const auto& [index, profile] : agents)
            {
                VERIFY_IS_TRUE(profile == settings->ActiveProfiles().GetAt(index));
            }

            VERIFY_ARE_EQUAL(1u, entries.Size());
            VERIFY_ARE_EQUAL(4u, entries.GetAt(0).as<Model::RemainingProfilesEntry>().Profiles().Size());
            VERIFY_IS_TRUE(before == settings->ToJson());
        }
    }

    void NewTabMenuTests::DefaultMenuOmitsEmptyGroups()
    {
        const auto settings = winrt::make_self<CascadiaSettings>(R"({})", profileDefaults);
        for (const auto& indices : { std::vector<int>{}, std::vector<int>{ 0, 2 }, std::vector<int>{ 1, 3 } })
        {
            auto profiles = winrt::single_threaded_map<int, Model::Profile>();
            for (const auto index : indices)
            {
                profiles.Insert(index, settings->ActiveProfiles().GetAt(index));
            }
            Model::RemainingProfilesEntry remaining;
            remaining.Profiles(profiles);
            const auto entries = winrt::single_threaded_vector<Model::NewTabMenuEntry>({ remaining });
            VERIFY_IS_TRUE(Menu::GroupDefaultProfiles(entries) == entries);
        }
    }

    void NewTabMenuTests::DefaultMenuExcludesHiddenAndOrphanedAgents()
    {
        static constexpr std::string_view user{ R"json({
            "defaultProfile": "{00000000-0000-0000-0000-000000000002}",
            "profiles": [
                {
                    "guid": "{00000000-0000-0000-0000-000000000001}",
                    "source": "IntelligentTerminal.AgentProfiles",
                    "hidden": true
                },
                {
                    "guid": "{00000000-0000-0000-0000-000000000005}",
                    "name": "Uninstalled agent",
                    "source": "IntelligentTerminal.AgentProfiles",
                    "agentProfile.id": "codex"
                }
            ]
        })json" };
        const auto settings = winrt::make_self<CascadiaSettings>(user, profileDefaults);
        const auto entries = Menu::GroupDefaultProfiles(settings->GlobalSettings().NewTabMenu());
        VERIFY_ARE_EQUAL(3u, entries.Size());
        const auto agents = entries.GetAt(2).as<Model::RemainingProfilesEntry>().Profiles();
        VERIFY_ARE_EQUAL(1u, agents.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, agents.First().Current().Value().AgentProfileId());
        VERIFY_ARE_EQUAL(3u, settings->ActiveProfiles().Size());
    }

    void NewTabMenuTests::CustomMenuPreservesLayout()
    {
        for (const std::string_view user : {
                 R"({"newTabMenu":[]})",
                 R"({"newTabMenu":[{"type":"separator"},{"type":"remainingProfiles"}]})",
                 R"({"newTabMenu":[{"type":"profile","profile":"Claude"},{"type":"remainingProfiles"}]})",
                 R"({"newTabMenu":[{"type":"folder","name":"All","entries":[{"type":"remainingProfiles"}]}]})",
                 R"({"newTabMenu":[{"type":"matchProfiles","source":"IntelligentTerminal\\.AgentProfiles"},{"type":"remainingProfiles"}]})" })
        {
            const auto settings = winrt::make_self<CascadiaSettings>(user, profileDefaults);
            const auto entries = settings->GlobalSettings().NewTabMenu();
            const auto before = settings->ToJson();
            VERIFY_IS_TRUE(Menu::GroupDefaultProfiles(entries) == entries);
            VERIFY_IS_TRUE(before == settings->ToJson());
        }
    }

    void NewTabMenuTests::DefaultsToRemainingProfiles()
    {
        Log::Comment(L"If the user doesn't customize the menu, put one entry for each profile");

        static constexpr std::string_view settingsString{ R"json({
        })json" };

        try
        {
            const auto settings{ winrt::make_self<CascadiaSettings>(settingsString, LoadStringResource(IDR_DEFAULTS)) };

            VERIFY_ARE_EQUAL(0u, settings->Warnings().Size());

            const auto& entries = settings->GlobalSettings().NewTabMenu();
            VERIFY_ARE_EQUAL(1u, entries.Size());
            VERIFY_ARE_EQUAL(winrt::Microsoft::Terminal::Settings::Model::NewTabMenuEntryType::RemainingProfiles, entries.GetAt(0).Type());
        }
        catch (const SettingsException& ex)
        {
            auto loadError = ex.Error();
            loadError;
            throw ex;
        }
        catch (const SettingsTypedDeserializationException& e)
        {
            auto deserializationErrorMessage = til::u8u16(e.what());
            Log::Comment(NoThrowString().Format(deserializationErrorMessage.c_str()));
            throw e;
        }
    }

    void NewTabMenuTests::ParseEmptyFolder()
    {
        Log::Comment(L"GH #14557 - An empty folder entry shouldn't crash");

        static constexpr std::string_view settingsString{ R"json({
            "newTabMenu": [
                { "type": "folder" }
            ]
        })json" };

        try
        {
            const auto settings{ winrt::make_self<CascadiaSettings>(settingsString, LoadStringResource(IDR_DEFAULTS)) };

            VERIFY_ARE_EQUAL(0u, settings->Warnings().Size());

            const auto& entries = settings->GlobalSettings().NewTabMenu();
            VERIFY_ARE_EQUAL(1u, entries.Size());
        }
        catch (const SettingsException& ex)
        {
            auto loadError = ex.Error();
            loadError;
            throw ex;
        }
        catch (const SettingsTypedDeserializationException& e)
        {
            auto deserializationErrorMessage = til::u8u16(e.what());
            Log::Comment(NoThrowString().Format(deserializationErrorMessage.c_str()));
            throw e;
        }
    }
}
