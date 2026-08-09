// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstddef>
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
        const bool hasKeepRunningPane,
        const bool hasAgentSession) noexcept
    {
        return hasUserInput || hasDurableId || hasKeepRunningPane || hasAgentSession;
    }

    inline constexpr bool ShouldResumeAgentSession(
        const bool hasAgentSession,
        const bool hasKeptSessionId,
        const bool hasShellSessionRestorePath,
        const bool useWorkspaceBuffer) noexcept
    {
        return hasAgentSession &&
               (hasKeptSessionId || hasShellSessionRestorePath || useWorkspaceBuffer);
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

        return actions;
    }
}
