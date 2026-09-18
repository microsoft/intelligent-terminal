// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPaneRestore.h"
#include <wil/win32_helpers.h>
#include <shellapi.h>
#include <json/json.h>
#include <til/string.h>
#include <winrt/base.h>

#include <algorithm>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace Microsoft::Terminal::AgentSource
{
    enum class SessionsSshKind
    {
        NonSsh,
        ValidTarget,
        UnsupportedSsh,
    };

    enum class SessionsSshCommand
    {
        Login,
        Tmux,
    };

    struct SessionsSshSource
    {
        SessionsSshKind kind{ SessionsSshKind::NonSsh };
        std::wstring destination;
        std::optional<uint16_t> port;
        std::wstring error;
        std::wstring executable;
        bool requestPty{ true };
        bool managedLaunch{ false };
    };

    namespace details
    {
        inline SessionsSshSource UnsupportedSessionsSsh(const std::wstring_view reason)
        {
            return { SessionsSshKind::UnsupportedSsh, {}, {}, std::wstring{ reason } + L" Use a Host alias in OpenSSH config for this profile." };
        }

        inline bool HasBalancedCommandlineQuotes(const std::wstring_view commandline) noexcept
        {
            bool quoted = false;
            size_t backslashes = 0;
            for (const auto ch : commandline)
            {
                if (ch == L'"' && backslashes % 2 == 0)
                {
                    quoted = !quoted;
                }
                backslashes = ch == L'\\' ? backslashes + 1 : 0;
            }
            return !quoted;
        }

        inline bool IsSafeSshIdentityPart(std::wstring_view value, const bool host) noexcept
        {
            if (host && value.starts_with(L'['))
            {
                if (!value.ends_with(L']') || value.find(L':') == std::wstring_view::npos)
                {
                    return false;
                }
                value = value.substr(1, value.size() - 2);
            }
            return !value.empty() && value.front() != L'-' &&
                   std::all_of(value.begin(), value.end(), [host](const wchar_t ch) {
                       return (ch >= L'a' && ch <= L'z') || (ch >= L'A' && ch <= L'Z') ||
                              (ch >= L'0' && ch <= L'9') || ch == L'.' || ch == L'_' ||
                              ch == L'-' || (host && ch == L':');
                   });
        }
    }

    // Sessions follows the ordinary terminal profile, independently of its ACP
    // backend. A generated SSH profile must not fall back to local history if
    // its command line was customized beyond the target identity we can replay.
    inline SessionsSshSource ResolveSessionsSshSource(
        const std::wstring_view profileSource,
        std::wstring_view commandline,
        const SessionsSshCommand command = SessionsSshCommand::Login)
    {
        const bool generatedSsh = profileSource == L"Windows.Terminal.SSH";
        const auto first = commandline.find_first_not_of(L" \t");
        if (first == std::wstring_view::npos)
        {
            return generatedSsh ? details::UnsupportedSessionsSsh(L"The SSH profile has no command line.") : SessionsSshSource{};
        }
        commandline.remove_prefix(first);

        int argc = 0;
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(std::wstring{ commandline }.c_str(), &argc) };
        if (!argv || argc == 0)
        {
            return generatedSsh ? details::UnsupportedSessionsSsh(L"The SSH profile command line could not be parsed.") : SessionsSshSource{};
        }
        std::wstring_view executable{ argv[0] };
        if (const auto separator = executable.find_last_of(L"\\/"); separator != std::wstring_view::npos)
        {
            executable.remove_prefix(separator + 1);
        }
        const bool directSsh = til::equals_insensitive_ascii(executable, L"ssh") ||
                               til::equals_insensitive_ascii(executable, L"ssh.exe");
        const bool managedSsh = argc > 1 && std::wstring_view{ argv[1] } == L"ssh" &&
                                (til::equals_insensitive_ascii(executable, L"wta") ||
                                 til::equals_insensitive_ascii(executable, L"wta.exe"));
        if (managedSsh && command == SessionsSshCommand::Tmux)
        {
            return details::UnsupportedSessionsSsh(L"A tmux backend must launch SSH directly to identify its session source.");
        }
        if (managedSsh)
        {
            if (commandline.find(L'\0') != std::wstring_view::npos || !details::HasBalancedCommandlineQuotes(commandline))
            {
                return details::UnsupportedSessionsSsh(L"The managed SSH command line is malformed.");
            }
            std::optional<std::wstring_view> destination;
            std::optional<std::wstring_view> managedPort;
            bool requestPty = true;
            bool remoteCommand = false;
            for (int index = 2; index < argc; ++index)
            {
                std::wstring_view option{ argv[index] };
                if (option == L"--no-pty")
                {
                    requestPty = false;
                    continue;
                }
                if (option == L"--no-hooks")
                {
                    continue;
                }
                const auto separator = option.find(L'=');
                const auto name = option.substr(0, separator);
                std::wstring_view value;
                if (separator == std::wstring_view::npos)
                {
                    if (++index == argc)
                    {
                        return details::UnsupportedSessionsSsh(L"The managed SSH command line is missing an option value.");
                    }
                    value = argv[index];
                }
                else
                {
                    value = option.substr(separator + 1);
                }
                if (name == L"--destination" && !destination)
                {
                    destination = value;
                }
                else if (name == L"--port" && !managedPort)
                {
                    managedPort = value;
                }
                else if (name == L"--remote-command" && !remoteCommand)
                {
                    remoteCommand = true;
                }
                else
                {
                    return details::UnsupportedSessionsSsh(L"The managed SSH command line contains unsupported or repeated options.");
                }
            }
            if (!destination || destination->empty())
            {
                return details::UnsupportedSessionsSsh(L"The managed SSH command line has no destination.");
            }
            std::wstring equivalent{ requestPty ? L"ssh.exe" : L"ssh.exe -T" };
            if (managedPort)
            {
                if (managedPort->empty())
                {
                    return details::UnsupportedSessionsSsh(L"The managed SSH port is empty.");
                }
                AgentPaneRestore::AppendFlag(equivalent, L"-p", *managedPort);
            }
            equivalent.append(L" -- ");
            AgentPaneRestore::AppendQuoted(equivalent, *destination);
            auto source = ResolveSessionsSshSource({}, equivalent);
            source.executable = argv[0];
            source.managedLaunch = true;
            return source;
        }
        if (!directSsh)
        {
            return generatedSsh ? details::UnsupportedSessionsSsh(L"The SSH profile must launch ssh.exe directly.") : SessionsSshSource{};
        }
        if (commandline.find(L'\0') != std::wstring_view::npos || !details::HasBalancedCommandlineQuotes(commandline))
        {
            return details::UnsupportedSessionsSsh(L"The SSH profile command line is malformed.");
        }

        std::optional<uint16_t> port;
        std::optional<std::wstring_view> user;
        bool requestPty = true;
        int index = 1;
        for (; index < argc; ++index)
        {
            const std::wstring_view arg{ argv[index] };
            if (arg.empty() || arg.front() != L'-')
            {
                break;
            }
            if (arg == L"--")
            {
                ++index;
                break;
            }
            if (arg.size() > 1 && std::all_of(arg.begin() + 1, arg.end(), [](const wchar_t ch) { return ch == L't' || ch == L'T'; }))
            {
                requestPty = arg.back() == L't';
                continue;
            }
            if (command == SessionsSshCommand::Tmux && arg.starts_with(L"-o"))
            {
                auto value = arg.substr(2);
                if (value.empty())
                {
                    if (++index == argc)
                    {
                        return details::UnsupportedSessionsSsh(L"The tmux SSH command is missing an option value.");
                    }
                    value = argv[index];
                }
                // BatchMode changes authentication interaction, not the source.
                // Other -o overrides may change the target or remote context.
                if (til::equals_insensitive_ascii(value, L"BatchMode=yes") ||
                    til::equals_insensitive_ascii(value, L"BatchMode=no"))
                {
                    continue;
                }
                return details::UnsupportedSessionsSsh(L"The tmux SSH command has source overrides that Sessions cannot reproduce.");
            }
            if (arg.size() < 2 || (arg[1] != L'p' && arg[1] != L'l'))
            {
                return details::UnsupportedSessionsSsh(L"The SSH profile uses options that Sessions cannot reproduce.");
            }
            const auto option = arg[1];
            auto value = arg.substr(2);
            if (value.empty())
            {
                if (++index == argc)
                {
                    return details::UnsupportedSessionsSsh(L"The SSH profile is missing an option value.");
                }
                value = argv[index];
            }
            if (option == L'p')
            {
                if (port)
                {
                    return details::UnsupportedSessionsSsh(L"The SSH profile specifies a port more than once.");
                }
                uint32_t number = 0;
                for (const auto ch : value)
                {
                    if (ch < L'0' || ch > L'9')
                    {
                        return details::UnsupportedSessionsSsh(L"The SSH port must be a number from 1 to 65535.");
                    }
                    number = number * 10 + static_cast<uint32_t>(ch - L'0');
                    if (number > 65535)
                    {
                        return details::UnsupportedSessionsSsh(L"The SSH port must be a number from 1 to 65535.");
                    }
                }
                if (number == 0)
                {
                    return details::UnsupportedSessionsSsh(L"The SSH port must be a number from 1 to 65535.");
                }
                port = static_cast<uint16_t>(number);
            }
            else
            {
                if (user)
                {
                    return details::UnsupportedSessionsSsh(L"The SSH profile specifies a user more than once.");
                }
                user = value;
            }
        }
        if (index == argc)
        {
            return details::UnsupportedSessionsSsh(L"The SSH profile has no destination.");
        }
        if (command == SessionsSshCommand::Tmux)
        {
            if (index + 1 == argc)
            {
                return details::UnsupportedSessionsSsh(L"The SSH backend has no explicit tmux command.");
            }
            const std::wstring_view remote{ argv[index + 1] };
            const auto program = remote.substr(0, remote.find_first_of(L" \t"));
            if (program != L"tmux" && program != L"/usr/bin/tmux" && program != L"/usr/local/bin/tmux")
            {
                return details::UnsupportedSessionsSsh(L"The SSH backend does not launch tmux directly.");
            }
            for (auto remoteIndex = index + 1; remoteIndex < argc; ++remoteIndex)
            {
                if (std::wstring_view{ argv[remoteIndex] }.find_first_of(L"\r\n;&|`$<>\\") != std::wstring_view::npos)
                {
                    return details::UnsupportedSessionsSsh(L"The tmux SSH command contains shell expressions whose source cannot be determined.");
                }
            }
        }
        else if (index + 1 != argc)
        {
            return details::UnsupportedSessionsSsh(L"Remote commands in SSH profiles are not supported by Sessions.");
        }

        std::wstring_view host{ argv[index] };
        if (const auto separator = host.find(L'@'); separator != std::wstring_view::npos)
        {
            if (user)
            {
                return details::UnsupportedSessionsSsh(L"The SSH profile specifies a user more than once.");
            }
            user = host.substr(0, separator);
            host.remove_prefix(separator + 1);
        }
        if (!details::IsSafeSshIdentityPart(host, true) ||
            (user && !details::IsSafeSshIdentityPart(*user, false)))
        {
            return details::UnsupportedSessionsSsh(L"The SSH destination or user is invalid.");
        }

        auto destination = user ? std::wstring{ *user } + L"@" + std::wstring{ host } : std::wstring{ host };
        return { SessionsSshKind::ValidTarget, std::move(destination), port, {}, std::wstring{ argv[0] }, requestPty };
    }

    inline std::optional<std::wstring> BuildManagedSshCommandline(
        const SessionsSshSource& source,
        const std::wstring_view wtaExecutable,
        const std::wstring_view systemSshExecutable)
    {
        if (source.kind != SessionsSshKind::ValidTarget || source.managedLaunch || wtaExecutable.empty())
        {
            return std::nullopt;
        }
        auto executable = source.executable;
        std::replace(executable.begin(), executable.end(), L'/', L'\\');
        if (!til::equals_insensitive_ascii(executable, L"ssh") &&
            !til::equals_insensitive_ascii(executable, L"ssh.exe") &&
            !til::equals_insensitive_ascii(executable, L"%SystemRoot%\\System32\\OpenSSH\\ssh.exe") &&
            !til::equals_insensitive_ascii(executable, L"%windir%\\System32\\OpenSSH\\ssh.exe") &&
            !til::equals_insensitive_ascii(executable, systemSshExecutable))
        {
            return std::nullopt;
        }

        std::wstring commandline;
        AgentPaneRestore::AppendQuoted(commandline, wtaExecutable);
        commandline.append(L" ssh");
        AgentPaneRestore::AppendFlag(commandline, L"--destination", source.destination);
        if (source.port)
        {
            AgentPaneRestore::AppendFlag(commandline, L"--port", std::to_wstring(*source.port));
        }
        if (!source.requestPty)
        {
            commandline.append(L" --no-pty");
        }
        return commandline;
    }

    inline std::vector<std::pair<std::wstring, std::wstring>> BuildSessionsSshHelperArguments(const SessionsSshSource& source)
    {
        if (source.kind == SessionsSshKind::UnsupportedSsh)
        {
            return { { L"--sessions-ssh-error", source.error } };
        }
        if (source.kind != SessionsSshKind::ValidTarget)
        {
            return {};
        }
        std::vector<std::pair<std::wstring, std::wstring>> args{ { L"--sessions-ssh-target", source.destination } };
        if (source.port)
        {
            args.emplace_back(L"--sessions-ssh-port", std::to_wstring(*source.port));
        }
        return args;
    }

    inline void WriteSessionsSshMetadata(Json::Value& params, const SessionsSshSource& source)
    {
        // Explicit null also clears the previous pane's SSH source when a
        // stashed helper is reused for a non-SSH terminal profile.
        auto& ssh = params["sessions_ssh"];
        ssh = Json::Value{};
        if (source.kind == SessionsSshKind::UnsupportedSsh)
        {
            ssh["error"] = winrt::to_string(source.error);
        }
        else if (source.kind == SessionsSshKind::ValidTarget)
        {
            ssh["destination"] = winrt::to_string(source.destination);
            ssh["port"] = source.port ? Json::Value{ *source.port } : Json::Value{};
        }
    }

    struct ResolvedWorkingDirectories
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
    inline ResolvedWorkingDirectories ResolveAgentAndHelperWorkingDirectories(
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