// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalProtocol/ProtocolParsing.h"
#include "../inc/AgentSessionHooks.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Protocol::Parsing;

namespace TerminalAppUnitTests
{
    class ProtocolParsingTests
    {
        TEST_CLASS(ProtocolParsingTests);

        TEST_METHOD(DefaultPasteRequestUsesDirectRoute);
        TEST_METHOD(AgentSessionsRetiredUsesDirectRoute);
        TEST_METHOD(RestartRequestIdentityIsStampedOnce);
        TEST_METHOD(SessionHooksPauseAndResume);
        TEST_METHOD(PausingHooksPreservesOtherProtocolEvents);
        TEST_METHOD(SessionHookSignalIsLiveAndScoped);
    };

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

    void ProtocolParsingTests::SessionHooksPauseAndResume()
    {
        for (const auto* cli : { "copilot", "claude", "codex", "gemini", "opencode" })
        {
            for (const auto* topic : {
                     "agent.session.start", "agent.session.end", "agent.prompt.submit", "agent.tool.starting", "agent.tool.finished", "agent.tool.failed", "agent.notification", "agent.error", "agent.stop", "agent.subagent.stop" })
            {
                Json::Value input;
                input["params"]["event"] = topic;
                input["params"]["data"]["cli_source"] = cli;
                Json::StreamWriterBuilder writer;
                const auto json = Json::writeString(writer, input);
                Json::Value event;

                VERIFY_ARE_EQUAL(SendEventRoute::Broadcast, ClassifySendEvent(json, event));
                VERIFY_ARE_EQUAL(SendEventRoute::Ignored, ClassifySendEvent(json, event, false));
                VERIFY_ARE_EQUAL(SendEventRoute::Broadcast, ClassifySendEvent(json, event, true));
                VERIFY_ARE_EQUAL("agent_event", event["method"].asString());
                VERIFY_ARE_EQUAL(topic, event["params"]["event"].asString());
            }
        }
    }

    void ProtocolParsingTests::PausingHooksPreservesOtherProtocolEvents()
    {
        Json::Value event;
        VERIFY_ARE_EQUAL(
            SendEventRoute::AgentState,
            ClassifySendEvent(R"({"method":"agent_state_changed","params":{}})", event, false));
        VERIFY_ARE_EQUAL(
            SendEventRoute::PaneAgentSession,
            ClassifySendEvent(R"({"method":"pane_agent_session_changed","params":{}})", event, false));
        VERIFY_ARE_EQUAL(
            SendEventRoute::Broadcast,
            ClassifySendEvent(R"({"params":{"event":"agent_config_changed","data":{}}})", event, false));
        VERIFY_ARE_EQUAL(
            SendEventRoute::Invalid,
            ClassifySendEvent(R"({"params":{}})", event, false));
    }

    void ProtocolParsingTests::SessionHookSignalIsLiveAndScoped()
    {
        namespace Hooks = Microsoft::Terminal::AgentSessionHooks;
        GUID serverId{};
        GUID otherServerId{};
        VERIFY_SUCCEEDED(CoCreateGuid(&serverId));
        VERIFY_SUCCEEDED(CoCreateGuid(&otherServerId));

        VERIFY_IS_TRUE(Hooks::IsEnabled(serverId));
        wil::unique_event disabledEvent;
        VERIFY_SUCCEEDED(Hooks::SetEnabled(disabledEvent, serverId, false));
        VERIFY_IS_FALSE(Hooks::IsEnabled(serverId));
        VERIFY_IS_TRUE(Hooks::IsEnabled(otherServerId));
        VERIFY_SUCCEEDED(Hooks::SetEnabled(disabledEvent, serverId, false));
        VERIFY_IS_FALSE(Hooks::IsEnabled(serverId));
        VERIFY_SUCCEEDED(Hooks::SetEnabled(disabledEvent, serverId, true));
        VERIFY_IS_TRUE(Hooks::IsEnabled(serverId));
        VERIFY_SUCCEEDED(Hooks::SetEnabled(disabledEvent, serverId, false));
        VERIFY_IS_FALSE(Hooks::IsEnabled(serverId));

        disabledEvent.reset();
        VERIFY_IS_TRUE(Hooks::IsEnabled(serverId));
        VERIFY_SUCCEEDED(Hooks::SetEnabled(disabledEvent, serverId, false));
        VERIFY_IS_FALSE(Hooks::IsEnabled(serverId));
    }
}
