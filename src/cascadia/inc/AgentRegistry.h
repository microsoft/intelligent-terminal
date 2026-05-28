// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <array>
#include <string_view>
#include <vector>
#include "AgentPolicy.h"

// Built-in agents shared by:
//   - Settings UI (TerminalSettingsEditor/AIAgentsViewModel.cpp) — populates
//     the ACP/Delegate dropdowns in the AI Agents settings page
//   - Bottom-bar selector (TerminalApp/TerminalPage.cpp
//     _PopulateAgentSelectorFlyout) — populates the quick-switch flyout
//   - FRE (TerminalApp/FreOverlay.cpp) — populates the first-run wizard
//
// Keep the two lists here so all consumers stay in sync. Custom agents
// configured by the user are appended separately by each consumer.
//
// Display names are English fallbacks; UI consumers should prefer
// localized names from .resw resources (e.g. "AgentName_Copilot").
// GPO-filtered variants are available via FilteredAcpAgents() /
// FilteredDelegateAgents() — always prefer these over the raw arrays.
namespace Microsoft::Terminal::Settings::Model::AgentRegistry
{
    struct BuiltinAgent
    {
        std::wstring_view id;
        // Fallback display name (English). UI consumers should prefer the
        // localized name from .resw resources (e.g. "AgentName_Copilot").
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

    // Return only agents whose IDs are permitted by GPO policy.
    // When AllowedAgents is not configured, returns all agents.
    // Consumers should always use these instead of iterating the raw arrays.
    // Defined inline so every consuming DLL gets its own copy without
    // needing a dllexport/dllimport dance across module boundaries.
    template<typename ArrayT>
    inline std::vector<BuiltinAgent> _FilterAgents(const ArrayT& agents)
    {
        std::vector<BuiltinAgent> result;
        for (const auto& a : agents)
        {
            if (AgentPolicy::IsAgentAllowed(a.id))
            {
                result.push_back(a);
            }
        }
        return result;
    }

    inline std::vector<BuiltinAgent> FilteredAcpAgents()
    {
        return _FilterAgents(BuiltinAcpAgents);
    }

    inline std::vector<BuiltinAgent> FilteredDelegateAgents()
    {
        return _FilterAgents(BuiltinDelegateAgents);
    }
}
