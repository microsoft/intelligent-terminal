// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPaneRestore.h"
#include "AgentRegistry.h"
#include <filesystem>
#include <map>
#include <shellapi.h>
#include <til/env.h>

namespace Microsoft::Terminal::AgentProfiles
{
    inline constexpr std::wstring_view Source{ L"IntelligentTerminal.AgentProfiles" };

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
        for (const auto extension : { L".exe", L".cmd" })
        {
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
                    const auto candidate = std::filesystem::path{ directory } / (std::wstring{ id } + extension);
                    if (std::filesystem::is_regular_file(candidate))
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
        for (const auto& agent : Settings::Model::AgentRegistry::BuiltinDelegateAgents)
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
            THROW_HR_IF(E_INVALIDARG, arguments.find(L'\0') != std::wstring_view::npos);
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

    inline void ValidateArguments(const std::wstring_view id, const std::vector<std::wstring>& arguments)
    {
        for (size_t i = 0; i < arguments.size(); ++i)
        {
            const std::wstring_view argument{ arguments[i] };
            const auto equals = argument.find(L'=');
            const auto flag = argument.substr(0, equals);
            const auto takesValue =
                ((id == L"claude" || id == L"copilot" || id == L"codex") && flag == L"--add-dir") ||
                (id == L"claude" && flag == L"--resume") ||
                (id == L"gemini" && (flag == L"--resume" || flag == L"--prompt-interactive")) ||
                (id == L"opencode" && (flag == L"--session" || flag == L"--prompt"));
            const auto standalone = flag == L"--help" || flag == L"--version" ||
                                    (id == L"copilot" && flag == L"--resume") ||
                                    ((id == L"copilot" || id == L"claude" || id == L"opencode") && flag == L"--continue") ||
                                    (id == L"codex" && flag == L"--no-alt-screen");
            THROW_HR_IF_MSG(E_INVALIDARG, !(takesValue || standalone), "Unsupported or conflicting native agent argument");
            if (takesValue)
            {
                std::wstring_view value;
                if (equals != std::wstring_view::npos)
                {
                    value = argument.substr(equals + 1);
                }
                else
                {
                    THROW_HR_IF(E_INVALIDARG, ++i == arguments.size());
                    value = arguments[i];
                }
                THROW_HR_IF(E_INVALIDARG, value.empty() || value.front() == L'-');
            }
            else
            {
                THROW_HR_IF(E_INVALIDARG, equals != std::wstring_view::npos);
            }
        }
    }

    inline std::wstring BuildCommand(const std::wstring_view executable,
                                     const std::wstring_view id,
                                     const std::wstring_view model,
                                     const std::wstring_view permissionMode,
                                     const std::wstring_view arguments,
                                     const bool validate = true)
    {
        THROW_HR_IF(E_INVALIDARG, validate && !std::ranges::any_of(Settings::Model::AgentRegistry::BuiltinDelegateAgents, [&](const auto& agent) { return agent.id == id; }));
        auto values = ParseArguments(arguments);
        if (validate)
        {
            ValidateArguments(id, values);
        }
        if (!permissionMode.empty())
        {
            if (id == L"copilot" && (permissionMode == L"allow-all-tools" || permissionMode == L"allow-all"))
            {
                values.insert(values.begin(), L"--" + std::wstring{ permissionMode });
            }
            else if (id == L"opencode" && permissionMode == L"auto")
            {
                values.insert(values.begin(), L"--auto");
            }
            else
            {
                const auto valid =
                    (id == L"claude" && (permissionMode == L"acceptEdits" || permissionMode == L"auto" ||
                                        permissionMode == L"bypassPermissions" || permissionMode == L"manual" ||
                                        permissionMode == L"dontAsk" || permissionMode == L"plan")) ||
                    (id == L"codex" && (permissionMode == L"untrusted" || permissionMode == L"on-request" || permissionMode == L"never")) ||
                    (id == L"gemini" && (permissionMode == L"default" || permissionMode == L"auto_edit" || permissionMode == L"yolo" || permissionMode == L"plan"));
                THROW_HR_IF(E_INVALIDARG, validate && !valid);
                const auto flag = id == L"claude" ? L"--permission-mode" : id == L"codex" ? L"--ask-for-approval" : L"--approval-mode";
                values.insert(values.begin(), { flag, std::wstring{ permissionMode } });
            }
        }
        if (!model.empty())
        {
            THROW_HR_IF(E_INVALIDARG, validate && (model.front() == L'-' || model.find(L'\0') != std::wstring_view::npos));
            values.insert(values.begin(), { L"--model", std::wstring{ model } });
        }
        THROW_HR_IF(E_INVALIDARG, executable.empty() || executable.find(L'\0') != std::wstring_view::npos);
        if (validate)
        {
            // ConPTY expands environment references before CreateProcess; keep validated argv unchanged.
            THROW_HR_IF_MSG(E_INVALIDARG, executable.find(L'%') != std::wstring_view::npos ||
                                            std::ranges::any_of(values, [](const auto& value) { return value.find(L'%') != std::wstring::npos; }),
                           "Environment references are not supported in managed native agent commands");
        }
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

    inline void CheckLaunchPolicy(const std::wstring_view id, const Settings::Model::AgentPolicy::PolicySnapshot& policy)
    {
        THROW_HR_IF_MSG(E_ACCESSDENIED, !Settings::Model::AgentRegistry::IsNativeAgentProviderAllowed(id, policy) ||
                                        policy.yoloMode == Settings::Model::AgentPolicy::PolicyState::Blocked,
                     "Managed native agent launch blocked by policy");
    }

    template<typename Profile>
    bool IsManaged(Profile&& profile)
    {
        if (profile.AgentProfileId().empty() || profile.HasCommandline())
        {
            return false;
        }
        const auto source = profile.CommandlineOverrideSource();
        return !source || source.Origin() == winrt::Microsoft::Terminal::Settings::Model::OriginTag::Generated;
    }

    template<typename Profile>
    std::wstring Command(const Profile& profile, const bool requireExecutable = false, const std::wstring_view path = NativePath())
    {
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
            executable = std::wstring{ std::wstring_view{ profile.AgentProfileId() } } + L".exe";
        }
        return BuildCommand(executable.native(), profile.AgentProfileId(), profile.AgentProfileModel(),
                            profile.AgentProfilePermissionMode(), profile.AgentProfileArguments(), requireExecutable);
    }
}
