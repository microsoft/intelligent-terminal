// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// CustomAgentAndPolicyTests.cpp
//
// Covers two areas that were previously untested at the settings-model
// layer (see PR #123):
//
//   1. JSON round-trip of the custom-agent settings. A regression that
//      drops the "custom:" prefix from AcpAgent / DelegateAgent breaks
//      every downstream consumer that keys on the prefix (the
//      EffectiveAcpAgent policy gate, the launcher's command-line
//      resolver, and the custom-edit/delete UI gates). The first half
//      of this file asserts that these settings survive an unmodified
//      load.
//
//   2. The GPO policy matrix on EffectiveAcpAgent / EffectiveDelegateAgent.
//      AllowedAgents (registry REG_MULTI_SZ) only filters built-in agent
//      IDs; the custom: scheme is gated separately by AllowCustomAgents
//      (registry REG_DWORD). This file pins that behavior so a future
//      refactor of the policy gate doesn't silently change it.
//      Policy state is injected via GlobalAppSettings::_TestHookSetAgentPolicy
//      so the tests do not touch the user's registry.

#include "pch.h"

#include "../TerminalSettingsModel/GlobalAppSettings.h"
#include "../TerminalSettingsModel/CascadiaSettings.h"
#include "../TerminalSettingsModel/AcpRuntimeState.h"
#include "../inc/AgentPolicy.h"
#include "../inc/CustomModelProviderUtils.h"
#include "JsonTestClass.h"

using namespace Microsoft::Console;
using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace winrt::Microsoft::Terminal::Settings::Model;
namespace AgentPolicy = ::Microsoft::Terminal::Settings::Model::AgentPolicy;

namespace SettingsModelUnitTests
{
    class CustomAgentAndPolicyTests : public JsonTestClass
    {
        TEST_CLASS(CustomAgentAndPolicyTests);

        // Round-trip tests
        TEST_METHOD(CustomAcpAgentRoundtrips);
        TEST_METHOD(CustomDelegateAgentRoundtrips);
        TEST_METHOD(CustomAgentCollectionsRoundtrip);
        TEST_METHOD(QuotedPathCustomCommandRoundtrips);

        // Policy: AcpAgent
        TEST_METHOD(EffectiveAcpAgentEmptyStaysEmpty);
        TEST_METHOD(EffectiveAcpAgentBuiltInPassesWhenNoAllowlist);
        TEST_METHOD(EffectiveAcpAgentBuiltInPassesWhenInAllowlist);
        TEST_METHOD(EffectiveAcpAgentBuiltInBlockedWhenMissingFromAllowlist);
        TEST_METHOD(EffectiveAcpAgentBuiltInMatchIsCaseInsensitive);
        TEST_METHOD(EffectiveAcpAgentBuiltInBlockedByEmptyAllowlist);
        TEST_METHOD(EffectiveAcpAgentCustomPassesWhenNoCustomPolicy);
        TEST_METHOD(EffectiveAcpAgentCustomBlockedByCustomPolicy);
        TEST_METHOD(EffectiveAcpAgentCustomIgnoresAllowedAgentsAllowlist);

        // Policy: DelegateAgent (parallel matrix)
        TEST_METHOD(EffectiveDelegateAgentEmptyStaysEmpty);
        TEST_METHOD(EffectiveDelegateAgentBuiltInPassesWhenNoAllowlist);
        TEST_METHOD(EffectiveDelegateAgentBuiltInBlockedWhenMissingFromAllowlist);
        TEST_METHOD(EffectiveDelegateAgentCustomBlockedByCustomPolicy);
        TEST_METHOD(EffectiveDelegateAgentCustomIgnoresAllowedAgentsAllowlist);

        // Lock-state mirroring
        TEST_METHOD(IsAgentPolicyLockedTracksAllowedAgents);
        TEST_METHOD(IsCustomAgentPolicyLockedTracksBlocked);

