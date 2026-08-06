// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

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
            .detach = restoreShellSessions && continueRunningCommands,
        };
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
