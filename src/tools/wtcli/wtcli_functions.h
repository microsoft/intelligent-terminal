// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Extracted pure functions from wtcli for fuzzing and testability.
// These functions have no COM/WinRT dependencies and can be called
// from a LibFuzzer harness.

#pragma once

#include <algorithm>
#include <cctype>
#include <string>
#include <sstream>
#include <vector>

#include <Windows.h>
#include <json/json.h>

namespace wtcli
{
    // Concatenate positional args as literal UTF-8 → UTF-16 text without
    // any tmux-style token interpretation. Use this when the caller's intent
    // is "send these exact characters" (e.g. wta forwarding agent-supplied
    // text), so payloads like the literal word "Enter" / "Tab" / "C-c" are
    // not silently rewritten into control bytes.
    inline std::wstring JoinAsUtf16(const std::vector<std::string>& parts)
    {
        std::wstring result;
        bool first = true;
        for (const auto& p : parts)
        {
            // Space-separate consecutive args so an unquoted human invocation
            // like `wtcli send-keys --raw hello world` reaches the pane as
            // "hello world" rather than "helloworld". wta callers pass a
            // single positional via `--`, so they are unaffected.
            if (!first)
            {
                result += L' ';
            }
            first = false;
            if (p.empty())
                continue;
            const int wlen = MultiByteToWideChar(CP_UTF8, 0, p.data(), static_cast<int>(p.size()), nullptr, 0);
            if (wlen > 0)
            {
                const size_t prev = result.size();
                result.resize(prev + static_cast<size_t>(wlen));
                MultiByteToWideChar(CP_UTF8, 0, p.data(), static_cast<int>(p.size()), result.data() + prev, wlen);
            }
        }
        return result;
    }

    // Translate tmux-style key names to the byte stream that should be sent
    // to a pane. Recognized tokens: Enter / Space / Tab / Escape (alias Esc) /
    // BSpace / C-a..C-z. Unrecognized tokens are passed through as UTF-8 →
    // UTF-16 text. "Enter" maps to a single CR — SendProtocolInput downstream
    // translates LF to CR as well, so emitting CRLF here would produce a
    // double-CR (two Enter keypresses).
    inline std::wstring TranslateKeys(const std::vector<std::string>& keys)
    {
        std::wstring result;
        for (const auto& key : keys)
        {
            if (key == "Enter" || key == "enter")
                result += L"\r";
            else if (key == "Space" || key == "space")
                result += L" ";
            else if (key == "Tab" || key == "tab")
                result += L"\t";
            else if (key == "Escape" || key == "escape" || key == "Esc" || key == "esc")
                result += L"\x1b";
            else if (key == "BSpace" || key == "bspace")
                result += L"\b";
            else if (key == "C-c")
                result += L"\x03";
            else if (key == "C-d")
                result += L"\x04";
            else if (key == "C-z")
                result += L"\x1a";
            else if (key == "C-l")
                result += L"\x0c";
            else if (key.size() == 3 && key[0] == 'C' && key[1] == '-' && key[2] >= 'a' && key[2] <= 'z')
                result += static_cast<wchar_t>(key[2] - 'a' + 1);
            else if (!key.empty())
            {
                const int wlen = MultiByteToWideChar(CP_UTF8, 0, key.data(), static_cast<int>(key.size()), nullptr, 0);
                if (wlen > 0)
                {
                    const size_t prev = result.size();
                    result.resize(prev + static_cast<size_t>(wlen));
                    MultiByteToWideChar(CP_UTF8, 0, key.data(), static_cast<int>(key.size()), result.data() + prev, wlen);
                }
            }
        }
        return result;
    }


    // Build the standard JSON envelope the COM server expects for an
    // `agent_event`. The caller provides the event name, an optional JSON
    // object string containing extra params, and the source pane Guid; this
    // function folds in `pane_id` and emits the wrapped
    // `{ type, method, params }` object in `outEvt`.
    //
    // Returns true on success and populates `outEvt`.
    // Returns false and leaves `outEvt` untouched if `paramsJson` is
    // non-empty but not a valid JSON object.
    //
    // |eventType|  — required event name (e.g. "agent.task.started")
    // |paramsJson| — optional JSON object string with extra params
    // |sessionId|  — source pane Guid as a string (already resolved by caller).
    //                Named `sessionId` for backwards compatibility with the
    //                old per-pane "session_id" terminology; the value is
    //                the WT pane GUID, which goes into `params["pane_id"]`
    //                — matching the rename in TerminalPage.cpp for
    //                connection_state / vt_sequence events.
    inline bool BuildSendEventJson(
        const std::string& eventType,
        const std::string& paramsJson,
        const std::string& sessionId,
        Json::Value& outEvt)
    {
        Json::Value params;
        if (!paramsJson.empty())
        {
            Json::CharReaderBuilder rb;
            std::string errs;
            std::istringstream ss(paramsJson);
            if (!Json::parseFromStream(rb, ss, &params, &errs) || !params.isObject())
            {
                return false;
            }
        }

        params["event"] = eventType;
        params["pane_id"] = sessionId;

        outEvt["type"] = "event";
        outEvt["method"] = "agent_event";
        outEvt["params"] = params;
        return true;
    }