        // Built-in agent + feature settings round-trip
        TEST_METHOD(BuiltInAcpAgentRoundtrips);
        TEST_METHOD(BuiltInDelegateAgentRoundtrips);
        TEST_METHOD(AcpAndDelegateModelRoundtrip);
        TEST_METHOD(CustomModelProvidersRoundtrip);
        TEST_METHOD(CustomModelProviderContractNormalization);
        TEST_METHOD(CustomModelProviderContractFiltering);
        TEST_METHOD(CustomModelProviderSelectionPreservesUnsupportedContract);
        TEST_METHOD(CustomModelProviderDerivedNameSanitizesEndpoint);
        TEST_METHOD(CustomModelProviderEditorMergePreservesUnsupported);
        TEST_METHOD(CustomModelProviderMultiModelDisplay);
        TEST_METHOD(AcpRuntimeModelsAreScopedByAgent);
        TEST_METHOD(AgentPanePositionRoundtripsAndDefaults);
        TEST_METHOD(ShowTokenUsageAndCostRoundtripsAndDefaultsOff);
        TEST_METHOD(AutoErrorSettingsRoundtrip);
        TEST_METHOD(EffectiveAutoFixFalseWhenDetectionOff);

        TEST_CLASS_CLEANUP(ClassCleanup)
        {
            // Defense in depth: never leave a test snapshot lying around
            // for the next test class to inherit.
            implementation::GlobalAppSettings::_TestHookResetAgentPolicy();
            return true;
        }

        TEST_METHOD_CLEANUP(MethodCleanup)
        {
            // Every test that calls const auto settings = MakeSettings({}); SetPolicy() should be followed by a
            // reset so the next test isn't poisoned by stale state.
            implementation::GlobalAppSettings::_TestHookResetAgentPolicy();
            return true;
        }

    private:
        // Build a minimal CascadiaSettings JSON with the supplied global
        // overrides spliced in. Profiles are required, so we provide one.
        static winrt::com_ptr<implementation::CascadiaSettings> MakeSettings(std::string_view globalsExtra)
        {
            const auto userJson = std::string{ R"({
                "defaultProfile": "{6239a42c-1111-49a3-80bd-e8fdd045185c}",
                "profiles": [
                    {
                        "name": "p0",
                        "guid": "{6239a42c-1111-49a3-80bd-e8fdd045185c}"
                    }
                ])" } +
                                  (globalsExtra.empty() ? "" : ("," + std::string{ globalsExtra })) +
                                  "}";
            return winrt::make_self<implementation::CascadiaSettings>(userJson, std::string_view{});
        }

        static std::shared_ptr<AgentPolicy::PolicySnapshot> MakePolicy(
            std::optional<std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>> allowedAgents = std::nullopt,
            AgentPolicy::PolicyState customAgents = AgentPolicy::PolicyState::NotConfigured)
        {
            auto snap = std::make_shared<AgentPolicy::PolicySnapshot>();
            snap->allowedAgents = std::move(allowedAgents);
            snap->customAgents = customAgents;
            return snap;
        }

        // Install a policy snapshot in the SettingsModel DLL for the
        // remainder of the test.
        //
        // IMPORTANT: Must be called AFTER MakeSettings(). CascadiaSettings'
        // load path calls AgentPolicy::Reload() which reads the real
        // registry and clobbers any test snapshot installed beforehand.
        static void SetPolicy(std::shared_ptr<const AgentPolicy::PolicySnapshot> snap)
        {
            implementation::GlobalAppSettings::_TestHookSetAgentPolicy(std::move(snap));
        }
    };

    // ── Round-trip ──────────────────────────────────────────────────────

    void CustomAgentAndPolicyTests::CustomAcpAgentRoundtrips()
    {
        // The whole point of PR #123: a custom agent must survive load
        // with its "custom:" prefix intact. If this regresses, the
        // settings page reverts to the default agent on next load.
        const auto settings = MakeSettings(R"("acpAgent": "custom:helper", "acpCustomCommand": "helper.cmd --acp")");
        const auto& globals = settings->GlobalSettings();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:helper" }, globals.AcpAgent());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"helper.cmd --acp" }, globals.AcpCustomCommand());
    }

