// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "TmuxProtocol.h"
#include "../../tools/wtcli/wtcli_functions.h"
#include <wincrypt.h>

#include <chrono>
#include <memory>
#include <optional>
#include <unordered_map>

#pragma comment(lib, "Crypt32.lib")

namespace Microsoft::Terminal::Tmux
{
    inline constexpr std::string_view AgentHookPrefix{ "IT_AGENT_HOOK/2 " };
    inline constexpr size_t MaxAgentHookMessageBytes = 8 * 1024;
    inline constexpr size_t MaxAgentHookPayloadBytes = 1024 * 1024;
    inline constexpr size_t MaxAgentHookEncodedBytes = ((MaxAgentHookPayloadBytes + 2) / 3) * 4;
    inline constexpr size_t AgentHookChunkBytes = 6000;
    inline constexpr size_t MaxAgentHookChunks = (MaxAgentHookEncodedBytes + AgentHookChunkBytes - 1) / AgentHookChunkBytes;

    struct AgentHookMessage
    {
        Id sessionId{};
        Id paneId{};
        std::string cliSource;
        std::string event;
        std::string payload;
    };

    struct AgentHookChunk
    {
        AgentHookMessage message;
        std::string transfer;
        size_t index{};
        size_t count{};
    };

    inline std::optional<AgentHookChunk> ParseAgentHookChunk(const std::string_view message)
    {
        if (!message.starts_with("IT_AGENT_HOOK/"))
        {
            return std::nullopt;
        }
        if (!message.starts_with(AgentHookPrefix) || message.size() > MaxAgentHookMessageBytes ||
            !std::all_of(message.begin(), message.end(), [](const unsigned char ch) { return ch >= 32 && ch < 127; }))
        {
            throw ProtocolError{ "Invalid tmux agent hook version, size or encoding" };
        }

        auto args = message.substr(AgentHookPrefix.size());
        const auto field = [&]() {
            const auto end = args.find(' ');
            if (end == 0 || end == std::string_view::npos)
            {
                throw ProtocolError{ "Missing tmux agent hook routing field" };
            }
            const auto value = args.substr(0, end);
            args.remove_prefix(end + 1);
            return value;
        };

        AgentHookChunk chunk;
        auto& hook = chunk.message;
        const auto session = field();
        if (session.size() < 2 || session.front() != '$')
        {
            throw ProtocolError{ "Invalid tmux agent hook session ID" };
        }
        hook.sessionId = details::Number(session.substr(1));
        hook.paneId = details::PaneId(field());
        hook.cliSource = field();
        hook.event = field();
        constexpr std::string_view sources[]{ "claude", "copilot", "codex", "gemini", "opencode" };
        constexpr std::string_view events[]{
            "agent.session.start", "agent.session.end", "agent.prompt.submit", "agent.notification", "agent.tool.starting", "agent.stop", "agent.error", "agent.subagent.stop"
        };
        if (std::find(std::begin(sources), std::end(sources), hook.cliSource) == std::end(sources) ||
            std::find(std::begin(events), std::end(events), hook.event) == std::end(events))
        {
            throw ProtocolError{ "Unsupported tmux agent hook source or event" };
        }
        chunk.transfer = field();
        if (chunk.transfer.size() > 64 ||
            !std::all_of(chunk.transfer.begin(), chunk.transfer.end(), [](const unsigned char ch) {
                return (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
                       (ch >= '0' && ch <= '9') || ch == '.' || ch == '_' || ch == '-';
            }))
        {
            throw ProtocolError{ "Invalid tmux agent hook transfer ID" };
        }
        const auto index = details::Number(field());
        const auto count = details::Number(field());
        if (!count || count > MaxAgentHookChunks || index >= count)
        {
            throw ProtocolError{ "Invalid tmux agent hook chunk index or count" };
        }
        chunk.index = static_cast<size_t>(index);
        chunk.count = static_cast<size_t>(count);
        if (args.size() > AgentHookChunkBytes || args.size() % 4 != 0 ||
            (chunk.index + 1 < chunk.count && args.size() != AgentHookChunkBytes) ||
            (args.empty() && chunk.count != 1))
        {
            throw ProtocolError{ "Invalid tmux agent hook chunk size" };
        }
        const auto padding = args.find('=');
        if (padding != std::string_view::npos &&
            (chunk.index + 1 != chunk.count || args.size() - padding > 2 ||
             args.find_first_not_of('=', padding) != std::string_view::npos))
        {
            throw ProtocolError{ "Invalid tmux agent hook base64 padding" };
        }
        const auto alphabet = args.substr(0, padding);
        if (!std::all_of(alphabet.begin(), alphabet.end(), [](const unsigned char ch) {
                return (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
                       (ch >= '0' && ch <= '9') || ch == '+' || ch == '/';
            }))
        {
            throw ProtocolError{ "Invalid tmux agent hook base64 data" };
        }
        hook.payload = args;
        return chunk;
    }

    class AgentHookAssembler
    {
    public:
        using Clock = std::chrono::steady_clock;
        static constexpr auto Timeout = std::chrono::seconds{ 10 };
        static constexpr size_t MaxPendingTransfers = 16;
        static constexpr size_t MaxPendingBytes = 4 * 1024 * 1024;

        size_t Expire(const Clock::time_point now = Clock::now())
        {
            size_t expired{};
            for (auto it = _pending.begin(); it != _pending.end();)
            {
                if (now - it->second.started >= Timeout)
                {
                    _bytes -= it->second.message.payload.size();
                    it = _pending.erase(it);
                    ++expired;
                }
                else
                {
                    ++it;
                }
            }
            return expired;
        }

        void Clear() noexcept
        {
            _pending.clear();
            _bytes = 0;
        }

        std::optional<AgentHookMessage> Append(AgentHookChunk chunk, const Clock::time_point now = Clock::now())
        {
            auto it = _pending.find(chunk.transfer);
            if (chunk.index == 0)
            {
                if (it != _pending.end())
                {
                    _bytes -= it->second.message.payload.size();
                    _pending.erase(it);
                    throw ProtocolError{ "Duplicate tmux agent hook transfer" };
                }
                if (chunk.count == 1)
                {
                    return _decode(std::move(chunk.message));
                }
                if (_pending.size() >= MaxPendingTransfers || chunk.message.payload.size() > MaxPendingBytes - _bytes)
                {
                    throw ProtocolError{ "Too many pending tmux agent hook transfers" };
                }
                const auto bytes = chunk.message.payload.size();
                _pending.emplace(std::move(chunk.transfer), Pending{ std::move(chunk.message), 1, chunk.count, now });
                _bytes += bytes;
                return std::nullopt;
            }
            if (it == _pending.end())
            {
                throw ProtocolError{ "Missing first tmux agent hook chunk" };
            }
            auto& pending = it->second;
            if (chunk.index != pending.next || chunk.count != pending.count ||
                chunk.message.sessionId != pending.message.sessionId || chunk.message.paneId != pending.message.paneId ||
                chunk.message.cliSource != pending.message.cliSource || chunk.message.event != pending.message.event)
            {
                _bytes -= pending.message.payload.size();
                _pending.erase(it);
                throw ProtocolError{ "Inconsistent tmux agent hook chunks" };
            }
            if (chunk.message.payload.size() > MaxAgentHookEncodedBytes - pending.message.payload.size() ||
                chunk.message.payload.size() > MaxPendingBytes - _bytes)
            {
                _bytes -= pending.message.payload.size();
                _pending.erase(it);
                throw ProtocolError{ "Tmux agent hook reassembly exceeds buffer limit" };
            }
            pending.message.payload.append(chunk.message.payload);
            _bytes += chunk.message.payload.size();
            if (++pending.next != pending.count)
            {
                return std::nullopt;
            }
            auto complete = std::move(pending.message);
            _bytes -= complete.payload.size();
            _pending.erase(it);
            return _decode(std::move(complete));
        }

    private:
        struct Pending
        {
            AgentHookMessage message;
            size_t next{};
            size_t count{};
            Clock::time_point started;
        };

        static AgentHookMessage _decode(AgentHookMessage hook)
        {
            if (hook.payload.empty())
            {
                return hook;
            }
            // The VT parser's Base64 helper converts to UTF-16 and replaces
            // malformed UTF-8. Keep raw bytes intact until JSON validation.
            DWORD size{};
            constexpr DWORD flags = CRYPT_STRING_BASE64 | CRYPT_STRING_STRICT;
            const auto encodedSize = static_cast<DWORD>(hook.payload.size());
            if (!CryptStringToBinaryA(hook.payload.data(), encodedSize, flags, nullptr, &size, nullptr, nullptr) ||
                size > MaxAgentHookPayloadBytes)
            {
                throw ProtocolError{ "Invalid or oversized tmux agent hook payload" };
            }
            std::string payload(size, '\0');
            if (!CryptStringToBinaryA(hook.payload.data(), encodedSize, flags, reinterpret_cast<BYTE*>(payload.data()), &size, nullptr, nullptr))
            {
                throw ProtocolError{ "Invalid tmux agent hook base64 payload" };
            }
            payload.resize(size);
            hook.payload = std::move(payload);
            return hook;
        }

        std::unordered_map<std::string, Pending> _pending;
        size_t _bytes{};
    };

    inline std::string NormalizeAgentHookPayload(const std::string& raw)
    {
        if (raw.size() > MaxAgentHookPayloadBytes)
        {
            throw ProtocolError{ "Tmux agent hook payload exceeds input limit" };
        }
        if (raw.find('\0') != std::string::npos ||
            (!raw.empty() && !MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, raw.data(), static_cast<int>(raw.size()), nullptr, 0)))
        {
            throw ProtocolError{ "Invalid tmux agent hook payload encoding" };
        }
        Json::Value payload;
        if (std::any_of(raw.begin(), raw.end(), [](const unsigned char ch) { return !std::isspace(ch); }))
        {
            Json::CharReaderBuilder builder;
            builder["collectComments"] = false;
            builder["allowComments"] = false;
            builder["allowTrailingCommas"] = false;
            builder["failIfExtra"] = true;
            builder["rejectDupKeys"] = true;
            builder["stackLimit"] = 64;
            const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
            std::string errors;
            bool parsed = false;
            try
            {
                parsed = reader->parse(raw.data(), raw.data() + raw.size(), &payload, &errors);
            }
            catch (const Json::Exception&)
            {
                throw ProtocolError{ "Invalid tmux agent hook JSON nesting" };
            }
            if (!parsed || (!payload.isNull() && !payload.isObject()))
            {
                throw ProtocolError{ "Invalid tmux agent hook JSON payload" };
            }
        }
        Json::Value projected{ Json::objectValue };
        const auto copyString = [](const Json::Value& source, const char* key, Json::Value& target) {
            if (const auto& value = source[key]; value.isString())
            {
                const auto text = value.asString();
                if (!text.empty() && !MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.data(), static_cast<int>(text.size()), nullptr, 0))
                {
                    throw ProtocolError{ "Invalid tmux agent hook metadata encoding" };
                }
                target[key] = value;
            }
        };
        if (payload.isObject())
        {
            for (const auto* key : { "session_id", "sessionId" })
            {
                if (payload.isMember(key))
                {
                    if (!payload[key].isString())
                    {
                        throw ProtocolError{ "Invalid tmux agent session ID" };
                    }
                    const auto id = payload[key].asString();
                    if (id.empty() || id.size() > 1024 ||
                        std::any_of(id.begin(), id.end(), [](const unsigned char ch) { return ch <= 32 || ch == 127; }))
                    {
                        throw ProtocolError{ "Invalid tmux agent session ID" };
                    }
                    copyString(payload, key, projected);
                }
            }
            if (projected.isMember("session_id") && projected.isMember("sessionId") &&
                projected["session_id"] != projected["sessionId"])
            {
                throw ProtocolError{ "Conflicting tmux agent session IDs" };
            }
            for (const auto* key : wtcli::kConsumedPayloadKeys)
            {
                if (std::string_view{ key } != "tool_input")
                {
                    copyString(payload, key, projected);
                }
            }
            if (payload["tool_input"].isObject())
            {
                Json::Value toolInput{ Json::objectValue };
                for (const auto* key : wtcli::kConsumedToolInputKeys)
                {
                    copyString(payload["tool_input"], key, toolInput);
                }
                if (!toolInput.empty())
                {
                    projected["tool_input"] = std::move(toolInput);
                }
            }
        }
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        return Json::writeString(writer, projected);
    }

