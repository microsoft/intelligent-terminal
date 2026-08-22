// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../inc/AcpModelUtils.h"
#include "../inc/AgentRegistry.h"
#include "../inc/CustomModelProviderUtils.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::AcpModels;

namespace TerminalAppUnitTests
{
    class AcpModelUtilsTests
    {
        TEST_CLASS(AcpModelUtilsTests);

        TEST_METHOD(MapsAgentIdsToAcpCommands);
        TEST_METHOD(AppendsSupportedModelFlags);
        TEST_METHOD(SuppressesCustomSelectionModelFlags);
        TEST_METHOD(CustomProvidersSupportOnlyChatCompletionsAgents);
        TEST_METHOD(LiveModelSwitchRequiresKnownSupportedAgent);
        TEST_METHOD(HostCatalogStatusRejectsSameAgentFromWsl);
        TEST_METHOD(RejectsInvalidCatalogShapes);
        TEST_METHOD(ParsesAndNormalizesCatalog);
        TEST_METHOD(ParsesCatalogValue);
        TEST_METHOD(ExcludesCustomSelectionIdsOnRequest);
        TEST_METHOD(CustomLaunchIdentityTracksOnlyLaunchFields);
        TEST_METHOD(CustomCatalogSerializationIsCredentialFree);
        TEST_METHOD(LargeCustomCatalogDoesNotExpandHelperBootstrap);
    };

    void AcpModelUtilsTests::MapsAgentIdsToAcpCommands()
    {
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot --acp --stdio" }, BuildAgentCommandLine(L"copilot"));
        VERIFY_ARE_EQUAL(std::wstring{ L"npx -y @agentclientprotocol/claude-agent-acp@0.65.0" }, BuildAgentCommandLine(L"claude"));
        VERIFY_ARE_EQUAL(std::wstring{ L"npx -y @agentclientprotocol/codex-acp@1.1.13" }, BuildAgentCommandLine(L"codex"));
        VERIFY_ARE_EQUAL(std::wstring{ L"gemini --experimental-acp" }, BuildAgentCommandLine(L"gemini"));
        VERIFY_ARE_EQUAL(std::wstring{ L"opencode acp" }, BuildAgentCommandLine(L"opencode"));
        VERIFY_ARE_EQUAL(std::wstring{ L"other-agent" }, BuildAgentCommandLine(L"other-agent"));
    }

    void AcpModelUtilsTests::AppendsSupportedModelFlags()
    {
        constexpr std::wstring_view model{ L"gpt-5" };
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot --acp --stdio --model gpt-5" }, BuildAgentCommandLine(L"copilot", model));
        VERIFY_ARE_EQUAL(std::wstring{ L"gemini --experimental-acp --model gpt-5" }, BuildAgentCommandLine(L"gemini", model));
        VERIFY_ARE_EQUAL(std::wstring{ L"npx -y @agentclientprotocol/claude-agent-acp@0.65.0" }, BuildAgentCommandLine(L"claude", model));
        VERIFY_ARE_EQUAL(std::wstring{ L"npx -y @agentclientprotocol/codex-acp@1.1.13" }, BuildAgentCommandLine(L"codex", model));
        VERIFY_ARE_EQUAL(std::wstring{ L"opencode acp" }, BuildAgentCommandLine(L"opencode", model));
        VERIFY_ARE_EQUAL(std::wstring{ L"other-agent" }, BuildAgentCommandLine(L"other-agent", model));
    }

    void AcpModelUtilsTests::SuppressesCustomSelectionModelFlags()
    {
        constexpr std::wstring_view customModel{ L"custom:provider:model" };
        VERIFY_ARE_EQUAL(std::wstring{ L"copilot --acp --stdio" }, BuildAgentCommandLine(L"copilot", customModel));
        VERIFY_ARE_EQUAL(std::wstring{ L"gemini --experimental-acp" }, BuildAgentCommandLine(L"gemini", customModel));
    }

    void AcpModelUtilsTests::CustomProvidersSupportOnlyChatCompletionsAgents()
    {
        namespace Registry = Microsoft::Terminal::Settings::Model::AgentRegistry;
        VERIFY_IS_TRUE(Registry::SupportsByok(L"copilot"));
        VERIFY_IS_TRUE(Registry::SupportsByok(L"opencode"));
        VERIFY_IS_FALSE(Registry::SupportsByok(L"claude"));
        VERIFY_IS_FALSE(Registry::SupportsByok(L"codex"));
        VERIFY_IS_FALSE(Registry::SupportsByok(L"gemini"));
    }

    void AcpModelUtilsTests::LiveModelSwitchRequiresKnownSupportedAgent()
    {
        namespace Registry = Microsoft::Terminal::Settings::Model::AgentRegistry;
        for (const auto agent : { L"copilot", L"claude", L"codex", L"opencode" })
        {
            VERIFY_IS_TRUE(Registry::SupportsLiveModelSwitch(agent));
        }
        for (const auto agent : { L"gemini", L"custom:test", L"unknown", L"" })
        {
            VERIFY_IS_FALSE(Registry::SupportsLiveModelSwitch(agent));
        }

        VERIFY_IS_TRUE(Registry::SupportsLiveModelSwitch(L"CoPiLoT"));
        VERIFY_IS_FALSE(Registry::SupportsLiveModelSwitch(L"GeMiNi"));
    }

