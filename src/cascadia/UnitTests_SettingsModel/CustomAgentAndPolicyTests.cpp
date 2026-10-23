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
#include "../TerminalSettingsModel/AgentProfileGenerator.h"
#include "../TerminalSettingsModel/SettingsTelemetry.h"
#include "../TerminalSettingsModel/ActionArgs.h"
#include "../TerminalSettingsModel/ApplicationState.h"
#include "../TerminalApp/AgentProviderTelemetry.h"
#include "../inc/AgentPolicy.h"
#include "../inc/AgentRegistry.h"
#include "../inc/CustomModelProviderUtils.h"
#include "../inc/AgentProfileUtils.h"
#include "../inc/ProfileSplitPolicy.h"
#include "JsonTestClass.h"
#include <cstring>
#include <fstream>
#include <tuple>
#include <winioctl.h>

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
        TEST_METHOD(AgentProfileSettingsRoundtrip);
        TEST_METHOD(AgentProfileAntigravitySettingsRoundtrip);
        TEST_METHOD(AgentProfileCommandQuoting);
        TEST_METHOD(AgentProfileNativeCommands);
        TEST_METHOD(AgentProfileArgumentValidation);
        TEST_METHOD(AgentProfileDiscoveryLifecycle);
        TEST_METHOD(AgentProfileDiscoveryExecutionAliases);
        TEST_METHOD(AgentProfileDiscoveryCacheLifecycle);
        TEST_METHOD(AgentProfileDiscoveryCacheFirstFailure);
        TEST_METHOD(AgentProfileNativeAllowlist);
        TEST_METHOD(AgentProfileUninstallPreservesOverrides);
        TEST_METHOD(AgentProfileLaunchPolicy);
        TEST_METHOD(AgentProfileBatchCommand);
        TEST_METHOD(AgentProfileSplitTargets);
        TEST_METHOD(CustomDelegateAgentRoundtrips);
        TEST_METHOD(CustomAgentCollectionsRoundtrip);
        TEST_METHOD(StructuredCustomAgentRoundtripAndProfiles);
        TEST_METHOD(StructuredCustomAgentValidation);
        TEST_METHOD(StructuredCustomAgentBatchInvocation);
        TEST_METHOD(CustomAgentLayoutIdentityRoundtrip);
        TEST_METHOD(QuotedPathCustomCommandRoundtrips);
        TEST_METHOD(AgentTelemetryProviderBuckets);
        TEST_METHOD(AgentTelemetryProviderTransitions);
        TEST_METHOD(AgentTelemetryPolicyAndInPlaceChanges);
        TEST_METHOD(AgentTelemetryUnusedCustomInventory);
        TEST_METHOD(AgentTelemetryCustomInventoryDeduplicates);
        TEST_METHOD(AgentTelemetryPolicyCategories);
        TEST_METHOD(AgentTelemetryAppliedSettingsLifecycle);
        TEST_METHOD(AgentTelemetryProviderSnapshots);

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
        TEST_METHOD(NativeAgentCreationPolicyUsesCurrentSnapshot);
        void _NativeAgentCreationPolicyUsesCurrentSnapshot()
        {
            namespace Registry = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
            AgentPolicy::PolicySnapshot policy;
            VERIFY_IS_TRUE(Registry::IsNativeAgentProviderAllowed(L"copilot", policy));
            policy.allowedAgents.emplace();
            VERIFY_IS_FALSE(Registry::IsNativeAgentProviderAllowed(L"copilot", policy));
            VERIFY_IS_TRUE(Registry::IsNativeAgentProviderAllowed(L"custom:fixture", policy));
            policy.allowedAgents->insert(L"copilot");
            VERIFY_IS_TRUE(Registry::IsNativeAgentProviderAllowed(L"COPILOT", policy));
            policy.customAgents = AgentPolicy::PolicyState::Blocked;
            VERIFY_IS_FALSE(Registry::IsNativeAgentProviderAllowed(L"custom:fixture", policy));
            policy.customAgents = AgentPolicy::PolicyState::Allowed;
            VERIFY_IS_TRUE(Registry::IsNativeAgentProviderAllowed(L"custom:fixture", policy));
            VERIFY_IS_FALSE(Registry::IsNativeAgentProviderAllowed(L"unknown", policy));
        }

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
        TEST_METHOD(ShowTokenUsageAndCostRoundtripsAndDefaultsOn);
        TEST_METHOD(AgentSessionManagementRoundtripsDefaultsOnAndHonorsPolicy);
        TEST_METHOD(AutoErrorSettingsRoundtrip);
        TEST_METHOD(EffectiveAutoFixFalseWhenDetectionOff);
        TEST_METHOD(AutoFixPolicyPreservesErrorDetection);
        TEST_METHOD(AgentPaneYoloModeRoundtripsAndDefaults);
        TEST_METHOD(EffectiveAgentPaneYoloModeFalseForOpenCode);
        TEST_METHOD(EffectiveAgentPaneYoloModeFalseWhenDefaultAgentBlocked);
        TEST_METHOD(CanEnableAgentPaneYoloModeTracksPolicyAndProvider);
        TEST_METHOD(OpenCodeDefaultClearsStoredAgentPaneYoloMode);
        TEST_METHOD(EffectiveAgentPaneYoloModeFalseWhenPolicyBlocked);
        TEST_METHOD(PolicyBlockClearsStoredAgentPaneYoloMode);
        TEST_METHOD(IsYoloModePolicyLockedTracksBlocked);
        TEST_METHOD(YoloSettingsNoticeTracksSelectedProviderAndPreference);
        TEST_METHOD(PolicyChangeWatcherTargetsProductKey);
        TEST_METHOD(PolicyChangeWatcherObservesRecursiveMutation);
        TEST_METHOD(PolicyChangeWatcherTracksPolicyCreatedFromMissingAncestors);

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
            AgentPolicy::PolicyState customAgents = AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState yoloMode = AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState autoFix = AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState agentSessionHooks = AgentPolicy::PolicyState::NotConfigured)
        {
            auto snap = std::make_shared<AgentPolicy::PolicySnapshot>();
            snap->allowedAgents = std::move(allowedAgents);
            snap->customAgents = customAgents;
            snap->yoloMode = yoloMode;
            snap->agentSessionHooks = agentSessionHooks;
            snap->autoFix = autoFix;
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

    void CustomAgentAndPolicyTests::AgentTelemetryProviderBuckets()
    {
        using namespace implementation::AgentSettingsTelemetry;
        for (const auto id : { L"copilot", L"claude", L"codex", L"gemini", L"opencode", L"antigravity" })
        {
            VERIFY_ARE_EQUAL(winrt::to_string(id), std::string{ ProviderId(id) });
        }
        VERIFY_ARE_EQUAL(std::string{ "none" }, std::string{ ProviderId(L"") });
        VERIFY_ARE_EQUAL(std::string{ "custom" }, std::string{ ProviderId(L"custom:private-agent --secret") });
        VERIFY_ARE_EQUAL(std::string{ "unknown" }, std::string{ ProviderId(L"private-agent --secret") });
        VERIFY_ARE_EQUAL(std::string{ "unknown" }, std::string{ ProviderId(L"custom") });
        VERIFY_ARE_EQUAL(std::string{ "unknown" }, std::string{ ProviderId(L"agy --model private") });
    }

    void CustomAgentAndPolicyTests::AgentTelemetryProviderTransitions()
    {
        using namespace implementation::AgentSettingsTelemetry;
        VERIFY_IS_FALSE(GetProviderChange(L"copilot", L"copilot").has_value());
        VERIFY_IS_FALSE(GetProviderChange(L"", L"").has_value());
        VERIFY_IS_FALSE(GetProviderChange(L"custom:one", L"custom:one").has_value());
        const auto builtin = GetProviderChange(L"copilot", L"claude");
        VERIFY_IS_TRUE(builtin.has_value());
        VERIFY_ARE_EQUAL(std::string{ "copilot" }, std::string{ builtin->from });
        VERIFY_ARE_EQUAL(std::string{ "claude" }, std::string{ builtin->to });
        const auto custom = GetProviderChange(L"custom:one", L"custom:two");
        VERIFY_IS_TRUE(custom.has_value());
        VERIFY_ARE_EQUAL(std::string{ "custom" }, std::string{ custom->from });
        VERIFY_ARE_EQUAL(std::string{ "custom" }, std::string{ custom->to });
        const auto cleared = GetProviderChange(L"custom:two", L"");
        VERIFY_IS_TRUE(cleared.has_value());
        VERIFY_ARE_EQUAL(std::string{ "none" }, std::string{ cleared->to });
    }

    void CustomAgentAndPolicyTests::AgentTelemetryAppliedSettingsLifecycle()
    {
        using namespace implementation::AgentSettingsTelemetry;
        for (const auto initialSucceeded : { false, true })
        {
            ::TerminalApp::AgentProviderTelemetryBaseline baseline;
            const auto globals = initialSucceeded ?
                                     MakeSettings(R"("acpAgent":"codex","delegateAgent":"gemini")")->GlobalSettings() :
                                     implementation::CascadiaSettings::LoadDefaults().GlobalSettings();
            const auto initialPrimary = globals.AcpAgent();
            const auto initialDelegate = globals.DelegateAgent();

            // A failed initial load applies defaults; neither startup path is a change.
            VERIFY_IS_FALSE(baseline.ObserveAppliedSettings({ initialPrimary, initialDelegate }).has_value());

            globals.AcpAgent(L"claude");
            const auto recovered = baseline.ObserveAppliedSettings({ globals.AcpAgent(), globals.DelegateAgent() });
            VERIFY_IS_TRUE(recovered.has_value());
            VERIFY_ARE_EQUAL(initialPrimary, recovered->primary);
            VERIFY_ARE_EQUAL(initialDelegate, recovered->delegate);
            const auto change = GetProviderChange(recovered->primary, globals.AcpAgent());
            VERIFY_IS_TRUE(change.has_value());
            VERIFY_ARE_EQUAL(std::string{ "claude" }, std::string{ change->to });
            VERIFY_IS_FALSE(GetProviderChange(recovered->delegate, globals.DelegateAgent()).has_value());

            const auto unchanged = baseline.ObserveAppliedSettings({ L"claude", initialDelegate });
            VERIFY_IS_TRUE(unchanged.has_value());
            VERIFY_IS_FALSE(GetProviderChange(unchanged->primary, L"claude").has_value());
            VERIFY_ARE_EQUAL(initialDelegate, unchanged->delegate);

            const auto customFirst = baseline.ObserveAppliedSettings({ L"custom:first", L"custom:delegate-first" });
            VERIFY_IS_TRUE(customFirst.has_value());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, customFirst->primary);
            const auto customNext = baseline.ObserveAppliedSettings({ L"custom:second", L"custom:delegate-second" });
            VERIFY_IS_TRUE(customNext.has_value());
            VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:first" }, customNext->primary);
            VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:delegate-first" }, customNext->delegate);
            VERIFY_IS_TRUE(GetProviderChange(customNext->primary, L"custom:second").has_value());
            VERIFY_IS_TRUE(GetProviderChange(customNext->delegate, L"custom:delegate-second").has_value());
        }
    }

    void CustomAgentAndPolicyTests::AgentTelemetryProviderSnapshots()
    {
        using namespace implementation::AgentSettingsTelemetry;
        const auto settings = MakeSettings(R"(
            "acpAgent":"copilot",
            "acpCustomCommands":["unused --acp"],
            "delegateAgent":"custom:delegate",
            "delegateCustomCommands":["delegate --stdio"])");
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{}, AgentPolicy::PolicyState::Blocked));
        const auto globals = settings->GlobalSettings();
        const auto primary = GetProviderSnapshot(globals, true);
        const auto delegate = GetProviderSnapshot(globals, false);
        VERIFY_ARE_EQUAL(std::string{ "copilot" }, std::string{ primary.configured });
        VERIFY_ARE_EQUAL(std::string{ "none" }, std::string{ primary.effective });
        VERIFY_ARE_EQUAL(uint32_t{ 1 }, primary.custom.count);
        VERIFY_IS_FALSE(primary.custom.selectedCommandConfigured);
        VERIFY_ARE_EQUAL(std::string{ "custom" }, std::string{ delegate.configured });
        VERIFY_ARE_EQUAL(std::string{ "none" }, std::string{ delegate.effective });
        VERIFY_ARE_EQUAL(uint32_t{ 1 }, delegate.custom.count);
        VERIFY_IS_TRUE(delegate.custom.selectedCommandConfigured);

        const auto inherited = winrt::get_self<implementation::GlobalAppSettings>(globals)->CreateChild().as<GlobalAppSettings>();
        const auto inheritedPrimary = GetProviderSnapshot(inherited, true);
        const auto inheritedDelegate = GetProviderSnapshot(inherited, false);
        VERIFY_ARE_EQUAL(std::string{ primary.configured }, std::string{ inheritedPrimary.configured });
        VERIFY_ARE_EQUAL(primary.custom.count, inheritedPrimary.custom.count);
        VERIFY_ARE_EQUAL(std::string{ delegate.configured }, std::string{ inheritedDelegate.configured });
        VERIFY_IS_TRUE(inheritedDelegate.custom.selectedCommandConfigured);
    }

    void CustomAgentAndPolicyTests::AgentTelemetryPolicyAndInPlaceChanges()
    {
        using namespace implementation::AgentSettingsTelemetry;
        const auto settings = MakeSettings(R"("acpAgent":"copilot","delegateAgent":"custom:delegate")");
        const auto globals = settings->GlobalSettings();
        const auto previousAcp = globals.AcpAgent();
        const auto previousDelegate = globals.DelegateAgent();
        SetPolicy(MakePolicy(std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{}, AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_TRUE(globals.EffectiveAcpAgent().empty());
        VERIFY_IS_TRUE(globals.EffectiveDelegateAgent().empty());
        VERIFY_IS_FALSE(GetProviderChange(previousAcp, globals.AcpAgent()).has_value());
        VERIFY_IS_FALSE(GetProviderChange(previousDelegate, globals.DelegateAgent()).has_value());

        globals.AcpAgent(L"claude");
        const auto change = GetProviderChange(previousAcp, globals.AcpAgent());
        VERIFY_IS_TRUE(change.has_value());
        VERIFY_ARE_EQUAL(std::string{ "copilot" }, std::string{ change->from });
        VERIFY_ARE_EQUAL(std::string{ "claude" }, std::string{ change->to });
        VERIFY_IS_FALSE(GetProviderChange(previousDelegate, globals.DelegateAgent()).has_value());
    }

    void CustomAgentAndPolicyTests::AgentTelemetryUnusedCustomInventory()
    {
        using namespace implementation::AgentSettingsTelemetry;
        const auto settings = MakeSettings(R"(
            "acpAgent":"copilot",
            "acpCustomCommands":["unused-one --acp","unused-two --acp"],
            "acpCustomCommand":"legacy-unused --acp",
            "delegateAgent":"custom:delegate",
            "delegateCustomCommands":["delegate --stdio"])");
        const auto globals = settings->GlobalSettings();
        const auto primary = GetCustomInventory(globals.AcpAgent(), globals.AcpCustomCommand(), globals.AcpCustomCommands());
        VERIFY_ARE_EQUAL(uint32_t{ 3 }, primary.count);
        VERIFY_IS_FALSE(primary.selectedCommandConfigured);
        const auto delegate = GetCustomInventory(globals.DelegateAgent(), globals.DelegateCustomCommand(), globals.DelegateCustomCommands());
        VERIFY_ARE_EQUAL(uint32_t{ 1 }, delegate.count);
        VERIFY_IS_TRUE(delegate.selectedCommandConfigured);

        const auto missing = GetCustomInventory(L"custom:missing", L"", nullptr);
        VERIFY_ARE_EQUAL(uint32_t{ 0 }, missing.count);
        VERIFY_IS_FALSE(missing.selectedCommandConfigured);
    }

    void CustomAgentAndPolicyTests::AgentTelemetryCustomInventoryDeduplicates()
    {
        using namespace implementation::AgentSettingsTelemetry;
        const auto commands = winrt::single_threaded_vector<winrt::hstring>(
            { L"helper.cmd --old", L"helper.exe --new", L"", L" \t", L"\"\" --invalid", L"\"C:\\Program Files\\other.exe\" --acp" });
        const auto inventory = GetCustomInventory(L"custom:helper", L"helper.bat --legacy", commands);
        VERIFY_ARE_EQUAL(uint32_t{ 2 }, inventory.count);
        VERIFY_IS_TRUE(inventory.selectedCommandConfigured);
    }

    void CustomAgentAndPolicyTests::AgentTelemetryPolicyCategories()
    {
        using namespace implementation::AgentSettingsTelemetry;
        for (const auto custom : { AgentPolicy::PolicyState::NotConfigured, AgentPolicy::PolicyState::Allowed, AgentPolicy::PolicyState::Blocked })
        {
            for (const auto allowlistKind : { 0, 1, 2 })
            {
                auto policy = MakePolicy(std::nullopt, custom);
                if (allowlistKind != 0)
                {
                    policy->allowedAgents.emplace();
                    if (allowlistKind == 2)
                    {
                        policy->allowedAgents->insert(L"private-agent-never-emitted");
                    }
                }
                const auto summary = SummarizePolicy(*policy);
                VERIFY_ARE_EQUAL(std::string{ allowlistKind == 0 ? "not_configured" : (allowlistKind == 1 ? "empty" : "allowlist") }, std::string{ summary.allowedAgentsCategory });
                VERIFY_ARE_EQUAL(std::string{ custom == AgentPolicy::PolicyState::NotConfigured ? "not_configured" : (custom == AgentPolicy::PolicyState::Allowed ? "allowed" : "blocked") }, std::string{ summary.allowCustomAgentsCategory });
            }
        }
    }

    // ── Round-trip ──────────────────────────────────────────────────────

    void CustomAgentAndPolicyTests::StructuredCustomAgentRoundtripAndProfiles()
    {
        namespace Custom = ::Microsoft::Terminal::CustomAgents;
        auto globals = winrt::make_self<implementation::GlobalAppSettings>();
        const auto json = VerifyParseSucceeded(R"({
            "customAgents": [
                {"id":"custom:work","name":"Work",
                 "acp":{"command":"adapter.exe","args":["--stdio"]},
                 "cli":{"command":"work.exe","args":["--config","C:\\config dir\\work.json","","--pattern","|","&&","||",">","<"]},
                 "resume":{"command":"work.exe","args":["resume","{sessionId}"]}},
                {"id":"custom:acp-only","acp":{"command":"adapter.exe"}}
            ]
        })");
        globals->LayerJson(json, OriginTag::User);
        VERIFY_ARE_EQUAL(2u, globals->CustomAgents().Size());
        VERIFY_ARE_EQUAL(std::wstring{ L"\"adapter.exe\" \"--stdio\"" }, Custom::AcpCommand(*globals, L"custom:work"));
        const auto copy = globals->Copy();
        globals->CustomAgents().GetAt(0).Name(L"Renamed");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"Work" }, copy->CustomAgents().GetAt(0).Name());
        const auto serialized = copy->ToJson();
        VERIFY_ARE_EQUAL(std::string{ "custom:work" }, serialized["customAgents"][0]["id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "" }, serialized["customAgents"][0]["cli"]["args"][2].asString());
        const auto originalCli = Custom::FromJson(json["customAgents"][0]["cli"]);
        const auto roundtripCli = Custom::Parse(copy->CustomAgents().GetAt(0).CliCommand());
        VERIFY_IS_TRUE(originalCli.args == roundtripCli.args);
        VERIFY_IS_TRUE(originalCli.args == Custom::FromJson(serialized["customAgents"][0]["cli"]).args);
        std::vector<winrt::com_ptr<implementation::Profile>> first;
        AgentProfileGenerator{ globals->CustomAgents() }.GenerateProfiles(first);
        const auto find = [](const auto& profiles, const std::wstring_view agentId = L"custom:work") {
            return std::ranges::find_if(profiles, [agentId](const auto& profile) { return profile->AgentProfileId() == agentId; });
        };
        const auto generated = find(first);
        VERIFY_IS_TRUE(generated != first.end());
        (*generated)->Origin(OriginTag::Generated);
        auto layered = winrt::make_self<implementation::Profile>();
        layered->AddLeastImportantParent(*generated);
        VERIFY_IS_TRUE(::Microsoft::Terminal::AgentProfiles::IsManaged(*layered));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"Renamed" }, (*generated)->Name());
        VERIFY_IS_TRUE(std::ranges::none_of(first, [](const auto& profile) { return profile->AgentProfileId() == L"custom:acp-only"; }));
        globals->CustomAgents().GetAt(0).Name(L"New name");
        globals->CustomAgents().GetAt(0).CliCommand(L"new-work.exe");
        std::vector<winrt::com_ptr<implementation::Profile>> second;
        AgentProfileGenerator{ globals->CustomAgents() }.GenerateProfiles(second);
        VERIFY_ARE_EQUAL((*generated)->Guid(), (*find(second))->Guid());
        VERIFY_ARE_EQUAL(std::wstring{ L"\"work.exe\" \"resume\" \"abc-123\"" }, Custom::ResumeCommand(*globals, L"custom:work", L"abc-123"));
        globals->CustomAgents().GetAt(0).Id(L"custom:WORK");
        std::vector<winrt::com_ptr<implementation::Profile>> caseVariant;
        AgentProfileGenerator{ globals->CustomAgents() }.GenerateProfiles(caseVariant);
        const auto caseGenerated = find(caseVariant, L"custom:WORK");
        VERIFY_IS_TRUE(caseGenerated != caseVariant.end());
        VERIFY_ARE_EQUAL((*generated)->Guid(), (*caseGenerated)->Guid());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:WORK" }, (*caseGenerated)->AgentProfileId());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"New name" }, (*caseGenerated)->Name());
        VERIFY_ARE_EQUAL((*find(second))->Commandline(), (*caseGenerated)->Commandline());
        globals->CustomAgents().GetAt(0).Id(L"custom:other");
        std::vector<winrt::com_ptr<implementation::Profile>> distinct;
        AgentProfileGenerator{ globals->CustomAgents() }.GenerateProfiles(distinct);
        const auto distinctGenerated = find(distinct, L"custom:other");
        VERIFY_IS_TRUE(distinctGenerated != distinct.end());
        VERIFY_ARE_NOT_EQUAL((*generated)->Guid(), (*distinctGenerated)->Guid());
        layered->Commandline(L"explicit.exe");
        VERIFY_IS_FALSE(::Microsoft::Terminal::AgentProfiles::IsManaged(*layered));
    }

    void CustomAgentAndPolicyTests::StructuredCustomAgentValidation()
    {
        namespace Custom = ::Microsoft::Terminal::CustomAgents;
        for (const auto text : {
                 R"({"customAgents":[{"id":"custom:bad id","cli":{"command":"agent.exe"}}]})",
                 R"({"customAgents":[{"id":"custom:work","acp":{"command":"agent.exe"},"resume":{"command":"agent.exe","args":["{sessionId}"]}}]})",
                 R"({"customAgents":[{"id":"custom:work","cli":{"command":"agent.exe"}},{"id":"custom:WORK","cli":{"command":"other.exe"}}]})",
                 R"({"customAgents":[{"id":"custom:work","cli":{"command":"agent\u0000.exe"}}]})",
                 R"({"customAgents":[{"id":"custom:work","cli":{"command":"agent.exe"},"resume":{"command":"agent.exe","args":["--id={sessionId}"]}}]})" })
        {
            const auto json = VerifyParseSucceeded(text);
            auto globals = winrt::make_self<implementation::GlobalAppSettings>();
            VERIFY_THROWS(globals->LayerJson(json, OriginTag::User), wil::ResultException);
        }
        const Custom::Invocation invocation{ LR"(C:\program files\agent.exe)", { L"", L"a \"quote\"", L"C:\\path dir\\", L"\u4e2d\u6587" } };
        const auto parsed = Custom::Parse(Custom::Format(invocation));
        VERIFY_ARE_EQUAL(invocation.command, parsed.command);
        VERIFY_IS_TRUE(invocation.args == parsed.args);
        for (const auto op : { L"&&", L"||", L"|", L">", L"<" })
        {
            VERIFY_THROWS(Custom::Parse(std::wstring{ L"agent.exe " } + op + L" other.exe"), wil::ResultException);
            const auto literal = Custom::Parse(std::wstring{ L"agent.exe \"" } + op + L"\"");
            VERIFY_ARE_EQUAL(std::wstring{ op }, literal.args.at(0));
        }
        VERIFY_THROWS(Custom::Parse(L"agent.exe \"unfinished"), wil::ResultException);
    }

    void CustomAgentAndPolicyTests::StructuredCustomAgentBatchInvocation()
    {
        namespace Custom = ::Microsoft::Terminal::CustomAgents;
        GUID guid{};
        THROW_IF_FAILED(CoCreateGuid(&guid));
        const auto root = std::filesystem::current_path() / (L"Custom Agent " + Utils::GuidToString(guid));
        std::filesystem::create_directory(root);
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        const auto shim = root / L"agent.cmd";
        std::ofstream{ shim } << "@exit /b 88\n";
        Custom::Invocation invocation{ shim.native(), { L"resume", L"abc-123", LR"(space and ' & | ! ^)", L"C:\\path dir\\file", L"C:\\plain\\", L"|", L"&&" } };
        VERIFY_THROWS(Custom::LaunchCommand(invocation), wil::ResultException);
        std::ofstream{ root / L"agent.ps1" }
            << "$ErrorActionPreference='Stop'\n"
               "& powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'capture.ps1') @args\n"
               "exit $LASTEXITCODE\n";
        std::ofstream{ root / L"capture.ps1" }
            << "$ErrorActionPreference='Stop'\n"
               "$capture = @{ args = @($args); nativeArgs = [Environment]::GetCommandLineArgs(); commandLine = [Environment]::CommandLine }\n"
               "[IO.File]::WriteAllText((Join-Path $PSScriptRoot 'args.json'), (ConvertTo-Json -Compress -InputObject $capture))\n"
               "exit 0\n";
        for (const auto unsupported : { L"", L"a\"b", LR"(quote " and ' & | ! ^)", L"C:\\path dir\\", L"C:\\path dir\\\\", L"C:\\path" L"\t" L"dir\\", L"C:\\path\u00a0dir\\" })
        {
            const Custom::Invocation invalid{ shim.native(), { unsupported } };
            HRESULT result = S_OK;
            try
            {
                Custom::LaunchCommand(invalid);
            }
            catch (const wil::ResultException& error)
            {
                result = error.GetErrorCode();
            }
            VERIFY_ARE_EQUAL(E_INVALIDARG, result);
            auto native = invalid;
            native.command = L"agent.exe";
            VERIFY_ARE_EQUAL(std::wstring{ unsupported }, Custom::Parse(Custom::LaunchCommand(native)).args.at(0));
        }
        auto globals = winrt::make_self<implementation::GlobalAppSettings>();
        globals->LayerJson(VerifyParseSucceeded(R"({"customAgents":[
            {"id":"custom:batch","cli":{"command":"agent.cmd","args":[""]}},
            {"id":"custom:native","cli":{"command":"agent.exe","args":["--pattern","|"]}}
        ]})"), OriginTag::User);
        std::vector<winrt::com_ptr<implementation::Profile>> profiles;
        AgentProfileGenerator{ globals->CustomAgents() }.GenerateProfiles(profiles);
        for (const auto id : { L"custom:batch", L"custom:native" })
        {
            VERIFY_IS_TRUE(std::ranges::any_of(profiles, [&](const auto& profile) { return profile->AgentProfileId() == id; }));
        }
        const auto inheritedEnvironment = GetEnvironmentStringsW();
        THROW_LAST_ERROR_IF(!inheritedEnvironment);
        const auto freeEnvironment = wil::scope_exit([&]() { FreeEnvironmentStringsW(inheritedEnvironment); });
        std::vector<std::wstring> environmentEntries;
        for (auto entry = inheritedEnvironment; *entry; entry += wcslen(entry) + 1)
        {
            const std::wstring_view value{ entry };
            if (!til::equals_insensitive_ascii(value.substr(0, value.find(L'=')), L"PSExecutionPolicyPreference"))
            {
                environmentEntries.emplace_back(value);
            }
        }
        environmentEntries.emplace_back(L"PSExecutionPolicyPreference=Restricted");
        std::ranges::sort(environmentEntries, [](const auto& lhs, const auto& rhs) { return _wcsicmp(lhs.c_str(), rhs.c_str()) < 0; });
        std::vector<wchar_t> environment;
        for (const auto& entry : environmentEntries)
        {
            environment.insert(environment.end(), entry.begin(), entry.end());
            environment.push_back(L'\0');
        }
        environment.push_back(L'\0');
        const auto run = [&](std::wstring command) {
            wil::unique_process_information process;
            STARTUPINFOW startup{ sizeof(startup) };
            THROW_IF_WIN32_BOOL_FALSE(CreateProcessW(nullptr, command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT, environment.data(), root.c_str(), &startup, &process));
            const auto terminate = wil::scope_exit([&]() {
                if (WaitForSingleObject(process.hProcess, 0) == WAIT_TIMEOUT)
                {
                    LOG_IF_WIN32_BOOL_FALSE(TerminateProcess(process.hProcess, 1));
                    WaitForSingleObject(process.hProcess, 5000);
                }
            });
            VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(process.hProcess, 30000));
            DWORD exitCode{};
            THROW_IF_WIN32_BOOL_FALSE(GetExitCodeProcess(process.hProcess, &exitCode));
            return exitCode;
        };
        const auto startupCommand = Custom::LaunchCommand(invocation);
        const auto startupInvocation = Custom::Parse(startupCommand);
        const auto withoutPolicy = Custom::Format({ startupInvocation.command, { L"-NoLogo", L"-NoProfile", L"-Command", startupInvocation.args.back() } });
        VERIFY_ARE_NOT_EQUAL(0u, run(withoutPolicy));
        VERIFY_IS_FALSE(std::filesystem::exists(root / L"args.json"));

        auto resumeInvocation = invocation;
        resumeInvocation.args[1] = L"{sessionId}";
        Json::Value settings{ Json::objectValue };
        Json::Value agent{ Json::objectValue };
        agent["id"] = "custom:batch";
        agent["cli"] = Custom::ToJson(invocation);
        agent["resume"] = Custom::ToJson(resumeInvocation);
        settings["customAgents"].append(agent);
        auto resumeGlobals = winrt::make_self<implementation::GlobalAppSettings>();
        resumeGlobals->LayerJson(settings, OriginTag::User);
        for (const auto& command : { startupCommand, Custom::ResumeCommand(*resumeGlobals, L"custom:batch", L"abc-123") })
        {
            VERIFY_ARE_EQUAL(0u, run(command));
            std::ifstream output{ root / L"args.json" };
            const std::string text{ std::istreambuf_iterator<char>{ output }, std::istreambuf_iterator<char>{} };
            const auto capture = VerifyParseSucceeded(text);
            const auto& args = capture["args"];
            const auto& nativeArgs = capture["nativeArgs"];
            VERIFY_ARE_EQUAL(static_cast<Json::ArrayIndex>(invocation.args.size()), args.size());
            VERIFY_ARE_EQUAL(static_cast<Json::ArrayIndex>(invocation.args.size() + 7), nativeArgs.size());
            for (Json::ArrayIndex i = 0; i < args.size(); ++i)
            {
                VERIFY_ARE_EQUAL(winrt::to_string(invocation.args[i]), args[i].asString());
                VERIFY_ARE_EQUAL(winrt::to_string(invocation.args[i]), nativeArgs[i + 7].asString());
            }
            output.close();
            std::filesystem::remove(root / L"args.json");
        }
    }

    void CustomAgentAndPolicyTests::CustomAgentLayoutIdentityRoundtrip()
    {
        const auto json = VerifyParseSucceeded(R"({"nativeAgentProviderId":"custom:work","customAgentSessionId":"session-123"})");
        const auto args = implementation::NewTerminalArgs::FromJson(json);
        const auto copy = args.Copy().as<NewTerminalArgs>();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:work" }, copy.NativeAgentProviderId());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"session-123" }, copy.CustomAgentSessionId());
        VERIFY_IS_TRUE(args.Equals(copy));
        VERIFY_IS_TRUE(std::wstring_view{ args.ToCommandline() }.find(L"--custom-agent-session") != std::wstring_view::npos);
        const auto saved = implementation::NewTerminalArgs::ToJson(copy);
        VERIFY_ARE_EQUAL(std::string{ "session-123" }, saved["customAgentSessionId"].asString());
        VERIFY_IS_TRUE(copy.CustomAgentBackend().empty());
        WindowLayout layout;
        copy.CustomAgentBackend(L"wsl:Ubuntu:custom:work");
        layout.TabLayout(winrt::single_threaded_vector<ActionAndArgs>({
            ActionAndArgs{ ShortcutAction::NewTab, NewTabArgs{ copy } } }));
        const auto restoredLayout = WindowLayout::FromJson(WindowLayout::ToJson(layout));
        const auto restored = restoredLayout.TabLayout().GetAt(0).Args().as<NewTabArgs>().ContentArgs().as<NewTerminalArgs>();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"wsl:Ubuntu:custom:work" }, restored.CustomAgentBackend());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"custom:work" }, restored.NativeAgentProviderId());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"session-123" }, restored.CustomAgentSessionId());
        VERIFY_THROWS(restored.ToCommandline(), winrt::hresult_error);
        copy.CustomAgentBackend(L"host:custom:work");
        VERIFY_IS_FALSE(args.Equals(copy));
        VERIFY_IS_FALSE(copy.ToCommandline().empty());
        // Original layouts did not record a backend and remain valid host resumes.
        VERIFY_IS_TRUE(implementation::NewTerminalArgs::FromJson(saved).CustomAgentBackend().empty());
        VERIFY_IS_FALSE(implementation::NewTerminalArgs::FromJson(saved).ToCommandline().empty());
        copy.CustomAgentSessionId(L"session-456");
        VERIFY_IS_FALSE(args.Equals(copy));
    }

    void CustomAgentAndPolicyTests::AgentProfileSettingsRoundtrip()
    {
        Json::Value json{ Json::objectValue };
        json["agentProfile.id"] = "claude";
        json["agentProfile.model"] = "example-model";
        json["agentProfile.permissionMode"] = "plan";
        json["agentProfile.arguments"] = "--add-dir \"C:\\source tree\"";
        json["defaultSplitProfile"] = "default";
        auto parent = implementation::Profile::FromJson(json);
        auto child = winrt::make_self<implementation::Profile>();
        child->AddLeastImportantParent(parent);
        VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, child->AgentProfileId());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"example-model" }, child->AgentProfileModel());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"default" }, child->DefaultSplitProfile());
        child->AgentProfileModel(L"override");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"override" }, child->AgentProfileModel());
        child->ClearAgentProfileModel();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"example-model" }, child->AgentProfileModel());
        const auto copy = parent->CopySettings();
        const auto restored = implementation::Profile::FromJson(copy->ToJson());
        VERIFY_ARE_EQUAL(parent->AgentProfileId(), restored->AgentProfileId());
        VERIFY_ARE_EQUAL(parent->AgentProfilePermissionMode(), restored->AgentProfilePermissionMode());
        VERIFY_ARE_EQUAL(parent->AgentProfileArguments(), restored->AgentProfileArguments());
        VERIFY_IS_TRUE(::Microsoft::Terminal::AgentProfiles::IsManaged(*restored));
        restored->Commandline(L"claude --model edited");
        VERIFY_IS_FALSE(::Microsoft::Terminal::AgentProfiles::IsManaged(*restored));
        VERIFY_ARE_EQUAL(std::string{ "claude --model edited" }, restored->ToJson()["commandline"].asString());
        restored->ClearCommandline();
        VERIFY_IS_TRUE(::Microsoft::Terminal::AgentProfiles::IsManaged(*restored));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"claude" }, parent->AgentProfileId());
    }

    void CustomAgentAndPolicyTests::AgentProfileCommandQuoting()
    {
        const auto command = ::Microsoft::Terminal::AgentProfiles::BuildCommand(
            LR"(C:\Program Files\Claude\claude.exe)", L"claude", LR"(model "name"\)", L"plan",
            LR"(--add-dir "C:\source tree")");
        int argc = 0;
        wil::unique_hlocal_ptr<PWSTR[]> argv{ ::CommandLineToArgvW(command.c_str(), &argc) };
        VERIFY_IS_NOT_NULL(argv.get());
        VERIFY_ARE_EQUAL(7, argc);
        VERIFY_ARE_EQUAL(std::wstring{ LR"(C:\Program Files\Claude\claude.exe)" }, std::wstring{ argv[0] });
        VERIFY_ARE_EQUAL(std::wstring{ L"--model" }, std::wstring{ argv[1] });
        VERIFY_ARE_EQUAL(std::wstring{ LR"(model "name"\)" }, std::wstring{ argv[2] });
        VERIFY_ARE_EQUAL(std::wstring{ L"--permission-mode" }, std::wstring{ argv[3] });
        VERIFY_ARE_EQUAL(std::wstring{ L"--add-dir" }, std::wstring{ argv[5] });
        VERIFY_ARE_EQUAL(std::wstring{ LR"(C:\source tree)" }, std::wstring{ argv[6] });
    }

    void CustomAgentAndPolicyTests::AgentProfileAntigravitySettingsRoundtrip()
    {
        Json::Value json{ Json::objectValue };
        json["agentProfile.id"] = "antigravity";
        json["agentProfile.model"] = "chosen-model";
        json["agentProfile.permissionMode"] = "plan";
        json["agentProfile.arguments"] = "--conversation \"session id\"";
        const auto profile = implementation::Profile::FromJson(json);
        const auto copied = profile->CopySettings();
        const auto restored = implementation::Profile::FromJson(copied->ToJson());
        VERIFY_ARE_EQUAL(profile->AgentProfileId(), restored->AgentProfileId());
        VERIFY_ARE_EQUAL(profile->AgentProfileModel(), restored->AgentProfileModel());
        VERIFY_ARE_EQUAL(profile->AgentProfilePermissionMode(), restored->AgentProfilePermissionMode());
        VERIFY_ARE_EQUAL(profile->AgentProfileArguments(), restored->AgentProfileArguments());
        VERIFY_IS_TRUE(::Microsoft::Terminal::AgentProfiles::IsManaged(*restored));
        VERIFY_ARE_EQUAL(std::wstring{ L"\"agy.exe\" \"--model\" \"chosen-model\" \"--mode\" \"plan\" \"--conversation\" \"session id\"" },
                         ::Microsoft::Terminal::AgentProfiles::Command(*restored, false, L""));
        restored->Commandline(L"agy --model edited");
        VERIFY_IS_FALSE(::Microsoft::Terminal::AgentProfiles::IsManaged(*restored));
        restored->ClearCommandline();
        VERIFY_IS_TRUE(::Microsoft::Terminal::AgentProfiles::IsManaged(*restored));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"chosen-model" }, profile->AgentProfileModel());
    }

    void CustomAgentAndPolicyTests::AgentProfileNativeCommands()
    {
        namespace Agents = ::Microsoft::Terminal::AgentProfiles;
        for (const auto& agent : ::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinNativeProfileAgents)
        {
            const auto executable = L"C:\\Native Tools\\" + std::wstring{ agent.cliExecutable } + L".exe";
            VERIFY_ARE_EQUAL(L"\"" + executable + L"\"", Agents::BuildCommand(executable, agent.id, {}, {}, {}));
            const auto args = Agents::ParseArguments(Agents::BuildCommand(executable, agent.id, L"provider/model", {}, L"--version"));
            VERIFY_ARE_EQUAL(size_t{ 4 }, args.size());
            VERIFY_ARE_EQUAL(executable, args[0]);
            VERIFY_ARE_EQUAL(std::wstring{ L"--model" }, args[1]);
            VERIFY_ARE_EQUAL(std::wstring{ L"provider/model" }, args[2]);
            VERIFY_ARE_EQUAL(std::wstring{ L"--version" }, args[3]);
        }
        for (const auto& [id, mode, flag] : {
                 std::tuple{ L"copilot", L"allow-all-tools", L"--allow-all-tools" },
                 std::tuple{ L"claude", L"plan", L"--permission-mode" },
                 std::tuple{ L"codex", L"on-request", L"--ask-for-approval" },
                 std::tuple{ L"gemini", L"auto_edit", L"--approval-mode" },
                 std::tuple{ L"opencode", L"auto", L"--auto" },
                 std::tuple{ L"antigravity", L"accept-edits", L"--mode" },
                 std::tuple{ L"antigravity", L"plan", L"--mode" } })
        {
            const auto args = Agents::ParseArguments(Agents::BuildCommand(L"C:\\agent.exe", id, {}, mode, {}));
            VERIFY_ARE_EQUAL(std::wstring{ flag }, args[1]);
        }
    }

    void CustomAgentAndPolicyTests::AgentProfileArgumentValidation()
    {
        namespace Agents = ::Microsoft::Terminal::AgentProfiles;
        for (const auto arguments : { L"--model other", L"--config approval_policy=never", L"-capproval_policy=never",
                                     L"--permission-mode=auto", L"--allow-all", L"--acp", L"login",
                                     L"--resume", L"--add-dir --model", L"--version=other" })
        {
            VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.exe", L"claude", {}, {}, arguments), wil::ResultException);
        }
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.exe", L"claude", L"--allow-all", {}, {}), wil::ResultException);
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.exe", L"claude", {}, L"invented-mode", {}), wil::ResultException);
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.cmd", L"claude", L"%PATH%", {}, {}), wil::ResultException);
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.exe", L"claude", L"%PATH%", {}, {}), wil::ResultException);
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.exe", L"claude", {}, {}, L"--add-dir %USERPROFILE%"), wil::ResultException);
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\claude.cmd", L"claude", L"embedded\"quote", {}, {}), wil::ResultException);
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\agent.exe", L"custom:claude", {}, {}, {}), wil::ResultException);
        const auto antigravity = Agents::ParseArguments(Agents::BuildCommand(
            L"C:\\agy.exe", L"antigravity", L"chosen-model", L"plan",
            LR"(--conversation "session id" --add-dir "C:\source tree" --prompt-interactive "Explain this project" --continue)"));
        VERIFY_ARE_EQUAL(size_t{ 12 }, antigravity.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"--mode" }, antigravity[3]);
        VERIFY_ARE_EQUAL(std::wstring{ L"plan" }, antigravity[4]);
        VERIFY_ARE_EQUAL(std::wstring{ L"session id" }, antigravity[6]);
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\source tree" }, antigravity[8]);
        VERIFY_ARE_EQUAL(std::wstring{ L"Explain this project" }, antigravity[10]);
        for (const auto arguments : { L"--conversation", L"--conversation=--model", L"--resume session",
                                     L"--mode plan", L"--dangerously-skip-permissions", L"--print prompt",
                                     L"--model other", L"--add-dir --model" })
        {
            VERIFY_THROWS(Agents::BuildCommand(L"C:\\agy.exe", L"antigravity", {}, {}, arguments), wil::ResultException);
        }
        VERIFY_THROWS(Agents::BuildCommand(L"C:\\agy.exe", L"antigravity", {}, L"yolo", {}), wil::ResultException);
    }

    void CustomAgentAndPolicyTests::AgentProfileDiscoveryLifecycle()
    {
        namespace Agents = ::Microsoft::Terminal::AgentProfiles;
        GUID guid;
        THROW_IF_FAILED(CoCreateGuid(&guid));
        const auto root = std::filesystem::current_path() / (L"Native Agent " + Utils::GuidToString(guid));
        const auto early = root / L"early";
        const auto late = root / L"late";
        std::filesystem::create_directories(early);
        std::filesystem::create_directory(late);
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        const auto shim = early / L"claude.cmd";
        const auto exe = late / L"claude.exe";
        std::filesystem::create_directory(early / L"claude.exe");
        std::ofstream{ shim }.put('\n');
        std::ofstream{ exe }.put('\n');
        std::ofstream{ early / L"antigravity.exe" }.put('\n');
        std::ofstream{ early / L"agy.exe" }.put('\n');
        const auto path = early.native() + L";" + late.native();
        VERIFY_ARE_EQUAL((early / L"agy.exe").native(), Agents::Discover(path).at(L"antigravity").native());
        std::filesystem::remove(early / L"agy.exe");
        std::ofstream{ early / L"agy.cmd" }.put('\n');
        std::ofstream{ early / L"agy_acp_server.exe" }.put('\n');
        std::ofstream{ early / L"localharness_external.exe" }.put('\n');
        VERIFY_IS_TRUE(Agents::ResolveExecutable(L"antigravity", path).empty());
        VERIFY_ARE_EQUAL(exe.native(), Agents::Discover(path).at(L"claude").native());
        std::filesystem::remove(exe);
        VERIFY_ARE_EQUAL(shim.native(), Agents::Discover(path).at(L"claude").native());
        std::filesystem::remove(shim);
        VERIFY_IS_TRUE(Agents::Discover(path).empty());
        std::ofstream{ shim }.put('\n');
        VERIFY_ARE_EQUAL(shim.native(), Agents::Discover(path).at(L"claude").native());
        VERIFY_IS_TRUE(Agents::Discover(L"").empty());
        std::ofstream{ late / L"agy.exe" }.put('\n');
        VERIFY_ARE_EQUAL((late / L"agy.exe").native(), Agents::Discover(path).at(L"antigravity").native());
    }

    void CustomAgentAndPolicyTests::AgentProfileDiscoveryExecutionAliases()
    {
        namespace Agents = ::Microsoft::Terminal::AgentProfiles;
        GUID guid;
        THROW_IF_FAILED(CoCreateGuid(&guid));
        const auto root = std::filesystem::current_path() / (L"Native Agent Alias " + Utils::GuidToString(guid));
        const auto alias = root / L"copilot.exe";
        std::filesystem::create_directory(root);
        const auto cleanup = wil::scope_exit([&]() {
            LOG_IF_WIN32_BOOL_FALSE(DeleteFileW(alias.c_str()));
            std::filesystem::remove_all(root);
        });
        const auto target = root / L"real-copilot.exe";
        std::ofstream{ target }.put('\n');
        std::ofstream{ root / L"claude.exe" }.put('\n');

        // Reproduce an app execution alias without registering a package or requiring symlink privileges.
        std::wstring strings;
        for (const auto& value : { std::wstring{ L"NativeAgentFixture" }, std::wstring{ L"NativeAgentFixture!App" }, target.native(), std::wstring{ L"0" } })
        {
            strings.append(value);
            strings.push_back(L'\0');
        }
        struct AliasHeader
        {
            DWORD reparseTag;
            USHORT dataLength;
            USHORT reserved;
            DWORD version;
        };
        const AliasHeader header{ IO_REPARSE_TAG_APPEXECLINK, gsl::narrow<USHORT>(sizeof(DWORD) + strings.size() * sizeof(wchar_t)), 0, 3 };
        std::vector<std::byte> buffer(sizeof(header) + strings.size() * sizeof(wchar_t));
        std::memcpy(buffer.data(), &header, sizeof(header));
        std::memcpy(buffer.data() + sizeof(header), strings.data(), strings.size() * sizeof(wchar_t));
        wil::unique_hfile file{ CreateFileW(alias.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW, FILE_FLAG_OPEN_REPARSE_POINT, nullptr) };
        THROW_LAST_ERROR_IF(!file);
        DWORD returned;
        THROW_IF_WIN32_BOOL_FALSE(DeviceIoControl(file.get(), FSCTL_SET_REPARSE_POINT, buffer.data(), gsl::narrow<DWORD>(buffer.size()), nullptr, 0, &returned, nullptr));
        file.reset();

        const auto discovered = Agents::Discover(L"\"" + root.native() + L"\"");
        VERIFY_ARE_EQUAL(size_t{ 2 }, discovered.size());
        VERIFY_ARE_EQUAL(alias.native(), discovered.at(L"copilot").native());
        VERIFY_ARE_EQUAL((root / L"claude.exe").native(), discovered.at(L"claude").native());
        auto profile = winrt::make_self<implementation::Profile>();
        profile->AgentProfileId(L"copilot");
        VERIFY_ARE_EQUAL(Agents::BuildCommand(alias.native(), L"copilot", {}, {}, {}), Agents::Command(*profile, true, root.native()));
    }

    void CustomAgentAndPolicyTests::AgentProfileDiscoveryCacheLifecycle()
    {
        using Cache = AgentProfileGenerator::DiscoveryCache;
        Cache cache;
        const auto start = std::chrono::steady_clock::time_point{};
        size_t probes = 0;
        bool fail = false;
        const auto probe = [&]() -> Cache::Snapshot {
            ++probes;
            THROW_HR_IF(E_FAIL, fail);
            return { { L"claude", probes == 1 ? L"C:\\first.exe" : L"C:\\refreshed.exe" } };
        };
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\first.exe" }, cache.Get(start, probe).at(L"claude").native());
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\first.exe" }, cache.Get(start + std::chrono::seconds{ 29 }, probe).at(L"claude").native());
        VERIFY_ARE_EQUAL(size_t{ 1 }, probes);
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\refreshed.exe" }, cache.Get(start + std::chrono::seconds{ 30 }, probe).at(L"claude").native());
        VERIFY_ARE_EQUAL(size_t{ 2 }, probes);
        fail = true;
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\refreshed.exe" }, cache.Get(start + std::chrono::seconds{ 60 }, probe).at(L"claude").native());
        VERIFY_ARE_EQUAL(size_t{ 3 }, probes);
        fail = false;
        cache.Get(start + std::chrono::seconds{ 61 }, probe);
        VERIFY_ARE_EQUAL(size_t{ 4 }, probes);
    }

    void CustomAgentAndPolicyTests::AgentProfileDiscoveryCacheFirstFailure()
    {
        using Cache = AgentProfileGenerator::DiscoveryCache;
        Cache cache;
        const auto start = std::chrono::steady_clock::time_point{};
        size_t probes = 0;
        const auto fail = [&]() -> Cache::Snapshot {
            ++probes;
            THROW_HR(E_FAIL);
        };
        VERIFY_THROWS(cache.Get(start, fail), wil::ResultException);
        const auto succeed = [&]() -> Cache::Snapshot {
            ++probes;
            return {};
        };
        VERIFY_IS_TRUE(cache.Get(start, succeed).empty());
        VERIFY_IS_TRUE(cache.Get(start + std::chrono::seconds{ 29 }, fail).empty());
        VERIFY_ARE_EQUAL(size_t{ 2 }, probes);
    }

    void CustomAgentAndPolicyTests::AgentProfileNativeAllowlist()
    {
        namespace Registry = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        VERIFY_ARE_EQUAL(size_t{ 6 }, Registry::BuiltinNativeProfileAgents.size());
        VERIFY_IS_TRUE(Registry::CanonicalNativeAgentProviderId(L"antigravity") == L"antigravity");
        VERIFY_IS_TRUE(Registry::CanonicalNativeAgentProviderId(L"ANTIGRAVITY") == L"antigravity");
        VERIFY_IS_TRUE(Registry::SupportsNativeProfile(L"antigravity"));
        VERIFY_IS_TRUE(Registry::CliExecutable(L"antigravity") == L"agy");
        VERIFY_IS_TRUE(std::ranges::any_of(Registry::BuiltinAcpAgents, [](const auto& agent) { return agent.id == L"antigravity"; }));
        VERIFY_IS_TRUE(std::ranges::any_of(Registry::BuiltinDelegateAgents, [](const auto& agent) { return agent.id == L"antigravity"; }));
        VERIFY_ARE_EQUAL(std::wstring{ L"\"C:\\agy.exe\"" },
                         ::Microsoft::Terminal::AgentProfiles::BuildCommand(L"C:\\agy.exe", L"antigravity", {}, {}, {}));
        auto profile = winrt::make_self<implementation::Profile>();
        profile->AgentProfileId(L"antigravity");
        VERIFY_IS_TRUE(::Microsoft::Terminal::AgentProfiles::IsManaged(*profile));
        VERIFY_ARE_EQUAL(std::wstring{ L"\"agy.exe\"" }, ::Microsoft::Terminal::AgentProfiles::Command(*profile, false, L""));
        VERIFY_THROWS(::Microsoft::Terminal::AgentProfiles::Command(*profile, true, L""), wil::ResultException);
        AgentPolicy::PolicySnapshot policy;
        VERIFY_IS_TRUE(Registry::IsNativeAgentProviderAllowed(L"antigravity", policy));
        ::Microsoft::Terminal::AgentProfiles::CheckLaunchPolicy(L"antigravity", policy);
        policy.allowedAgents.emplace();
        VERIFY_IS_FALSE(Registry::IsNativeAgentProviderAllowed(L"antigravity", policy));
        VERIFY_THROWS(::Microsoft::Terminal::AgentProfiles::CheckLaunchPolicy(L"antigravity", policy), wil::ResultException);
        policy.allowedAgents->insert(L"antigravity");
        VERIFY_IS_TRUE(Registry::IsNativeAgentProviderAllowed(L"ANTIGRAVITY", policy));
        ::Microsoft::Terminal::AgentProfiles::CheckLaunchPolicy(L"antigravity", policy);
        policy.yoloMode = AgentPolicy::PolicyState::Blocked;
        VERIFY_THROWS(::Microsoft::Terminal::AgentProfiles::CheckLaunchPolicy(L"antigravity", policy), wil::ResultException);
    }

    void CustomAgentAndPolicyTests::AgentProfileUninstallPreservesOverrides()
    {
        static constexpr std::string_view user{ R"({
            "defaultProfile":"{00000000-0000-0000-0000-000000000001}",
            "profiles":[
                {"guid":"{00000000-0000-0000-0000-000000000001}","name":"Shell","commandline":"cmd.exe"},
                {"guid":"{00000000-0000-0000-0000-000000000002}","name":"My Claude",
                 "source":"IntelligentTerminal.AgentProfiles","agentProfile.model":"chosen-model",
                 "startingDirectory":"C:\\Project"}]})" };
        static constexpr std::string_view generated{ R"({"profiles":[
            {"guid":"{00000000-0000-0000-0000-000000000002}","name":"Claude",
             "source":"IntelligentTerminal.AgentProfiles","agentProfile.id":"claude",
             "commandline":"C:\\Native\\claude.exe"}]})" };
        const winrt::guid agentGuid{ L"{00000000-0000-0000-0000-000000000002}" };
        const auto installed = winrt::make_self<implementation::CascadiaSettings>(user, generated);
        VERIFY_ARE_EQUAL(2u, installed->ActiveProfiles().Size());
        VERIFY_IS_FALSE(installed->FindProfile(agentGuid).Orphaned());
        const auto removed = winrt::make_self<implementation::CascadiaSettings>(user, std::string_view{ R"({"profiles":[]})" });
        const auto orphan = removed->FindProfile(agentGuid);
        VERIFY_IS_TRUE(orphan.Orphaned());
        VERIFY_ARE_EQUAL(1u, removed->ActiveProfiles().Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"chosen-model" }, orphan.AgentProfileModel());
        const auto serialized = removed->ToJson();
        VERIFY_ARE_EQUAL(std::string{ "chosen-model" }, serialized["profiles"]["list"][1]["agentProfile.model"].asString());
        const auto restored = winrt::make_self<implementation::CascadiaSettings>(toString(serialized), generated);
        const auto profile = restored->FindProfile(agentGuid);
        VERIFY_IS_FALSE(profile.Orphaned());
        VERIFY_ARE_EQUAL(2u, restored->ActiveProfiles().Size());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"chosen-model" }, profile.AgentProfileModel());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"C:\\Project" }, profile.StartingDirectory());
        VERIFY_ARE_EQUAL(winrt::hstring{ L"My Claude" }, profile.Name());
    }

    void CustomAgentAndPolicyTests::AgentProfileLaunchPolicy()
    {
        namespace Agents = ::Microsoft::Terminal::AgentProfiles;
        AgentPolicy::PolicySnapshot policy;
        Agents::CheckLaunchPolicy(L"claude", policy);
        policy.allowedAgents.emplace();
        VERIFY_THROWS(Agents::CheckLaunchPolicy(L"claude", policy), wil::ResultException);
        policy.allowedAgents->insert(L"claude");
        Agents::CheckLaunchPolicy(L"claude", policy);
        policy.yoloMode = AgentPolicy::PolicyState::Blocked;
        VERIFY_THROWS(Agents::CheckLaunchPolicy(L"claude", policy), wil::ResultException);
    }

    void CustomAgentAndPolicyTests::AgentProfileBatchCommand()
    {
        namespace Agents = ::Microsoft::Terminal::AgentProfiles;
        GUID guid;
        THROW_IF_FAILED(CoCreateGuid(&guid));
        const auto root = std::filesystem::temp_directory_path() / (L"Native Agent " + Utils::GuidToString(guid));
        std::filesystem::create_directory(root);
        const auto cleanup = wil::scope_exit([&]() { std::filesystem::remove_all(root); });
        const auto shim = root / L"claude.cmd";
        std::ofstream{ shim } << "@echo off\r\n> \"%~dp0result.txt\" echo %*\r\nexit /b 37\r\n";
        auto command = Agents::BuildCommand(shim.native(), L"claude", L"model with spaces", L"plan", LR"(--add-dir "C:\source & (tree)\\")");
        STARTUPINFOW startup{ sizeof(startup) };
        wil::unique_process_information process;
        THROW_IF_WIN32_BOOL_FALSE(CreateProcessW(nullptr, command.data(), nullptr, nullptr, false, CREATE_NO_WINDOW,
                                                nullptr, root.c_str(), &startup, &process));
        const auto stop = wil::scope_exit([&]() {
            if (WaitForSingleObject(process.hProcess, 0) == WAIT_TIMEOUT)
            {
                LOG_IF_WIN32_BOOL_FALSE(TerminateProcess(process.hProcess, 1));
                WaitForSingleObject(process.hProcess, 5000);
            }
        });
        VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(process.hProcess, 10000));
        DWORD code;
        THROW_IF_WIN32_BOOL_FALSE(GetExitCodeProcess(process.hProcess, &code));
        VERIFY_ARE_EQUAL(DWORD{ 37 }, code);
        std::ifstream output{ root / L"result.txt" };
        std::string text;
        std::getline(output, text);
        if (!text.empty() && text.back() == '\r')
        {
            text.pop_back();
        }
        const auto args = Agents::ParseArguments(winrt::to_hstring(text));
        VERIFY_ARE_EQUAL(size_t{ 6 }, args.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"--model" }, args[0]);
        VERIFY_ARE_EQUAL(std::wstring{ L"model with spaces" }, args[1]);
        VERIFY_ARE_EQUAL(std::wstring{ L"--permission-mode" }, args[2]);
        VERIFY_ARE_EQUAL(std::wstring{ L"plan" }, args[3]);
        VERIFY_ARE_EQUAL(std::wstring{ L"--add-dir" }, args[4]);
        VERIFY_ARE_EQUAL(std::wstring{ LR"(C:\source & (tree)\)" }, args[5]);
    }

    void CustomAgentAndPolicyTests::AgentProfileSplitTargets()
    {
        Json::Value action{ Json::objectValue };
        action["splitMode"] = "profile";
        const auto [parsed, warnings] = implementation::SplitPaneArgs::FromJson(action);
        VERIFY_IS_TRUE(warnings.empty());
        VERIFY_ARE_EQUAL(SplitType::Profile, parsed.as<SplitPaneArgs>().SplitMode());
        VERIFY_ARE_EQUAL(std::string{ "profile" }, implementation::SplitPaneArgs::ToJson(parsed)["splitMode"].asString());

        const auto settings = winrt::make<implementation::CascadiaSettings>(std::string_view{
            R"({"defaultProfile":"{00000000-0000-0000-0000-000000000001}","profiles":[
              {"guid":"{00000000-0000-0000-0000-000000000001}","name":"Shell","commandline":"cmd.exe"},
              {"guid":"{00000000-0000-0000-0000-000000000002}","name":"Agent","commandline":"cmd.exe"}]})" });
        auto source = settings.ActiveProfiles().GetAt(1);
        const auto shell = settings.ActiveProfiles().GetAt(0);
        namespace Splits = ::Microsoft::Terminal::ProfileSplits;
        VERIFY_IS_TRUE(Splits::Resolve(settings, source, nullptr).duplicate);
        source.DefaultSplitProfile(L"default");
        VERIFY_ARE_EQUAL(shell.Guid(), Splits::Resolve(settings, source, nullptr).target.Guid());
        source.DefaultSplitProfile(L"{00000000-0000-0000-0000-000000000001}");
        VERIFY_ARE_EQUAL(shell.Guid(), Splits::Resolve(settings, source, nullptr).target.Guid());
        source.DefaultSplitProfile(L"invalid-guid");
        const auto missing = Splits::Resolve(settings, source, nullptr);
        VERIFY_IS_TRUE(missing.unavailable);
        VERIFY_ARE_EQUAL(shell.Guid(), missing.target.Guid());
        NewTerminalArgs explicitArgs{};
        explicitArgs.Profile(L"Agent");
        const auto explicitDecision = Splits::Resolve(settings, source, explicitArgs);
        VERIFY_IS_FALSE(explicitDecision.duplicate);
        VERIFY_IS_FALSE(explicitDecision.unavailable);
        VERIFY_IS_NULL(explicitDecision.target);
        explicitArgs.Profile(L"");
        explicitArgs.Commandline(L"cmd.exe");
        VERIFY_IS_NULL(Splits::Resolve(settings, source, explicitArgs).target);
        explicitArgs.Commandline(L"");
        explicitArgs = NewTerminalArgs{ 0 };
        VERIFY_IS_NULL(Splits::Resolve(settings, source, explicitArgs).target);
        VERIFY_IS_NULL(Splits::Resolve(settings, source, BaseContentArgs{ L"scratchpad" }).target);
        source.DefaultSplitProfile(L"{00000000-0000-0000-0000-000000000002}");
        source.Hidden(true);
        VERIFY_IS_TRUE(Splits::Resolve(settings, source, nullptr).unavailable);
    }

    void CustomAgentAndPolicyTests::NativeAgentCreationPolicyUsesCurrentSnapshot()
    {
        _NativeAgentCreationPolicyUsesCurrentSnapshot();
    }

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

    void CustomAgentAndPolicyTests::ShowTokenUsageAndCostRoundtripsAndDefaultsOn()
    {
        const auto enabled = MakeSettings(R"("showTokenUsageAndCost": true)");
        VERIFY_IS_TRUE(enabled->GlobalSettings().ShowTokenUsageAndCost());

        const auto disabled = MakeSettings(R"("showTokenUsageAndCost": false)");
        VERIFY_IS_FALSE(disabled->GlobalSettings().ShowTokenUsageAndCost());

        const auto defaulted = MakeSettings({});
        VERIFY_IS_TRUE(defaulted->GlobalSettings().ShowTokenUsageAndCost());
    }

    void CustomAgentAndPolicyTests::AgentSessionManagementRoundtripsDefaultsOnAndHonorsPolicy()
    {
        const auto disabled = MakeSettings(R"("agentSessionManagementEnabled": false)");
        SetPolicy(MakePolicy());
        VERIFY_IS_FALSE(disabled->GlobalSettings().AgentSessionManagementEnabled());
        VERIFY_IS_FALSE(disabled->GlobalSettings().EffectiveAgentSessionManagementEnabled());

        const auto defaulted = MakeSettings({});
        SetPolicy(MakePolicy());
        VERIFY_IS_TRUE(defaulted->GlobalSettings().AgentSessionManagementEnabled());
        VERIFY_IS_TRUE(defaulted->GlobalSettings().EffectiveAgentSessionManagementEnabled());

        const auto blocked = MakeSettings(R"("agentSessionManagementEnabled": true)");
        SetPolicy(MakePolicy(
            std::nullopt,
            AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_TRUE(blocked->GlobalSettings().AgentSessionManagementEnabled());
        VERIFY_IS_FALSE(blocked->GlobalSettings().EffectiveAgentSessionManagementEnabled());
    }

    void CustomAgentAndPolicyTests::AutoErrorSettingsRoundtrip()
    {
        const auto enabled = MakeSettings(R"("autoErrorDetectionEnabled": true, "autoFixEnabled": true)");
        VERIFY_IS_TRUE(enabled->GlobalSettings().AutoErrorDetectionEnabled());
        VERIFY_IS_TRUE(enabled->GlobalSettings().AutoFixEnabled());

        const auto detectionOnly = MakeSettings(R"("autoErrorDetectionEnabled": true, "autoFixEnabled": false)");
        VERIFY_IS_TRUE(detectionOnly->GlobalSettings().AutoErrorDetectionEnabled());
        VERIFY_IS_FALSE(detectionOnly->GlobalSettings().AutoFixEnabled());

        const auto off = MakeSettings(R"("autoErrorDetectionEnabled": false, "autoFixEnabled": false)");
        VERIFY_IS_FALSE(off->GlobalSettings().AutoErrorDetectionEnabled());
        VERIFY_IS_FALSE(off->GlobalSettings().AutoFixEnabled());

        const auto defaulted = MakeSettings({});
        VERIFY_IS_TRUE(defaulted->GlobalSettings().AutoErrorDetectionEnabled());
        VERIFY_IS_FALSE(defaulted->GlobalSettings().AutoFixEnabled());
    }

    void CustomAgentAndPolicyTests::EffectiveAutoFixFalseWhenDetectionOff()
    {
        const auto detectionOff = MakeSettings(
            R"("autoErrorDetectionEnabled": false, "autoFixEnabled": true)");
        SetPolicy(MakePolicy());
        VERIFY_IS_FALSE(detectionOff->GlobalSettings().EffectiveAutoFixEnabled());

        const auto bothOn = MakeSettings(
            R"("autoErrorDetectionEnabled": true, "autoFixEnabled": true)");
        SetPolicy(MakePolicy());
        VERIFY_IS_TRUE(bothOn->GlobalSettings().EffectiveAutoFixEnabled());
    }

    void CustomAgentAndPolicyTests::AutoFixPolicyPreservesErrorDetection()
    {
        const auto settings = MakeSettings(
            R"("autoErrorDetectionEnabled": true, "autoFixEnabled": true)");
        SetPolicy(MakePolicy(
            std::nullopt,
            AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState::Blocked));

        VERIFY_IS_TRUE(settings->GlobalSettings().EffectiveAutoErrorDetectionEnabled());
        VERIFY_IS_FALSE(settings->GlobalSettings().EffectiveAutoFixEnabled());
        VERIFY_IS_TRUE(settings->GlobalSettings().IsAutoFixPolicyLocked());
    }

    void CustomAgentAndPolicyTests::AgentPaneYoloModeRoundtripsAndDefaults()
    {
        // Explicit value survives load, and the underlying setting is
        // readable independent of policy (policy NotConfigured → allowed).
        const auto settings = MakeSettings(R"("agentPane.yoloMode": true)");
        SetPolicy(MakePolicy());
        VERIFY_IS_TRUE(settings->GlobalSettings().AgentPaneYoloMode());
        VERIFY_IS_TRUE(settings->GlobalSettings().EffectiveAgentPaneYoloMode());

        // Absent → falls back to the `false` default (MTSMSettings.h).
        const auto defaulted = MakeSettings({});
        SetPolicy(MakePolicy());
        VERIFY_IS_FALSE(defaulted->GlobalSettings().AgentPaneYoloMode());
        VERIFY_IS_FALSE(defaulted->GlobalSettings().EffectiveAgentPaneYoloMode());
    }

    void CustomAgentAndPolicyTests::EffectiveAgentPaneYoloModeFalseForOpenCode()
    {
        const auto openCode = MakeSettings(
            R"("acpAgent": "opencode", "agentPane.yoloMode": true)");
        SetPolicy(MakePolicy());
        VERIFY_IS_TRUE(openCode->GlobalSettings().AgentPaneYoloMode());
        VERIFY_IS_FALSE(openCode->GlobalSettings().EffectiveAgentPaneYoloMode());

        const auto custom = MakeSettings(
            R"("acpAgent": "custom:local", "agentPane.yoloMode": true)");
        VERIFY_IS_TRUE(custom->GlobalSettings().EffectiveAgentPaneYoloMode());
    }

    void CustomAgentAndPolicyTests::EffectiveAgentPaneYoloModeFalseWhenDefaultAgentBlocked()
    {
        const auto builtIn = MakeSettings(
            R"("acpAgent": "copilot", "agentPane.yoloMode": true)");
        SetPolicy(MakePolicy(
            std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"gemini" }));
        VERIFY_ARE_EQUAL(winrt::hstring{}, builtIn->GlobalSettings().EffectiveAcpAgent());
        VERIFY_IS_TRUE(builtIn->GlobalSettings().AgentPaneYoloMode());
        VERIFY_IS_FALSE(builtIn->GlobalSettings().EffectiveAgentPaneYoloMode());

        const auto custom = MakeSettings(
            R"("acpAgent": "custom:local", "agentPane.yoloMode": true)");
        SetPolicy(MakePolicy(
            /*allowedAgents*/ std::nullopt,
            AgentPolicy::PolicyState::Blocked));
        VERIFY_ARE_EQUAL(winrt::hstring{}, custom->GlobalSettings().EffectiveAcpAgent());
        VERIFY_IS_TRUE(custom->GlobalSettings().AgentPaneYoloMode());
        VERIFY_IS_FALSE(custom->GlobalSettings().EffectiveAgentPaneYoloMode());
    }

    void CustomAgentAndPolicyTests::CanEnableAgentPaneYoloModeTracksPolicyAndProvider()
    {
        const auto settings = MakeSettings(R"("acpAgent": "copilot")");
        SetPolicy(MakePolicy());
        const auto globals = settings->GlobalSettings();

        VERIFY_IS_TRUE(globals.CanEnableAgentPaneYoloMode());
        VERIFY_IS_TRUE(globals.CanEnableAgentPaneYoloModeForAgent(L"gemini"));
        VERIFY_IS_TRUE(globals.CanEnableAgentPaneYoloModeForAgent(L"custom:local"));
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloModeForAgent(L"opencode"));
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloModeForAgent(L""));

        SetPolicy(MakePolicy(
            /*allowedAgents*/ std::nullopt,
            AgentPolicy::PolicyState::NotConfigured,
            AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloMode());
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloModeForAgent(L"gemini"));

        SetPolicy(MakePolicy(
            std::set<std::wstring, AgentPolicy::CaseInsensitiveLess>{ L"gemini" }));
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloMode());
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloModeForAgent(L"copilot"));

        SetPolicy(MakePolicy(
            /*allowedAgents*/ std::nullopt,
            AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_FALSE(globals.CanEnableAgentPaneYoloModeForAgent(L"custom:local"));
    }

    void CustomAgentAndPolicyTests::OpenCodeDefaultClearsStoredAgentPaneYoloMode()
    {
        const auto openCode = MakeSettings(
            R"("acpAgent": "opencode", "agentPane.yoloMode": true)");
        const auto openCodeGlobals = openCode->GlobalSettings();
        const auto openCodeImpl = winrt::get_self<implementation::GlobalAppSettings>(openCodeGlobals);
        VERIFY_IS_TRUE(openCodeImpl->ClearAgentPaneYoloModeIfUnavailableDefault());
        VERIFY_IS_FALSE(openCodeGlobals.AgentPaneYoloMode());
        VERIFY_IS_FALSE(openCodeImpl->ClearAgentPaneYoloModeIfUnavailableDefault());

        const auto custom = MakeSettings(
            R"("acpAgent": "custom:opencode", "agentPane.yoloMode": true)");
        const auto customGlobals = custom->GlobalSettings();
        const auto customImpl = winrt::get_self<implementation::GlobalAppSettings>(customGlobals);
        VERIFY_IS_FALSE(customImpl->ClearAgentPaneYoloModeIfUnavailableDefault());
        VERIFY_IS_TRUE(customGlobals.AgentPaneYoloMode());
    }

    void CustomAgentAndPolicyTests::EffectiveAgentPaneYoloModeFalseWhenPolicyBlocked()
    {
        // The AllowAutomaticApproval admin policy overrides the user's toggle: even
        // with the setting on, a Blocked policy must force the effective
        // value to false, matching the AutoFix policy-gate pattern above.
        const auto settings = MakeSettings(R"("agentPane.yoloMode": true)");
        SetPolicy(MakePolicy(/*allowedAgents*/ std::nullopt,
                              /*customAgents*/ AgentPolicy::PolicyState::NotConfigured,
                              /*yoloMode*/ AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_FALSE(settings->GlobalSettings().EffectiveAgentPaneYoloMode());
        // The raw (non-effective) setting still reflects the user's choice —
        // only the effective/gated accessor is policy-clamped.
        VERIFY_IS_TRUE(settings->GlobalSettings().AgentPaneYoloMode());
    }

    void CustomAgentAndPolicyTests::PolicyBlockClearsStoredAgentPaneYoloMode()
    {
        const auto settings = MakeSettings(R"("agentPane.yoloMode": true)");
        SetPolicy(MakePolicy(/*allowedAgents*/ std::nullopt,
                              /*customAgents*/ AgentPolicy::PolicyState::NotConfigured,
                              /*yoloMode*/ AgentPolicy::PolicyState::Blocked));

        const auto globals = settings->GlobalSettings();
        const auto impl = winrt::get_self<implementation::GlobalAppSettings>(globals);
        VERIFY_IS_TRUE(impl->ClearAgentPaneYoloModeIfPolicyBlocked());
        VERIFY_IS_FALSE(globals.AgentPaneYoloMode());
        VERIFY_IS_FALSE(impl->ClearAgentPaneYoloModeIfPolicyBlocked());
    }

    void CustomAgentAndPolicyTests::YoloSettingsNoticeTracksSelectedProviderAndPreference()
    {
        using namespace ::Microsoft::Terminal::Settings::Model::AgentRegistry;

        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"opencode", true, false, true));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::Conditional,
                         GetYoloSettingsNotice(L"gemini", true, false, true));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"gemini", false, false, true));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"copilot", true, false, true));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"opencode", false, false, true));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"opencode", true, true, true));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"opencode", true, false, false));
        VERIFY_ARE_EQUAL(YoloSettingsNotice::None,
                         GetYoloSettingsNotice(L"custom:opencode", true, false, true));
    }

    void CustomAgentAndPolicyTests::IsYoloModePolicyLockedTracksBlocked()
    {
        const auto settings = MakeSettings({});

        SetPolicy(MakePolicy(std::nullopt, AgentPolicy::PolicyState::NotConfigured, AgentPolicy::PolicyState::NotConfigured));
        VERIFY_IS_FALSE(settings->GlobalSettings().IsYoloModePolicyLocked());

        SetPolicy(MakePolicy(std::nullopt, AgentPolicy::PolicyState::NotConfigured, AgentPolicy::PolicyState::Allowed));
        VERIFY_IS_FALSE(settings->GlobalSettings().IsYoloModePolicyLocked());

        SetPolicy(MakePolicy(std::nullopt, AgentPolicy::PolicyState::NotConfigured, AgentPolicy::PolicyState::Blocked));
        VERIFY_IS_TRUE(settings->GlobalSettings().IsYoloModePolicyLocked());
    }

    void CustomAgentAndPolicyTests::PolicyChangeWatcherObservesRecursiveMutation()
    {
        const auto keyPath = L"Software\\IntelligentTerminal\\Tests\\PolicyWatcher-" +
                             std::to_wstring(GetCurrentProcessId()) + L"-" +
                             std::to_wstring(GetTickCount64());
        const auto cleanup = wil::scope_exit([&] {
            RegDeleteTreeW(HKEY_CURRENT_USER, keyPath.c_str());
        });

        wil::unique_hkey key;
        VERIFY_ARE_EQUAL(
            static_cast<LSTATUS>(ERROR_SUCCESS),
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                keyPath.c_str(),
                0,
                nullptr,
                0,
                KEY_NOTIFY | KEY_CREATE_SUB_KEY,
                nullptr,
                &key,
                nullptr));
        wil::unique_event changed;
        changed.create();
        std::atomic_uint32_t callbackCount{ 0 };
        auto watcher = AgentPolicy::details::CreatePolicyChangeWatcher(
            std::move(key),
            [&](wil::RegistryChangeKind) {
                callbackCount.fetch_add(1, std::memory_order_relaxed);
                changed.SetEvent();
            });
        VERIFY_IS_TRUE(static_cast<bool>(watcher));

        wil::unique_hkey policyKey;
        const auto policyPath = keyPath + L"\\IntelligentTerminal";
        VERIFY_ARE_EQUAL(
            static_cast<LSTATUS>(ERROR_SUCCESS),
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                policyPath.c_str(),
                0,
                nullptr,
                0,
                KEY_SET_VALUE,
                nullptr,
                &policyKey,
                nullptr));
        constexpr DWORD blocked = 0;
        VERIFY_ARE_EQUAL(
            static_cast<LSTATUS>(ERROR_SUCCESS),
            RegSetValueExW(
                policyKey.get(),
                L"AllowAutomaticApproval",
                0,
                REG_DWORD,
                reinterpret_cast<const BYTE*>(&blocked),
                sizeof(blocked)));

        VERIFY_ARE_EQUAL(WAIT_OBJECT_0, WaitForSingleObject(changed.get(), 5000));
        VERIFY_IS_GREATER_THAN_OR_EQUAL(callbackCount.load(std::memory_order_relaxed), 1u);
    }

    void CustomAgentAndPolicyTests::PolicyChangeWatcherTargetsProductKey()
    {
        VERIFY_ARE_EQUAL(
            std::wstring_view{ AgentPolicy::PolicyRegKey },
            std::wstring_view{ AgentPolicy::PolicyWatchRegKey });
    }

    void CustomAgentAndPolicyTests::PolicyChangeWatcherTracksPolicyCreatedFromMissingAncestors()
    {
        const auto keyPath = L"Software\\IntelligentTerminal\\Tests\\PolicyWatcherAncestors-" +
                             std::to_wstring(GetCurrentProcessId()) + L"-" +
                             std::to_wstring(GetTickCount64());
        const auto cleanup = wil::scope_exit([&] {
            RegDeleteTreeW(HKEY_CURRENT_USER, keyPath.c_str());
        });

        wil::unique_hkey root;
        VERIFY_ARE_EQUAL(
            static_cast<LSTATUS>(ERROR_SUCCESS),
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                keyPath.c_str(),
                0,
                nullptr,
                0,
                KEY_NOTIFY | KEY_CREATE_SUB_KEY,
                nullptr,
                &root,
                nullptr));
        wil::unique_event changed;
        changed.create();
        wil::unique_registry_watcher_nothrow watcher;
        const auto arm = [&] {
            watcher = AgentPolicy::details::CreatePolicyChangeWatcher(
                root.get(),
                L"Policies\\Microsoft",
                [&](wil::RegistryChangeKind) {
                    changed.SetEvent();
                });
            VERIFY_IS_TRUE(static_cast<bool>(watcher));
        };
        const auto createAndObserve = [&](const wchar_t* relativePath) {
            VERIFY_WIN32_BOOL_SUCCEEDED(ResetEvent(changed.get()));
            arm();
            wil::unique_hkey created;
            VERIFY_ARE_EQUAL(
                static_cast<LSTATUS>(ERROR_SUCCESS),
                RegCreateKeyExW(
                    root.get(),
                    relativePath,
                    0,
                    nullptr,
                    0,
                    KEY_NOTIFY | KEY_CREATE_SUB_KEY | KEY_SET_VALUE,
                    nullptr,
                    &created,
                    nullptr));
            VERIFY_ARE_EQUAL(WAIT_OBJECT_0, WaitForSingleObject(changed.get(), 5000));
            return created;
        };

        createAndObserve(L"Policies");
        createAndObserve(L"Policies\\Microsoft");
        auto policyKey = createAndObserve(L"Policies\\Microsoft\\IntelligentTerminal");

        VERIFY_WIN32_BOOL_SUCCEEDED(ResetEvent(changed.get()));
        arm();
        constexpr DWORD blocked = 0;
        VERIFY_ARE_EQUAL(
            static_cast<LSTATUS>(ERROR_SUCCESS),
            RegSetValueExW(
                policyKey.get(),
                L"AllowAutomaticApproval",
                0,
                REG_DWORD,
                reinterpret_cast<const BYTE*>(&blocked),
                sizeof(blocked)));
        VERIFY_ARE_EQUAL(WAIT_OBJECT_0, WaitForSingleObject(changed.get(), 5000));
    }
}
