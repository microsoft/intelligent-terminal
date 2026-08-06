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
    };

    inline constexpr ShellSessionCloseActions GetShellSessionCloseActions(const bool restoreShellSessions, const bool continueRunningCommands) noexcept
    {
        return {
            .save = restoreShellSessions,
            .detach = continueRunningCommands,
        };
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

    enum class HeadlessTrayActivationMode
    {
        SummonExistingWindow = 0,
        RestorePersistedLayoutsBeforeFreshWindow = 1,
        OpenFreshWindow = 2,
    };

    inline HeadlessTrayActivationMode ClassifyHeadlessTrayActivation(const bool hasWindows, const bool hasKeptSessions) noexcept
    {
        if (hasWindows)
        {
            return HeadlessTrayActivationMode::SummonExistingWindow;
        }

        return hasKeptSessions ? HeadlessTrayActivationMode::RestorePersistedLayoutsBeforeFreshWindow :
                                 HeadlessTrayActivationMode::OpenFreshWindow;
    }

    enum class ForcedKeptLayoutCloseAction
    {
        None = 0,
        StartGeneration = 1,
        AppendGeneration = 2,
    };

    inline constexpr ForcedKeptLayoutCloseAction ClassifyForcedKeptLayoutClose(const bool generationActive,
                                                                                const size_t windowCount,
                                                                                const bool hasKeptSessions) noexcept
    {
        if (windowCount == 0 || !hasKeptSessions)
        {
            return ForcedKeptLayoutCloseAction::None;
        }

        return generationActive ? ForcedKeptLayoutCloseAction::AppendGeneration :
                                  ForcedKeptLayoutCloseAction::StartGeneration;
    }

    inline constexpr bool ShouldPreserveForcedKeptLayoutGeneration(const bool generationActive, const bool hasKeptSessions) noexcept
    {
        return generationActive && hasKeptSessions;
    }

    inline constexpr bool ShouldCompleteForcedKeptLayoutGeneration(const bool generationActive, const bool hasKeptSessions) noexcept
    {
        return generationActive && !hasKeptSessions;
    }

    inline std::vector<winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs> BuildKeptGroupRestoreActions(const winrt::TerminalApp::KeptGroupRestoreResult& restoredGroup)
    {
        using namespace winrt::Microsoft::Terminal::Settings::Model;

        std::vector<ActionAndArgs> actions;
        if (!restoredGroup)
        {
            return actions;
        }

        const auto contentIds = restoredGroup.ContentIds();
        if (!contentIds || contentIds.Size() == 0)
        {
            return actions;
        }

        actions.reserve(static_cast<size_t>(contentIds.Size()));

        const auto shellSessionId = restoredGroup.ShellSessionId();
        const auto shellSessionRevision = restoredGroup.ShellSessionRevision();

        for (const auto contentId : contentIds)
        {
            NewTerminalArgs terminalArgs;
            terminalArgs.ContentId(contentId);

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
