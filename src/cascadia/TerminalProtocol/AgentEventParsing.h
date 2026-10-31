// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string>
#include <string_view>
#include <optional>
#include <til/u8u16convert.h>

#include "../../terminal/parser/base64.hpp"
#include "../../tools/wtcli/wtcli_functions.h"
#include "../inc/WslDistroName.h"

namespace Microsoft::Terminal::Protocol::Parsing
{
    inline constexpr std::wstring_view AgentEventPrefix{ L"osc:9001;AgentEvent;" };
    inline constexpr std::wstring_view EncodedAgentEventPrefix{ L"osc:9001;AgentEvent64;" };

    inline bool IsAgentEventSequence(const std::wstring_view sequence) noexcept
    {
        return sequence.starts_with(AgentEventPrefix) || sequence.starts_with(EncodedAgentEventPrefix);
    }

    inline bool ParseAgentEventSequence(const std::wstring_view sequence, const std::string& paneId, Json::Value& params)
    {
        if (paneId.empty() || sequence.size() > 2 * 1024 * 1024)
        {
            return false;
        }

        std::wstring decoded;
        std::wstring_view json;
        if (sequence.starts_with(EncodedAgentEventPrefix))
        {
            if (FAILED(::Microsoft::Console::VirtualTerminal::Base64::Decode(sequence.substr(EncodedAgentEventPrefix.size()), decoded)))
            {
                return false;
            }
            json = decoded;
        }
        else if (sequence.starts_with(AgentEventPrefix))
        {
            json = sequence.substr(AgentEventPrefix.size());
        }
        else
        {
            return false;
        }

        Json::Value incoming;
        Json::CharReaderBuilder reader;
        std::string errors;
        std::istringstream stream{ til::u16u8(json) };
        if (!Json::parseFromStream(reader, stream, &incoming, &errors) ||
            !incoming.isObject() || !incoming["event"].isString() ||
            !incoming["event"].asString().starts_with("agent."))
        {
            return false;
        }

        const auto cli = incoming.get("cli_source", Json::Value{});
        if (!cli.isNull() && (!cli.isString() || cli.asString().empty()))
        {
            return false;
        }
        const auto sessionId = incoming.get("agent_session_id", Json::Value{});
        if (!sessionId.isNull() && !sessionId.isString())
        {
            return false;
        }
        if (incoming.isMember("agent") && !incoming["agent"].isString())
        {
            return false;
        }
        const auto distro = incoming.get("wsl_distro", Json::Value{});
        if (!distro.isNull() && (!distro.isString() || !WslDistroName::IsSafe(std::string_view{ distro.asString() })))
        {
            return false;
        }
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        if (cli.isString())
        {
            Json::Value event;
            if (!wtcli::BuildAgentHookEventJson(
                    incoming["event"].asString(), cli.asString(), Json::writeString(writer, incoming.get("payload", Json::Value{})), paneId, sessionId.isString() ? sessionId.asString() : std::string{}, event))
            {
                return false;
            }
            incoming = std::move(event["params"]);
        }

        const auto id = incoming.get("agent_session_id", Json::Value{});
        if (id.isString() && !id.asString().empty() &&
            !::Microsoft::Terminal::AgentSessionId::IsSafeForCliResume(std::string_view{ id.asString() }))
        {
            return false;
        }
        incoming["pane_id"] = paneId;
        for (const auto key : { "tab_id", "window_id", "session_id", "agent_backend", "resume_commandline" })
        {
            incoming.removeMember(key);
        }
        if (distro.isString())
        {
            incoming["wsl_distro"] = distro;
        }
        if (Json::writeString(writer, incoming).size() > wtcli::kMaxHookEventChars)
        {
            return false;
        }
        params = std::move(incoming);
        return true;
    }

    enum class AgentProgressState
    {
        Clear = 0,
        Error = 2,
        Working = 3,
        Attention = 4,
    };

    struct AgentHookProgress
    {
        std::string sessionId;
        std::string cli;
        bool turnActive{};
        bool nativeProgress{};

