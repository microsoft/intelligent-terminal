// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// FreAgentPrereq.h
//
// Pure-function predicates for the FRE agent-selection page. Encode
// which ACP agent IDs require which prerequisites (Copilot CLI, Node.js
// runtime) so the Save path's install logic and the agent-pane hint-row
// visibility logic share one definition.
//
// Two callsites in `FreOverlay.cpp` previously open-coded these
// predicates separately (the XAML hint row on
// `_OnAgentSelectionChanged` and the install gate inside
// `_SaveAndInstallAsync`). Extracting them eliminates the drift risk
// where one site is updated to recognize a new agent ID and the other
// is forgotten.
//
// Agent runtime requirements (see `inc/AgentRegistry.h` comment block):
//   * copilot  — uses GitHub.Copilot CLI; no Node dependency (speaks ACP natively)
//   * gemini   — no winget-installed prerequisite (speaks ACP natively)
//   * claude   — uses `@zed-industries/claude-code-acp` npm adapter; requires Node
//   * codex    — uses `@zed-industries/codex-acp` npm adapter; requires Node
//
// This header is consumed by both `FreOverlay.cpp` (TerminalApp project)
// and `FreAgentPrereqTests.cpp` (ut_app project). No `winrt`, no XAML,
// no filesystem access — just `std::wstring_view` in, `bool` out.

#pragma once

#include <string_view>

namespace Microsoft::Terminal::FreAgentPrereq
{
    // Does this agent require a Node runtime to function at all?
    // Used for UI affordances (the FRE hint row at the agent picker)
    // that warn the user about the Node dependency before they hit Save.
    // Independent of whether Node is currently installed.
    inline bool AgentNeedsNodeRuntime(std::wstring_view agentId) noexcept
    {
        return agentId == L"claude" || agentId == L"codex";
    }

    // Combined gate consumed by `_SaveAndInstallAsync` to decide whether
    // to kick off `_WingetInstallAsync(OpenJS.NodeJS.LTS)` for the
    // selected agent. Returns true iff the agent requires Node *and*
    // Node is not already installed. The caller is responsible for the
    // runtime install check; this predicate just keeps the agent-ID
    // half of the decision pinned.
    inline bool ShouldInstallNodeFor(std::wstring_view agentId, bool nodeAlreadyInstalled) noexcept
    {
        return AgentNeedsNodeRuntime(agentId) && !nodeAlreadyInstalled;
    }

    // Combined gate consumed by `_SaveAndInstallAsync` to decide whether
    // to kick off `_WingetInstallAsync(GitHub.Copilot)`. Only the
    // Copilot agent ever triggers a Copilot-CLI install; the other
    // built-in agents (Claude/Codex/Gemini) must never trigger it
    // regardless of whether Copilot CLI happens to also be installed
    // on the box.
    inline bool ShouldInstallCopilotFor(std::wstring_view agentId, bool copilotAlreadyInstalled) noexcept
    {
        return agentId == L"copilot" && !copilotAlreadyInstalled;
    }
}
