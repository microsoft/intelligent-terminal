// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - FileHyperlink.h
//
// Abstract:
// - Resolves terminal file hyperlinks, including optional line and column.

#pragma once

#include <filesystem>
#include <optional>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::Control
{
    struct FileHyperlinkTarget
    {
        std::filesystem::path Path;
        std::optional<uint32_t> Line;
        std::optional<uint32_t> Column;
    };

    inline std::optional<FileHyperlinkTarget> ResolveFileHyperlink(const std::wstring_view text,
                                                                   const std::wstring_view workingDirectory) noexcept
    try
    {
        if (text.empty() || text.find(L"://") != std::wstring_view::npos)
        {
            return std::nullopt;
        }

        auto pathEnd = text.size();
        const auto parseTrailingNumber = [&](const size_t end) -> std::optional<std::pair<size_t, uint32_t>> {
            const auto colon = text.rfind(L':', end - 1);
            if (colon == std::wstring_view::npos || colon + 1 >= end)
            {
                return std::nullopt;
            }

            uint64_t value = 0;
            for (auto i = colon + 1; i < end; ++i)
            {
                const auto ch = text[i];
                if (ch < L'0' || ch > L'9')
                {
                    return std::nullopt;
                }
                value = value * 10 + static_cast<uint32_t>(ch - L'0');
                if (value > UINT32_MAX)
                {
                    return std::nullopt;
                }
            }
            return std::pair{ colon, static_cast<uint32_t>(value) };
        };

        std::optional<uint32_t> line;
        std::optional<uint32_t> column;
        if (const auto last = parseTrailingNumber(pathEnd))
        {
            pathEnd = last->first;
            line = last->second;
            if (const auto previous = parseTrailingNumber(pathEnd))
            {
                pathEnd = previous->first;
                column = line;
                line = previous->second;
            }
        }

        std::filesystem::path path{ text.substr(0, pathEnd) };
        if (path.empty())
        {
            return std::nullopt;
        }
        if (path.is_relative())
        {
            if (workingDirectory.empty())
            {
                return std::nullopt;
            }
            path = std::filesystem::path{ workingDirectory } / path;
        }

        std::error_code error;
        path = std::filesystem::weakly_canonical(path, error);
        if (error || !std::filesystem::is_regular_file(path, error) || error)
        {
            return std::nullopt;
        }

        return FileHyperlinkTarget{ std::move(path), line, column };
    }
    catch (...)
    {
        return std::nullopt;
    }
}