        void ObserveNativeProgress(const bool active = true) noexcept
        {
            nativeProgress = active;
        }

        void ResetSession() noexcept
        {
            // Explicit OSC progress has a separate lifetime from hook sessions.
            sessionId.clear();
            cli.clear();
            turnActive = false;
        }

        std::optional<AgentProgressState> ShellPrompt()
        {
            const auto clear = !sessionId.empty() && !nativeProgress;
            ResetSession();
            return clear ? std::optional{ AgentProgressState::Clear } : std::nullopt;
        }

        std::optional<AgentProgressState> Apply(const Json::Value& params)
        {
            const auto id = params.get("agent_session_id", "").asString();
            if (id.empty())
            {
                return std::nullopt;
            }
            const auto event = params["event"].asString();
            auto source = params.get("cli_source", "").asString();
            std::transform(source.begin(), source.end(), source.begin(), [](const unsigned char ch) {
                return static_cast<char>(std::tolower(ch));
            });
            const auto& payload = params["payload"];
            const bool start = event == "agent.session.start" || event == "agent.session.started";
            const bool ownsPane = !payload.isObject() || payload.get("pane_owner", Json::Value{}) != Json::Value{ false };
            bool newlyBound = false;
            if (sessionId.empty() || (ownsPane && (start || (event == "agent.prompt.submit" && source != "copilot"))))
            {
                const bool activates = start || event == "agent.prompt.submit" ||
                                       event == "agent.tool.starting" || event == "agent.tool.started" || event == "agent.notification";
                if (!ownsPane || !activates)
                {
                    return std::nullopt;
                }
                if (sessionId != id || cli != source)
                {
                    ResetSession();
                    sessionId = id;
                    cli = source;
                    newlyBound = true;
                }
            }
            if (sessionId != id || cli != source)
            {
                return std::nullopt;
            }

            std::optional<AgentProgressState> state;
            if (start && (newlyBound || (payload.isObject() && payload["running"].isBool())))
            {
                turnActive = payload.isObject() && payload["running"].isBool() && payload["running"].asBool();
                state = turnActive ? AgentProgressState::Working : AgentProgressState::Clear;
            }
            else if (event == "agent.prompt.submit")
            {
                turnActive = true;
                state = AgentProgressState::Working;
            }
            else if (event == "agent.tool.starting" || event == "agent.tool.started")
            {
                turnActive = true;
                const auto tool = payload.isObject() ? payload.get("tool_name", payload.get("toolName", Json::Value{})) : Json::Value{};
                state = tool.isString() && wtcli::IsAgentUserInputTool(tool.asString()) ?
                            AgentProgressState::Attention :
                            AgentProgressState::Working;
            }
            else if ((event == "agent.tool.finished" || event == "agent.tool.completed") && cli == "copilot" && turnActive)
            {
                state = AgentProgressState::Working;
            }
            else if (event == "agent.notification")
            {
                const auto typeValue = payload.isObject() ? payload.get("notification_type", payload.get("notificationType", Json::Value{})) : Json::Value{};
                const auto type = typeValue.isString() ? typeValue.asString() : std::string{};
                if (type == "idle_prompt" ||
                    (cli == "copilot" && (type == "shell_completed" || type == "shell_detached_completed" || type == "agent_completed" || type == "agent_idle")))
                {
                    return std::nullopt;
                }
                turnActive = true;
                state = AgentProgressState::Attention;
            }
            else if (event == "agent.stop" || event == "agent.error" ||
                     event == "agent.session.end" || event == "agent.session.stopped" || event == "agent.session.unbind")
            {
                turnActive = false;
                state = event == "agent.error" ? AgentProgressState::Error : AgentProgressState::Clear;
            }
            const auto result = nativeProgress ? std::nullopt : state;
            if (event == "agent.session.end" || event == "agent.session.stopped" || event == "agent.session.unbind")
            {
                ResetSession();
            }
            return result;
        }
    };
}
