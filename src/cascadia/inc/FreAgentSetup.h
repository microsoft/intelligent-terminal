// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string_view>

namespace Microsoft::Terminal::FreAgentSetup
{
    enum class AvailabilityState
    {
        Available,
        Unavailable,
        BlockedByPolicy,
    };

    inline AvailabilityState ClassifyAvailability(
        const bool hasSelectableAgent,
        const bool hasPermittedAgent,
        const bool agentPolicyLocked) noexcept
    {
        if (hasSelectableAgent)
        {
            return AvailabilityState::Available;
        }
        return !hasPermittedAgent && agentPolicyLocked ?
                   AvailabilityState::BlockedByPolicy :
                   AvailabilityState::Unavailable;
    }

    inline bool CanSave(const std::wstring_view agentId) noexcept
    {
        return !agentId.empty();
    }

    inline bool ShouldInstallHooks(
        const std::wstring_view agentId,
        const bool hooksEnabled,
        const bool hooksBlockedByPolicy) noexcept
    {
        return CanSave(agentId) && hooksEnabled && !hooksBlockedByPolicy;
    }

    inline bool CanContinueAfterPostInstallRefresh(
        const bool refreshSucceeded,
        const std::wstring_view agentId,
        const bool selectedAgentIsAvailable) noexcept
    {
        return refreshSucceeded && CanSave(agentId) && selectedAgentIsAvailable;
    }
}
