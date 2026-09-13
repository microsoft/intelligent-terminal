// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <initializer_list>
#include <optional>
#include <string>
#include <string_view>
#include <json/json.h>

namespace TerminalApp::AgentSessionTelemetry
{
    template<typename Char>
    constexpr const Char* Bucket(
        std::basic_string_view<Char> value,
        std::initializer_list<const Char*> allowed,
        const Char* other) noexcept
    {
        for (const auto candidate : allowed)
        {
            if (value == candidate)
            {
                return candidate;
            }
        }
        return other;
    }

    inline const char* AgentId(std::string_view value) noexcept
    {
        return value.empty() ? "none" :
                               Bucket<char>(value, { "copilot", "claude", "codex", "gemini", "opencode" }, "custom");
    }

    struct Start
    {
        std::string startId;
        std::string sessionId;
        const char* kind;
        const char* agentId;
        const char* source;
        const char* delegateAgentId;
        const char* modelSource;
        bool autofix;
        const char* automaticYolo;
        bool yoloPolicyBlocked;
        const char* yoloControlOwner;
    };

    inline std::optional<Start> Parse(const Json::Value& value)
    {
        if (!value.isObject())
        {
            return std::nullopt;
        }
        for (const auto name : { "start_id", "session_id", "start_kind", "agent_id", "agent_source", "model_source" })
        {
            if (!value[name].isString() || value[name].asString().empty())
            {
                return std::nullopt;
            }
        }
        if (!value["autofix_enabled"].isBool() || !value["yolo_policy_blocked"].isBool() ||
            !value.isMember("automatic_yolo") || !(value["automatic_yolo"].isNull() || value["automatic_yolo"].isBool()) ||
            !value.isMember("delegate_agent_id") || !(value["delegate_agent_id"].isNull() || value["delegate_agent_id"].isString()) ||
            !value.isMember("yolo_control_owner") || !(value["yolo_control_owner"].isNull() || value["yolo_control_owner"].isString()))
        {
            return std::nullopt;
        }
        const auto kind = value["start_kind"].asString();
        if (kind != "New" && kind != "Load")
        {
            return std::nullopt;
        }
        return Start{
            value["start_id"].asString(),
            value["session_id"].asString(),
            kind == "New" ? "New" : "Load",
            AgentId(value["agent_id"].asString()),
            Bucket<char>(value["agent_source"].asString(), { "host", "wsl" }, "unknown"),
            AgentId(value["delegate_agent_id"].asString()),
            Bucket<char>(value["model_source"].asString(), { "byok", "provider" }, "unknown"),
            value["autofix_enabled"].asBool(),
            value["automatic_yolo"].isNull() ? "provider" : (value["automatic_yolo"].asBool() ? "enabled" : "disabled"),
            value["yolo_policy_blocked"].asBool(),
            Bucket<char>(value["yolo_control_owner"].asString(), { "automatic", "manual", "provider-restored" }, "unknown"),
        };
    }
}
