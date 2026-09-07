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
        TEST_METHOD(BoundedCommandPreservesUtf8Characters);
        TEST_METHOD(BoundedBufferTailAppliesLineAndCharacterLimits);
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

    void ProtocolParsingTests::BoundedCommandPreservesUtf8Characters()
    {
        const auto result = BuildBoundedCommand("a\xF0\x9F\x8D\xA6"
                                                "bc",
                                                1,
                                                3);
        VERIFY_ARE_EQUAL(std::string{ "a\xF0\x9F\x8D\xA6"
                                     "b" },
                         result.content);
        VERIFY_ARE_EQUAL(1, result.lineCount);
        VERIFY_IS_TRUE(result.truncated);

        const auto lines = BuildBoundedCommand("command\r\nfirst\r\nsecond\r\n", 2, 100);
        VERIFY_ARE_EQUAL("command\nfirst", lines.content);
        VERIFY_ARE_EQUAL(2, lines.lineCount);
        VERIFY_IS_TRUE(lines.truncated);

        const auto newlineLookahead = BuildBoundedCommand("command\n", 2, 7);
        VERIFY_ARE_EQUAL("command", newlineLookahead.content);
        VERIFY_IS_TRUE(newlineLookahead.truncated);

        const auto blankLineLookahead = BuildBoundedCommand("command\n\n", 2, 100);
        VERIFY_ARE_EQUAL("command\n", blankLineLookahead.content);
        VERIFY_ARE_EQUAL(2, blankLineLookahead.lineCount);
        VERIFY_IS_TRUE(blankLineLookahead.truncated);

        const auto leadingBlankLines = BuildBoundedCommand("\n\ncommand\n", 10, 100);
        VERIFY_ARE_EQUAL("\n\ncommand\n", leadingBlankLines.content);
        VERIFY_ARE_EQUAL(4, leadingBlankLines.lineCount);
        VERIFY_IS_FALSE(leadingBlankLines.truncated);
    }

    void ProtocolParsingTests::BoundedBufferTailAppliesLineAndCharacterLimits()
    {
        const auto byLines = BuildBoundedBufferTail("first\r\nsecond\r\nthird\r\n", 2, 100);
        VERIFY_ARE_EQUAL("second\nthird", byLines.content);
        VERIFY_ARE_EQUAL(2, byLines.lineCount);
        VERIFY_IS_TRUE(byLines.truncated);

        const auto byCharacters = BuildBoundedBufferTail("one\r\ntwo\r\nthree\r\n", 3, 6);
        VERIFY_ARE_EQUAL("\nthree", byCharacters.content);
        VERIFY_ARE_EQUAL(2, byCharacters.lineCount);
        VERIFY_IS_TRUE(byCharacters.truncated);

        const auto exact = BuildBoundedBufferTail("one\r\ntwo\r\n", 2, 7);
        VERIFY_ARE_EQUAL("one\ntwo", exact.content);
        VERIFY_ARE_EQUAL(2, exact.lineCount);
        VERIFY_IS_FALSE(exact.truncated);
    }
}
