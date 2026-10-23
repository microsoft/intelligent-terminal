// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPaneRestore.h"
#include "CustomAgentId.h"
#include <json/json.h>
#include <filesystem>
#include <optional>

namespace Microsoft::Terminal::CustomAgents
{
    struct Invocation
    {
        std::wstring command;
        std::vector<std::wstring> args;
    };

    inline bool IsId(const std::wstring_view id) noexcept
    {
        return id.starts_with(L"custom:") && id.size() > 7 &&
               std::ranges::all_of(id.substr(7), [](const wchar_t ch) {
                   return (ch >= L'a' && ch <= L'z') || (ch >= L'A' && ch <= L'Z') ||
                          (ch >= L'0' && ch <= L'9') || ch == L'-' || ch == L'_' || ch == L'.';
               });
    }

    inline void Validate(const Invocation& invocation, const bool resume = false)
    {
        THROW_HR_IF_MSG(E_INVALIDARG, invocation.command.find_first_not_of(L" \t") == std::wstring::npos ||
                                         invocation.command.find(L'\0') != std::wstring::npos ||
                                         invocation.command.find_first_of(L"\r\n") != std::wstring::npos ||
                                         invocation.command.find(L"{sessionId}") != std::wstring::npos, "Invalid custom agent executable");
        size_t sessionIds = 0;
        for (const auto& argument : invocation.args)
        {
            THROW_HR_IF_MSG(E_INVALIDARG, argument.find(L'\0') != std::wstring::npos, "Custom agent arguments cannot contain NUL");
            if (resume && argument == L"{sessionId}")
            {
                ++sessionIds;
            }
            else
            {
                THROW_HR_IF_MSG(E_INVALIDARG, resume && argument.find_first_of(L"{}") != std::wstring::npos, "A resume placeholder must be one complete {sessionId} argument");
            }
        }
        THROW_HR_IF_MSG(E_INVALIDARG, resume && sessionIds != 1, "A custom resume command requires exactly one {sessionId} argument");
    }

