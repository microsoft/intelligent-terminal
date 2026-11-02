// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "../inc/AgentPolicy.h"
#include <charconv>
#include <string_view>

namespace TerminalApp::AgentPolicyTelemetry
{
    inline bool ShouldRecordBlockedCommandFailure(
        const bool detectionEnabled,
        const ::Microsoft::Terminal::Settings::Model::AgentPolicy::PolicyState policy,
        const std::string_view sequence) noexcept
    {
        using ::Microsoft::Terminal::Settings::Model::AgentPolicy::PolicyState;
        constexpr std::string_view prefix{ "osc:133;D;" };
        if (!detectionEnabled || policy != PolicyState::Blocked || !sequence.starts_with(prefix))
        {
            return false;
        }
        auto code = sequence.substr(prefix.size());
        const auto first = code.find_first_not_of(" \t\r\n");
        if (first == std::string_view::npos)
        {
            return false;
        }
        code = code.substr(first, code.find_last_not_of(" \t\r\n") - first + 1);
        if (code.front() == '+')
        {
            code.remove_prefix(1);
            if (code.starts_with('-'))
            {
                return false;
            }
        }
        if (code.empty())
        {
            return false;
        }
        int exitCode{};
        const auto [end, error] = std::from_chars(code.data(), code.data() + code.size(), exitCode);
        return error == std::errc{} && end == code.data() + code.size() && exitCode != 0;
    }

    constexpr const char* AutoFixPolicyName(const ::Microsoft::Terminal::Settings::Model::AgentPolicy::PolicyState state) noexcept
    {
        using ::Microsoft::Terminal::Settings::Model::AgentPolicy::PolicyState;
        switch (state)
        {
        case PolicyState::NotConfigured:
            return "notConfigured";
        case PolicyState::Allowed:
            return "enabled";
        case PolicyState::Blocked:
            return "disabled";
        default:
            return "unknown";
        }
    }
}
