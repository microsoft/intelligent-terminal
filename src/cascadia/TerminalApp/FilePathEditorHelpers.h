// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <filesystem>
#include <iterator>
#include <limits>
#include <optional>
#include <ranges>
#include <span>
#include <string>
#include <string_view>
#include <vector>

namespace winrt::TerminalApp::implementation::FilePathEditorHelpers
{
    struct FileUriTarget
    {
        std::wstring uri;
        std::optional<uint32_t> line;
        std::optional<uint32_t> column;
    };

    inline bool TryParsePositiveInteger(const std::wstring_view text, uint32_t& value) noexcept
    {
        if (text.empty())
        {
            return false;
        }

        uint32_t result = 0;
        for (const auto ch : text)
        {
            if (ch < L'0' || ch > L'9')
            {
                return false;
            }
            const auto digit = static_cast<uint32_t>(ch - L'0');
            if (result > (std::numeric_limits<uint32_t>::max() - digit) / 10)
            {
                return false;
            }
            result = result * 10 + digit;
        }
        if (result == 0)
        {
            return false;
        }
        value = result;
        return true;
    }

    inline std::optional<FileUriTarget> ParseFileUriTarget(const std::wstring_view uri, const bool parseGeneratedLocation)
    {
        FileUriTarget result{ std::wstring{ uri } };
        if (!parseGeneratedLocation ||
            uri.size() < 5 ||
            !((uri[0] == L'f' || uri[0] == L'F') &&
              (uri[1] == L'i' || uri[1] == L'I') &&
              (uri[2] == L'l' || uri[2] == L'L') &&
              (uri[3] == L'e' || uri[3] == L'E') &&
              uri[4] == L':'))
        {
            return result;
        }
        const auto fragment = uri.rfind(L"#L");
        if (fragment == std::wstring_view::npos)
        {
            return result;
        }

        const auto location = uri.substr(fragment + 2);
        const auto separator = location.find(L':');
        uint32_t line = 0;
        if (separator == std::wstring_view::npos)
        {
            if (!TryParsePositiveInteger(location, line))
            {
                return std::nullopt;
            }
        }
        else
        {
            uint32_t column = 0;
            if (location.find(L':', separator + 1) != std::wstring_view::npos ||
                !TryParsePositiveInteger(location.substr(0, separator), line) ||
                !TryParsePositiveInteger(location.substr(separator + 1), column))
            {
                return std::nullopt;
            }
            result.column = column;
        }

        result.uri.resize(fragment);
        result.line = line;
        return result;
    }

    inline std::wstring EditorBasename(const std::wstring_view executable)
    {
        auto name = std::filesystem::path{ executable }.filename().native();
        std::transform(name.begin(), name.end(), name.begin(), [](const auto ch) {
            return static_cast<wchar_t>(ch >= L'A' && ch <= L'Z' ? ch + (L'a' - L'A') : ch);
        });
        static constexpr std::wstring_view extensions[]{ L".exe", L".com", L".cmd", L".bat" };
        for (const auto extension : extensions)
        {
            if (name.ends_with(extension))
            {
                name.resize(name.size() - extension.size());
                break;
            }
        }
        return name;
    }

