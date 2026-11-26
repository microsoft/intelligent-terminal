// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>

namespace Microsoft::Terminal::ProgressPresentation
{
    constexpr bool HasVisibleProgress(const uint64_t state, const uint64_t value) noexcept
    {
        // A zero-value determinate ring has no arc. Keep the identity icon
        // instead of replacing it with an empty slot or inventing progress.
        return state == 3 || (state != 0 && value != 0);
    }
}
