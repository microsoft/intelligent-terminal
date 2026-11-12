// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <optional>
#include <utility>

namespace TerminalApp::SidebarTelemetry
{
    constexpr const char* View(const bool agentsOnly, const bool recent) noexcept
    {
        return recent ? (agentsOnly ? "agentsAndRecent" : "recent") : (agentsOnly ? "agents" : "all");
    }

    constexpr int64_t AgeDays(const std::optional<uint64_t> lastActivityMs, const uint64_t nowMs) noexcept
    {
        if (!lastActivityMs || *lastActivityMs > nowMs)
        {
            return -1;
        }
        return static_cast<int64_t>((nowMs - *lastActivityMs) / 86'400'000);
    }

    class KeyboardEntry
    {
    public:
        void Preparing(const bool wasInside, const bool willBeInside, const bool keyboard) noexcept
        {
            _pending = !wasInside && willBeInside && keyboard;
        }

        bool Focused(const bool inside) noexcept
        {
            return std::exchange(_pending, false) && inside;
        }

    private:
        bool _pending{};
    };
}
