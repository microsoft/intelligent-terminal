// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "../inc/AgentPolicy.h"

namespace TerminalApp::AgentPolicyTelemetry
{
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
