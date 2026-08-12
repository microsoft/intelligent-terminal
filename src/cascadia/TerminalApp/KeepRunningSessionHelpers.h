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
        bool detach{ false };
        bool persistScrollback{ false };
    };

    inline constexpr ShellSessionCloseActions GetShellSessionCloseActions(
        const winrt::Microsoft::Terminal::Settings::Model::FirstWindowPreference preference,
        const bool keepRunning) noexcept
    {
        using winrt::Microsoft::Terminal::Settings::Model::FirstWindowPreference;

        switch (preference)
        {
        case FirstWindowPreference::PersistedLayout:
            return { true, keepRunning, false };
        case FirstWindowPreference::PersistedLayoutAndContent:
            return { true, keepRunning, true };
        case FirstWindowPreference::DefaultProfile:
        default:
            return { false, keepRunning, false };
        }
    }

    inline constexpr bool ShouldPersistShellSession(
        const bool hasUserInput,
        const bool hasDurableId,
        const bool keepRunning,
        const bool hasAgentSession) noexcept
    {
        return hasUserInput || hasDurableId || keepRunning || hasAgentSession;
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

    inline winrt::guid GetKeepRunningGroupId(
        const winrt::hstring& tabStableId,
        const winrt::hstring& durableShellSessionId)
    {
        if (const auto durableId = TryParseShellSessionId(durableShellSessionId))
        {
            return *durableId;
        }

        GUID tabId{};
        winrt::check_hresult(IIDFromString(tabStableId.c_str(), &tabId));
        return winrt::guid{ tabId };
    }

    inline constexpr bool ShouldResumeAgentSession(
        const bool hasAgentSession,
        const bool hasKeptSessionId,
        const bool hasShellSessionRestorePath) noexcept
    {
        return hasAgentSession &&
               (hasKeptSessionId || hasShellSessionRestorePath);
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

    // Rebinds a persisted layout to shells that are still running.
    //
    // A persisted layout describes what was on screen; keep-running means some
    // of those shells outlived the window and are still alive with no pane. On
    // the way back the two have to be reconciled, or replaying the layout would
    // start fresh shells beside the live ones and strand them in the tray.
    //
    // `KeptSessionId` is what asks a pane to adopt live content, so stamp it
    // onto every pane whose session is still being kept and give the pane a new
    // identity of its own, exactly as the tray and protocol restores do. Panes
    // with no live session are left untouched and start normally, which is also
    // what makes a kept session that had no tab at close time stay closed: it
    // never appears in the layout, so nothing here brings it back.
    //
    // Call after `SetPersistedLayoutAgentRestorePaths`, which keys off the
    // original `SessionId`. The snapshot path it assigned is deliberately kept:
    // if the live session dies before the pane is built, the reattach misses and
    // the snapshot is the fallback.
    template<typename TIsKeptSession, typename TNewSessionId>
    inline void ReattachKeptPanesInPersistedLayout(
        std::vector<winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs>& actions,
        TIsKeptSession&& isKeptSession,
        TNewSessionId&& newSessionId)
    {
        for (const auto& action : actions)
        {
            const auto terminalArgs = GetTerminalArgsForRestoreAction(action);
            if (!terminalArgs)
            {
                continue;
            }

            const auto sessionId = terminalArgs.SessionId();
            if (sessionId == winrt::guid{} || !isKeptSession(sessionId))
            {
                continue;
            }

            terminalArgs.KeptSessionId(sessionId);
            terminalArgs.SessionId(newSessionId());
        }
    }

    // Clears the keep-running opt-in a persisted window layout recorded for one
    // durable shell session.
    //
    // The layout is the whole-window restore's input, so a stale `true` here
    // would put the flag back on the next launch even after the database copy
    // was cleared. Matching is by durable id because that is the only thing
    // tying a saved layout entry to the session the user just closed.
    inline bool ClearPersistedKeepRunningInLayouts(
        const winrt::Windows::Foundation::Collections::IVector<winrt::Microsoft::Terminal::Settings::Model::WindowLayout>& layouts,
        const winrt::hstring& shellSessionId)
    {
        if (!layouts || shellSessionId.empty())
        {
            return false;
        }

        auto changed = false;
        for (const auto& layout : layouts)
        {
            if (!layout || !layout.TabLayout())
            {
                continue;
            }

            for (const auto& action : layout.TabLayout())
            {
                const auto terminalArgs = GetTerminalArgsForRestoreAction(action);
                if (terminalArgs &&
                    terminalArgs.KeepRunning() &&
                    terminalArgs.DurableShellSessionId() == shellSessionId)
                {
                    terminalArgs.KeepRunning(false);
                    changed = true;
                }
            }
        }

        return changed;
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

    template<typename TRestore>
    inline bool RestoreAllKeptGroups(const winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring>& groups,
                                     TRestore&& restore)
    {
        std::vector<winrt::guid> groupIds;
        if (groups)
        {
            groupIds.reserve(groups.Size());
            for (const auto& group : groups)
            {
                groupIds.emplace_back(group.Key());
            }
        }

        auto restoredAny = false;
        for (const auto& groupId : groupIds)
        {
            restoredAny = restore(groupId) || restoredAny;
        }
        return restoredAny;
    }

    inline std::vector<winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs> BuildKeptGroupRestoreActions(const winrt::TerminalApp::KeptGroupRestoreResult& restoredGroup)
    {
        using namespace winrt::Microsoft::Terminal::Settings::Model;

        std::vector<ActionAndArgs> actions;
        if (!restoredGroup)
        {
            return actions;
        }

        const auto restoreArgs = restoredGroup.RestoreArgs();
        if (!restoreArgs || restoreArgs.Size() == 0)
        {
            return actions;
        }

        actions.reserve(static_cast<size_t>(restoreArgs.Size()));

        const auto shellSessionId = restoredGroup.ShellSessionId();
        const auto shellSessionRevision = restoredGroup.ShellSessionRevision();

        for (const auto& storedArgs : restoreArgs)
        {
            if (!storedArgs)
            {
                continue;
            }

            auto terminalArgs = storedArgs.Copy().try_as<NewTerminalArgs>();
            terminalArgs.DurableShellSessionId(L"");
            terminalArgs.DurableShellSessionRevision(0);
            terminalArgs.KeptSessionId(terminalArgs.SessionId());
            // Survives the JSON hop into the target window, unlike
            // KeptSessionId. The tab was explicitly opted into keep-running
            // before it was detached, so restoring it keeps that opt-in.
            terminalArgs.KeepRunning(true);

            if (actions.empty())
            {
                if (!shellSessionId.empty())
                {
                    terminalArgs.DurableShellSessionId(shellSessionId);
                    terminalArgs.DurableShellSessionRevision(shellSessionRevision);
                }

                actions.emplace_back(ShortcutAction::NewTab, NewTabArgs{ terminalArgs });
            }
            else
            {
                actions.emplace_back(ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Automatic, 0.5f, terminalArgs });
            }
        }

        // The tray row the user clicked is labeled with the detached tab's
        // title. Reattaching only rebuilds panes, so without this the tab comes
        // back labeled from its profile instead of the name it was closed under.
        if (const auto title = restoredGroup.Title();
            !actions.empty() && !title.empty())
        {
            actions.emplace_back(ShortcutAction::RenameTab, RenameTabArgs{ title });
        }

        return actions;
    }
}
