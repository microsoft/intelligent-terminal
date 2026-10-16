// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ShellIntegrationProfileGate.h"
#include "../../types/inc/utils.hpp"

#include <shellapi.h>
#include <wil/win32_helpers.h>

#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

namespace Microsoft::Terminal::AgentSource
{
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

    inline std::wstring ResolvePaneCwd(
        const std::wstring_view paneCwd,
        const bool reportedByShell,
        const std::wstring_view commandline,
        const std::wstring_view startingDirectory)
    {
        if (reportedByShell && !paneCwd.empty())
        {
            return std::wstring{ paneCwd };
        }
        if (!ShellIntegration::IsWslProfile(commandline))
        {
            return ResolveCwd(paneCwd, {}, startingDirectory, {});
        }
        if (!ShellIntegration::details::CommandlineHasExeToken(commandline, L"wsl") ||
            commandline.find(L'\0') != std::wstring_view::npos)
        {
            return {};
        }

        // A noninteractive WSL launch may never report its cwd. Its --cd is
        // authoritative over the Windows starting directory, not the hook's cwd.
        const auto isPosix = [](const std::wstring_view path) {
            return path.starts_with(L'/') && !path.starts_with(L"//");
        };
        const std::wstring command{ commandline.substr(commandline.find_first_not_of(L" \t")) };
        int argc{};
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(command.c_str(), &argc) };
        THROW_LAST_ERROR_IF_NULL(argv.get());
        std::wstring_view launchCwd;
        bool hasLaunchCwd = false;
        for (int i = 1; i < argc; ++i)
        {
            const std::wstring_view arg{ argv.get()[i] };
            if (arg == L"--" || arg == L"--exec" || arg == L"-e")
            {
                break;
            }
            if (arg == L"~")
            {
                return {};
            }
            if (arg == L"--cd")
            {
                if (hasLaunchCwd || ++i == argc)
                {
                    return {};
                }
                launchCwd = argv.get()[i];
                hasLaunchCwd = true;
                if (!isPosix(launchCwd))
                {
                    return {};
                }
            }
            else if (arg == L"-d" || arg == L"--distribution" || arg == L"--distribution-id" ||
                     arg == L"-u" || arg == L"--user" || arg == L"--shell-type")
            {
                if (++i == argc || std::wstring_view{ argv.get()[i] }.empty() ||
                    std::wstring_view{ argv.get()[i] }.starts_with(L'-'))
                {
                    return {};
                }
            }
            else if (arg == L"--system")
            {
                continue;
            }
            else if (arg.starts_with(L'-'))
            {
                return {};
            }
            else
            {
                break;
            }
        }
        if (hasLaunchCwd)
        {
            return std::wstring{ launchCwd };
        }
        if (isPosix(startingDirectory) &&
            std::get<1>(::Microsoft::Console::Utils::MangleStartingDirectoryForWSL(commandline, startingDirectory)).empty())
        {
            return std::wstring{ startingDirectory };
        }
        return {};
    }

    inline std::optional<std::wstring> WslLaunchIdentity(const std::wstring_view commandline)
    {
        if (!ShellIntegration::details::CommandlineHasExeToken(commandline, L"wsl") ||
            commandline.find(L'\0') != std::wstring_view::npos)
        {
            return std::nullopt;
        }
        const std::wstring command{ commandline.substr(commandline.find_first_not_of(L" \t")) };
        int argc{};
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(command.c_str(), &argc) };
        THROW_LAST_ERROR_IF_NULL(argv.get());
        std::wstring identity;
        for (int i = 1; i < argc; ++i)
        {
            const std::wstring_view arg{ argv.get()[i] };
            if (arg == L"~")
            {
                continue;
            }
            if (arg == L"--" || arg == L"--exec" || arg == L"-e" || !arg.starts_with(L'-'))
            {
                break;
            }
            if (arg == L"-d" || arg == L"--distribution" || arg == L"--distribution-id")
            {
                if (!identity.empty() || ++i == argc || std::wstring_view{ argv.get()[i] }.empty() ||
                    std::wstring_view{ argv.get()[i] }.starts_with(L'-'))
                {
                    return std::nullopt;
                }
                identity = arg == L"--distribution-id" ? L"id:" : L"name:";
                identity.append(argv.get()[i]);
            }
            else if (arg == L"--cd" || arg == L"-u" || arg == L"--user" || arg == L"--shell-type")
            {
                if (++i == argc || std::wstring_view{ argv.get()[i] }.empty() ||
                    std::wstring_view{ argv.get()[i] }.starts_with(L'-'))
                {
                    return std::nullopt;
                }
            }
            else
            {
                return std::nullopt;
            }
        }
        // The default distribution may have changed since the source process started.
        return identity.empty() ? std::nullopt : std::optional{ std::move(identity) };
    }

    template<typename IsWindowsDirectory>
    inline std::wstring ResolveSplitCwd(const std::wstring_view sourceCwd,
                                        const std::wstring_view sourceCommand,
                                        const std::wstring_view targetCommand,
                                        IsWindowsDirectory&& isWindowsDirectory)
    {
        if (sourceCwd.empty())
        {
            return {};
        }
        if (sourceCwd.find(L'\0') != std::wstring_view::npos)
        {
            LOG_HR_MSG(E_INVALIDARG, "Split working directory contains an embedded NUL");
            return {};
        }
        if (sourceCwd.starts_with(L'/') && !sourceCwd.starts_with(L"//"))
        {
            // The WSL launcher embeds --cd in a quoted command-line argument.
            if (sourceCwd.find_first_of(L"\"\\%\r\n") != std::wstring_view::npos)
            {
                LOG_HR_MSG(E_INVALIDARG, "Split working directory cannot be safely passed to WSL");
                return {};
            }
            const auto source = WslLaunchIdentity(sourceCommand);
            const auto target = WslLaunchIdentity(targetCommand);
            // POSIX paths are meaningful only in the same known launch namespace.
            if (source && target && *source == *target &&
                std::get<1>(::Microsoft::Console::Utils::MangleStartingDirectoryForWSL(targetCommand, sourceCwd)).empty())
            {
                return std::wstring{ sourceCwd };
            }
            return {};
        }
        return std::filesystem::path{ sourceCwd }.is_absolute() && isWindowsDirectory(sourceCwd) ?
                   std::wstring{ sourceCwd } :
                   std::wstring{};
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