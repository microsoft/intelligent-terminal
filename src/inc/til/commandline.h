// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string>
#include <string_view>

namespace til
{
    // Quote one Windows argv value, without wt's additional action-separator escaping.
    inline void append_quoted_argument(std::wstring& out, const std::wstring_view value)
    {
        out.push_back(L'"');
        size_t backslashes = 0;
        for (const auto ch : value)
        {
            if (ch == L'\\')
            {
                ++backslashes;
            }
            else
            {
                if (ch == L'"')
                {
                    out.append(backslashes + 1, L'\\');
                }
                backslashes = 0;
            }
            out.push_back(ch);
        }
        out.append(backslashes, L'\\');
        out.push_back(L'"');
    }
}