    inline Json::Value BuildAgentHookParams(const AgentHookMessage& hook,
                                            const std::string& paneId,
                                            const std::string& tabId,
                                            const std::string& windowId,
                                            const std::string& sessionName,
                                            const std::string& socketPath,
                                            const Json::Value& sshTarget = {})
    {
        Json::Value event;
        // Reuse the native bridge's redaction and wire budget, not a second
        // permissive path from remote hook JSON to the COM broadcast.
        if (tabId.empty() || windowId.empty() ||
            !wtcli::BuildAgentHookEventJson(hook.event, hook.cliSource, NormalizeAgentHookPayload(hook.payload), paneId, {}, event))
        {
            throw ProtocolError{ "Cannot normalize tmux agent hook" };
        }
        auto& params = event["params"];
        params["tab_id"] = tabId;
        params["window_id"] = windowId;
        auto& tmux = params["tmux"];
        tmux["session_id"] = "$" + std::to_string(hook.sessionId);
        tmux["pane_id"] = "%" + std::to_string(hook.paneId);
        tmux["session_name"] = wtcli::ClampUtf8(sessionName, 512);
        tmux["socket_path"] = wtcli::ClampUtf8(socketPath, 512);
        if (!sshTarget.isNull())
        {
            // Only the native launch command supplies this identity. Remote
            // hook JSON is projected separately and cannot override it.
            tmux["ssh_target"] = sshTarget;
        }

        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        const auto size = Json::writeString(writer, event).size();
        if (size > wtcli::kMaxHookEventChars)
        {
            params["payload"] = wtcli::ReduceOversizedHookPayload(params["payload"], size);
            if (Json::writeString(writer, event).size() > wtcli::kMaxHookEventChars)
            {
                throw ProtocolError{ "Tmux agent hook exceeds event budget" };
            }
        }
        return std::move(params);
    }
}
