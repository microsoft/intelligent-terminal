// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string_view>

namespace winrt::Microsoft::Terminal::Settings::Model::implementation
{
    // AI configuration is reported by the session-start snapshot, not per-setting events.
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
}
