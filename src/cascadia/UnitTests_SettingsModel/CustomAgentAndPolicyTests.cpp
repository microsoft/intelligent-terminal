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
#include "../inc/AgentPolicy.h"
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
        TEST_METHOD(AgentPanePositionRoundtripsAndDefaults);
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

    void CustomAgentAndPolicyTests::AgentPanePositionRoundtripsAndDefaults()
    {
        // Explicit value survives load.
        const auto settings = MakeSettings(R"("agentPanePosition": "right")");
        VERIFY_ARE_EQUAL(winrt::hstring{ L"right" }, settings->GlobalSettings().AgentPanePosition());

        // Absent → falls back to the "bottom" default (MTSMSettings.h).
        const auto defaulted = MakeSettings({});
        VERIFY_ARE_EQUAL(winrt::hstring{ L"bottom" }, defaulted->GlobalSettings().AgentPanePosition());
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
