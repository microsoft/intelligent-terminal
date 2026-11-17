// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::Omni
{
    inline bool IsPosixDirectory(const std::wstring_view path) noexcept
    {
        return path.starts_with(L'/') && !path.starts_with(L"//");
    }

    inline std::wstring NormalizeDirectory(const std::wstring_view path, const bool posix)
    {
        std::wstring result{ path };
        if (!posix)
            std::replace(result.begin(), result.end(), L'/', L'\\');
        const auto separator = posix ? L'/' : L'\\';
        while (result.size() > 1 && result.back() == separator && !(result.size() == 3 && result[1] == L':'))
            result.pop_back();
        return result;
    }

    inline bool DirectoryEquals(const std::wstring_view left, const std::wstring_view right, const bool posix)
    {
        return posix ? left == right :
                       CompareStringOrdinal(left.data(), static_cast<int>(left.size()), right.data(), static_cast<int>(right.size()), TRUE) == CSTR_EQUAL;
    }

    inline bool DirectoryMatches(const std::wstring_view cwd, const std::wstring_view selector)
    {
        if (cwd.empty() || selector.empty())
            return false;
        const auto posix = IsPosixDirectory(cwd);
        const auto path = NormalizeDirectory(cwd, posix);
        const auto query = NormalizeDirectory(selector, posix);
        if (DirectoryEquals(path, query, posix))
            return true;
        // Only a complete path or the final folder name is an identity.
        if (query.find_first_of(L"/\\:") != std::wstring::npos || query == L"." || query == L"..")
            return false;
        const auto separator = path.find_last_of(posix ? L"/" : L"\\");
        return separator != std::wstring::npos && DirectoryEquals(std::wstring_view{ path }.substr(separator + 1), query, posix);
    }

    inline bool DirectorySelectorInPrompt(const std::wstring_view prompt, const std::wstring_view selector, const bool posix = false)
    {
        // Do not allow the planner to silently expand an ambiguous folder name
        // into one candidate's full path.
        const auto text = NormalizeDirectory(prompt, posix);
        const auto query = NormalizeDirectory(selector, posix);
        if (query.empty() || query.size() > text.size())
            return false;
        for (size_t i = 0; i + query.size() <= text.size(); ++i)
        {
            if (std::wstring_view{ text }.substr(i, query.size()) == query)
                return true;
        }
        return false;
    }
}