    inline std::vector<std::filesystem::path> EditorShimExecutableCandidates(const std::filesystem::path& shimPath)
    {
        const auto editor = EditorBasename(shimPath.native());
        std::span<const std::wstring_view> executableNames;
        static constexpr std::wstring_view code[]{ L"Code.exe" };
        static constexpr std::wstring_view codeInsiders[]{ L"Code - Insiders.exe" };
        static constexpr std::wstring_view codium[]{ L"VSCodium.exe", L"Codium.exe" };
        static constexpr std::wstring_view cursor[]{ L"Cursor.exe" };
        static constexpr std::wstring_view windsurf[]{ L"Windsurf.exe" };

        if (editor == L"code")
        {
            executableNames = code;
        }
        else if (editor == L"code-insiders")
        {
            executableNames = codeInsiders;
        }
        else if (editor == L"codium" || editor == L"vscodium")
        {
            executableNames = codium;
        }
        else if (editor == L"cursor")
        {
            executableNames = cursor;
        }
        else if (editor == L"windsurf")
        {
            executableNames = windsurf;
        }
        else
        {
            return {};
        }

        std::vector<std::filesystem::path> candidates;
        const auto shimDirectory = shimPath.parent_path();
        const auto addCandidates = [&](const std::filesystem::path& directory) {
            if (directory.empty())
            {
                return;
            }
            for (const auto executableName : executableNames)
            {
                const auto candidate = directory / executableName;
                if (std::ranges::find(candidates, candidate) == candidates.end())
                {
                    candidates.emplace_back(candidate);
                }
            }
        };

        // VS Code/VSCodium place the shim in <root>\bin. Cursor/Windsurf use
        // <root>\resources\app\bin. A same-directory executable is also safe.
        addCandidates(shimDirectory);
        addCandidates(shimDirectory.parent_path());
        auto appRoot = shimDirectory;
        for (size_t depth = 0; depth < 3 && !appRoot.empty(); ++depth)
        {
            appRoot = appRoot.parent_path();
        }
        addCandidates(appRoot);
        return candidates;
    }

    inline std::wstring PathWithLocation(const std::wstring_view path,
                                         const std::optional<uint32_t> line,
                                         const std::optional<uint32_t> column,
                                         const bool defaultColumn)
    {
        std::wstring result{ path };
        if (line)
        {
            result.push_back(L':');
            result.append(std::to_wstring(*line));
            if (column || defaultColumn)
            {
                result.push_back(L':');
                result.append(std::to_wstring(column.value_or(1)));
            }
        }
        return result;
    }

    inline std::vector<std::wstring> BuildEditorArguments(const std::wstring_view executable,
                                                          const std::span<const std::wstring> configuredArguments,
                                                          const std::wstring_view path,
                                                          const std::optional<uint32_t> line,
                                                          const std::optional<uint32_t> column)
    {
        std::vector<std::wstring> arguments{ configuredArguments.begin(), configuredArguments.end() };
        const auto editor = EditorBasename(executable);

        if (editor == L"code" ||
            editor == L"code-insiders" ||
            editor == L"code - insiders" ||
            editor == L"codium" ||
            editor == L"vscodium" ||
            editor == L"cursor" ||
            editor == L"windsurf")
        {
            if (line)
            {
                arguments.emplace_back(L"--goto");
                arguments.emplace_back(PathWithLocation(path, line, column, true));
            }
            else
            {
                arguments.emplace_back(path);
            }
        }
        else if (editor == L"vim" || editor == L"nvim" || editor == L"gvim")
        {
            if (line)
            {
                arguments.emplace_back(L"+" + std::to_wstring(*line));
            }
            arguments.emplace_back(path);
        }
        else if (editor == L"emacs" || editor == L"emacsclient")
        {
            if (line)
            {
                auto location = L"+" + std::to_wstring(*line);
                if (column)
                {
                    location.push_back(L':');
                    location.append(std::to_wstring(*column));
                }
                arguments.emplace_back(std::move(location));
            }
            arguments.emplace_back(path);
        }
        else if (editor == L"zed")
        {
            arguments.emplace_back(PathWithLocation(path, line, column, true));
        }
        else if (editor == L"notepad++")
        {
            if (line)
            {
                arguments.emplace_back(L"-n" + std::to_wstring(*line));
            }
            if (column)
            {
                arguments.emplace_back(L"-c" + std::to_wstring(*column));
            }
            arguments.emplace_back(path);
        }
        else
        {
            static constexpr std::wstring_view jetBrainsEditors[]{
                L"idea",
                L"idea64",
                L"pycharm",
                L"pycharm64",
                L"webstorm",
                L"webstorm64",
                L"clion",
                L"clion64",
                L"rider",
                L"rider64",
                L"goland",
                L"goland64",
                L"phpstorm",
                L"phpstorm64",
                L"datagrip",
                L"datagrip64",
                L"rustrover",
                L"rustrover64",
            };
            if (std::ranges::find(jetBrainsEditors, editor) != std::end(jetBrainsEditors) && line)
            {
                arguments.emplace_back(L"--line");
                arguments.emplace_back(std::to_wstring(*line));
            }
            arguments.emplace_back(path);
        }
        return arguments;
    }
}
