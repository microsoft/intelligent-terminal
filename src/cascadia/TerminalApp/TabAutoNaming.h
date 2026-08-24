/*++
Copyright (c) Microsoft Corporation
Licensed under the MIT license.

Module Name:
- TabAutoNaming.h

Abstract:
- Deterministic, offline helpers that turn a tab's shell-integration context
  into a readable durable-session name, so the recents list stops being a wall
  of identical shell names.
- Everything here is either a pure transform over data we already own, or one
  cheap read of `.git/HEAD`. No AI, no network, no `git.exe`, no libgit2 - the
  same inputs always produce the same name.
- Consumed by TerminalPage::_PersistDurableTabSession at tab-close time only.
  Nothing on this path runs per-frame, which is why it can afford to call the
  (relatively expensive) TermControl::CommandHistory() once.

Author(s):
- Intelligent Terminal contributors
--*/

#pragma once

#include <algorithm>
#include <array>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

namespace winrt::TerminalApp::implementation
{
    // U+00B7 MIDDLE DOT, spaced. Written as an escape so the meaning of this
    // literal does not depend on how the file is decoded.
    inline constexpr std::wstring_view TabAutoNameSeparator{ L" \u00B7 " };

    // A generated name shares its row with the working directory (see
    // durable_tab_sessions_view.rs), and that row drops the cwd column entirely
    // once the name gets too wide. Cap the name so the cwd stays visible.
    inline constexpr size_t TabAutoNameMaxLength{ 48 };

    // An unrecognized command is shown verbatim rather than guessed at, but a
    // full command line can be arbitrarily long.
    inline constexpr size_t TabAutoNameMaxTaskLength{ 32 };

    // Upward search for a `.git` entry is bounded: a deep or unlucky path
    // should never turn tab close into a directory walk.
    inline constexpr size_t TabAutoNameMaxGitWalkDepth{ 32 };

#pragma region string helpers

    inline std::wstring_view TrimAscii(std::wstring_view value) noexcept
    {
        constexpr std::wstring_view whitespace{ L" \t\r\n" };
        const auto begin = value.find_first_not_of(whitespace);
        if (begin == std::wstring_view::npos)
        {
            return {};
        }
        const auto end = value.find_last_not_of(whitespace);
        return value.substr(begin, end - begin + 1);
    }

    inline std::wstring ToLowerAscii(std::wstring_view value)
    {
        std::wstring lowered{ value };
        std::transform(lowered.begin(), lowered.end(), lowered.begin(), [](wchar_t c) noexcept {
            return (c >= L'A' && c <= L'Z') ? static_cast<wchar_t>(c - L'A' + L'a') : c;
        });
        return lowered;
    }

    // Truncates on a character boundary, appending an ellipsis when it cuts.
    // Guards against splitting a surrogate pair, which would emit a lone
    // surrogate into the session name.
    inline std::wstring TruncateForName(std::wstring_view value, const size_t maxLength)
    {
        if (value.size() <= maxLength || maxLength == 0)
        {
            return std::wstring{ value };
        }

        auto cut = maxLength - 1;
        if (cut > 0 && value[cut - 1] >= 0xD800 && value[cut - 1] <= 0xDBFF)
        {
            // Never leave a high surrogate as the last retained unit.
            --cut;
        }

        std::wstring truncated{ value.substr(0, cut) };
        while (!truncated.empty() && (truncated.back() == L' ' || truncated.back() == L'\t'))
        {
            truncated.pop_back();
        }
        truncated.push_back(L'\u2026');
        return truncated;
    }

    // Splits off the leading whitespace-delimited token, honoring double quotes
    // so a quoted path stays one token. Returns {token, remainder}.
    inline std::pair<std::wstring_view, std::wstring_view> SplitLeadingToken(std::wstring_view commandline) noexcept
    {
        const auto trimmed = TrimAscii(commandline);
        if (trimmed.empty())
        {
            return { {}, {} };
        }

        size_t index = 0;
        auto inQuotes = false;
        for (; index < trimmed.size(); ++index)
        {
            const auto c = trimmed[index];
            if (c == L'"')
            {
                inQuotes = !inQuotes;
                continue;
            }
            if (!inQuotes && (c == L' ' || c == L'\t'))
            {
                break;
            }
        }

        return { trimmed.substr(0, index), TrimAscii(trimmed.substr(index)) };
    }

#pragma endregion

#pragma region git

