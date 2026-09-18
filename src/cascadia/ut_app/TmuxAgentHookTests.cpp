// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/TmuxAgentHook.h"
#include <filesystem>
#include <fstream>
#include <wincrypt.h>

#pragma comment(lib, "Crypt32.lib")

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::string Encode(const std::string_view raw)
        {
            if (raw.empty())
            {
                return {};
            }
            DWORD length{};
            const auto data = reinterpret_cast<const BYTE*>(raw.data());
            const auto size = static_cast<DWORD>(raw.size());
            constexpr DWORD flags = CRYPT_STRING_BASE64 | CRYPT_STRING_NOCRLF;
            VERIFY_IS_TRUE(CryptBinaryToStringA(data, size, flags, nullptr, &length));
            std::string encoded(length, '\0');
            VERIFY_IS_TRUE(CryptBinaryToStringA(data, size, flags, encoded.data(), &length));
            encoded.resize(length);
            if (!encoded.empty() && encoded.back() == '\0')
            {
                encoded.pop_back();
            }
            return encoded;
        }

        std::string Wire(const std::string_view data, const size_t index = 0, const size_t count = 1, const std::string_view transfer = "transfer")
        {
            return std::string{ AgentHookPrefix } + "$7 %3 copilot agent.notification " + std::string{ transfer } + " " +
                   std::to_string(index) + " " + std::to_string(count) + " " + std::string{ data };
        }

        std::vector<std::string> Messages(const std::string_view raw, const std::string_view transfer = "transfer")
        {
            const auto encoded = Encode(raw);
            const auto count = (std::max)(size_t{ 1 }, (encoded.size() + AgentHookChunkBytes - 1) / AgentHookChunkBytes);
            std::vector<std::string> result;
            for (size_t index = 0; index < count; ++index)
            {
                result.emplace_back(Wire(std::string_view{ encoded }.substr(index * AgentHookChunkBytes, AgentHookChunkBytes), index, count, transfer));
            }
            return result;
        }

        AgentHookMessage Receive(const std::string_view raw)
        {
            AgentHookAssembler assembler;
            std::optional<AgentHookMessage> hook;
            for (const auto& message : Messages(raw))
            {
                hook = assembler.Append(*ParseAgentHookChunk(message));
            }
            VERIFY_IS_TRUE(hook.has_value());
            return std::move(*hook);
        }
    }

    class TmuxAgentHookTests
    {
        TEST_CLASS(TmuxAgentHookTests);
        TEST_METHOD(ParsesLiteralMessageAtEveryFragmentBoundary);
        TEST_METHOD(RejectsMalformedOrUnsupportedMessages);
        TEST_METHOD(NormalizesAndRedactsNativeEnvelope);
        TEST_METHOD(UsesOnlyControllerSshSourceAndIncludesItInTheEnvelopeBudget);
        TEST_METHOD(BoundsEntireEnvelopeIncludingTmuxMetadata);
        TEST_METHOD(ReassemblesMaximumPayloadBeforeParsingOrDecodingUtf8);
        TEST_METHOD(IsolatesInterleavedTransfersAndRejectsIdentityChanges);
        TEST_METHOD(RejectsMissingDuplicateAndOversizedTransfers);
        TEST_METHOD(BoundsPendingTransfersAndMemory);
        TEST_METHOD(ExpiresAndClearsIncompleteTransfers);
        TEST_METHOD(RejectsMalformedJsonOnlyOnTheNativeSide);
        TEST_METHOD(PreservesOnlyConsumedQuestionFields);
        TEST_METHOD(ConsumesCapturedShellMessages);
    };

    void TmuxAgentHookTests::ParsesLiteralMessageAtEveryFragmentBoundary()
    {
        const auto message = Messages(R"({"session_id":"sid","message":"\u4f60\u597d\n#{session_name} %s \\ \u001b"})").front();
        const auto wire = "%message " + message + "\n";
        for (size_t split = 0; split <= wire.size(); ++split)
        {
            Parser parser;
            auto events = parser.Feed(std::string_view{ wire }.substr(0, split));
            const auto remaining = parser.Feed(std::string_view{ wire }.substr(split));
            events.insert(events.end(), remaining.begin(), remaining.end());
            parser.Finish();
            VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
            VERIFY_IS_TRUE(events[0].kind == Event::Kind::Notification);
            VERIFY_ARE_EQUAL(std::string{ "message" }, events[0].name);
            AgentHookAssembler assembler;
            const auto hook = assembler.Append(*ParseAgentHookChunk(events[0].text));
            VERIFY_IS_TRUE(hook.has_value());
            VERIFY_ARE_EQUAL(Id{ 7 }, hook->sessionId);
            VERIFY_ARE_EQUAL(Id{ 3 }, hook->paneId);
            const auto params = BuildAgentHookParams(*hook, "native-pane", "native-tab", "9", "work", "/socket");
            VERIFY_ARE_EQUAL(std::string{ "\xe4\xbd\xa0\xe5\xa5\xbd\n#{session_name} %s \\ \x1b" },
                             params["payload"]["message"].asString());
        }
    }

    void TmuxAgentHookTests::RejectsMalformedOrUnsupportedMessages()
    {
        VERIFY_IS_FALSE(ParseAgentHookChunk("ordinary tmux message").has_value());
        const auto valid = Messages("{}").front();
        VERIFY_IS_TRUE(ParseAgentHookChunk(valid).has_value());
        for (const auto text : {
                 "IT_AGENT_HOOK/1 {}", "IT_AGENT_HOOK/3 {}", "IT_AGENT_HOOK/2 {}", "IT_AGENT_HOOK/2 7 %3 copilot agent.stop t 0 1 e30=", "IT_AGENT_HOOK/2 $7 %3;kill-server copilot agent.stop t 0 1 e30=", "IT_AGENT_HOOK/2 $7 %3 custom:command agent.stop t 0 1 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot send_input t 0 1 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t;command 0 1 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t 1 1 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t 0 0 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t 0 235 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t -1 1 e30=", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t 0 1", "IT_AGENT_HOOK/2 $7 %3 copilot agent.stop t 0 1  e30=" })
        {
            VERIFY_THROWS(ParseAgentHookChunk(text), ProtocolError);
        }
        VERIFY_THROWS(ParseAgentHookChunk(valid + "\n%exit\n"), ProtocolError);
        VERIFY_THROWS(ParseAgentHookChunk(valid + std::string(MaxAgentHookMessageBytes, ' ')), ProtocolError);
        VERIFY_THROWS(ParseAgentHookChunk(Wire("e30=", 0, 1, std::string(65, 't'))), ProtocolError);
        VERIFY_THROWS(ParseAgentHookChunk(Wire("e30=", 0, 2)), ProtocolError);
        VERIFY_THROWS(ParseAgentHookChunk(Wire(std::string(6004, 'A'))), ProtocolError);
        for (const auto data : { "x", "====", "a===", "=AAA", "AA=A", "e3_=", "e3\n=", "e30=tail" })
        {
            VERIFY_THROWS(ParseAgentHookChunk(Wire(data)), ProtocolError);
        }
        VERIFY_THROWS(NormalizeAgentHookPayload(Receive(std::string{ "\xff" }).payload), ProtocolError);
    }

    void TmuxAgentHookTests::NormalizesAndRedactsNativeEnvelope()
    {
        const auto hook = Receive(
            R"({"session_id":"sid","cwd":"/repo","pane_id":"forged","tab_id":"forged","window_id":"forged","prompt":"secret","transcript_path":"/secret","model":"secret","messages":["secret"],"tool_result":"secret","tool_name":"shell","tool_input":{"command":"secret"},"unknown_secret":"secret"})");
        const auto params = BuildAgentHookParams(hook, "native-pane", "native-tab", "9", "work", "/socket");
        VERIFY_ARE_EQUAL(std::string{ "native-pane" }, params["pane_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "native-tab" }, params["tab_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "9" }, params["window_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "sid" }, params["agent_session_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "$7" }, params["tmux"]["session_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "%3" }, params["tmux"]["pane_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "/repo" }, params["payload"]["cwd"].asString());
        for (const auto* key : { "prompt", "transcript_path", "model", "messages", "tool_result", "tool_input", "unknown_secret", "pane_id", "tab_id", "window_id" })
        {
            VERIFY_IS_FALSE(params["payload"].isMember(key));
        }
        VERIFY_THROWS(BuildAgentHookParams(hook, {}, "tab", "9", {}, {}), ProtocolError);
        VERIFY_THROWS(BuildAgentHookParams(hook, "pane", {}, "9", {}, {}), ProtocolError);
    }

    void TmuxAgentHookTests::BoundsEntireEnvelopeIncludingTmuxMetadata()
    {
        const auto hook = Receive(R"({"session_id":"sid","message":")" + std::string(8100, 'x') + R"(","cwd":"/repo"})");
        const auto params = BuildAgentHookParams(hook, "pane", "tab", "9", std::string(2000, 's'), std::string(2000, 'p'));
        Json::Value event;
        event["type"] = "event";
        event["method"] = "agent_event";
        event["params"] = params;
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        VERIFY_IS_TRUE(Json::writeString(writer, event).size() <= wtcli::kMaxHookEventChars);
        VERIFY_ARE_EQUAL(std::string{ "sid" }, params["agent_session_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "/repo" }, params["payload"]["cwd"].asString());
        VERIFY_IS_TRUE(params["payload"]["_truncated"].asBool());
    }

    void TmuxAgentHookTests::UsesOnlyControllerSshSourceAndIncludesItInTheEnvelopeBudget()
    {
        const auto hook = Receive(
            R"({"session_id":"sid","cwd":"/repo","ssh_target":{"destination":"forged"},"sessions_ssh":{"destination":"forged"},"tmux":{"ssh_target":{"destination":"forged"}},"message":")" +
            std::string(8100, 'x') + R"("})");
        const auto opaque = BuildAgentHookParams(hook, "pane", "tab", "9", "work", "/socket");
        VERIFY_IS_FALSE(opaque["tmux"].isMember("ssh_target"));
        Json::Value target;
        target["destination"] = "user@wsl-ubuntu";
        target["port"] = 2222;
        const auto params = BuildAgentHookParams(hook, "pane", "tab", "9", "work", "/socket", target);
        VERIFY_IS_TRUE(params["tmux"]["ssh_target"] == target);
        for (const auto* key : { "ssh_target", "sessions_ssh", "tmux" })
        {
            VERIFY_IS_FALSE(params["payload"].isMember(key));
        }
        Json::Value event;
        event["type"] = "event";
        event["method"] = "agent_event";
        event["params"] = params;
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        VERIFY_IS_TRUE(Json::writeString(writer, event).size() <= wtcli::kMaxHookEventChars);
        VERIFY_ARE_EQUAL(std::string{ "sid" }, params["agent_session_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "/repo" }, params["payload"]["cwd"].asString());
    }

    void TmuxAgentHookTests::ReassemblesMaximumPayloadBeforeParsingOrDecodingUtf8()
    {
        std::string raw = R"({"session_id":"sid","cwd":"/repo","tool_result":")";
        raw.append(4499 - raw.size(), 'x');
        raw.append("\xe4\xbd\xa0\xe5\xa5\xbd");
        raw.append(MaxAgentHookPayloadBytes - raw.size() - 2, 'x');
        raw.append("\"}");
        const auto messages = Messages(raw);
        VERIFY_ARE_EQUAL(MaxAgentHookChunks, messages.size());
        AgentHookAssembler assembler;
        for (size_t index = 0; index < messages.size(); ++index)
        {
            VERIFY_IS_TRUE(messages[index].size() <= MaxAgentHookMessageBytes);
            const auto hook = assembler.Append(*ParseAgentHookChunk(messages[index]));
            if (index + 1 < messages.size())
            {
                VERIFY_IS_FALSE(hook.has_value());
            }
            else
            {
                VERIFY_IS_TRUE(hook.has_value());
                VERIFY_ARE_EQUAL(raw, hook->payload);
                const auto params = BuildAgentHookParams(*hook, "pane", "tab", "9", "work", "/socket");
                VERIFY_ARE_EQUAL(std::string{ "sid" }, params["agent_session_id"].asString());
                VERIFY_ARE_EQUAL(std::string{ "/repo" }, params["payload"]["cwd"].asString());
                VERIFY_IS_FALSE(params["payload"].isMember("tool_result"));
            }
        }
    }

    void TmuxAgentHookTests::IsolatesInterleavedTransfersAndRejectsIdentityChanges()
    {
        const auto first = Messages(std::string(5000, 'a'), "first");
        const auto second = Messages(std::string(5000, 'b'), "second");
        AgentHookAssembler assembler;
        VERIFY_IS_FALSE(assembler.Append(*ParseAgentHookChunk(first[0])).has_value());
        VERIFY_IS_FALSE(assembler.Append(*ParseAgentHookChunk(second[0])).has_value());
        VERIFY_ARE_EQUAL(std::string(5000, 'a'), assembler.Append(*ParseAgentHookChunk(first[1]))->payload);
        VERIFY_ARE_EQUAL(std::string(5000, 'b'), assembler.Append(*ParseAgentHookChunk(second[1]))->payload);

        for (size_t field = 0; field < 5; ++field)
        {
            assembler.Append(*ParseAgentHookChunk(first[0]));
            auto changed = *ParseAgentHookChunk(first[1]);
            switch (field)
            {
            case 0:
                ++changed.message.sessionId;
                break;
            case 1:
                ++changed.message.paneId;
                break;
            case 2:
                changed.message.cliSource = "claude";
                break;
            case 3:
                changed.message.event = "agent.stop";
                break;
            case 4:
                ++changed.count;
                break;
            }
            VERIFY_THROWS(assembler.Append(changed), ProtocolError);
            VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(first[1])), ProtocolError);
        }
    }

    void TmuxAgentHookTests::RejectsMissingDuplicateAndOversizedTransfers()
    {
        const auto messages = Messages(std::string(10000, 'a'));
        AgentHookAssembler assembler;
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(messages[1])), ProtocolError);
        assembler.Append(*ParseAgentHookChunk(messages[0]));
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(messages[0])), ProtocolError);
        assembler.Append(*ParseAgentHookChunk(messages[0]));
        assembler.Append(*ParseAgentHookChunk(messages[1]));
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(messages[1])), ProtocolError);
        VERIFY_THROWS(Receive(std::string(MaxAgentHookPayloadBytes + 1, 'x')), ProtocolError);
        VERIFY_ARE_EQUAL(std::string{}, Receive({}).payload);
    }

    void TmuxAgentHookTests::BoundsPendingTransfersAndMemory()
    {
        const std::string data(AgentHookChunkBytes, 'A');
        AgentHookAssembler assembler;
        for (size_t index = 0; index < AgentHookAssembler::MaxPendingTransfers; ++index)
        {
            assembler.Append(*ParseAgentHookChunk(Wire(data, 0, 2, std::to_string(index))));
        }
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(Wire(data, 0, 2, "overflow"))), ProtocolError);
        assembler.Clear();

        const auto start = AgentHookAssembler::Clock::now();
        constexpr auto transfers = size_t{ 4 };
        const auto rounds = AgentHookAssembler::MaxPendingBytes / (transfers * AgentHookChunkBytes);
        for (size_t index = 0; index < rounds; ++index)
        {
            for (size_t transfer = 0; transfer < transfers; ++transfer)
            {
                assembler.Append(*ParseAgentHookChunk(Wire(data, index, MaxAgentHookChunks, std::to_string(transfer))), start);
            }
        }
        for (size_t transfer = 0; transfer < transfers - 1; ++transfer)
        {
            assembler.Append(*ParseAgentHookChunk(Wire(data, rounds, MaxAgentHookChunks, std::to_string(transfer))), start);
        }
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(Wire(data, rounds, MaxAgentHookChunks, "3")), start), ProtocolError);
        VERIFY_ARE_EQUAL(size_t{ 3 }, assembler.Expire(start + AgentHookAssembler::Timeout));
        VERIFY_IS_FALSE(assembler.Append(*ParseAgentHookChunk(Wire(data, 0, 2, "new"))).has_value());
    }

    void TmuxAgentHookTests::ExpiresAndClearsIncompleteTransfers()
    {
        const auto messages = Messages(std::string(5000, 'a'));
        AgentHookAssembler assembler;
        const auto start = AgentHookAssembler::Clock::now();
        assembler.Append(*ParseAgentHookChunk(messages[0]), start);
        VERIFY_ARE_EQUAL(size_t{ 0 }, assembler.Expire(start + AgentHookAssembler::Timeout - std::chrono::seconds{ 1 }));
        VERIFY_ARE_EQUAL(size_t{ 1 }, assembler.Expire(start + AgentHookAssembler::Timeout));
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(messages[1])), ProtocolError);
        assembler.Append(*ParseAgentHookChunk(messages[0]), start);
        assembler.Clear();
        VERIFY_THROWS(assembler.Append(*ParseAgentHookChunk(messages[1])), ProtocolError);
        VERIFY_ARE_EQUAL(size_t{ 0 }, assembler.Expire(start + AgentHookAssembler::Timeout));
    }

    void TmuxAgentHookTests::RejectsMalformedJsonOnlyOnTheNativeSide()
    {
        for (const auto raw : {
                 "{", "[]", "42", "false", "{\"session_id\":\"one\",\"session_id\":\"two\"}", "{\"session_id\":\"one\",\"sessionId\":\"two\"}", "{\"session_id\":2}", "{\"session_id\":\"\"}", "{\"session_id\":\"bad id\"}", "{\"session_id\":\"bad\\u0000id\"}", "{\"cwd\":\"/repo\",}", "{\"cwd\":NaN}", "{} trailing", "{/*comment*/}" })
        {
            const auto hook = Receive(raw);
            VERIFY_ARE_EQUAL(std::string{ raw }, hook.payload);
            VERIFY_THROWS(BuildAgentHookParams(hook, "pane", "tab", "9", {}, {}), ProtocolError);
        }
        VERIFY_THROWS(NormalizeAgentHookPayload(std::string{ "{\"nested\":" } + std::string(100, '[') + "0" + std::string(100, ']') + "}"), ProtocolError);
        VERIFY_THROWS(NormalizeAgentHookPayload("{\"session_id\":\"" + std::string(1025, 's') + "\"}"), ProtocolError);
        VERIFY_THROWS(NormalizeAgentHookPayload(std::string{ "{}\0{}", 5 }), ProtocolError);
        for (const auto raw : { "", " \r\n\t", "null", "{}" })
        {
            const auto params = BuildAgentHookParams(Receive(raw), "pane", "tab", "9", {}, {});
            VERIFY_ARE_EQUAL(std::string{}, params["agent_session_id"].asString());
            VERIFY_IS_TRUE(params["payload"].isObject());
        }
    }

    void TmuxAgentHookTests::PreservesOnlyConsumedQuestionFields()
    {
        const auto hook = Receive(R"({"sessionId":"sid","tool_name":"AskUserQuestion","tool_input":{"question":"Which?","prompt":"Pick one","message":"Waiting","command":"secret","choices":["secret"]}})");
        const auto params = BuildAgentHookParams(hook, "pane", "tab", "9", {}, {});
        VERIFY_ARE_EQUAL(std::string{ "sid" }, params["agent_session_id"].asString());
        const auto& input = params["payload"]["tool_input"];
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 3 }, input.size());
        VERIFY_ARE_EQUAL(std::string{ "Which?" }, input["question"].asString());
        VERIFY_IS_FALSE(input.isMember("command"));
        VERIFY_IS_FALSE(input.isMember("choices"));
        const auto invalidInput = BuildAgentHookParams(
            Receive(R"({"sessionId":"sid","tool_name":"AskUserQuestion","tool_input":"secret"})"), "pane", "tab", "9", {}, {});
        VERIFY_IS_FALSE(invalidInput["payload"].isMember("tool_input"));
    }

    void TmuxAgentHookTests::ConsumesCapturedShellMessages()
    {
        WEX::Common::String directory;
        if (FAILED(RuntimeParameters::TryGetValue(L"TmuxHookCaptureDirectory", directory)))
        {
            WEX::Logging::Log::Result(WEX::Logging::TestResults::Skipped,
                                      L"Provide TmuxHookCaptureDirectory from Test-TmuxAgentHook.ps1 -CaptureDirectory.");
            return;
        }
        const wchar_t* directoryText = directory;
        const std::filesystem::path root{ directoryText };
        std::ifstream messages{ root / L"real-shell-v2.messages", std::ios::binary };
        std::ifstream payload{ root / L"real-shell-v2.payload", std::ios::binary };
        VERIFY_IS_TRUE(messages.is_open());
        VERIFY_IS_TRUE(payload.is_open());
        const std::string wire{ std::istreambuf_iterator<char>{ messages }, std::istreambuf_iterator<char>{} };
        const std::string expected{ std::istreambuf_iterator<char>{ payload }, std::istreambuf_iterator<char>{} };
        VERIFY_IS_FALSE(messages.bad());
        VERIFY_IS_FALSE(payload.bad());

        Parser parser;
        AgentHookAssembler assembler;
        std::optional<AgentHookMessage> hook;
        const auto events = parser.Feed(wire);
        parser.Finish();
        VERIFY_ARE_EQUAL(size_t{ 2 }, events.size());
        for (const auto& event : events)
        {
            VERIFY_IS_TRUE(event.kind == Event::Kind::Notification);
            VERIFY_ARE_EQUAL(std::string{ "message" }, event.name);
            hook = assembler.Append(*ParseAgentHookChunk(event.text));
        }
        VERIFY_IS_TRUE(hook.has_value());
        VERIFY_ARE_EQUAL(expected, hook->payload);
        VERIFY_ARE_EQUAL(Id{ 0 }, hook->sessionId);
        VERIFY_ARE_EQUAL(Id{ 0 }, hook->paneId);
        const auto params = BuildAgentHookParams(*hook, "pane", "tab", "9", "work", "/socket");
        VERIFY_ARE_EQUAL(std::string{ "native-fixture-session" }, params["agent_session_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "/repo" }, params["payload"]["cwd"].asString());
        VERIFY_ARE_EQUAL(std::string{ R"(literal \n; #{session_name}; %Y)" }, params["payload"]["message"].asString());
        VERIFY_IS_FALSE(params["payload"].isMember("prompt"));
        VERIFY_IS_FALSE(params["payload"].isMember("tool_result"));
    }
}