    void AcpModelUtilsTests::HostCatalogStatusRejectsSameAgentFromWsl()
    {
        Json::Value host{ Json::objectValue };
        host["agent_id"] = "copilot";
        host["agent_source"] = "host";
        host["backend"] = "Windows";

        Json::Value wsl{ Json::objectValue };
        wsl["agent_id"] = "copilot";
        wsl["agent_source"] = "wsl";
        wsl["backend"] = "Ubuntu";

        VERIFY_IS_TRUE(StatusUsesHostCatalog(host));
        VERIFY_IS_FALSE(StatusUsesHostCatalog(wsl));

        Json::Value legacyHost{ Json::objectValue };
        legacyHost["agent_id"] = "copilot";
        legacyHost["backend"] = "Windows";
        VERIFY_IS_TRUE(StatusUsesHostCatalog(legacyHost));

        Json::Value legacyWsl{ Json::objectValue };
        legacyWsl["agent_id"] = "copilot";
        legacyWsl["backend"] = "Ubuntu";
        VERIFY_IS_FALSE(StatusUsesHostCatalog(legacyWsl));
    }

    void AcpModelUtilsTests::RejectsInvalidCatalogShapes()
    {
        VERIFY_IS_FALSE(ParseModelCatalog(std::string_view{}).has_value());
        VERIFY_IS_FALSE(ParseModelCatalog(std::string_view{ "{not json" }).has_value());
        VERIFY_IS_FALSE(ParseModelCatalog(std::string_view{ "[]" }).has_value());
        VERIFY_IS_FALSE(ParseModelCatalog(std::string_view{ R"({})" }).has_value());
        VERIFY_IS_FALSE(ParseModelCatalog(std::string_view{ R"({"available_models":{}})" }).has_value());

        const auto empty = ParseModelCatalog(std::string_view{ R"({"available_models":[]})" });
        VERIFY_IS_TRUE(empty.has_value());
        VERIFY_ARE_EQUAL(size_t{ 0 }, empty->availableModels.size());
    }

