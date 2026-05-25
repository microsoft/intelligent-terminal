// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPipeConnection.g.h"

#include <wil/resource.h>

namespace winrt::Microsoft::Terminal::TerminalConnection::implementation
{
    struct AgentPipeConnection : AgentPipeConnectionT<AgentPipeConnection>
    {
    public:
        AgentPipeConnection(uint32_t rows, uint32_t columns);
        ~AgentPipeConnection();

        void Start();
        void WriteInput(const winrt::array_view<const char16_t> data);
        void Resize(uint32_t rows, uint32_t columns);
        void Close();

        void Initialize(const Windows::Foundation::Collections::ValueSet& /*settings*/) const noexcept {}

        winrt::guid SessionId() const noexcept { return _sessionId; }
        ConnectionState State() const noexcept;

        uint64_t WtaInputHandle() const noexcept;
        uint64_t WtaOutputHandle() const noexcept;

        til::event<TerminalOutputHandler> TerminalOutput;
        til::typed_event<ITerminalConnection, IInspectable> StateChanged;

    private:
        void _readerLoop();
        void _setState(ConnectionState newState);

        wil::unique_handle _terminalReadHandle;   // VT from wta → TermControl
        wil::unique_handle _terminalWriteHandle;  // keystrokes from TermControl → wta
        wil::unique_handle _wtaReadHandle;        // wta's keystroke read end
        wil::unique_handle _wtaWriteHandle;       // wta's VT write end

        std::thread _reader;
        std::atomic<bool> _closing{ false };

        uint32_t _rows{ 24 };
        uint32_t _cols{ 80 };

        ConnectionState _state{ ConnectionState::NotConnected };
        winrt::guid _sessionId;
        std::mutex _stateMutex;
    };
}

namespace winrt::Microsoft::Terminal::TerminalConnection::factory_implementation
{
    BASIC_FACTORY(AgentPipeConnection);
}
