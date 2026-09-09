// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalProtocol/ProtocolParsing.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Protocol::Parsing;

namespace TerminalAppUnitTests
{
    class ProtocolParsingTests
    {
        TEST_CLASS(ProtocolParsingTests);

        TEST_METHOD(DefaultPasteRequestUsesDirectRoute);
        TEST_METHOD(AgentSessionsRetiredUsesDirectRoute);
        TEST_METHOD(EnableSessionTrackingUsesValidatedDirectRoute);
        TEST_METHOD(EnableSessionTrackingHonorsPolicyAndIsIdempotent);
        TEST_METHOD(RestartRequestIdentityIsStampedOnce);
        TEST_METHOD(SettingsResponseIncludesEffectiveSessionTracking);
        TEST_METHOD(SettingsResponseRejectsMalformedJson);
    };

    void ProtocolParsingTests::SettingsResponseIncludesEffectiveSessionTracking()
    {
        const auto blocked = BuildSettingsResponse(
            R"({
                // User preference remains visible while policy blocks tracking.
                "agentSessionManagementEnabled": true,
                "effectiveAgentSessionManagementEnabled": true,
                "agentSessionManagementPolicyBlocked": false,
                "autoFixEnabled": true
            })",
            false,
            true);
        VERIFY_IS_TRUE(blocked.has_value());
        VERIFY_IS_TRUE((*blocked)["agentSessionManagementEnabled"].asBool());
        VERIFY_IS_TRUE((*blocked)["effectiveAgentSessionManagementEnabled"].isBool());
        VERIFY_IS_FALSE((*blocked)["effectiveAgentSessionManagementEnabled"].asBool());
        VERIFY_IS_TRUE((*blocked)["agentSessionManagementPolicyBlocked"].isBool());
        VERIFY_IS_TRUE((*blocked)["agentSessionManagementPolicyBlocked"].asBool());
        VERIFY_IS_TRUE((*blocked)["autoFixEnabled"].asBool());

        const auto enabled = BuildSettingsResponse("{}", true, false);
        VERIFY_IS_TRUE(enabled.has_value());
        VERIFY_IS_TRUE((*enabled)["effectiveAgentSessionManagementEnabled"].asBool());
        VERIFY_IS_FALSE((*enabled)["agentSessionManagementPolicyBlocked"].asBool());
        VERIFY_IS_FALSE(enabled->isMember("agentSessionManagementEnabled"));

        const auto missingFile = BuildSettingsResponse("", false, false);
        VERIFY_IS_TRUE(missingFile.has_value());
        VERIFY_IS_FALSE((*missingFile)["effectiveAgentSessionManagementEnabled"].asBool());
        VERIFY_IS_FALSE((*missingFile)["agentSessionManagementPolicyBlocked"].asBool());

        const auto disabled = BuildSettingsResponse(
            R"({"agentSessionManagementEnabled":false,"agentSessionManagementPolicyBlocked":true})",
            false,
            false);
        VERIFY_IS_TRUE(disabled.has_value());
        VERIFY_IS_FALSE((*disabled)["agentSessionManagementEnabled"].asBool());
        VERIFY_IS_FALSE((*disabled)["effectiveAgentSessionManagementEnabled"].asBool());
        VERIFY_IS_FALSE((*disabled)["agentSessionManagementPolicyBlocked"].asBool());
    }

    void ProtocolParsingTests::SettingsResponseRejectsMalformedJson()
    {
        VERIFY_IS_FALSE(BuildSettingsResponse("{", false, false).has_value());
        VERIFY_IS_FALSE(BuildSettingsResponse("[]", false, false).has_value());
        VERIFY_IS_FALSE(BuildSettingsResponse("null", false, false).has_value());
    }

    void ProtocolParsingTests::EnableSessionTrackingUsesValidatedDirectRoute()
    {
        Json::Value event;
        VERIFY_ARE_EQUAL(
            SendEventRoute::EnableSessionTracking,
            ClassifySendEvent(
                R"({"type":"event","method":"enable_session_tracking","params":{"window_id":"42","tab_id":"tab-a","request_id":"request-1"}})",
                event));
        VERIFY_ARE_EQUAL("enable_session_tracking", event["method"].asString());
        VERIFY_ARE_EQUAL("request-1", event["params"]["request_id"].asString());

        const auto valid = event;
        for (const auto key : { "window_id", "tab_id", "request_id" })
        {
            for (const auto& invalid : { Json::Value{}, Json::Value{ "" }, Json::Value{ 42 }, Json::Value{ true } })
            {
                auto malformed = valid;
                malformed["params"][key] = invalid;
                VERIFY_ARE_EQUAL(
                    SendEventRoute::Invalid,
                    ClassifySendEvent(Json::writeString(Json::StreamWriterBuilder{}, malformed), event));
            }
            auto missing = valid;
            missing["params"].removeMember(key);
            VERIFY_ARE_EQUAL(
                SendEventRoute::Invalid,
                ClassifySendEvent(Json::writeString(Json::StreamWriterBuilder{}, missing), event));
        }
        VERIFY_ARE_EQUAL(SendEventRoute::Invalid,
                         ClassifySendEvent(R"({"method":"enable_session_tracking","params":[]})", event));
        VERIFY_ARE_EQUAL(SendEventRoute::Invalid,
                         ClassifySendEvent(R"({"method":"enable_session_tracking"})", event));
    }

    void ProtocolParsingTests::EnableSessionTrackingHonorsPolicyAndIsIdempotent()
    {
        VERIFY_ARE_EQUAL(SessionTrackingEnableAction::PolicyBlocked, DecideSessionTrackingEnable(true, false));
        VERIFY_ARE_EQUAL(SessionTrackingEnableAction::PolicyBlocked, DecideSessionTrackingEnable(true, true));
        VERIFY_ARE_EQUAL(SessionTrackingEnableAction::AlreadyEnabled, DecideSessionTrackingEnable(false, true));
        VERIFY_ARE_EQUAL(SessionTrackingEnableAction::Persist, DecideSessionTrackingEnable(false, false));
    }

    void ProtocolParsingTests::DefaultPasteRequestUsesDirectRoute()
    {
        Json::Value event;
        const auto route = ClassifySendEvent(
            R"({"type":"event","method":"request_default_paste","params":{"window_id":"1","tab_id":"tab-a","pane_id":"pane-a"}})",
            event);

        VERIFY_ARE_EQUAL(SendEventRoute::DefaultPaste, route);
        VERIFY_ARE_EQUAL("request_default_paste", event["method"].asString());
    }

    void ProtocolParsingTests::AgentSessionsRetiredUsesDirectRoute()
    {
        Json::Value event;
        const auto route = ClassifySendEvent(
            R"({"type":"event","method":"agent_sessions_retired","params":{"operation_id":"123-1","success":true,"reason":"restart_agent_stack","failed_tabs":[]}})",
            event);

        VERIFY_ARE_EQUAL(SendEventRoute::AgentSessionsRetired, route);
        VERIFY_ARE_EQUAL("123-1", event["params"]["operation_id"].asString());
    }

    void ProtocolParsingTests::RestartRequestIdentityIsStampedOnce()
    {
        Json::Value event;
        VERIFY_IS_TRUE(ParseJson(
            R"({"type":"event","method":"restart_agent_stack","params":{}})",
            event));

        EnsureRequestId(event, "request-1");
        EnsureRequestId(event, "request-2");

        VERIFY_ARE_EQUAL("request-1", event["params"]["request_id"].asString());
    }
}
