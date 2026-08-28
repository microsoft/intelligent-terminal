// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <wil/win32_helpers.h>

#include <string>
#include <string_view>
#include <utility>

namespace Microsoft::Terminal::AgentSource
{
    struct ResolvedCwds
    {
        std::wstring agent;
        std::wstring helper;
    };

    inline std::wstring ReadEnvironmentVariable(const wchar_t* name)
    {
        return wil::TryGetEnvironmentVariableW<std::wstring>(name);
    }

    inline std::wstring ResolveCwd(
        const std::wstring_view paneCwd,
        const std::wstring_view windowCwd,
        const std::wstring_view profileCwd,
        const std::wstring_view homeCwd)
    {
        for (const auto candidate : { paneCwd, windowCwd, profileCwd, homeCwd })
        {
            if (!candidate.empty())
            {
                return std::wstring{ candidate };
            }
        }
        return {};
    }

    template<typename IsWindowsDirectory>
    inline ResolvedCwds ResolveAgentAndHelperCwds(
        const bool agentRunsInWsl,
        const std::wstring_view paneCwd,
        const std::wstring_view windowCwd,
        const std::wstring_view profileCwd,
        const std::wstring_view homeCwd,
        IsWindowsDirectory&& isWindowsDirectory)
    {
        std::wstring helperCwd;
        for (const auto candidate : { paneCwd, windowCwd, profileCwd, homeCwd })
        {
            if (!candidate.empty() && isWindowsDirectory(candidate))
            {
                helperCwd = candidate;
                break;
            }
        }

        auto agentCwd = agentRunsInWsl ?
                            ResolveCwd(paneCwd, windowCwd, profileCwd, homeCwd) :
                            helperCwd;
        return { std::move(agentCwd), std::move(helperCwd) };
    }
}