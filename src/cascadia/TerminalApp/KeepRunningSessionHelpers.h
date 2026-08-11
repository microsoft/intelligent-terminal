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

    inline constexpr bool ShouldBindPaneAgentSession(
        const bool sessionStarted,
        const bool paneBound) noexcept
    {
        return !sessionStarted || paneBound;
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
