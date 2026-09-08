// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "SharedWta.h"
#include <winrt/Microsoft.Terminal.Control.h>

namespace winrt::TerminalApp::implementation
{
    // Only agile terminal content and the master lease cross window threads.
    // The XAML wrapper is never part of this ownership object.
    class AgentPaneLifetime
    {
    public:
        AgentPaneLifetime() = default;
        AgentPaneLifetime(SharedWtaLease lease, Microsoft::Terminal::Control::ControlInteractivity content) noexcept;
        AgentPaneLifetime(AgentPaneLifetime&& other) noexcept;
        AgentPaneLifetime& operator=(AgentPaneLifetime&& other) noexcept;
        AgentPaneLifetime(const AgentPaneLifetime&) = delete;
        AgentPaneLifetime& operator=(const AgentPaneLifetime&) = delete;
        ~AgentPaneLifetime();

        explicit operator bool() const noexcept { return static_cast<bool>(_lease); }
        void CaptureHelperProcess() noexcept;
        void RetireClosedContent() noexcept;
        void Close() noexcept;

    private:
        SharedWtaLease _lease;
        Microsoft::Terminal::Control::ControlInteractivity _content{ nullptr };
        wil::unique_handle _helperProcess;

        void _Abandon() noexcept;
        static winrt::fire_and_forget _CloseDetached(Microsoft::Terminal::Control::ControlInteractivity content, wil::unique_handle process);
    };
}
