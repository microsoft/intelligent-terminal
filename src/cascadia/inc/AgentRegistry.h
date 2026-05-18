// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <array>
#include <string_view>

// Built-in agents shared by:
//   - Settings UI (TerminalSettingsEditor/AIAgentsViewModel.cpp) — populates
//     the ACP/Delegate dropdowns in the AI Agents settings page
//   - Bottom-bar selector (TerminalApp/TerminalPage.cpp
//     _PopulateAgentSelectorFlyout) — populates the quick-switch flyout
//
// Keep the two lists here so both consumers stay in sync. Custom agents
// configured by the user are appended separately by each consumer.
namespace Microsoft::Terminal::Settings::Model::AgentRegistry
{
    struct BuiltinAgent
    {
        std::wstring_view id;
        std::wstring_view displayName;
    };

    // ACP-capable agents. Either the CLI itself speaks the Agent Control
    // Protocol (copilot, gemini), or an npm-distributed adapter does
    // (claude, codex via @zed-industries/{claude-code,codex}-acp).
    // Only these agents can be hosted in an agent pane.
    inline constexpr std::array<BuiltinAgent, 4> BuiltinAcpAgents{ {
        { L"copilot", L"GitHub Copilot" },
        { L"claude", L"Claude" },
        { L"codex", L"Codex" },
        { L"gemini", L"Gemini" },
    } };

    // Delegate agents. Invoked for `?<prompt>` background delegation and
    // similar flows. The set is broader than ACP because delegation doesn't
    // require an ACP-speaking agent — any CLI agent that accepts a prompt
    // as input works.
    inline constexpr std::array<BuiltinAgent, 4> BuiltinDelegateAgents{ {
        { L"copilot", L"GitHub Copilot" },
        { L"claude", L"Claude" },
        { L"codex", L"Codex" },
        { L"gemini", L"Gemini" },
    } };
}
