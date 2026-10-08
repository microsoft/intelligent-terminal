// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <optional>

namespace Microsoft::Terminal::Telemetry
{
    class DailyInteraction
    {
    public:
        bool Observe(const uint32_t utcDay) noexcept
        {
            if (_lastDay && utcDay <= *_lastDay)
            {
                return false;
            }
            _lastDay = utcDay;
            return true;
        }

    private:
        // A backwards clock adjustment must not count an already observed day again.
        std::optional<uint32_t> _lastDay;
    };
}
