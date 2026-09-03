// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

namespace TerminalApp::Autofix
{
    enum class State
    {
        Idle,
        Detected,
        Pending,
        Review,
    };

    [[nodiscard]] constexpr bool HasDiagnostics(const State state) noexcept
    {
        return state == State::Detected ||
               state == State::Pending ||
               state == State::Review;
    }
}
