// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxSshCommand.h
//
// Abstract:
// - Validates and quotes explicit default-server SSH tmux launches.

#pragma once

#include <algorithm>
#include <cstdint>
#include <stdexcept>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::Tmux
{
    inline constexpr size_t MaxSshLaunchTextLength = 32766;

    inline bool IsValidSshLaunchText(const std::wstring_view value) noexcept
    {
        if (value.size() > MaxSshLaunchTextLength || value.find_first_not_of(L' ') == std::wstring_view::npos)
        {
            return false;
        }
        for (size_t i = 0; i < value.size(); ++i)
        {
            const auto ch = value[i];
            if (ch < L' ' || (ch >= L'\x7f' && ch <= L'\x9f'))
            {
                return false;
            }
            if (ch >= 0xd800 && ch <= 0xdbff)
            {
                if (++i == value.size() || value[i] < 0xdc00 || value[i] > 0xdfff)
                {
                    return false;
                }
            }
            else if (ch >= 0xdc00 && ch <= 0xdfff)
            {
                return false;
            }
        }
        return true;
    }

    inline bool IsValidSshDestination(const std::wstring_view destination) noexcept
    {
        // Keep the user's alias (and thus their SSH configuration) intact, but
        // never accept options or shell metacharacters in a host/user name.
        if (!IsValidSshLaunchText(destination) || destination.front() == L'-' ||
            destination.find_first_of(L" \"'\\`$;&|<>(){}*!?#=/") != std::wstring_view::npos)
        {
            return false;
        }
        const auto at = destination.find(L'@');
        return at == std::wstring_view::npos ||
               (at != 0 && at + 1 < destination.size() && destination[at + 1] != L'-' &&
                destination.find(L'@', at + 1) == std::wstring_view::npos);
    }

    inline bool IsSshSessionId(const std::wstring_view session) noexcept
    {
        return session.size() > 1 && session.front() == L'$' &&
               std::all_of(session.begin() + 1, session.end(), [](const wchar_t ch) { return ch >= L'0' && ch <= L'9'; });
    }

    namespace details
    {
        inline void AppendSshWindowsArgument(std::wstring& out, const std::wstring_view value)
        {
            // Same CreateProcess/CommandLineToArgvW rules as
            // AgentPaneRestore::AppendQuoted; unlike wt argument quoting,
            // this Windows layer must leave semicolons intact.
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

        inline std::wstring QuoteSshRemoteArgument(const std::wstring_view value)
        {
            // POSIX single quoting, as in WTA's sh_quote: an apostrophe closes
            // the quoted span, is escaped outside it, and reopens the span.
            std::wstring result{ L"'" };
            for (const auto ch : value)
            {
                if (ch == L'\'')
                {
                    result.append(L"'\\''");
                }
                else
                {
                    result.push_back(ch);
                }
            }
            result.push_back(L'\'');
            return result;
        }

        inline std::wstring SshCommandlinePrefix(const std::wstring_view destination, const uint16_t port, const bool list)
        {
            if (!IsValidSshDestination(destination))
            {
                throw std::invalid_argument{ "tmux --ssh requires a non-empty Unicode SSH alias or [user@]host without options, whitespace, controls, or shell metacharacters (maximum 32766 UTF-16 code units)." };
            }

            std::wstring commandline{ list ? L"ssh.exe -T -n -o BatchMode=yes " : L"ssh.exe -T -o BatchMode=yes " };
            if (port != 0)
            {
                commandline.append(L"-p ").append(std::to_wstring(port)).push_back(L' ');
            }
            AppendSshWindowsArgument(commandline, destination);
            return commandline;
        }

        inline void CheckSshCommandlineLength(const std::wstring_view commandline)
        {
            if (commandline.size() > MaxSshLaunchTextLength)
            {
                throw std::invalid_argument{ "The quoted SSH tmux command exceeds the Windows limit of 32766 UTF-16 code units." };
            }
        }
    }

    // Throws std::invalid_argument on invalid inputs or an oversized command.
    // This is only for explicit SSH launches; never parse an opaque commandline.
    inline std::wstring BuildSshCommandline(const std::wstring_view destination, const std::wstring_view session, const uint16_t port = 0)
    {
        auto commandline = details::SshCommandlinePrefix(destination, port, false);
        if (!IsValidSshLaunchText(session))
        {
            throw std::invalid_argument{ "tmux --session requires a non-empty Unicode session name or $<digits> ID without control characters (maximum 32766 UTF-16 code units)." };
        }

        // '=' disables tmux's prefix/glob matching. Stable IDs must bypass
        // that prefix so a menu selection survives a concurrent session rename.
        std::wstring target{ IsSshSessionId(session) ? L"" : L"=" };
        target.append(session);
        // tmux's argv parser treats a trailing ';' as a command separator even
        // after shell quoting. Its own '\;' escape removes exactly one slash.
        if (target.back() == L';')
        {
            target.insert(target.size() - 1, 1, L'\\');
        }

        // An explicit default label overrides a remote TMUX environment that
        // might otherwise select a named server. This is not a user option.
        commandline.append(L" tmux -L default -C attach-session -t ");
        // SSH joins its command arguments for the remote shell. Preserve the
        // POSIX quoting through Windows parsing before that second parse.
        details::AppendSshWindowsArgument(commandline, details::QuoteSshRemoteArgument(target));
        details::CheckSshCommandlineLength(commandline);
        return commandline;
    }

    inline std::wstring BuildSshListCommandline(const std::wstring_view destination, const uint16_t port = 0)
    {
        auto commandline = details::SshCommandlinePrefix(destination, port, true);
        // A deterministic locale lets the query distinguish a missing default
        // server from authentication, permissions and missing-command failures.
        commandline.append(L" LC_ALL=C tmux -L default list-sessions -F ");
        details::AppendSshWindowsArgument(commandline, details::QuoteSshRemoteArgument(L"#{session_id} #{session_name}"));
        details::CheckSshCommandlineLength(commandline);
        return commandline;
    }
}
