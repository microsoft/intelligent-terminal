// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentRegistry.h"

#include <string_view>

namespace Microsoft::Terminal::Settings::Model::AgentYoloPolicy
{
    // A user-initiated provider command is blocked only by organization
    // policy. Provider support and errors remain provider-owned and are
    // surfaced through the existing command path.
    inline constexpr bool CanUserRequestEnable(const bool policyBlocked) noexcept
    {
        return !policyBlocked;
    }

    inline constexpr bool IsAutomaticProviderKnownUnsupported(
        const std::wstring_view providerId) noexcept
    {
        return AgentRegistry::IsYoloSettingUnavailableForDefaultAgent(providerId);
    }

    // Settings can offer automatic enablement only for a provider that the
    // host knows how to initialize. Runtime-advertised capability and
    // acknowledgement remain WTA-owned.
    inline constexpr bool IsAutomaticEnableAvailable(
        const bool policyBlocked,
        const std::wstring_view providerId) noexcept
    {
        return CanUserRequestEnable(policyBlocked) &&
               !providerId.empty() &&
               !IsAutomaticProviderKnownUnsupported(providerId);
    }

    // Decides only the Settings-owned automatic request. User-initiated
    // provider commands are a separate path and do not require a default
    // provider match when policy allows them.
    inline constexpr bool ShouldRequestAutomaticEnable(
        const bool configuredEnabled,
        const bool policyBlocked,
        const std::wstring_view defaultProviderId,
        const std::wstring_view currentProviderId) noexcept
    {
        return configuredEnabled &&
               IsAutomaticEnableAvailable(policyBlocked, defaultProviderId) &&
               !currentProviderId.empty() &&
               AgentRegistry::AgentIdEquals(defaultProviderId, currentProviderId);
    }
}