    void CustomAgentAndPolicyTests::CustomDelegateAgentRoundtrips()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "custom:helper", "delegateCustomCommand": "helper.cmd --acp")");
        const auto& globals = settings->GlobalSettings();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:helper" }, globals.DelegateAgent());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"helper.cmd --acp" }, globals.DelegateCustomCommand());
    }

    void CustomAgentAndPolicyTests::CustomAgentCollectionsRoundtrip()
    {
        const auto settings = MakeSettings(
            R"("acpCustomCommands": ["test", "test2"], "delegateCustomCommands": ["delegate1", "delegate2"])");
        const auto& globals = settings->GlobalSettings();

        const auto acpCommands = globals.AcpCustomCommands();
        VERIFY_ARE_EQUAL(uint32_t{ 2 }, acpCommands.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"test" }, acpCommands.GetAt(0));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"test2" }, acpCommands.GetAt(1));

        const auto delegateCommands = globals.DelegateCustomCommands();
        VERIFY_ARE_EQUAL(uint32_t{ 2 }, delegateCommands.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"delegate1" }, delegateCommands.GetAt(0));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"delegate2" }, delegateCommands.GetAt(1));

        const auto serialized = settings->ToJson();
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 2 }, serialized["acpCustomCommands"].size());
        VERIFY_ARE_EQUAL(std::string{ "test" }, serialized["acpCustomCommands"][0].asString());
        VERIFY_ARE_EQUAL(std::string{ "test2" }, serialized["acpCustomCommands"][1].asString());
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 2 }, serialized["delegateCustomCommands"].size());
        VERIFY_ARE_EQUAL(std::string{ "delegate1" }, serialized["delegateCustomCommands"][0].asString());
        VERIFY_ARE_EQUAL(std::string{ "delegate2" }, serialized["delegateCustomCommands"][1].asString());
    }

    void CustomAgentAndPolicyTests::QuotedPathCustomCommandRoundtrips()
    {
        // Commands containing spaces (so containing JSON-escaped quotes)
        // are common for users on the Windows installer paths. Make sure
        // the parser preserves them verbatim.
        const auto settings = MakeSettings(
            R"("acpAgent": "custom:helper", "acpCustomCommand": "\"C:\\Program Files\\helper\\helper.cmd\" --acp")");
        const auto& globals = settings->GlobalSettings();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:helper" }, globals.AcpAgent());
        VERIFY_ARE_EQUAL(winrt::hstring{ LR"("C:\Program Files\helper\helper.cmd" --acp)" },
                         globals.AcpCustomCommand());
    }

    // ── EffectiveAcpAgent ───────────────────────────────────────────────

    void CustomAgentAndPolicyTests::EffectiveAcpAgentEmptyStaysEmpty()
    {
        // User explicitly cleared the agent (vs. relying on the "copilot"
        // default). EffectiveAcpAgent must short-circuit before policy
        // checks and return empty unchanged.
        const auto settings = MakeSettings(R"("acpAgent": "")");
        SetPolicy(MakePolicy());
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentBuiltInPassesWhenNoAllowlist()
    {
        // No AllowedAgents policy → all built-in agents pass through.
        const auto settings = MakeSettings(R"("acpAgent": "copilot")");
        SetPolicy(MakePolicy());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentBuiltInPassesWhenInAllowlist()
    {
        const auto settings = MakeSettings(R"("acpAgent": "copilot")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"copilot", L"gemini" }));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentBuiltInBlockedWhenMissingFromAllowlist()
    {
        // IT admin published an allowlist that does NOT contain "copilot".
        // EffectiveAcpAgent must collapse the user's choice to empty.
        const auto settings = MakeSettings(R"("acpAgent": "copilot")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"gemini" }));
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentBuiltInMatchIsCaseInsensitive()
    {
        // AgentPolicy::CaseInsensitiveLess is used so admin can spell
        // "Copilot" / "COPILOT" / "copilot" and they all match.
        const auto settings = MakeSettings(R"("acpAgent": "copilot")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"Copilot" }));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentBuiltInBlockedByEmptyAllowlist()
    {
        // Empty allowlist (configured but empty) means *nothing* is
        // allowed. Distinct from "not configured" (nullopt) which means
        // everything is allowed.
        const auto settings = MakeSettings(R"("acpAgent": "copilot")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{}));
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentCustomPassesWhenNoCustomPolicy()
    {
        const auto settings = MakeSettings(R"("acpAgent": "custom:helper", "acpCustomCommand": "helper.cmd")");
        SetPolicy(MakePolicy(/*allowedAgents*/ std::nullopt, AgentPolicy::PolicyState::NotConfigured));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:helper" }, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentCustomBlockedByCustomPolicy()
    {
        const auto settings = MakeSettings(R"("acpAgent": "custom:helper", "acpCustomCommand": "helper.cmd")");
        SetPolicy(MakePolicy(/*allowedAgents*/ std::nullopt, AgentPolicy::PolicyState::Blocked));
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveAcpAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveAcpAgentCustomIgnoresAllowedAgentsAllowlist()
    {
        // Documented behavior: AllowedAgents only filters built-in IDs.
        // A custom: agent is gated solely by AllowCustomAgents.
        //
        // Admin allowlist with only "gemini" — would block built-in
        // copilot. But a custom: agent passes through unchanged because
        // customAgents policy is NotConfigured / Allowed.
        const auto settings = MakeSettings(R"("acpAgent": "custom:helper", "acpCustomCommand": "helper.cmd")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"gemini" },
                             AgentPolicy::PolicyState::NotConfigured));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:helper" }, settings->GlobalSettings().EffectiveAcpAgent());
    }

    // ── EffectiveDelegateAgent ──────────────────────────────────────────

    void CustomAgentAndPolicyTests::EffectiveDelegateAgentEmptyStaysEmpty()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "")");
        SetPolicy(MakePolicy());
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveDelegateAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveDelegateAgentBuiltInPassesWhenNoAllowlist()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "copilot")");
        SetPolicy(MakePolicy());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"copilot" }, settings->GlobalSettings().EffectiveDelegateAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveDelegateAgentBuiltInBlockedWhenMissingFromAllowlist()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "copilot")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"gemini" }));
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveDelegateAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveDelegateAgentCustomBlockedByCustomPolicy()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "custom:helper", "delegateCustomCommand": "helper.cmd")");
        SetPolicy(MakePolicy(/*allowedAgents*/ std::nullopt, AgentPolicy::PolicyState::Blocked));
        VERIFY_ARE_EQUAL(winrt::hstring{}, settings->GlobalSettings().EffectiveDelegateAgent());
    }

    void CustomAgentAndPolicyTests::EffectiveDelegateAgentCustomIgnoresAllowedAgentsAllowlist()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "custom:helper", "delegateCustomCommand": "helper.cmd")");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"gemini" },
                             AgentPolicy::PolicyState::NotConfigured));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:helper" }, settings->GlobalSettings().EffectiveDelegateAgent());
    }

    // ── Lock-state ──────────────────────────────────────────────────────

    void CustomAgentAndPolicyTests::IsAgentPolicyLockedTracksAllowedAgents()
    {
        // No allowlist → not locked.
        auto settings = MakeSettings({});
        SetPolicy(MakePolicy());
        VERIFY_IS_FALSE(settings->GlobalSettings().IsAgentPolicyLocked());

        // Allowlist present → locked.
        settings = MakeSettings({});
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"copilot" }));
        VERIFY_IS_TRUE(settings->GlobalSettings().IsAgentPolicyLocked());

        // Empty allowlist also counts as configured → locked.
        settings = MakeSettings({});
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{}));
        VERIFY_IS_TRUE(settings->GlobalSettings().IsAgentPolicyLocked());
    }

    void CustomAgentAndPolicyTests::IsCustomAgentPolicyLockedTracksBlocked()
    {
        auto settings = MakeSettings({});
        SetPolicy(MakePolicy(std::nullopt, AgentPolicy::PolicyState::NotConfigured));
        VERIFY_IS_FALSE(settings->GlobalSettings().IsCustomAgentPolicyLocked());

        settings = MakeSettings({});
        SetPolicy(MakePolicy(std::nullopt, AgentPolicy::PolicyState::Allowed));
        VERIFY_IS_FALSE(settings->GlobalSettings().IsCustomAgentPolicyLocked());

        settings = MakeSettings({});
        SetPolicy(MakePolicy(std::nullopt, AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_TRUE(settings->GlobalSettings().IsCustomAgentPolicyLocked());
    }

    // ── Built-in agent + feature settings round-trip ────────────────────

    void CustomAgentAndPolicyTests::BuiltInAcpAgentRoundtrips()
    {
        // A built-in agent id (no "custom:" prefix) must survive load verbatim.
        const auto settings = MakeSettings(R"("acpAgent": "gemini")");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"gemini" }, settings->GlobalSettings().AcpAgent());
    }

    void CustomAgentAndPolicyTests::BuiltInDelegateAgentRoundtrips()
    {
        const auto settings = MakeSettings(R"("delegateAgent": "claude")");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, settings->GlobalSettings().DelegateAgent());
    }

    void CustomAgentAndPolicyTests::AcpAndDelegateModelRoundtrip()
    {
        const auto settings = MakeSettings(R"("acpModel": "gpt-5", "delegateModel": "claude-4")");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"gpt-5" }, settings->GlobalSettings().AcpModel());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"claude-4" }, settings->GlobalSettings().DelegateModel());
    }

    void CustomAgentAndPolicyTests::CustomModelProvidersRoundtrip()
    {
        const auto settings = MakeSettings(
            R"("customModelSelection": "custom:provider-openrouter:qwen/qwen3.5-9b", "customModelProviders": [{"id":"provider-openrouter","name":"OpenRouter","baseUrl":"https://openrouter.ai/api/v1","apiContract":"openai-compatible","location":"cloud","apiKeyCredential":"{11111111-1111-1111-1111-111111111111}","models":[{"id":"qwen/qwen3.5-9b","name":"Qwen 3.5 9B"},{"id":"deepseek/deepseek-v3","name":"DeepSeek V3"}]},{"id":"provider-ollama","baseUrl":"http://localhost:11434/v1","location":"auto","models":[{"id":"qwen3.5:9b","name":"Qwen 3.5 9B"}]}])");
        const auto& globals = settings->GlobalSettings();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:provider-openrouter:qwen/qwen3.5-9b" }, globals.CustomModelSelection());
        const auto providers = globals.CustomModelProviders();
        VERIFY_ARE_EQUAL(2u, providers.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"provider-openrouter" }, providers.GetAt(0).Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"https://openrouter.ai/api/v1" }, providers.GetAt(0).BaseUrl());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"openai-compatible" }, providers.GetAt(0).ApiContract());
        VERIFY_IS_TRUE(providers.GetAt(0).ApiKeyRequired());
        VERIFY_ARE_EQUAL(2u, providers.GetAt(0).Models().Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"qwen/qwen3.5-9b" }, providers.GetAt(0).Models().GetAt(0).Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"provider-ollama" }, providers.GetAt(1).Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"localhost" }, providers.GetAt(1).Name());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"openai-compatible" }, providers.GetAt(1).ApiContract());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"auto" }, providers.GetAt(1).Location());
        VERIFY_IS_FALSE(providers.GetAt(1).ApiKeyRequired());
        VERIFY_ARE_EQUAL(1u, providers.GetAt(1).Models().Size());

        const auto copy = settings->Copy();
        const auto copyImpl = winrt::get_self<implementation::CascadiaSettings>(copy);
        copyImpl->GlobalSettings().CustomModelProviders().GetAt(0).Name(L"Changed");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"OpenRouter" }, providers.GetAt(0).Name());

        auto addedProviders = winrt::single_threaded_vector<CustomModelProvider>();
        auto addedProvider = CustomModelProvider{ L"provider-added", L"Added", L"https://example.test/v1" };
        addedProvider.Location(L"cloud");
        addedProvider.ApiKeyCredential(L"{22222222-2222-2222-2222-222222222222}");
        addedProvider.ApiKeyRequired(true);
        addedProvider.Models().Append(CustomModel{ L"test-model", L"Test model" });
        addedProviders.Append(addedProvider);
        globals.CustomModelProviders(addedProviders);

        const auto serialized = settings->ToJson();
        const auto& serializedProviders = serialized["customModelProviders"];
        VERIFY_IS_TRUE(serializedProviders.isArray());
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 1 }, serializedProviders.size());
        VERIFY_ARE_EQUAL(std::string{ "provider-added" }, serializedProviders[0]["id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "openai-compatible" }, serializedProviders[0]["apiContract"].asString());
        VERIFY_IS_TRUE(serializedProviders[0]["apiKeyRequired"].asBool());
    }

    void CustomAgentAndPolicyTests::CustomModelProviderContractNormalization()
    {
        const auto settings = MakeSettings(
            R"("customModelProviders": [{"id":"missing","baseUrl":"https://missing.test/v1","models":[]},{"id":"blank","baseUrl":"https://blank.test/v1","apiContract":" \t ","models":[]},{"id":"unsupported","baseUrl":"https://unsupported.test/v1","apiContract":"openai-responses","models":[]},{"id":"padded","baseUrl":"https://padded.test/v1","apiContract":" openai-compatible ","models":[]}])");
        const auto providers = settings->GlobalSettings().CustomModelProviders();
        VERIFY_ARE_EQUAL(4u, providers.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"openai-compatible" }, providers.GetAt(0).ApiContract());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"openai-compatible" }, providers.GetAt(1).ApiContract());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"openai-responses" }, providers.GetAt(2).ApiContract());
        VERIFY_ARE_EQUAL(winrt::hstring{ L" openai-compatible " }, providers.GetAt(3).ApiContract());
    }

    void CustomAgentAndPolicyTests::CustomModelProviderContractFiltering()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        auto providers = winrt::single_threaded_vector<CustomModelProvider>();
        auto supported = CustomModelProvider{ L"supported", L"Supported", L"https://supported.test/v1" };
        supported.Models().Append(CustomModel{ L"model-a", L"Model A" });
        providers.Append(supported);

        auto unsupported = CustomModelProvider{ L"unsupported", L"Unsupported", L"https://unsupported.test/v1" };
        unsupported.ApiContract(L"openai-responses");
        unsupported.Models().Append(CustomModel{ L"model-b", L"Model B" });
        providers.Append(unsupported);

        const auto catalog = CustomModels::CaptureCatalog(providers);
        VERIFY_ARE_EQUAL(size_t{ 1 }, catalog.size());
        VERIFY_ARE_EQUAL(std::string{ "custom:supported:model-a" }, catalog[0].selectionId);
        VERIFY_ARE_EQUAL(
            std::string{ CustomModels::CanonicalApiContractUtf8 },
            catalog[0].apiContract);
    }

    void CustomAgentAndPolicyTests::CustomModelProviderSelectionPreservesUnsupportedContract()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        const auto settings = MakeSettings(
            R"("customModelSelection":"custom:future:future-model","customModelProviders":[{"id":"future","baseUrl":"https://user:password@future.example/v2?token=secret#fragment","apiContract":"future-contract-v2","models":[{"id":"future-model","name":"Future Model"}]}])");
        const auto& globals = settings->GlobalSettings();
        VERIFY_ARE_EQUAL(
            winrt::hstring{ L"custom:future:future-model" },
            globals.CustomModelSelection());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"future.example" }, globals.CustomModelProviders().GetAt(0).Name());
        VERIFY_IS_TRUE(CustomModels::SelectionExists(
            globals.CustomModelProviders(),
            std::wstring_view{ globals.CustomModelSelection() }));
        VERIFY_IS_FALSE(CustomModels::SelectionExists(
            globals.CustomModelProviders(),
            L"custom:future:missing-model"));

        const auto helperCatalog = CustomModels::CaptureCatalog(globals.CustomModelProviders());
        VERIFY_ARE_EQUAL(size_t{ 0 }, helperCatalog.size());
        const auto serialized = settings->ToJson();
        VERIFY_ARE_EQUAL(
            std::string{ "custom:future:future-model" },
            serialized["customModelSelection"].asString());
    }

    void CustomAgentAndPolicyTests::CustomModelProviderDerivedNameSanitizesEndpoint()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        const auto settings = MakeSettings(
            R"("customModelProviders":[{"id":"provider","name":"https://user:password@models.example:8443/v1?token=secret#fragment","baseUrl":"https://user:password@models.example:8443/v1?token=secret#fragment","models":[{"id":"model-a"}]}])");
        const auto provider = settings->GlobalSettings().CustomModelProviders().GetAt(0);
        VERIFY_ARE_EQUAL(winrt::hstring{ L"models.example" }, provider.Name());

        const auto catalog = CustomModels::CaptureCatalog(
            settings->GlobalSettings().CustomModelProviders());
        VERIFY_ARE_EQUAL(size_t{ 1 }, catalog.size());
        const auto serialized = CustomModels::SerializeCatalog(catalog);
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("user"));
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("password"));
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("token"));
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("base_url"));
        VERIFY_ARE_EQUAL(std::string::npos, serialized.find("/v1"));
        VERIFY_IS_TRUE(serialized.find("models.example") != std::string::npos);
    }


    void CustomAgentAndPolicyTests::CustomModelProviderEditorMergePreservesUnsupported()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        auto removed = CustomModelProvider{ L"removed", L"Removed", L"https://removed.test/v1" };
        removed.Models().Append(CustomModel{ L"removed-model", L"Removed Model" });

        auto hidden = CustomModelProvider{ L"future", L"Future Provider", L"https://future.test/v2" };
        hidden.ApiContract(L"future-contract-v2");
        hidden.Location(L"cloud");
        hidden.ApiKeyCredential(L"{11111111-1111-1111-1111-111111111111}");
        hidden.ApiKeyRequired(true);
        hidden.Models().Append(CustomModel{ L"future-model", L"Future Model" });

        auto editedOriginal = CustomModelProvider{ L"edited", L"Before", L"https://before.test/v1" };
        editedOriginal.Models().Append(CustomModel{ L"old-model", L"Old Model" });

        auto editedVisible = CustomModelProvider{ L"edited", L"After", L"https://after.test/v1" };
        editedVisible.Models().Append(CustomModel{ L"new-model", L"New Model" });
        auto addedVisible = CustomModelProvider{ L"added", L"Added", L"https://added.test/v1" };
        addedVisible.Models().Append(CustomModel{ L"added-model", L"Added Model" });

        const std::array original{ removed, hidden, editedOriginal };
        const std::array visible{ editedVisible, addedVisible };
        const auto merged =
            CustomModels::MergeProviderEditsPreservingUnsupported(original, visible);

        VERIFY_ARE_EQUAL(size_t{ 3 }, merged.size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"future" }, merged[0].Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"future-contract-v2" }, merged[0].ApiContract());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"https://future.test/v2" }, merged[0].BaseUrl());
        VERIFY_ARE_EQUAL(
            winrt::hstring{ L"{11111111-1111-1111-1111-111111111111}" },
            merged[0].ApiKeyCredential());
        VERIFY_IS_TRUE(merged[0].ApiKeyRequired());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"future-model" }, merged[0].Models().GetAt(0).Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"edited" }, merged[1].Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"After" }, merged[1].Name());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"new-model" }, merged[1].Models().GetAt(0).Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"added" }, merged[2].Id());
        VERIFY_IS_FALSE(std::ranges::any_of(merged, [](const auto& provider) {
            return provider.Id() == L"removed";
        }));
    }

    void CustomAgentAndPolicyTests::CustomModelProviderMultiModelDisplay()
    {
        namespace CustomModels = ::Microsoft::Terminal::CustomModels;

        auto provider = CustomModelProvider{ L"provider", L"Provider", L"https://example.test/v1" };
        provider.Models().Append(CustomModel{ L"model-a", L"Friendly Model" });
        provider.Models().Append(CustomModel{ L"model-b", L"model-b" });

        VERIFY_ARE_EQUAL(
            winrt::hstring{ L"Friendly Model (model-a)\n" L"model-b" },
            CustomModels::FormatModelDisplayText(provider));

        const auto providers = std::array{ provider };
        const auto catalog = CustomModels::CaptureCatalog(providers);
        VERIFY_ARE_EQUAL(size_t{ 2 }, catalog.size());
        VERIFY_ARE_EQUAL(std::string{ "custom:provider:model-a" }, catalog[0].selectionId);
        VERIFY_ARE_EQUAL(std::string{ "custom:provider:model-b" }, catalog[1].selectionId);
    }

    void CustomAgentAndPolicyTests::AcpRuntimeModelsAreScopedByAgent()
    {
        const auto state = AcpRuntimeState::Current();
        auto copilotModels = winrt::single_threaded_vector<AcpModelInfo>();
        copilotModels.Append(winrt::make<implementation::AcpModelInfo>(
            L"custom:openrouter:deepseek",
            L"deepseek (BYOK)",
            L"OpenRouter"));
        auto claudeModels = winrt::single_threaded_vector<AcpModelInfo>();
        claudeModels.Append(winrt::make<implementation::AcpModelInfo>(
            L"default",
            L"Default",
            L"Claude default"));

        state.SetAvailableModels(L"custom:Test-Copilot", copilotModels.GetView(), L"custom:openrouter:deepseek");
        state.SetAvailableModels(L"test-claude", claudeModels.GetView(), L"default");

        const auto cachedCopilot = state.AvailableModels(L"custom:test-copilot");
        const auto cachedClaude = state.AvailableModels(L"test-claude");
        VERIFY_ARE_EQUAL(1u, cachedCopilot.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:openrouter:deepseek" }, cachedCopilot.GetAt(0).Id());
        VERIFY_ARE_EQUAL(1u, cachedClaude.Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"default" }, cachedClaude.GetAt(0).Id());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:openrouter:deepseek" }, state.CurrentModelId(L"CUSTOM:TEST-COPILOT"));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"default" }, state.CurrentModelId(L"test-claude"));
        VERIFY_ARE_EQUAL(0u, state.AvailableModels(L"test-missing").Size());

        const auto staleRevision = state.Revision(L"custom:test-copilot");
        state.SetAvailableModels(L"custom:test-copilot", claudeModels.GetView(), L"default");
        VERIFY_IS_FALSE(state.TrySetAvailableModels(
            L"custom:test-copilot",
            staleRevision,
            copilotModels.GetView(),
            L"custom:openrouter:deepseek"));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"default" }, state.CurrentModelId(L"custom:test-copilot"));

        VERIFY_IS_FALSE(state.TrySetAvailableModels(
            L"test-missing-write",
            1,
            copilotModels.GetView(),
            L"custom:openrouter:deepseek"));
        VERIFY_ARE_EQUAL(0ull, state.Revision(L"test-missing-write"));
        VERIFY_ARE_EQUAL(winrt::hstring{}, state.CurrentModelId(L"test-missing-write"));
        VERIFY_ARE_EQUAL(0u, state.AvailableModels(L"test-missing-write").Size());
        VERIFY_IS_TRUE(state.TrySetAvailableModels(
            L"test-missing-write",
            0,
            copilotModels.GetView(),
            L"custom:openrouter:deepseek"));
        VERIFY_ARE_EQUAL(1ull, state.Revision(L"test-missing-write"));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:openrouter:deepseek" }, state.CurrentModelId(L"test-missing-write"));
    }

    void CustomAgentAndPolicyTests::AgentPanePositionRoundtripsAndDefaults()
    {
        // Explicit value survives load.
        const auto settings = MakeSettings(R"("agentPanePosition": "right")");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"right" }, settings->GlobalSettings().AgentPanePosition());

        // Absent → falls back to the "bottom" default (MTSMSettings.h).
        const auto defaulted = MakeSettings({});
        VERIFY_ARE_EQUAL(winrt::hstring{ L"bottom" }, defaulted->GlobalSettings().AgentPanePosition());
    }

    void CustomAgentAndPolicyTests::ShowTokenUsageAndCostRoundtripsAndDefaultsOff()
    {
        const auto enabled = MakeSettings(R"("showTokenUsageAndCost": true)");
        VERIFY_IS_TRUE(enabled->GlobalSettings().ShowTokenUsageAndCost());

        const auto defaulted = MakeSettings({});
        VERIFY_IS_FALSE(defaulted->GlobalSettings().ShowTokenUsageAndCost());
    }

    void CustomAgentAndPolicyTests::AutoErrorSettingsRoundtrip()
    {
        const auto settings = MakeSettings(R"("autoErrorDetectionEnabled": true, "autoFixEnabled": true)");
        VERIFY_IS_TRUE(settings->GlobalSettings().AutoErrorDetectionEnabled());
        VERIFY_IS_TRUE(settings->GlobalSettings().AutoFixEnabled());

        const auto off = MakeSettings(R"("autoErrorDetectionEnabled": false, "autoFixEnabled": false)");
        VERIFY_IS_FALSE(off->GlobalSettings().AutoErrorDetectionEnabled());
        VERIFY_IS_FALSE(off->GlobalSettings().AutoFixEnabled());
    }

    void CustomAgentAndPolicyTests::EffectiveAutoFixFalseWhenDetectionOff()
    {
        // Auto-suggest depends on detection: even with autoFixEnabled=true, the
        // effective value must be false when detection is off, so failures with
        // nothing to detect never reach the agent.
        const auto detectionOff = MakeSettings(
            R"("autoErrorDetectionEnabled": false, "autoFixEnabled": true)");
        SetPolicy(MakePolicy()); // autoFix NotConfigured → allowed
        VERIFY_IS_FALSE(detectionOff->GlobalSettings().EffectiveAutoFixEnabled());

        // Both on (and policy allows) → effective true.
        const auto bothOn = MakeSettings(
            R"("autoErrorDetectionEnabled": true, "autoFixEnabled": true)");
        SetPolicy(MakePolicy());
        VERIFY_IS_TRUE(bothOn->GlobalSettings().EffectiveAutoFixEnabled());
    }
}