    inline bool IsHexRun(std::wstring_view value) noexcept
    {
        return !value.empty() && std::all_of(value.begin(), value.end(), [](wchar_t c) noexcept {
            return (c >= L'0' && c <= L'9') || (c >= L'a' && c <= L'f') || (c >= L'A' && c <= L'F');
        });
    }

    // Parses the contents of a `.git/HEAD`.
    // - "ref: refs/heads/main"       -> "main"
    // - "ref: refs/heads/feat/x"     -> "feat/x"  (slashes in branch names are kept)
    // - a bare object id             -> short form, e.g. "a1b2c3d" (detached HEAD)
    // Kept separate from any file IO so it is directly unit-testable.
    inline std::optional<std::wstring> ParseGitHeadContent(std::wstring_view headContent)
    {
        const auto trimmed = TrimAscii(headContent);
        if (trimmed.empty())
        {
            return std::nullopt;
        }

        constexpr std::wstring_view refPrefix{ L"ref:" };
        if (trimmed.starts_with(refPrefix))
        {
            const auto ref = TrimAscii(trimmed.substr(refPrefix.size()));
            if (ref.empty())
            {
                return std::nullopt;
            }

            constexpr std::wstring_view headsPrefix{ L"refs/heads/" };
            if (ref.starts_with(headsPrefix))
            {
                const auto branch = ref.substr(headsPrefix.size());
                return branch.empty() ? std::nullopt : std::optional{ std::wstring{ branch } };
            }

            // Some other ref namespace (refs/tags/..., a bare name, ...). The
            // trailing segment is still the most meaningful part.
            const auto lastSlash = ref.find_last_of(L'/');
            const auto tail = lastSlash == std::wstring_view::npos ? ref : ref.substr(lastSlash + 1);
            return tail.empty() ? std::nullopt : std::optional{ std::wstring{ tail } };
        }

        // Detached HEAD: git writes the raw object id. SHA-1 repositories use
        // 40 hex digits, SHA-256 repositories use 64.
        if ((trimmed.size() == 40 || trimmed.size() == 64) && IsHexRun(trimmed))
        {
            return std::wstring{ trimmed.substr(0, 7) };
        }

        return std::nullopt;
    }

    // Parses the contents of a `.git` *file*, which git writes instead of a
    // directory for linked worktrees and submodules:
    //     gitdir: C:/repo/.git/worktrees/feature
    // This is not an edge case for this repository - development happens in
    // `.worktree/<name>` checkouts, where `.git` is always a file.
    inline std::optional<std::wstring> ParseGitDirPointer(std::wstring_view gitFileContent)
    {
        const auto trimmed = TrimAscii(gitFileContent);
        constexpr std::wstring_view gitdirPrefix{ L"gitdir:" };
        if (!trimmed.starts_with(gitdirPrefix))
        {
            return std::nullopt;
        }

        const auto target = TrimAscii(trimmed.substr(gitdirPrefix.size()));
        return target.empty() ? std::nullopt : std::optional{ std::wstring{ target } };
    }

    // Reads a small text file without throwing. Returns nullopt for anything
    // unreadable, oversized, or not valid UTF-8.
    inline std::optional<std::wstring> TryReadSmallTextFile(const std::filesystem::path& path)
    {
        std::error_code error;
        if (!std::filesystem::is_regular_file(path, error) || error)
        {
            return std::nullopt;
        }

        const auto size = std::filesystem::file_size(path, error);
        if (error || size > 64u * 1024u)
        {
            return std::nullopt;
        }

        std::string bytes;
        try
        {
            std::ifstream stream{ path, std::ios::binary };
            if (!stream)
            {
                return std::nullopt;
            }
            bytes.assign(std::istreambuf_iterator<char>{ stream }, std::istreambuf_iterator<char>{});
        }
        catch (...)
        {
            return std::nullopt;
        }

        // Branch names may be UTF-8; decode properly rather than widening bytes.
        std::wstring wide;
        if (FAILED(til::u8u16(bytes, wide)))
        {
            return std::nullopt;
        }
        return wide;
    }

