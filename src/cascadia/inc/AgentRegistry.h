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
    enum class ByokMode
    {
        Unsupported,
        CopilotProviderEnvironment,
        OpenCodeConfigContent,
    };

    enum class YoloSettingsNotice
    {
        None,
        Unavailable,
        Conditional,
    };

    struct BuiltinAgent
    {
        std::wstring_view id;
        // Fallback display name (English). UI consumers should prefer the
        // localized name from .resw resources (e.g. "AgentName_Copilot").
        std::wstring_view displayName;
        // Describes whether and how the agent consumes the shared BYOK
        // provider selected in settings.
        ByokMode byokMode;
    };

    // ACP-capable agents. Either the CLI itself speaks the Agent Control
    // Protocol (copilot, gemini, opencode), or an npm-distributed adapter does
    // (claude via @agentclientprotocol/claude-agent-acp, codex via
    // @agentclientprotocol/codex-acp).
    // Only these agents can be hosted in an agent pane.
    inline constexpr std::array<BuiltinAgent, 5> BuiltinAcpAgents{ {
        { L"copilot", L"GitHub Copilot", ByokMode::CopilotProviderEnvironment },
        { L"claude", L"Claude", ByokMode::Unsupported },
        { L"codex", L"Codex", ByokMode::Unsupported },
        { L"gemini", L"Gemini", ByokMode::Unsupported },
        { L"opencode", L"OpenCode", ByokMode::OpenCodeConfigContent },
    } };

    // Delegate agents. Invoked for `?<prompt>` background delegation and
    // similar flows. The set is broader than ACP because delegation doesn't
    // require an ACP-speaking agent — any CLI agent that accepts a prompt
    // as input works.
    inline constexpr std::array<BuiltinAgent, 5> BuiltinDelegateAgents{ {
        { L"copilot", L"GitHub Copilot", ByokMode::CopilotProviderEnvironment },
        { L"claude", L"Claude", ByokMode::Unsupported },
        { L"codex", L"Codex", ByokMode::Unsupported },
        { L"gemini", L"Gemini", ByokMode::Unsupported },
        { L"opencode", L"OpenCode", ByokMode::OpenCodeConfigContent },
    } };

    inline constexpr ByokMode GetByokMode(const std::wstring_view agentId) noexcept
    {
        for (const auto& agent : BuiltinAcpAgents)
        {
            if (agent.id == agentId)
            {
                return agent.byokMode;
            }
        }
        return ByokMode::Unsupported;
    }

    inline constexpr bool SupportsByok(const std::wstring_view agentId) noexcept
    {
        return GetByokMode(agentId) != ByokMode::Unsupported;
    }

    inline constexpr bool AgentIdEquals(const std::wstring_view lhs, const std::wstring_view rhs) noexcept
    {
        if (lhs.size() != rhs.size())
        {
            return false;
        }

        for (size_t i = 0; i < lhs.size(); ++i)
        {
            const auto lower = [](const wchar_t ch) {
                return ch >= L'A' && ch <= L'Z' ? ch + (L'a' - L'A') : ch;
            };
            if (lower(lhs[i]) != lower(rhs[i]))
            {
                return false;
            }
        }
        return true;
    }

    inline constexpr YoloSettingsNotice GetYoloSettingsNotice(const std::wstring_view agentId,
                                                              const bool yoloModeEnabled,
                                                              const bool policyLocked,
                                                              const bool selectedProviderAvailable) noexcept
    {
        if (!yoloModeEnabled || policyLocked || !selectedProviderAvailable)
        {
            return YoloSettingsNotice::None;
        }
        if (agentId == L"opencode")
        {
            return YoloSettingsNotice::Unavailable;
        }
        if (agentId == L"gemini")
        {
            return YoloSettingsNotice::Conditional;
        }
        return YoloSettingsNotice::None;
    }

    inline constexpr bool SupportsLiveModelSwitch(const std::wstring_view agentId) noexcept
    {
        for (const auto& agent : BuiltinAcpAgents)
        {
            if (AgentIdEquals(agent.id, agentId))
            {
                // Gemini accepts --model at process launch but its ACP server
                // does not implement either model-switch request.
                return agent.id != L"gemini";
            }
        }
        return false;
    }

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
