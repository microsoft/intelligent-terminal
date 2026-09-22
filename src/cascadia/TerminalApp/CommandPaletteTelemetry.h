// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

namespace TerminalApp::CommandPaletteTelemetry
{
    class AgentPromptEntry
    {
    public:
        bool Update(const bool visible, const bool foreground) noexcept
        {
            const auto active = visible && foreground;
            const auto entered = active && !_active;
            _active = active;
            return entered;
        }

    private:
        bool _active = false;
    };
}