    inline bool IsNetworkPath(const std::filesystem::path& path)
    {
        const auto native = path.native();
        return native.starts_with(L"\\\\") || native.starts_with(L"//");
    }

    inline std::optional<std::wstring> ReadBranchFromGitDir(const std::filesystem::path& gitDirectory)
    {
        if (const auto head = TryReadSmallTextFile(gitDirectory / L"HEAD"))
        {
            return ParseGitHeadContent(*head);
        }
        return std::nullopt;
    }

    // Walks up from `startingDirectory` looking for a `.git` entry and returns
    // the checked-out branch (or a short object id when detached).
    //
    // Deliberately cheap and best-effort: any failure yields nullopt and the
    // caller simply omits the branch segment. Network paths are skipped so a
    // disconnected share cannot stall closing a tab.
    inline std::optional<std::wstring> TryReadGitBranch(const std::filesystem::path& startingDirectory)
    {
        if (startingDirectory.empty() || IsNetworkPath(startingDirectory))
        {
            return std::nullopt;
        }

        std::error_code error;
        auto current = std::filesystem::absolute(startingDirectory, error);
        if (error)
        {
            current = startingDirectory;
        }

        for (size_t depth = 0; depth < TabAutoNameMaxGitWalkDepth && !current.empty(); ++depth)
        {
            const auto gitEntry = current / L".git";

            if (std::filesystem::is_directory(gitEntry, error) && !error)
            {
                return ReadBranchFromGitDir(gitEntry);
            }

            if (std::filesystem::is_regular_file(gitEntry, error) && !error)
            {
                const auto pointer = TryReadSmallTextFile(gitEntry);
                if (!pointer)
                {
                    return std::nullopt;
                }
                const auto target = ParseGitDirPointer(*pointer);
                if (!target)
                {
                    return std::nullopt;
                }

                std::filesystem::path realGitDirectory{ *target };
                if (realGitDirectory.is_relative())
                {
                    realGitDirectory = current / realGitDirectory;
                }
                return ReadBranchFromGitDir(realGitDirectory);
            }

            auto parent = current.parent_path();
            if (parent == current)
            {
                break;
            }
            current = std::move(parent);
        }

        return std::nullopt;
    }

#pragma endregion

#pragma region command labels

    // Prefixes that describe *how* a command runs rather than what it is, and
    // so are stripped before matching. `env`-style KEY=VALUE assignments are
    // handled separately below.
    inline constexpr std::array<std::wstring_view, 6> TabAutoNameIgnoredPrefixes{
        L"sudo",
        L"doas",
        L"env",
        L"time",
        L"command",
        L"npx",
    };

    // Package managers whose `run <script>` form should be read as the script.
    inline constexpr std::array<std::wstring_view, 4> TabAutoNamePackageManagers{
        L"npm",
        L"pnpm",
        L"yarn",
        L"bun",
    };

    // Scripts common enough across ecosystems to deserve a friendly label.
    // English literals by design: the underlying commands are English too, and
    // localizing these would cost far more than the table itself is worth.
    inline constexpr std::array<std::pair<std::wstring_view, std::wstring_view>, 8> TabAutoNameScriptLabels{ {
        { L"dev", L"dev server" },
        { L"start", L"dev server" },
        { L"serve", L"dev server" },
        { L"watch", L"watch" },
        { L"build", L"build" },
        { L"test", L"tests" },
        { L"lint", L"lint" },
        { L"typecheck", L"typecheck" },
    } };

    // Exact matches on the first one-to-three lowercased tokens. Longest key
    // wins, so "docker compose up" beats "docker".
    inline constexpr std::array<std::pair<std::wstring_view, std::wstring_view>, 24> TabAutoNameCommandLabels{ {
        { L"docker compose up", L"compose" },
        { L"docker compose", L"compose" },
        { L"docker build", L"docker build" },
        { L"python -m pytest", L"python tests" },
        { L"python -m http.server", L"http server" },
        { L"pytest", L"python tests" },
        { L"cargo run", L"cargo run" },
        { L"cargo build", L"cargo build" },
        { L"cargo test", L"cargo tests" },
        { L"cargo watch", L"cargo watch" },
        { L"go run", L"go run" },
        { L"go test", L"go tests" },
        { L"dotnet run", L"dotnet run" },
        { L"dotnet test", L"dotnet tests" },
        { L"dotnet watch", L"dotnet watch" },
        { L"msbuild", L"build" },
        { L"make", L"make" },
        { L"terraform apply", L"terraform apply" },
        { L"terraform plan", L"terraform plan" },
        { L"kubectl logs", L"kubectl logs" },
        { L"git rebase", L"rebase" },
        { L"git bisect", L"bisect" },
        { L"vim", L"vim" },
        { L"nvim", L"nvim" },
    } };

