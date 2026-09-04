// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Extracted pure-parsing functions from the Terminal Protocol server layer
// for fuzzing and testability. These functions have no COM, WinRT, or XAML
// dependencies and can be called from a LibFuzzer harness.

#pragma once

#include <string>
#include <string_view>
#include <sstream>
#include <vector>

#include <json/json.h>

namespace Microsoft::Terminal::Protocol::Parsing
{
    // ── JSON helper ──

    // Parse a JSON string. Returns true on success.
    // Equivalent to _parseJson() in TerminalProtocolComServer.cpp.
    inline bool ParseJson(const std::string& str, Json::Value& out)
    {
        Json::CharReaderBuilder rb;
        std::string errs;
        std::istringstream ss(str);
        return Json::parseFromStream(rb, ss, &out, &errs);
    }

    // ── SendEvent dispatch ──

    // The dispatch routes for IProtocolServer::SendEvent.
    enum class SendEventRoute
    {
        AutofixState,         // Direct to TerminalPage, no broadcast
        AgentStatus,          // Direct to TerminalPage, no broadcast
        AgentSwitch,          // Direct to TerminalPage, no broadcast — `/agent` per-tab switch
        CloseAgentPane,       // Direct to TerminalPage, no broadcast
        DefaultPaste,         // Direct to TerminalPage, no broadcast — WTA-owned right-click copy-or-paste
        AgentState,           // Direct to TerminalPage, no broadcast — unified per-tab agent-pane UI snapshot (view + pane_open + ...)
        ResumeInNewAgentTab,  // Direct to TerminalPage, no broadcast
        PaneAgentSession,     // Direct to TerminalPage, no broadcast — hookless delegate pane/session binding
        AgentChipTarget,      // Direct to TerminalPage, no broadcast — "draw the Agent chip on this pane (or hide override)"
        RestartAgentStack,    // Direct to TerminalPage, no broadcast — `/restart` from any agent pane TUI
        AgentSessionsRetired, // Direct to TerminalPage, no broadcast — destructive retirement transaction completed
        Broadcast,            // Normalize envelope + broadcast to all subscribers
        Invalid               // Failed validation
    };

    // Classify and validate a SendEvent JSON payload.
    //
    // On success, |outEvt| contains the parsed JSON. For the Broadcast route
    // the envelope is normalized (type=event, method=agent_event).
    //
    // Returns Invalid when:
    //   - JSON parsing fails
    //   - The broadcast path is selected but params.event is missing
    inline SendEventRoute ClassifySendEvent(const std::string& eventJson, Json::Value& outEvt)
    {
        if (!ParseJson(eventJson, outEvt))
        {
            return SendEventRoute::Invalid;
        }

        // Event JSON must be an object to inspect fields.
        if (!outEvt.isObject())
        {
            return SendEventRoute::Invalid;
        }

        // Check method-based direct dispatch routes
        if (outEvt.isMember("method") && outEvt["method"].isString())
        {
            const auto method = outEvt["method"].asString();
            if (method == "autofix_state")
            {
                return SendEventRoute::AutofixState;
            }
            if (method == "agent_status")
            {
                return SendEventRoute::AgentStatus;
            }
            if (method == "switch_agent")
            {
                return SendEventRoute::AgentSwitch;
            }
            if (method == "close_agent_pane")
            {
                return SendEventRoute::CloseAgentPane;
            }
            if (method == "request_default_paste")
            {
                return SendEventRoute::DefaultPaste;
            }
            if (method == "agent_state_changed")
            {
                return SendEventRoute::AgentState;
            }
            if (method == "resume_in_new_agent_tab")
            {
                return SendEventRoute::ResumeInNewAgentTab;
            }
            if (method == "pane_agent_session_changed")
            {
                return SendEventRoute::PaneAgentSession;
            }
            if (method == "set_agent_chip_target")
            {
                return SendEventRoute::AgentChipTarget;
            }
            if (method == "restart_agent_stack")
            {
                return SendEventRoute::RestartAgentStack;
            }
            if (method == "agent_sessions_retired")
            {
                return SendEventRoute::AgentSessionsRetired;
            }
        }

        // Broadcast path: params.event is required
        if (!outEvt.isMember("params") || !outEvt["params"].isObject() ||
            !outEvt["params"].isMember("event"))
        {
            return SendEventRoute::Invalid;
        }

        // Normalize the envelope
        outEvt["type"] = "event";
        outEvt["method"] = "agent_event";

        return SendEventRoute::Broadcast;
    }

