// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Extracted pure functions from wtcli for fuzzing and testability.
// These functions have no COM/WinRT dependencies and can be called
// from a LibFuzzer harness.

#pragma once

#include <algorithm>
#include <cctype>
#include <iterator>
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
    //                Empty is valid and means "source pane unknown". WTA
    //                treats an empty `pane_id` as unattributed rather than
    //                binding the event to a pane, so callers that cannot
    //                identify their pane must pass empty instead of
    //                substituting some other pane.
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

    // ── Hook event wire budget ──
    //
    // `agent_event` is broadcast: `TerminalProtocolComServer::SendEvent` routes
    // it to `s_NotifyEventToComClients`, which copies the serialized string into
    // *every* connected subscriber's bounded queue (one per agent-pane helper,
    // one for wta-master, plus any `wtcli listen`). That queue holds
    // `s_maxQueuedEvents = 4096` entries with drop-oldest back-pressure, and a
    // subscriber that stops draining (the documented case: wta not reading
    // wtcli's stdout) backs its queue up to the full 4096.
    //
    // So the limit is a *memory* bound, not a latency one — the producer never
    // blocks on delivery. Budgeting ~32 MB per stalled subscriber gives
    // 32 MB / 4096 = 8 KB per event.
    //
    // This replaces a 25000 limit inherited from the PowerShell bridge, where
    // the hook JSON travelled as a `CreateProcess` argv and the real ceiling was
    // Windows' ~32768-char command line (25000 left room for worst-case
    // `CommandLineToArgvW` backslash doubling). That constraint disappeared when
    // the payload moved to stdin; nothing on this path reads an argv anymore.
    inline constexpr size_t kMaxHookEventChars = 8192;

    // Per-field ceiling applied when an event overflows `kMaxHookEventChars`.
    // Sized so the reduced payload is always well under budget even if every
    // retained member is at its limit (7 strings + 3 nested = 5 KB of values).
    inline constexpr size_t kMaxRetainedFieldChars = 512;

    // Members of the hook payload that WTA reads, and the members it reads out
    // of `tool_input`. These mirror `app::CONSUMED_PAYLOAD_KEYS` and
    // `app::CONSUMED_TOOL_INPUT_KEYS`; `hook_contract_tests` on the Rust side
    // fails if they drift.
    inline constexpr const char* kConsumedPayloadKeys[] = {
        "cwd",
        "tool_name",
        "toolName",
        "tool_input",
        "message",
        "notification_type",
        "reason",
        "error",
    };
    inline constexpr const char* kConsumedToolInputKeys[] = {
        "question",
        "prompt",
        "message",
    };

    // Trim to at most `limit` bytes without splitting a UTF-8 sequence — the
    // result is handed to `winrt::to_hstring`, which needs well-formed UTF-8.
    inline std::string ClampUtf8(const std::string& value, const size_t limit)
    {
        if (value.size() <= limit)
        {
            return value;
        }
        auto end = limit;
        while (end > 0 && (static_cast<unsigned char>(value[end]) & 0xC0) == 0x80)
        {
            --end;
        }
        return value.substr(0, end);
    }

    // Degrade an oversized hook payload while keeping what WTA actually reads.
    //
    // The former behavior replaced the entire payload with a bare marker, which
    // threw away precisely the members the consumer needs (`cwd` for the session
    // row, `message` for a notification, `error` for a failure). That trade made
    // sense when the alternative was the event failing to spawn at all; now that
    // the cap only bounds the broadcast queue, keep the consumed members —
    // clamped — and drop the rest.
    inline Json::Value ReduceOversizedHookPayload(const Json::Value& payload, const size_t originalSize)
    {
        Json::Value reduced{ Json::objectValue };
        reduced["_truncated"] = true;
        reduced["_original_size"] = Json::UInt64{ originalSize };
        if (!payload.isObject())
        {
            return reduced;
        }

        for (const auto* key : kConsumedPayloadKeys)
        {
            const auto member = payload.get(key, Json::Value{});
            if (member.isString())
            {
                reduced[key] = ClampUtf8(member.asString(), kMaxRetainedFieldChars);
            }
            else if (member.isObject())
            {
                // `tool_input` is the only structurally-consumed member; project
                // it to the sub-members WTA reads so a large `choices` array or
                // an unknown sibling field can't blow the budget on its own.
                Json::Value projected{ Json::objectValue };
                for (const auto* nested : kConsumedToolInputKeys)
                {
                    const auto value = member.get(nested, Json::Value{});
                    if (value.isString())
                    {
                        projected[nested] = ClampUtf8(value.asString(), kMaxRetainedFieldChars);
                    }
                }
                if (!projected.empty())
                {
                    reduced[key] = std::move(projected);
                }
            }
        }
        return reduced;
    }

    // Build an agent hook event directly from the hook JSON delivered on stdin.
    // This is the native equivalent of the former PowerShell bridge.
    //
    // Returns false for malformed JSON or missing routing metadata and leaves
    // outEvt untouched. Empty or whitespace-only stdin is accepted as a null
    // payload because some lifecycle hooks do not provide a body. A body that
    // parses but is not a JSON object is reduced to null — see the redaction
    // note below.
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

        // The redaction below can only inspect and remove *object members*, so a
        // body that parses as an array, string, or number would skip it entirely
        // and reach the COM broadcast verbatim. Redaction here is a disclosure
        // control (see "Event broadcast disclosure" in doc/security-model.md), so
        // it has to fail closed. WTA reads nothing but object members out of the
        // payload, so discarding a non-object body costs no functionality.
        if (!payload.isNull() && !payload.isObject())
        {
            payload = Json::Value{ Json::nullValue };
        }

        std::string agentSessionId = environmentSessionId;
        if (payload.isObject())
        {
            for (const auto* key : { "session_id", "sessionId" })
            {
                // get() rather than operator[]: jsoncpp's non-const operator[]
                // *creates* a null member for every key it misses, which would
                // then ride the broadcast as noise on every event.
                const auto value = payload.get(key, Json::Value{});
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
                // Non-mutating lookup, same reason as the session id above.
                const auto value = payload.get(key, Json::Value{});
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
                "askuserquestion",
                "ask_user_question",
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
        params["event"] = eventType;
        params["pane_id"] = paneId;
        params["payload"] = payload;

        Json::Value event;
        event["type"] = "event";
        event["method"] = "agent_event";
        event["params"] = std::move(params);

        // Bound what actually goes on the wire. The routing fields are attached
        // *before* measuring so the limit applies to the serialized envelope
        // rather than a subset of it. See `kMaxHookEventChars` for the budget.
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        const auto serialized = Json::writeString(writer, event);
        if (serialized.size() > kMaxHookEventChars)
        {
            event["params"]["payload"] = ReduceOversizedHookPayload(payload, serialized.size());
            // The reduction is bounded by construction, but the routing fields
            // (`agent_session_id` in particular) come from the CLI and are not.
            // Fall back to the bare marker so the function always returns an
            // envelope that a subscriber's queue can budget for.
            if (Json::writeString(writer, event).size() > kMaxHookEventChars)
            {
                Json::Value marker{ Json::objectValue };
                marker["_truncated"] = true;
                marker["_original_size"] = Json::UInt64{ serialized.size() };
                event["params"]["payload"] = std::move(marker);

                // Emptying the payload cannot save an envelope whose *routing*
                // fields are themselves over budget, and those are read out of
                // the hook JSON on stdin. Measured before this check: a 200 KB
                // `session_id` rode a 200 KB envelope onto the COM broadcast
                // while reporting `_truncated`. Publishing nothing is the only
                // answer that keeps the promise made just above; the caller
                // drops the event and the hook still exits 0, so a fail-closed
                // CLI is unaffected.
                if (Json::writeString(writer, event).size() > kMaxHookEventChars)
                {
                    return false;
                }
            }
        }

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
