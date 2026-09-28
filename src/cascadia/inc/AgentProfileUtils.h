// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPaneRestore.h"
#include <filesystem>

namespace Microsoft::Terminal::AgentProfiles
{
    inline constexpr std::wstring_view Source{ L"IntelligentTerminal.AgentProfiles" };

    // Never resolve a different installed Terminal's launcher through PATH.
    inline std::wstring ResolveLauncher()
    {
        auto directory = std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>(nullptr) }.parent_path();
        const auto sibling = directory / L"wta.exe";
        if (std::filesystem::is_regular_file(sibling))
        {
            return sibling.native();
        }

        while (!directory.empty())
        {
            for (const auto configuration : { L"debug", L"release" })
            {
                const auto candidate = directory / L"tools" / L"wta" / L"target" / L"x86_64-pc-windows-msvc" / configuration / L"wta.exe";
                if (std::filesystem::is_regular_file(candidate))
                {
                    return candidate.native();
                }
            }
            const auto parent = directory.parent_path();
            if (parent == directory)
            {
                break;
            }
            directory = parent;
        }
        return {};
    }

    inline std::wstring BuildCommand(const std::wstring_view launcher,
                                     const std::wstring_view id,
                                     const std::wstring_view model,
                                     const std::wstring_view permissionMode,
                                     const std::wstring_view arguments)
    {
        std::wstring command;
        AgentPaneRestore::AppendQuoted(command, launcher);
        command.append(L" launch-agent");
        AgentPaneRestore::AppendFlag(command, L"--agent-id", id);
        AgentPaneRestore::AppendFlag(command, L"--model", model);
        AgentPaneRestore::AppendFlag(command, L"--permission-mode", permissionMode);
        if (!arguments.empty())
        {
            command.append(L" -- ");
            command.append(arguments);
        }
        return command;
    }

    template<typename Profile>
    bool IsManaged(const Profile& profile)
    {
        return !profile.AgentProfileId().empty() && !profile.AgentProfileCustomCommand();
    }

    template<typename Profile>
    std::wstring Command(const Profile& profile)
    {
        auto launcher = ResolveLauncher();
        if (launcher.empty())
        {
            // Keep settings/appearance previews usable when WTA is missing.
            // Launching this absolute path reports the ordinary process error.
            launcher = (std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>(nullptr) }.parent_path() / L"wta.exe").native();
        }
        return BuildCommand(launcher, profile.AgentProfileId(), profile.AgentProfileModel(),
                            profile.AgentProfilePermissionMode(), profile.AgentProfileArguments());
    }
}
