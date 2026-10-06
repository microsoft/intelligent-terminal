// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string_view>
#include <optional>
#include <set>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Microsoft.Terminal.Settings.Model.h>

#include "../inc/AgentPolicy.h"
#include "../inc/CustomAgentId.h"

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    // AI configuration uses bounded dedicated events, never generic per-setting events.
    // Match the context and JSON key, leaving actions and unrelated settings untouched.
    constexpr bool IsAISettingChange(const std::string_view change) noexcept
    {
        if (change.starts_with("global."))
        {
            const auto setting = change.substr(std::string_view{ "global." }.size());
            constexpr std::string_view keys[]{
                "acpAgent",
                "acpModel",
                "acpCustomCommand",
                "acpCustomCommands",
                "delegateAgent",
                "delegateModel",
                "delegateCustomCommand",
                "delegateCustomCommands",
                "customModelSelection",
                "customModelProviders",
                "autoErrorDetectionEnabled",
                "autoFixEnabled",
                "agentSessionManagementEnabled",
                "showTokenUsageAndCost",
                "agentPanePosition",
                "agentPane.yoloMode",
                "aiIntegration.coordinator.enabled",
                "aiIntegration.coordinator.commandline",
                "aiIntegration.coordinator.profile",
                "aiIntegration.confirmation.readOperations",
                "aiIntegration.confirmation.createOperations",
                "aiIntegration.confirmation.inputOperations",
            };
            for (const auto key : keys)
            {
                if (setting == key)
                {
                    return true;
                }
            }

            // Also protect nested provider fields if change logging expands them later.
            return setting.starts_with("customModelProviders.") || setting.starts_with("customModelProviders[");
        }

        const auto separator = change.find('.');
        if (separator != std::string_view::npos)
        {
            const auto context = change.substr(0, separator);
            const auto setting = change.substr(separator + 1);
            if (context == "profile" || context == "profileDefaults")
            {
                return setting == "agentPaneBackend" || setting == "commandPaletteAgent";
            }
        }
        return false;
    }

    namespace AgentSettingsTelemetry
    {
        constexpr const char* ProviderId(const std::wstring_view value) noexcept
        {
            if (value.empty())
            {
                return "none";
            }
            if (value == L"copilot")
            {
                return "copilot";
            }
            if (value == L"claude")
            {
                return "claude";
            }
            if (value == L"codex")
            {
                return "codex";
            }
            if (value == L"gemini")
            {
                return "gemini";
            }
            if (value == L"opencode")
            {
                return "opencode";
            }
            return value.starts_with(L"custom:") ? "custom" : "unknown";
        }

        struct ProviderChange
        {
            const char* from;
            const char* to;
        };

        constexpr std::optional<ProviderChange> GetProviderChange(const std::wstring_view previous, const std::wstring_view current) noexcept
        {
            // Compare before bucketing: switching between two custom agents is still a change.
            if (previous == current)
            {
                return std::nullopt;
            }
            return ProviderChange{ ProviderId(previous), ProviderId(current) };
        }

        struct CustomInventory
        {
            uint32_t count;
            bool selectedCommandConfigured;
        };

        inline CustomInventory GetCustomInventory(const std::wstring_view selectedId,
                                                  const std::wstring_view legacyCommand,
                                                  const winrt::Windows::Foundation::Collections::IVector<winrt::hstring>& commands)
        {
            // The editor keys custom entries by the derived executable ID, not command arguments.
            std::set<winrt::hstring> ids;
            const auto add = [&](const std::wstring_view command) {
                const auto id = ::Microsoft::Terminal::Settings::Model::DeriveCustomAgentId(command);
                if (!id.empty())
                {
                    ids.insert(id);
                }
            };
            if (commands)
            {
                for (const auto& command : commands)
                {
                    add(std::wstring_view{ command });
                }
            }
            add(legacyCommand);

            const auto selected = selectedId.starts_with(L"custom:");
            const auto configured = selected && ids.contains(winrt::hstring{ selectedId.substr(7) });
            return { static_cast<uint32_t>(ids.size()), configured };
        }

        struct PolicySummary
        {
            const char* allowedAgentsCategory;
            const char* allowCustomAgentsCategory;
        };

        struct ProviderSnapshot
        {
            const char* configured;
            const char* effective;
            CustomInventory custom;
        };

        inline ProviderSnapshot GetProviderSnapshot(const Model::GlobalAppSettings& globals, const bool primary)
        {
            const auto selected = primary ? globals.AcpAgent() : globals.DelegateAgent();
            const auto effective = primary ? globals.EffectiveAcpAgent() : globals.EffectiveDelegateAgent();
            return {
                ProviderId(selected),
                ProviderId(effective),
                primary ? GetCustomInventory(selected, globals.AcpCustomCommand(), globals.AcpCustomCommands()) :
                          GetCustomInventory(selected, globals.DelegateCustomCommand(), globals.DelegateCustomCommands()),
            };
        }

        inline PolicySummary SummarizePolicy(const ::Microsoft::Terminal::Settings::Model::AgentPolicy::PolicySnapshot& policy) noexcept
        {
            using ::Microsoft::Terminal::Settings::Model::AgentPolicy::PolicyState;
            return {
                !policy.allowedAgents ? "not_configured" : (policy.allowedAgents->empty() ? "empty" : "allowlist"),
                policy.customAgents == PolicyState::NotConfigured ? "not_configured" : (policy.customAgents == PolicyState::Blocked ? "blocked" : "allowed"),
            };
        }
    }
}
