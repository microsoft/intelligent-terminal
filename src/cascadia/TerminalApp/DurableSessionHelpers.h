// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cstddef>
#include <optional>
#include <string>
#include <vector>

namespace winrt::TerminalApp::implementation
{
    struct DurableTabSessionCloseActions
    {
        bool save{ false };
        bool persistScrollback{ false };
    };

    inline constexpr DurableTabSessionCloseActions GetDurableTabSessionCloseActions(
        const winrt::Microsoft::Terminal::Settings::Model::FirstWindowPreference preference) noexcept
    {
        using winrt::Microsoft::Terminal::Settings::Model::FirstWindowPreference;

        switch (preference)
        {
        case FirstWindowPreference::PersistedLayout:
            return { true, false };
        case FirstWindowPreference::PersistedLayoutAndContent:
            return { true, true };
        case FirstWindowPreference::DefaultProfile:
        default:
            return { false, false };
        }
    }

    inline constexpr bool ShouldPersistDurableTabSession(
        const bool hasUserInput,
        const bool hasDurableId,
        const bool hasAgentSession) noexcept
    {
        return hasUserInput || hasDurableId || hasAgentSession;
    }

    inline std::optional<winrt::guid> TryParseTabSessionId(
        const winrt::hstring& durableTabSessionId)
    {
        if (durableTabSessionId.empty())
        {
            return std::nullopt;
        }

        std::wstring text{ durableTabSessionId };
        if (text.front() != L'{')
        {
            text.insert(text.begin(), L'{');
            text.push_back(L'}');
        }

        GUID durableId{};
        if (FAILED(IIDFromString(text.c_str(), &durableId)))
        {
            return std::nullopt;
        }
        return winrt::guid{ durableId };
    }

    inline constexpr bool ShouldResumeAgentSession(
        const bool hasAgentSession,
        const bool hasDurableTabSessionBufferPath) noexcept
    {
        return hasAgentSession && hasDurableTabSessionBufferPath;
    }

    // A persisted agent pane is worth restoring as soon as it carries any
    // user-visible state. The ACP session id is optional: wta only projects one
    // after the conversation becomes meaningful, so requiring it would drop the
    // open/view/position of a pane the user opened but never chatted in.
    inline constexpr bool ShouldRestoreAgentPane(
        const bool hasAgentPaneSessionId,
        const bool hasAgentPaneView,
        const bool agentPaneOpen) noexcept
    {
        return hasAgentPaneSessionId || hasAgentPaneView || agentPaneOpen;
    }

    inline winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs GetTerminalArgsForRestoreAction(
        const winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs& action)
    {
        using namespace winrt::Microsoft::Terminal::Settings::Model;

        if (const auto newTabArgs = action.Args().try_as<NewTabArgs>())
        {
            return newTabArgs.ContentArgs().try_as<NewTerminalArgs>();
        }
        if (const auto splitPaneArgs = action.Args().try_as<SplitPaneArgs>())
        {
            return splitPaneArgs.ContentArgs().try_as<NewTerminalArgs>();
        }
        return nullptr;
    }

    inline void RemoveAgentPaneSessionFromShellBindings(
        std::vector<winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs>& actions,
        const winrt::hstring& agentPaneSessionId)
    {
        if (agentPaneSessionId.empty())
        {
            return;
        }

        for (const auto& action : actions)
        {
            if (const auto terminalArgs = GetTerminalArgsForRestoreAction(action);
                terminalArgs && terminalArgs.AgentSessionId() == agentPaneSessionId)
            {
                terminalArgs.AgentSessionId(L"");
                terminalArgs.AgentSessionAgent(L"");
                terminalArgs.AgentResumeCommandline(L"");
            }
        }
    }

    inline bool TryAcceptWindowClose(bool& closeAccepted) noexcept
    {
        if (closeAccepted)
        {
            return false;
        }

        closeAccepted = true;
        return true;
    }

    // A window can reach its final save from either the close path or the
    // quit/session-end path, and both may run for the same window. Latch the
    // first one so a tab is never written to the store twice.
    inline bool TryClaimDurableTabSessionPersist(bool& alreadyPersisted) noexcept
    {
        if (alreadyPersisted)
        {
            return false;
        }

        alreadyPersisted = true;
        return true;
    }

    template<typename TPathForSession>
    inline void SetPersistedLayoutAgentRestorePaths(
        std::vector<winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs>& actions,
        TPathForSession&& pathForSession)
    {
        std::vector<winrt::hstring> agentPaneSessionIds;
        for (const auto& action : actions)
        {
            if (const auto terminalArgs = GetTerminalArgsForRestoreAction(action);
                terminalArgs && !terminalArgs.AgentPaneSessionId().empty())
            {
                agentPaneSessionIds.emplace_back(terminalArgs.AgentPaneSessionId());
            }
        }

        for (const auto& action : actions)
        {
            const auto terminalArgs = GetTerminalArgsForRestoreAction(action);
            if (terminalArgs &&
                !terminalArgs.AgentSessionId().empty() &&
                std::find(agentPaneSessionIds.begin(), agentPaneSessionIds.end(), terminalArgs.AgentSessionId()) == agentPaneSessionIds.end() &&
                terminalArgs.SessionId() != winrt::guid{})
            {
                terminalArgs.DurableTabSessionBufferPath(pathForSession(terminalArgs.SessionId()));
            }
        }
    }
}
