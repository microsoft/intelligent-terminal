// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string>
#include <string_view>

namespace Microsoft::Terminal::AgentSource
{
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
}