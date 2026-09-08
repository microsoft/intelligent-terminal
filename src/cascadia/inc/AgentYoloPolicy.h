// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentRegistry.h"

#include <string_view>

namespace Microsoft::Terminal::Settings::Model::AgentYoloPolicy
{
    // DefaultProvider applies the persisted preference only when the running
    // provider matches the Settings default. LegacyGlobalPreference preserves
    // the existing behavior for binding owners that have not adopted that
    // automatic scope yet.
    enum class AutomaticScope
    {
        DefaultProvider,
        LegacyGlobalPreference,
    };

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

    inline constexpr AutomaticScope ResolveAutomaticScope(
        const bool usesSettingsDefaultProvider,
        const bool scopeToDefaultProvider) noexcept
    {
        return usesSettingsDefaultProvider || scopeToDefaultProvider ?
                   AutomaticScope::DefaultProvider :
                   AutomaticScope::LegacyGlobalPreference;
    }

    // Decides only the Settings-owned automatic request. User-initiated
    // provider commands are a separate path and do not require a default
    // provider match when policy allows them.
    inline constexpr bool ShouldRequestAutomaticEnable(
        const bool configuredEnabled,
        const bool policyBlocked,
        const std::wstring_view defaultProviderId,
        const std::wstring_view currentProviderId,
        const AutomaticScope scope) noexcept
    {
        if (!configuredEnabled || policyBlocked)
        {
            return false;
        }

        if (scope == AutomaticScope::LegacyGlobalPreference)
        {
            return true;
        }

        return IsAutomaticEnableAvailable(policyBlocked, defaultProviderId) &&
               !currentProviderId.empty() &&
               AgentRegistry::AgentIdEquals(defaultProviderId, currentProviderId);
    }
}