    inline bool IsEnvironmentAssignment(std::wstring_view token) noexcept
    {
        if (token.empty() || token.front() == L'-' || token.front() == L'=')
        {
            return false;
        }
        return token.find(L'=') != std::wstring_view::npos;
    }

    // Keeps only the first command of a chain/pipeline. Quote-aware so a
    // separator inside a quoted argument is not treated as a break.
    inline std::wstring_view FirstCommandSegment(std::wstring_view commandline) noexcept
    {
        auto inQuotes = false;
        for (size_t index = 0; index < commandline.size(); ++index)
        {
            const auto c = commandline[index];
            if (c == L'"')
            {
                inQuotes = !inQuotes;
                continue;
            }
            if (inQuotes)
            {
                continue;
            }
            if (c == L'|' || c == L';' || c == L'&')
            {
                return TrimAscii(commandline.substr(0, index));
            }
        }
        return TrimAscii(commandline);
    }

    // Strips leading tokens that only describe how the command is invoked.
    inline std::wstring_view StripInvocationPrefixes(std::wstring_view commandline)
    {
        auto remaining = TrimAscii(commandline);

        // Bounded so a pathological line of assignments cannot spin here.
        for (size_t iteration = 0; iteration < 8; ++iteration)
        {
            const auto [token, rest] = SplitLeadingToken(remaining);
            if (token.empty() || rest.empty())
            {
                break;
            }

            const auto lowered = ToLowerAscii(token);
            const auto isIgnored = std::find(TabAutoNameIgnoredPrefixes.begin(),
                                             TabAutoNameIgnoredPrefixes.end(),
                                             std::wstring_view{ lowered }) != TabAutoNameIgnoredPrefixes.end();

            if (isIgnored || IsEnvironmentAssignment(token))
            {
                remaining = rest;
                continue;
            }
            break;
        }

        return remaining;
    }

    // `ssh user@host`, `ssh -p 22 host` -> "host". The destination is what the
    // user actually recognizes, so it beats any generic "ssh" label.
    inline std::optional<std::wstring> TryLabelSshTarget(std::wstring_view commandline)
    {
        auto [token, rest] = SplitLeadingToken(commandline);
        if (ToLowerAscii(token) != L"ssh")
        {
            return std::nullopt;
        }

        // Options that consume the following argument.
        constexpr std::array<std::wstring_view, 8> valueFlags{
            L"-p", L"-i", L"-l", L"-o", L"-F", L"-J", L"-L", L"-R"
        };

        while (!rest.empty())
        {
            auto [candidate, next] = SplitLeadingToken(rest);
            if (candidate.empty())
            {
                break;
            }

            if (candidate.front() == L'-')
            {
                const auto loweredFlag = ToLowerAscii(candidate);
                const auto consumesValue = std::find(valueFlags.begin(),
                                                     valueFlags.end(),
                                                     std::wstring_view{ loweredFlag }) != valueFlags.end();
                if (consumesValue)
                {
                    rest = SplitLeadingToken(next).second;
                }
                else
                {
                    rest = next;
                }
                continue;
            }

            // First non-flag argument is the destination.
            const auto at = candidate.find(L'@');
            const auto host = at == std::wstring_view::npos ? candidate : candidate.substr(at + 1);
            return host.empty() ? std::nullopt : std::optional{ std::wstring{ host } };
        }

        return std::nullopt;
    }

