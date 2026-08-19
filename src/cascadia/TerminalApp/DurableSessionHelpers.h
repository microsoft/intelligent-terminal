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
    struct ShellSessionCloseActions
    {
        bool save{ false };
        bool persistScrollback{ false };
    };

    inline constexpr ShellSessionCloseActions GetShellSessionCloseActions(
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

    inline constexpr bool ShouldPersistShellSession(
        const bool hasUserInput,
        const bool hasDurableId,
        const bool hasAgentSession) noexcept
    {
        return hasUserInput || hasDurableId || hasAgentSession;
    }

    inline std::optional<winrt::guid> TryParseShellSessionId(
        const winrt::hstring& durableShellSessionId)
    {
        if (durableShellSessionId.empty())
        {
            return std::nullopt;
        }

        std::wstring text{ durableShellSessionId };
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
        const bool hasShellSessionRestorePath) noexcept
    {
        return hasAgentSession && hasShellSessionRestorePath;
    }

    // `paneBound` mirrors `params["pane_bound"]`, which wtcli stamps from
    // whether the event carried an explicit `--pane`. False means the pane id
    // was inferred from the focused pane purely so the event could be
    // delivered — an agent-pane CLI has no WT_SESSION and would otherwise have
    // its ACP session attributed to whatever shell pane happens to be focused.
    //
    // wtcli ships in the same package as this code, so the flag is always
    // present for events that came through it; anything else is not
    // authoritative about its origin and must not create a binding either.
    inline constexpr bool ShouldBindPaneAgentSession(
        const bool sessionStarted,
        const bool paneBound) noexcept
    {
        return !sessionStarted || paneBound;
    }

    // An agent that exited leaves nothing to resume, so its pane drops the
    // binding and comes back as the plain shell it now is.
    //
    // Only an event that named its own pane may clear one. An agent-pane CLI
    // has no WT_SESSION, so wtcli stamps its `agent.session.end` with whichever
    // pane happens to be focused; acting on that would clear an unrelated shell
    // pane's binding.
    inline constexpr bool ShouldUnbindPaneAgentSession(const bool paneBound) noexcept
    {
        return paneBound;
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
                terminalArgs.ShellSessionRestorePath(pathForSession(terminalArgs.SessionId()));
            }
        }
    }
}