    inline Invocation Parse(const std::wstring_view commandline, const bool resume = false)
    {
        THROW_HR_IF(E_INVALIDARG, commandline.empty() || commandline.find(L'\0') != std::wstring_view::npos);
        bool quoted = false;
        size_t slashes = 0;
        size_t tokenStart = 0;
        bool tokenQuoted = false;
        const auto checkToken = [&](const size_t end) {
            const auto token = commandline.substr(tokenStart, end - tokenStart);
            THROW_HR_IF_MSG(E_INVALIDARG, !tokenQuoted && (token == L"&&" || token == L"||" || token == L"|" || token == L">" || token == L"<"), "Custom agent commands require an executable and arguments, not shell operators");
        };
        size_t offset = 0;
        for (const auto ch : commandline)
        {
            if (!quoted && (ch == L' ' || ch == L'\t'))
            {
                checkToken(offset);
                tokenStart = offset + 1;
                tokenQuoted = false;
            }
            if (ch == L'"' && slashes % 2 == 0)
            {
                quoted = !quoted;
                tokenQuoted = true;
            }
            slashes = ch == L'\\' ? slashes + 1 : 0;
            ++offset;
        }
        checkToken(commandline.size());
        THROW_HR_IF_MSG(E_INVALIDARG, quoted, "Unclosed quote in custom agent command");
        const auto first = commandline.find_first_not_of(L" \t");
        THROW_HR_IF(E_INVALIDARG, first == std::wstring_view::npos);
        const std::wstring text{ commandline.substr(first) };
        int argc = 0;
        wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(text.c_str(), &argc) };
        THROW_LAST_ERROR_IF(!argv);
        THROW_HR_IF(E_INVALIDARG, argc == 0);
        Invocation invocation{ argv[0], {} };
        for (int i = 1; i < argc; ++i)
        {
            const std::wstring_view argument{ argv[i] };
            invocation.args.emplace_back(argument);
        }
        Validate(invocation, resume);
        return invocation;
    }

    inline Invocation FromJson(const Json::Value& json, const bool resume = false)
    {
        THROW_HR_IF_MSG(E_INVALIDARG, !json.isObject() || !json["command"].isString() || (json.isMember("args") && !json["args"].isArray()), "A custom agent invocation requires command and an argument array");
        Invocation invocation{ std::wstring{ winrt::to_hstring(json["command"].asString()) }, {} };
        for (const auto& argument : json["args"])
        {
            THROW_HR_IF(E_INVALIDARG, !argument.isString());
            invocation.args.emplace_back(winrt::to_hstring(argument.asString()));
        }
        Validate(invocation, resume);
        return invocation;
    }

    inline Json::Value ToJson(const Invocation& invocation)
    {
        Json::Value json{ Json::objectValue };
        json["command"] = winrt::to_string(invocation.command);
        json["args"] = Json::Value{ Json::arrayValue };
        for (const auto& argument : invocation.args)
        {
            json["args"].append(winrt::to_string(argument));
        }
        return json;
    }

    inline std::wstring Format(const Invocation& invocation)
    {
        std::wstring command;
        AgentPaneRestore::AppendQuoted(command, invocation.command);
        for (const auto& argument : invocation.args)
        {
            command.push_back(L' ');
            AgentPaneRestore::AppendQuoted(command, argument);
        }
        return command;
    }

    inline std::wstring LaunchCommand(const Invocation& invocation)
    {
        Validate(invocation);
        THROW_HR_IF_MSG(E_INVALIDARG, invocation.command.find(L'%') != std::wstring::npos || std::ranges::any_of(invocation.args, [](const auto& arg) { return arg.find(L'%') != std::wstring::npos; }), "Environment references are not supported in generated custom agent commands");
        auto command = Format(invocation);
        const auto extension = std::filesystem::path{ invocation.command }.extension().native();
        if (til::equals_insensitive_ascii(extension, L".cmd") || til::equals_insensitive_ascii(extension, L".bat"))
        {
            // Windows PowerShell 5 loses these values when a companion forwards
            // $args to a native executable, even though the script receives them.
            const auto unsupported = std::ranges::any_of(invocation.args, [](const auto& arg) {
                // Match .NET Char.IsWhiteSpace, which determines legacy native
                // quoting. A trailing backslash then escapes the closing quote.
                constexpr std::wstring_view whitespace{ L" \t\r\n\v\f\u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000" };
                return arg.empty() || arg.find(L'"') != std::wstring::npos ||
                       (arg.back() == L'\\' && arg.find_first_of(whitespace) != std::wstring::npos);
            });
            THROW_HR_IF_MSG(E_INVALIDARG, unsupported, "Custom agent batch companions cannot forward empty arguments, embedded double quotes, or whitespace-containing arguments ending in a backslash; use a native executable");
            auto shim = std::filesystem::path{ invocation.command };
            if (!std::filesystem::is_regular_file(shim) && !shim.has_parent_path())
            {
                const auto length = SearchPathW(nullptr, invocation.command.c_str(), nullptr, 0, nullptr, nullptr);
                THROW_HR_IF_MSG(HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), length == 0, "Custom agent batch launcher is unavailable");
                std::wstring resolved(length, L'\0');
                const auto written = SearchPathW(nullptr, invocation.command.c_str(), nullptr, length, resolved.data(), nullptr);
                THROW_LAST_ERROR_IF(written == 0);
                THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER), written >= length);
                resolved.resize(written);
                shim = resolved;
            }
            shim = std::filesystem::absolute(shim);
            shim.replace_extension(L".ps1");
            THROW_HR_IF_MSG(HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), !std::filesystem::is_regular_file(shim), "Custom agent batch startup requires an adjacent PowerShell companion");
            const auto literal = [](const std::wstring_view value) {
                std::wstring result{ L"'" };
                for (const auto ch : value)
                {
                    result.push_back(ch);
                    if (ch == L'\'')
                        result.push_back(ch);
                }
                result.push_back(L'\'');
                return result;
            };
            std::wstring script{ L"$ErrorActionPreference='Stop'; & " };
            script.append(literal(shim.native()));
            for (const auto& argument : invocation.args)
            {
                script.push_back(L' ');
                script.append(literal(argument));
            }
            script.append(L"; exit $LASTEXITCODE");
            return Format({ L"powershell.exe", { L"-NoLogo", L"-NoProfile", L"-ExecutionPolicy", L"Bypass", L"-Command", std::move(script) } });
        }

        return command;
    }

    template<typename Globals>
    auto Find(const Globals& globals, const std::wstring_view id)
    {
        using Agent = decltype(globals.CustomAgents().GetAt(0));
        for (const auto& agent : globals.CustomAgents())
        {
            if (Settings::Model::AgentRegistry::AgentIdEquals(agent.Id(), id))
            {
                return agent;
            }
        }
        return Agent{ nullptr };
    }

    template<typename Globals>
    std::wstring ResumeCommand(const Globals& globals, const std::wstring_view id, const std::wstring_view sessionId)
    {
        THROW_HR_IF_MSG(E_INVALIDARG, !AgentPaneRestore::IsValidSessionId(sessionId) || sessionId.starts_with(L"-"), "Invalid custom CLI resume identifier");
        const auto agent = Find(globals, id);
        THROW_HR_IF_MSG(E_INVALIDARG, !agent || agent.CliCommand().empty() || agent.ResumeCommand().empty(), "Custom agent no longer has an interactive resume command");
        auto invocation = Parse(agent.ResumeCommand(), true);
        for (auto& arg : invocation.args)
        {
            if (arg == L"{sessionId}")
                arg = sessionId;
        }
        return LaunchCommand(invocation);
    }

    template<typename Globals>
    std::wstring AcpCommand(const Globals& globals, const std::wstring_view id)
    {
        if (const auto agent = Find(globals, id))
        {
            return std::wstring{ agent.AcpCommand() };
        }
        for (const auto& command : globals.AcpCustomCommands())
        {
            const auto legacyId = L"custom:" + std::wstring{ Settings::Model::DeriveCustomAgentId(command) };
            if (Settings::Model::AgentRegistry::AgentIdEquals(legacyId, id))
            {
                return std::wstring{ command };
            }
        }
        const auto command = globals.AcpCustomCommand();
        const auto legacyId = L"custom:" + std::wstring{ Settings::Model::DeriveCustomAgentId(command) };
        if (!command.empty() && Settings::Model::AgentRegistry::AgentIdEquals(legacyId, id))
        {
            return std::wstring{ command };
        }
        return {};
    }

}