    void AcpModelUtilsTests::ParsesAndNormalizesCatalog()
    {
        constexpr std::string_view json = R"({
            "available_models": [
                { "id": "model-a", "name": "Model A", "description": "First" },
                { "id": "model-b" },
                { "id": "model-c", "name": "   ", "description": 7 },
                { "id": "" },
                { "id": " \t " },
                { "name": "missing id" },
                null
            ],
            "current_model_id": "model-b"
        })";

        const auto catalog = ParseModelCatalog(json);
        VERIFY_IS_TRUE(catalog.has_value());
        VERIFY_ARE_EQUAL(size_t{ 3 }, catalog->availableModels.size());

        VERIFY_ARE_EQUAL(std::string{ "model-a" }, catalog->availableModels[0].id);
        VERIFY_ARE_EQUAL(std::string{ "Model A" }, catalog->availableModels[0].name);
        VERIFY_ARE_EQUAL(std::string{ "First" }, catalog->availableModels[0].description);

        VERIFY_ARE_EQUAL(std::string{ "model-b" }, catalog->availableModels[1].id);
        VERIFY_ARE_EQUAL(std::string{ "model-b" }, catalog->availableModels[1].name);
        VERIFY_ARE_EQUAL(std::string{}, catalog->availableModels[1].description);

        VERIFY_ARE_EQUAL(std::string{ "model-c" }, catalog->availableModels[2].id);
        VERIFY_ARE_EQUAL(std::string{ "model-c" }, catalog->availableModels[2].name);
        VERIFY_ARE_EQUAL(std::string{}, catalog->availableModels[2].description);

        VERIFY_IS_TRUE(catalog->currentModelId.has_value());
        VERIFY_ARE_EQUAL(std::string{ "model-b" }, *catalog->currentModelId);
    }

    void AcpModelUtilsTests::ParsesCatalogValue()
    {
        Json::Value root{ Json::objectValue };
        root["available_models"] = Json::Value{ Json::arrayValue };
        Json::Value model{ Json::objectValue };
        model["id"] = "model-a";
        model["name"] = "";
        root["available_models"].append(std::move(model));

        const auto catalog = ParseModelCatalog(root);
        VERIFY_IS_TRUE(catalog.has_value());
        VERIFY_ARE_EQUAL(size_t{ 1 }, catalog->availableModels.size());
        VERIFY_ARE_EQUAL(std::string{ "model-a" }, catalog->availableModels[0].name);
        VERIFY_IS_FALSE(catalog->currentModelId.has_value());
    }

    void AcpModelUtilsTests::ExcludesCustomSelectionIdsOnRequest()
    {
        constexpr std::string_view json = R"({
            "available_models": [
                { "id": "native", "name": "Native" },
                { "id": "custom:provider:model", "name": "Custom" }
            ]
        })";

        const auto included = ParseModelCatalog(json);
        VERIFY_IS_TRUE(included.has_value());
        VERIFY_ARE_EQUAL(size_t{ 2 }, included->availableModels.size());

        const auto excluded = ParseModelCatalog(
            json,
            { .excludeCustomSelectionIds = true });
        VERIFY_IS_TRUE(excluded.has_value());
        VERIFY_ARE_EQUAL(size_t{ 1 }, excluded->availableModels.size());
        VERIFY_ARE_EQUAL(std::string{ "native" }, excluded->availableModels[0].id);
    }

    void AcpModelUtilsTests::CustomLaunchIdentityTracksOnlyLaunchFields()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        const auto original = CustomModels::MakeLaunchConfiguration(
            L"custom:provider:model-a",
            L"https://example.test/v1",
            L"model-a",
            L"credential-id",
            true);

        const std::optional<CustomModels::LaunchConfiguration> byok{ original };
        const std::optional<CustomModels::LaunchConfiguration> cloud;
        VERIFY_IS_FALSE(byok == cloud);

        VERIFY_IS_FALSE(
            original ==
            CustomModels::MakeLaunchConfiguration(
                L"custom:provider:model-a",
                L"https://changed.test/v1",
                L"model-a",
                L"credential-id",
                true));

        VERIFY_IS_FALSE(
            original ==
            CustomModels::MakeLaunchConfiguration(
                L"custom:provider:model-a",
                L"https://example.test/v1",
                L"model-a",
                L"other-credential",
                true));

        VERIFY_IS_FALSE(
            original ==
            CustomModels::MakeLaunchConfiguration(
                L"custom:provider:model-a",
                L"https://example.test/v1",
                L"model-a",
                L"credential-id",
                false));

        VERIFY_IS_FALSE(
            original ==
            CustomModels::MakeLaunchConfiguration(
                L"custom:provider:model-a",
                L"https://example.test/v1",
                L"model-b",
                L"credential-id",
                true));

        VERIFY_IS_FALSE(
            original ==
            CustomModels::MakeLaunchConfiguration(
                L"custom:other:model-a",
                L"https://example.test/v1",
                L"model-a",
                L"credential-id",
                true));
    }

    void AcpModelUtilsTests::CustomCatalogSerializationIsCredentialFree()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        const std::array catalog{
            CustomModels::CatalogEntry{
                "custom:provider:model-a",
                "provider",
                "Provider",
                "openai-compatible",
                "cloud",
                "model-a",
                "Model A",
            },
        };

        const auto json = CustomModels::CatalogToJson(catalog);
        VERIFY_ARE_EQUAL(std::string{ "custom:provider:model-a" }, json[0]["selection_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "provider" }, json[0]["provider_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "Provider" }, json[0]["provider_name"].asString());
        VERIFY_IS_FALSE(json[0].isMember("base_url"));
        VERIFY_ARE_EQUAL(std::string{ "openai-compatible" }, json[0]["api_contract"].asString());
        VERIFY_ARE_EQUAL(std::string{ "cloud" }, json[0]["location"].asString());
        VERIFY_ARE_EQUAL(std::string{ "model-a" }, json[0]["model_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "Model A" }, json[0]["name"].asString());
        VERIFY_IS_FALSE(json[0].isMember("credential_id"));
        VERIFY_IS_FALSE(json[0].isMember("api_key_required"));

        const auto serialized = CustomModels::SerializeCatalog(catalog);
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("must-not-leak"));
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("credential"));
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("api_key"));
    }

    void AcpModelUtilsTests::LargeCustomCatalogDoesNotExpandHelperBootstrap()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        std::vector<CustomModels::CatalogEntry> catalog;
        catalog.reserve(2000);
        for (size_t i = 0; i < 2000; ++i)
        {
            catalog.emplace_back(CustomModels::CatalogEntry{
                fmt::format("custom:provider-{}:model-{}", i, i),
                fmt::format("provider-{}", i),
                std::string(1024, 'N'),
                "openai-compatible",
                "cloud",
                fmt::format("model-{}", i),
                std::string(2048, 'D'),
            });
        }

        constexpr std::wstring_view selection{ L"custom:provider-1999:model-1999" };
        const auto args =
            CustomModels::BuildHelperModelBootstrapArguments(selection, catalog, true);
        VERIFY_ARE_EQUAL(size_t{ 1 }, args.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"--custom-model-selection" }, args[0].first);
        VERIFY_ARE_EQUAL(std::wstring{ selection }, args[0].second);
        VERIFY_IS_LESS_THAN(args[0].first.size() + args[0].second.size(), size_t{ 128 });
        VERIFY_IS_TRUE(
            CustomModels::BuildHelperModelBootstrapArguments(selection, catalog, false).empty(),
            L"the same agent running in WSL must not inherit Host BYOK bootstrap metadata");
    }

}