    // `npm run dev`, `yarn dev`, `pnpm run build` -> the script's label.
    inline std::optional<std::wstring> TryLabelPackageManagerScript(std::wstring_view commandline)
    {
        auto [manager, rest] = SplitLeadingToken(commandline);
        const auto loweredManager = ToLowerAscii(manager);
        const auto isPackageManager = std::find(TabAutoNamePackageManagers.begin(),
                                                TabAutoNamePackageManagers.end(),
                                                std::wstring_view{ loweredManager }) != TabAutoNamePackageManagers.end();
        if (!isPackageManager || rest.empty())
        {
            return std::nullopt;
        }

        auto [second, afterSecond] = SplitLeadingToken(rest);
        auto loweredSecond = ToLowerAscii(second);

        // `npm run dev` / `npm run-script dev` -> the script is the next token.
        if (loweredSecond == L"run" || loweredSecond == L"run-script")
        {
            const auto script = SplitLeadingToken(afterSecond).first;
            if (script.empty())
            {
                return std::nullopt;
            }
            loweredSecond = ToLowerAscii(script);
        }

        for (const auto& [script, label] : TabAutoNameScriptLabels)
        {
            if (loweredSecond == script)
            {
                return std::wstring{ label };
            }
        }

        // Unknown script: still more useful than the raw line.
        return loweredSecond.empty() ? std::nullopt : std::optional{ loweredSecond };
    }

    inline std::optional<std::wstring> TryLabelFromTable(std::wstring_view commandline)
    {
        // Build the lowercased first three tokens once, then probe longest-first.
        std::wstring probe;
        auto rest = commandline;
        std::array<std::wstring, 3> prefixes;
        size_t built = 0;

        for (; built < prefixes.size(); ++built)
        {
            auto [token, next] = SplitLeadingToken(rest);
            if (token.empty())
            {
                break;
            }
            if (!probe.empty())
            {
                probe.push_back(L' ');
            }
            probe += ToLowerAscii(token);
            prefixes[built] = probe;
            rest = next;
        }

        for (size_t index = built; index > 0; --index)
        {
            const auto& candidate = prefixes[index - 1];
            for (const auto& [key, label] : TabAutoNameCommandLabels)
            {
                if (candidate == key)
                {
                    return std::wstring{ label };
                }
            }
        }

        return std::nullopt;
    }

    // Maps a raw command line to a short, friendly label.
    //
    // Intentionally *not* a general parser: it normalizes, then matches a small
    // hand-maintained table. Anything unrecognized falls back to the truncated
    // original, so the worst case is "shows the command you ran" - never an
    // empty or wrong label.
    inline std::wstring FriendlyCommandLabel(std::wstring_view commandline)
    {
        const auto trimmed = TrimAscii(commandline);
        if (trimmed.empty())
        {
            return {};
        }

        const auto segment = FirstCommandSegment(trimmed);
        const auto stripped = StripInvocationPrefixes(segment);
        if (stripped.empty())
        {
            return TruncateForName(trimmed, TabAutoNameMaxTaskLength);
        }

        if (auto ssh = TryLabelSshTarget(stripped))
        {
            return TruncateForName(*ssh, TabAutoNameMaxTaskLength);
        }
        if (auto script = TryLabelPackageManagerScript(stripped))
        {
            return TruncateForName(*script, TabAutoNameMaxTaskLength);
        }
        if (auto table = TryLabelFromTable(stripped))
        {
            return TruncateForName(*table, TabAutoNameMaxTaskLength);
        }

        return TruncateForName(stripped, TabAutoNameMaxTaskLength);
    }

#pragma endregion

    // Joins the available segments into the final session name.
    //
    // `fallbackTitle` is the shell-reported title we used before auto-naming;
    // it is returned unchanged when shell integration gave us nothing to work
    // with. The working directory is deliberately *not* a segment: the recents
    // list already renders it in its own column next to the name.
    inline std::wstring ComposeTabAutoName(std::wstring_view branch,
                                           std::wstring_view task,
                                           std::wstring_view fallbackTitle)
    {
        const auto trimmedBranch = TrimAscii(branch);
        const auto trimmedTask = TrimAscii(task);

        std::wstring name;
        if (!trimmedBranch.empty())
        {
            name += trimmedBranch;
        }
        if (!trimmedTask.empty())
        {
            if (!name.empty())
            {
                name += TabAutoNameSeparator;
            }
            name += trimmedTask;
        }

        if (name.empty())
        {
            return std::wstring{ TrimAscii(fallbackTitle) };
        }

        return TruncateForName(name, TabAutoNameMaxLength);
    }
}
