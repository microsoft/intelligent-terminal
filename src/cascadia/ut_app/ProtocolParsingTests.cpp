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
        TEST_METHOD(PromptCaptureUsesMarksWhenAvailable);
        TEST_METHOD(PromptCaptureReportsScrollbackFallbackReason);
        TEST_METHOD(PromptCaptureZeroLinesIsMetadataOnly);
        TEST_METHOD(PromptTailMetadataIsTruthful);
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

    void ProtocolParsingTests::PromptCaptureUsesMarksWhenAvailable()
    {
        const auto route = ResolvePromptCaptureRoute(true, true);
        VERIFY_ARE_EQUAL(std::string_view{ "last_prompt" }, route.outputSource);
        VERIFY_IS_TRUE(route.fallbackReason.empty());
        VERIFY_IS_TRUE(route.hasMarks);
    }

    void ProtocolParsingTests::PromptCaptureReportsScrollbackFallbackReason()
    {
        const auto missingMarks = ResolvePromptCaptureRoute(true, false);
        VERIFY_ARE_EQUAL(std::string_view{ "scrollback" }, missingMarks.outputSource);
        VERIFY_ARE_EQUAL(std::string_view{ "marks_unavailable" }, missingMarks.fallbackReason);
        VERIFY_IS_FALSE(missingMarks.hasMarks);

        const auto readError = ResolvePromptCaptureRoute(false, false);
        VERIFY_ARE_EQUAL(std::string_view{ "scrollback" }, readError.outputSource);
        VERIFY_ARE_EQUAL(std::string_view{ "last_prompt_error" }, readError.fallbackReason);
        VERIFY_IS_FALSE(readError.hasMarks);
    }

    void ProtocolParsingTests::PromptCaptureZeroLinesIsMetadataOnly()
    {
        VERIFY_IS_FALSE(ShouldCapturePromptOutput(0));
        VERIFY_IS_FALSE(ShouldCapturePromptOutput(-1));
        VERIFY_IS_TRUE(ShouldCapturePromptOutput(1));
        VERIFY_IS_TRUE(ShouldCapturePromptOutput(24));
    }

    void ProtocolParsingTests::PromptTailMetadataIsTruthful()
    {
        const auto one = BuildPromptTail("first\r\nsecond\r\nthird\r\n", 1);
        VERIFY_ARE_EQUAL("third", one.content);
        VERIFY_ARE_EQUAL(1, one.lineCount);
        VERIFY_IS_TRUE(one.truncated);

        const auto fewer = BuildPromptTail("first\r\nsecond\r\n", 5);
        VERIFY_ARE_EQUAL("first\nsecond", fewer.content);
        VERIFY_ARE_EQUAL(2, fewer.lineCount);
        VERIFY_IS_FALSE(fewer.truncated);

        const auto blank = BuildPromptTail("\r\nsecond\r\n", 2);
        VERIFY_ARE_EQUAL("\nsecond", blank.content);
        VERIFY_ARE_EQUAL(2, blank.lineCount);
        VERIFY_IS_FALSE(blank.truncated);

        const auto zero = BuildPromptTail("must not be returned\r\n", 0);
        VERIFY_IS_TRUE(zero.content.empty());
        VERIFY_ARE_EQUAL(0, zero.lineCount);
        VERIFY_IS_FALSE(zero.truncated);
    }
}
