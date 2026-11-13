// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPaneRestore.h"
#include "AgentRegistry.h"
#include "DynamicProfileIdentity.h"
#include "../../types/inc/utils.hpp"
#include <filesystem>
#include <map>
#include <shellapi.h>
#include <span>
#include <til/env.h>

namespace Microsoft::Terminal::AgentProfiles
{
    inline constexpr std::wstring_view Source{ L"IntelligentTerminal.AgentProfiles" };

    inline std::wstring GeneratedIdentity(const std::wstring_view providerId)
    {
        return std::wstring{ Source } + L":" + std::wstring{ providerId };
    }

    inline winrt::guid GeneratedGuid(const std::wstring_view providerId)
    {
        const auto identity = GeneratedIdentity(providerId);
        return winrt::guid{ ::Microsoft::Console::Utils::CreateV5Uuid(TERMINAL_PROFILE_NAMESPACE_GUID, std::as_bytes(std::span{ identity })) };
    }

    template<typename Profiles>
    auto FindOriginal(const Profiles& profiles, const std::wstring_view providerId)
    {
        using Profile = decltype(profiles.GetAt(0));
        Profile original{ nullptr };
        if (Settings::Model::AgentRegistry::CanonicalNativeAgentProviderId(providerId).empty())
        {
            return original;
        }
        for (const auto& profile : profiles)
        {
            const auto id = profile.AgentProfileId();
            const auto inherited = profile.AgentProfileIdOverrideSource();
            // A saved case-only override does not change the generated parent's identity.
            const auto originalId = inherited && inherited.Source() == Source && inherited.Guid() == profile.Guid() &&
                                            (inherited.Origin() == winrt::Microsoft::Terminal::Settings::Model::OriginTag::Generated ||
                                             inherited.Origin() == winrt::Microsoft::Terminal::Settings::Model::OriginTag::InBox) &&
                                            Settings::Model::AgentRegistry::AgentIdEquals(inherited.AgentProfileId(), id) ?
                                        inherited.AgentProfileId() :
                                        id;
            if (profile.Source() == Source &&
                Settings::Model::AgentRegistry::AgentIdEquals(id, providerId) &&
                profile.Guid() == GeneratedGuid(originalId))
            {
                // Case-distinct generated identities must not select an arbitrary original.
                if (original)
                {
                    return Profile{ nullptr };
                }
                original = profile;
            }
        }
        return original;
    }

    inline std::wstring NativePath()
    {
        til::env environment;
        environment.regenerate();
        auto path = environment.as_map()[L"PATH"];
        const auto inherited = wil::TryGetEnvironmentVariableW<std::wstring>(L"PATH");
        if (!inherited.empty())
        {
            if (!path.empty())
            {
                path.push_back(L';');
            }
            path.append(inherited);
        }
        return path;
    }

    inline std::filesystem::path ResolveExecutable(const std::wstring_view id, const std::wstring_view path)
    {
        const auto executable = Settings::Model::AgentRegistry::CliExecutable(id);
        for (const auto extension : { L".exe", L".cmd" })
        {
            if (id == L"antigravity" && std::wstring_view{ extension } == L".cmd")
            {
                continue;
            }
            size_t start = 0;
            while (start < path.size())
            {
                const auto end = path.find(L';', start);
                auto directory = path.substr(start, end == std::wstring_view::npos ? path.size() - start : end - start);
                if (directory.size() >= 2 && directory.front() == L'"' && directory.back() == L'"')
                {
                    directory = directory.substr(1, directory.size() - 2);
                }
                if (!directory.empty())
                {
                    const auto candidate = std::filesystem::path{ directory } / (std::wstring{ executable } + extension);
                    WIN32_FIND_DATAW findData{};
                    const wil::unique_hfind findHandle{ FindFirstFileW(candidate.c_str(), &findData) };
                    const auto findError = findHandle ? ERROR_SUCCESS : GetLastError();
                    // App execution aliases are launchable, but filesystem::status can fail to follow them.
                    const auto appExecutionAlias = findHandle &&
                                                   WI_IsFlagSet(findData.dwFileAttributes, FILE_ATTRIBUTE_REPARSE_POINT) &&
                                                   WI_IsFlagClear(findData.dwFileAttributes, FILE_ATTRIBUTE_DIRECTORY) &&
                                                   findData.dwReserved0 == IO_REPARSE_TAG_APPEXECLINK;
                    const auto missing = findError == ERROR_FILE_NOT_FOUND || findError == ERROR_PATH_NOT_FOUND;
                    const auto launchable = !missing &&
                                            (findHandle && WI_IsFlagClear(findData.dwFileAttributes, FILE_ATTRIBUTE_REPARSE_POINT) ?
                                                 WI_IsFlagClear(findData.dwFileAttributes, FILE_ATTRIBUTE_DIRECTORY) :
                                                 appExecutionAlias || std::filesystem::is_regular_file(candidate));
                    if (launchable)
                    {
                        return std::filesystem::absolute(candidate);
                    }
                }
                if (end == std::wstring_view::npos)
                {
                    break;
                }
                start = end + 1;
            }
        }
        return {};
    }

