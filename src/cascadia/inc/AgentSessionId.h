// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <string_view>

namespace Microsoft::Terminal::AgentSessionId
{
    // CLI resume passes through shells; ACP identifiers remain opaque elsewhere.
    template<typename Char>
    constexpr bool IsSafeForCliResume(const std::basic_string_view<Char> value) noexcept
    {
        constexpr std::string_view sidekick{ "sidekick-" };
        return !value.empty() &&
               value.size() <= 256 &&
               !(value.size() >= sidekick.size() && std::equal(sidekick.begin(), sidekick.end(), value.begin())) &&
               std::all_of(value.begin(), value.end(), [](const Char ch) {
                   return (ch >= 'a' && ch <= 'z') || (ch >= 'A' && ch <= 'Z') ||
                          (ch >= '0' && ch <= '9') || ch == '-' || ch == '_' || ch == '.' || ch == ':';
               });
    }
}