    // Build an agent hook event directly from the hook JSON delivered on stdin.
    // This is the native equivalent of the former PowerShell bridge.
    //
    // Returns false for malformed JSON or missing routing metadata and leaves
    // outEvt untouched. Empty or whitespace-only stdin is accepted as a null
    // payload because some lifecycle hooks do not provide a body.
    inline bool BuildAgentHookEventJson(
        const std::string& eventType,
        const std::string& cliSource,
        const std::string& hookJson,
        const std::string& paneId,
        const std::string& environmentSessionId,
        Json::Value& outEvt)
    {
        if (eventType.empty() || cliSource.empty() || paneId.empty())
        {
            return false;
        }

        Json::Value payload{ Json::nullValue };
        const auto hasJson = std::any_of(hookJson.begin(), hookJson.end(), [](const unsigned char ch) {
            return std::isspace(ch) == 0;
        });
        if (hasJson)
        {
            Json::CharReaderBuilder reader;
            std::string errors;
            std::istringstream stream{ hookJson };
            if (!Json::parseFromStream(reader, stream, &payload, &errors))
            {
                return false;
            }
        }

        std::string agentSessionId = environmentSessionId;
        if (payload.isObject())
        {
            for (const auto* key : { "session_id", "sessionId" })
            {
                const auto& value = payload[key];
                if (value.isString())
                {
                    agentSessionId = value.asString();
                    break;
                }
            }

            static constexpr const char* alwaysStrip[] = {
                "tool_result",
                "tool_response",
                "tool_output",
                "toolResult",
                "toolResponse",
                "toolOutput",
                "prompt",
                "user_prompt",
                "userPrompt",
                "transcript_path",
                "transcriptPath",
                "hook_event_name",
                "hookEventName",
                "permission_mode",
                "permissionMode",
                "model",
                "model_info",
                "modelInfo",
                "output_style",
                "outputStyle",
                "version",
                "source",
                "apiKeySource",
                "transcript",
                "messages",
                "history",
                "conversation",
                "systemPrompt",
                "system_prompt",
                "instructions",
                "context",
                "files",
                "attachments",
                "events",
                "chat",
                "chatHistory",
            };
            for (const auto* key : alwaysStrip)
            {
                payload.removeMember(key);
            }

            std::string toolName;
            for (const auto* key : { "tool_name", "toolName" })
            {
                const auto& value = payload[key];
                if (value.isString())
                {
                    toolName = value.asString();
                    break;
                }
            }
            std::transform(toolName.begin(), toolName.end(), toolName.begin(), [](const unsigned char ch) {
                return static_cast<char>(std::tolower(ch));
            });

            static constexpr const char* userInputTools[] = {
                "ask_user",
                "askuser",
                "ask-user",
                "ask_question",
                "askquestion",
                "ask_for_clarification",
                "request_input",
                "request_user_input",
                "user_input",
                "prompt_user",
                "clarification_request",
            };
            const auto isUserInputTool = std::any_of(
                std::begin(userInputTools),
                std::end(userInputTools),
                [&](const char* value) { return toolName == value; });
            if (!isUserInputTool)
            {
                payload.removeMember("tool_input");
                payload.removeMember("toolInput");
            }
        }

        Json::Value params;
        params["cli_source"] = cliSource;
        params["agent_session_id"] = agentSessionId;
        params["payload"] = payload;

        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        const auto serialized = Json::writeString(writer, params);
        constexpr size_t maxPayloadChars = 25000;
        if (serialized.size() > maxPayloadChars)
        {
            Json::Value truncated;
            truncated["_truncated"] = true;
            truncated["_original_size"] = Json::UInt64{ serialized.size() };
            params["payload"] = std::move(truncated);
        }

        params["event"] = eventType;
        params["pane_id"] = paneId;

        Json::Value event;
        event["type"] = "event";
        event["method"] = "agent_event";
        event["params"] = std::move(params);
        outEvt = std::move(event);
        return true;
    }

    // Check whether an event JSON string passes the session_id and event type
    // filters used by the "listen" command.
    //
    // Returns true if the event should be emitted (matches filters or filters
    // are empty). Returns true on parse failure to match original behavior
    // (unparseable events are passed through).
    //
    // |eventTypeFilter| supports a trailing wildcard: "agent.*" matches
    // "agent.task.started".
    inline bool MatchesEventFilter(
        const std::string& eventJson,
        const std::string& sessionIdFilter,
        const std::string& eventTypeFilter)
    {
        if (sessionIdFilter.empty() && eventTypeFilter.empty())
        {
            return true;
        }

        Json::Value ev;
        Json::CharReaderBuilder rb;
        std::string errs;
        std::istringstream ss(eventJson);
        if (!Json::parseFromStream(rb, ss, &ev, &errs))
        {
            return true;
        }

        // Event JSON must be an object with a "params" object inside.
        // Reject structurally invalid events when filters are active —
        // missing fields can't match any filter.
        if (!ev.isObject() || !ev.isMember("params") || !ev["params"].isObject())
        {
            return false;
        }

        if (!sessionIdFilter.empty())
        {
            // Look for pane_id (current name) first, then fall back to
            // session_id (old name) so older listen consumers / events
            // produced before the rename keep matching during a
            // partial upgrade.
            auto paneId = ev["params"].get("pane_id", "").asString();
            if (paneId.empty())
            {
                paneId = ev["params"].get("session_id", "").asString();
            }
            if (paneId != sessionIdFilter)
            {
                return false;
            }
        }

        if (!eventTypeFilter.empty())
        {
            auto eventType = ev["params"].get("event", "").asString();
            if (eventTypeFilter.back() == '*')
            {
                auto prefix = eventTypeFilter.substr(0, eventTypeFilter.size() - 1);
                if (eventType.substr(0, prefix.size()) != prefix)
                {
                    return false;
                }
            }
            else if (eventType != eventTypeFilter)
            {
                return false;
            }
        }

        return true;
    }
}
