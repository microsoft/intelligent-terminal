// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <vector>

namespace winrt::TerminalApp::implementation
{
    // An agent CLI is only resumed for a pane that came back from a persisted
    // layout. `PersistedBufferPath` is what marks such a pane: the startup path
    // stamps it onto every restored shell pane, so an `AgentSessionId` arriving
    // any other way (a `wt` commandline, say) starts a normal shell instead of
    // silently re-attaching to somebody's old conversation.
    inline constexpr bool ShouldResumeAgentSession(
        const bool hasAgentSession,
        const bool hasPersistedBufferPath) noexcept
    {
        return hasAgentSession && hasPersistedBufferPath;
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

    // The agent pane's own ACP session also shows up as the agent binding of
    // the shell pane that hosts the helper. Leaving it there would make the
    // restore relaunch the helper's CLI as a shell command too, so the pane is
    // resumed twice — once as an agent pane and once as a shell.
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

    // Points each restored shell pane at the `buffer_{guid}.txt` file its
    // scrollback was written to, which is also what tells the pane it came from
    // a persisted layout.
    //
    // Panes whose agent session belongs to an agent pane are skipped: that
    // conversation is replayed through ACP `session/load`, and seeding the
    // terminal buffer as well would show it twice.
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
                terminalArgs.PersistedBufferPath(pathForSession(terminalArgs.SessionId()));
            }
        }
    }
}
