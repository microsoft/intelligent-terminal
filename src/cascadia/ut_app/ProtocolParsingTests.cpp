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
        TEST_METHOD(RestartRequestIdentityIsStampedOnce);
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
}
