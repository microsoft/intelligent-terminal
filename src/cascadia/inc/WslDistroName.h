// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <string_view>

namespace Microsoft::Terminal::WslDistroName
{
    template<typename Char>
    constexpr bool IsSafe(const std::basic_string_view<Char> name) noexcept
    {
        return !name.empty() && name.size() <= 256 &&
               std::all_of(name.begin(), name.end(), [](const Char ch) {
                   return (ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
                          (ch >= '0' && ch <= '9') || ch == '.' || ch == '-' || ch == '_' || ch == '+';
               });
    }
}
