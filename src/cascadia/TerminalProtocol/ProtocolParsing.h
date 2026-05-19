// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Extracted pure-parsing functions from the Terminal Protocol server layer
// for fuzzing and testability. These functions have no COM, WinRT, or XAML
// dependencies and can be called from a LibFuzzer harness.

#pragma once

#include <string>
#include <sstream>

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
        CloseAgentPane,       // Direct to TerminalPage, no broadcast
        ViewChanged,          // Direct to TerminalPage, no broadcast
        ResumeInNewAgentTab,  // Direct to TerminalPage, no broadcast
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

        // autofix_state — direct dispatch, no broadcast
        if (outEvt.isMember("method") && outEvt["method"].isString() &&
            outEvt["method"].asString() == "autofix_state")
        {
            return SendEventRoute::AutofixState;
        }

        // agent_status — direct dispatch, no broadcast
        if (outEvt.isMember("method") && outEvt["method"].isString() &&
            outEvt["method"].asString() == "agent_status")
        {
            return SendEventRoute::AgentStatus;
        }

        // close_agent_pane — direct dispatch, no broadcast.
        // Emitted by the wta TUI when the user presses Ctrl+C twice.
        if (outEvt.isMember("method") && outEvt["method"].isString() &&
            outEvt["method"].asString() == "close_agent_pane")
        {
            return SendEventRoute::CloseAgentPane;
        }

        // view_changed — direct dispatch, no broadcast.
        // Emitted by the wta TUI when its internal view flips
        // (Esc out of session view, `/sessions` slash command).
        if (outEvt.isMember("method") && outEvt["method"].isString() &&
            outEvt["method"].asString() == "view_changed")
        {
            return SendEventRoute::ViewChanged;
        }

        // resume_in_new_agent_tab — direct dispatch, no broadcast.
        // Emitted by the wta TUI's session management view on Shift+Enter
        // over a historical session row. Carries {session_id, cwd}; WT
        // creates a new tab + agent pane and calls back with `load_session`.
        if (outEvt.isMember("method") && outEvt["method"].isString() &&
            outEvt["method"].asString() == "resume_in_new_agent_tab")
        {
            return SendEventRoute::ResumeInNewAgentTab;
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

    // ── QuickPick choices validation ──

    // Validate that a JSON string is a valid array (for QuickPick choices).
    // On success, |outChoices| contains the parsed array.
    inline bool ValidateQuickPickChoices(const std::string& choicesJson, Json::Value& outChoices)
    {
        if (!ParseJson(choicesJson, outChoices))
        {
            return false;
        }
        return outChoices.isArray();
    }

    // ── SetSettings validation ──

    // Validate that a string is non-empty valid JSON (for SetSettings).
    inline bool ValidateSettingsJson(const std::string& settingsJson)
    {
        if (settingsJson.empty())
        {
            return false;
        }
        Json::Value parsed;
        return ParseJson(settingsJson, parsed);
    }
}