    inline void EnsureRequestId(Json::Value& event, const std::string_view requestId)
    {
        auto& params = event["params"];
        if (!params.isObject())
        {
            params = Json::Value{ Json::objectValue };
        }
        if (!params.isMember("request_id") ||
            !params["request_id"].isString() ||
            params["request_id"].asString().empty())
        {
            params["request_id"] = std::string{ requestId };
        }
    }

    // ── SplitPane direction mapping ──

    // Mirror of TerminalSettingsModel::SplitDirection enum values.
    // Kept in sync with ActionArgs.idl.
    enum class SplitDirection
    {
        Automatic = 0,
        Up = 1,
        Right = 2,
        Down = 3,
        Left = 4
    };

    // Map a direction string to a SplitDirection value.
    // Accepts: "right", "left", "up", "down", "auto", "automatic",
    // and legacy values "horizontal" (→ Down) / "vertical" (→ Right).
    // Returns Right for unrecognized strings (matching server default).
    inline SplitDirection ParseSplitDirection(const std::string& direction)
    {
        if (direction.empty())
        {
            return SplitDirection::Right;
        }

        if (direction == "right")
        {
            return SplitDirection::Right;
        }
        if (direction == "left")
        {
            return SplitDirection::Left;
        }
        if (direction == "up")
        {
            return SplitDirection::Up;
        }
        if (direction == "down")
        {
            return SplitDirection::Down;
        }
        if (direction == "auto" || direction == "automatic")
        {
            return SplitDirection::Automatic;
        }
        if (direction == "horizontal")
        {
            return SplitDirection::Down;
        }
        if (direction == "vertical")
        {
            return SplitDirection::Right;
        }

        // Unrecognized — default to Right
        return SplitDirection::Right;
    }

    // ── ReadPaneOutput source routing ──

    enum class PaneOutputSource
    {
        Scrollback,
        Screen,
        LastPrompt
    };

    // Classify the source parameter for ReadPaneOutput.
    inline PaneOutputSource ClassifyPaneOutputSource(const std::string& source)
    {
        if (source == "last_prompt")
        {
            return PaneOutputSource::LastPrompt;
        }
        if (source == "screen")
        {
            return PaneOutputSource::Screen;
        }
        return PaneOutputSource::Scrollback;
    }

    struct PromptCaptureRoute
    {
        std::string_view outputSource;
        std::string_view fallbackReason;
        bool hasMarks;
    };

    inline bool ShouldCapturePromptOutput(const int32_t fallbackLines) noexcept
    {
        return fallbackLines > 0;
    }

    struct PromptTail
    {
        std::string content;
        int32_t lineCount;
        bool truncated;
    };

    inline PromptTail BuildPromptTail(const std::string_view boundedTail, const int32_t maxLines)
    {
        if (maxLines <= 0)
        {
            return {};
        }

        std::vector<std::string> lines;
        std::istringstream stream{ std::string{ boundedTail } };
        std::string line;
        while (std::getline(stream, line))
        {
            if (!line.empty() && line.back() == '\r')
            {
                line.pop_back();
            }
            lines.emplace_back(std::move(line));
        }

        const bool truncated = lines.size() > static_cast<size_t>(maxLines);
        const auto start = truncated ? lines.size() - maxLines : 0;
        std::string content;
        for (auto index = start; index < lines.size(); ++index)
        {
            if (index != start)
            {
                content.push_back('\n');
            }
            content.append(lines[index]);
        }
        return {
            std::move(content),
            static_cast<int32_t>(lines.size() - start),
            truncated,
        };
    }

    inline PromptCaptureRoute ResolvePromptCaptureRoute(bool lastPromptReadSucceeded, bool hasPromptContent) noexcept
    {
        if (lastPromptReadSucceeded && hasPromptContent)
        {
            return { "last_prompt", "", true };
        }
        return {
            "scrollback",
            lastPromptReadSucceeded ? "marks_unavailable" : "last_prompt_error",
            false,
        };
    }
}
