// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <vector>

namespace winrt::TerminalApp::implementation
{
    // An agent CLI is only resumed for a pane that came back from a persisted
    // layout. `PersistedBufferPath` is what marks such a pane: the startup path
    // stamps it onto every restored agent-bound shell pane, so an
    // `AgentSessionId` arriving any other way (a `wt` commandline, say) starts a
    // normal shell instead of silently re-attaching to somebody's old
    // conversation.
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

    // Marks each agent-bound shell pane as having come out of a persisted
    // layout by pointing it at the `buffer_{guid}.txt` file its scrollback was
    // written to. `_MakeTerminalPane` derives that same path from `SessionId`
    // on its own, so the value is only ever an override — what matters is that
    // a non-empty path is the marker `ShouldResumeAgentSession` looks for.
    //
    // A session that belongs to an agent pane is skipped and so never gets the
    // marker, which is what keeps the restore from relaunching that CLI as a
    // shell command on top of the agent pane that already owns it. This runs
    // over the whole window's actions, so it also covers a session that was
    // first run in a shell pane and later resumed into an agent pane on a
    // different tab.
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