    inline std::map<std::wstring, std::filesystem::path> Discover(const std::wstring_view path)
    {
        std::map<std::wstring, std::filesystem::path> agents;
        for (const auto& agent : Settings::Model::AgentRegistry::BuiltinNativeProfileAgents)
        {
            if (auto executable = ResolveExecutable(agent.id, path); !executable.empty())
            {
                agents.emplace(agent.id, std::move(executable));
            }
        }
        return agents;
    }

    inline std::vector<std::wstring> ParseArguments(const std::wstring_view arguments)
    {
        std::vector<std::wstring> values;
        if (!arguments.empty())
        {
            THROW_HR_IF_MSG(E_INVALIDARG, arguments.find(L'\0') != std::wstring_view::npos, "Native agent command contains an embedded NUL.");
            const auto command = L"agent " + std::wstring{ arguments };
            int argc = 0;
            wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(command.c_str(), &argc) };
            THROW_LAST_ERROR_IF(!argv);
            for (int i = 1; i < argc; ++i)
            {
                values.emplace_back(argv[i]);
            }
        }
        return values;
    }

    inline std::wstring BuildCommand(const std::wstring_view executable,
                                     const std::wstring_view id,
                                     const std::wstring_view model,
                                     const std::wstring_view permissionMode,
                                     const std::wstring_view arguments,
                                     const bool validate = true)
    {
        THROW_HR_IF(E_INVALIDARG, !Settings::Model::AgentRegistry::SupportsNativeProfile(id));
        auto values = ParseArguments(arguments);
        if (!permissionMode.empty())
        {
            // Map the structured permission option to the provider's CLI syntax.
            if (id == L"copilot" || id == L"opencode")
            {
                values.insert(values.begin(), L"--" + std::wstring{ permissionMode });
            }
            else
            {
                const auto flag = id == L"claude" ? L"--permission-mode" : id == L"codex" ? L"--ask-for-approval" : id == L"antigravity" ? L"--mode" : L"--approval-mode";
                values.insert(values.begin(), { flag, std::wstring{ permissionMode } });
            }
        }
        if (!model.empty())
        {
            values.insert(values.begin(), { L"--model", std::wstring{ model } });
        }
        THROW_HR_IF(E_INVALIDARG, executable.empty() || executable.find(L'\0') != std::wstring_view::npos);
        THROW_HR_IF_MSG(E_INVALIDARG, validate && std::ranges::any_of(values, [](const auto& value) { return value.find(L'\0') != std::wstring::npos; }),
                       "Native agent command contains an embedded NUL.");
        const auto batch = til::equals_insensitive_ascii(std::filesystem::path{ executable }.extension().native(), L".cmd");
        if (batch && validate)
        {
            const auto safe = [](const std::wstring_view value) { return value.find_first_of(L"%!\"\r\n^") == std::wstring_view::npos; };
            THROW_HR_IF_MSG(E_INVALIDARG, !safe(executable) || !std::ranges::all_of(values, safe), "Unsafe native batch launcher argument");
        }
        std::wstring command;
        AgentPaneRestore::AppendQuoted(command, executable);
        for (const auto& value : values)
        {
            command.push_back(L' ');
            AgentPaneRestore::AppendQuoted(command, value);
        }
        if (batch)
        {
            wchar_t directory[MAX_PATH];
            const auto length = GetSystemDirectoryW(directory, ARRAYSIZE(directory));
            THROW_LAST_ERROR_IF(length == 0);
            THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER), length >= ARRAYSIZE(directory));
            std::wstring wrapped;
            AgentPaneRestore::AppendQuoted(wrapped, std::wstring{ directory, length } + L"\\cmd.exe");
            wrapped.append(L" /d /s /v:off /c \"");
            wrapped.append(command);
            wrapped.push_back(L'"');
            return wrapped;
        }
        return command;
    }

    template<typename Profile>
    bool IsManaged(Profile&& profile)
    {
        // Custom providers have no discoverable native CLI contract. Their
        // profile command is explicitly configured independently of ACP.
        // Per-launch shell overrides still revoke the receipt in the adapter.
        if (std::wstring_view{ profile.AgentProfileId() }.starts_with(L"custom:"))
        {
            return Settings::Model::AgentRegistry::SupportsNativeProfile(std::wstring_view{ profile.AgentProfileId() });
        }
        if (!Settings::Model::AgentRegistry::SupportsNativeProfile(std::wstring_view{ profile.AgentProfileId() }) || profile.HasCommandline())
        {
            return false;
        }
        const auto source = profile.CommandlineOverrideSource();
        return !source || source.Origin() == winrt::Microsoft::Terminal::Settings::Model::OriginTag::Generated;
    }

    template<typename Profile>
    std::wstring Command(const Profile& profile, const bool requireExecutable = false, const std::wstring_view path = NativePath())
    {
        THROW_HR_IF(E_INVALIDARG, !Settings::Model::AgentRegistry::SupportsNativeProfile(std::wstring_view{ profile.AgentProfileId() }));
        if (std::wstring_view{ profile.AgentProfileId() }.starts_with(L"custom:"))
        {
            const auto values = ParseArguments(profile.Commandline());
            if (values.empty())
            {
                THROW_HR_IF_MSG(E_INVALIDARG, requireExecutable, "Configure the native interactive command for this custom agent profile.");
                return {};
            }
            std::wstring arguments;
            for (size_t i = 1; i < values.size(); ++i)
            {
                AgentPaneRestore::AppendQuoted(arguments, values[i]);
                arguments.push_back(L' ');
            }
            if (!profile.AgentProfileModel().empty())
            {
                AgentPaneRestore::AppendQuoted(arguments, L"--model");
                arguments.push_back(L' ');
                AgentPaneRestore::AppendQuoted(arguments, profile.AgentProfileModel());
                arguments.push_back(L' ');
            }
            arguments.append(profile.AgentProfileArguments());
            return BuildCommand(values.front(), profile.AgentProfileId(), {}, {}, arguments, requireExecutable);
        }
        std::filesystem::path executable;
        try
        {
            executable = ResolveExecutable(profile.AgentProfileId(), path);
        }
        catch (const std::filesystem::filesystem_error&)
        {
            if (requireExecutable)
            {
                throw;
            }
            LOG_CAUGHT_EXCEPTION_MSG("Native agent command preview could not inspect the executable");
        }
        if (executable.empty())
        {
            THROW_HR_IF_MSG(HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), requireExecutable, "Native agent executable is unavailable");
            // Orphaned profiles must remain editable without an installed CLI.
            executable = std::wstring{ Settings::Model::AgentRegistry::CliExecutable(std::wstring_view{ profile.AgentProfileId() }) } + L".exe";
        }
        return BuildCommand(executable.native(), profile.AgentProfileId(), profile.AgentProfileModel(), profile.AgentProfilePermissionMode(), profile.AgentProfileArguments(), requireExecutable);
    }

    // The profile owns both launch and resume options. The conversation ID is
    // data, not a user-entered placeholder or a shell command.
    template<typename Profile>
    std::wstring ResumeCommand(Profile&& profile, const std::wstring_view sessionId, const bool requireExecutable = false)
    {
        THROW_HR_IF(E_INVALIDARG, !AgentSessionId::IsSafeForCliResume(sessionId));
        const auto arguments = ParseArguments(profile.AgentProfileResumeArguments());
        auto command = IsManaged(profile) ? Command(profile, requireExecutable) : std::wstring{ profile.Commandline() };
        THROW_HR_IF(E_INVALIDARG, command.empty());
        const auto batch = command.find(L" /d /s /v:off /c \"") != std::wstring::npos;
        if (batch)
        {
            const auto safe = [](const std::wstring_view value) { return value.find_first_of(L"%!\"\r\n^") == std::wstring_view::npos; };
            THROW_HR_IF(E_INVALIDARG, !safe(sessionId) || !std::ranges::all_of(arguments, safe));
            THROW_HR_IF(E_INVALIDARG, command.back() != L'"');
            command.pop_back();
        }
        for (const auto& argument : arguments)
        {
            command.push_back(L' ');
            AgentPaneRestore::AppendQuoted(command, argument);
        }
        command.push_back(L' ');
        AgentPaneRestore::AppendQuoted(command, sessionId);
        if (batch)
        {
            command.push_back(L'"');
        }
        return command;
    }

    template<typename Profile>
    bool IsProtected(Profile&& profile)
    {
        const auto provider = profile.AgentProfileId();
        const auto id = std::wstring_view{ provider };
        return profile.Guid() == winrt::guid{ L"{fd19208a-412b-4857-8a2d-9ca592b4b16e}" } ||
               (profile.Source() == Source &&
                (!profile.Orphaned() ||
                 (id.empty() && std::ranges::any_of(Settings::Model::AgentRegistry::BuiltinNativeProfileAgents,
                                                    [&](const auto& agent) { return profile.Guid() == GeneratedGuid(agent.id); })) ||
                 (!id.starts_with(L"custom:") && Settings::Model::AgentRegistry::SupportsNativeProfile(id))));
    }
}
