// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <atomic>
#include <cstdint>
#include <functional>
#include <mutex>
#include <string>
#include <til/u8u16convert.h>
#include <winrt/Microsoft.Terminal.TerminalConnection.h>
#include <til/winrt.h>

namespace winrt::TerminalApp::implementation
{
    struct TmuxPaneConnection : winrt::implements<TmuxPaneConnection, Microsoft::Terminal::TerminalConnection::ITerminalConnection>
    {
        using InputHandler = std::function<void(std::string_view)>;
        using ResizeHandler = std::function<void(uint32_t, uint32_t)>;

        TmuxPaneConnection(InputHandler input, ResizeHandler resize);

        void Initialize(const Windows::Foundation::Collections::ValueSet&) noexcept;
        void Start();
        void WriteInput(winrt::array_view<const char16_t> data);
        void Resize(uint32_t rows, uint32_t columns);
        void Close();
        winrt::guid SessionId() const noexcept;
        Microsoft::Terminal::TerminalConnection::ConnectionState State() const noexcept;

        void WriteOutput(std::string_view bytes);
        void SetState(Microsoft::Terminal::TerminalConnection::ConnectionState state);
        bool IsViewportReady(uint32_t rows, uint32_t columns) const noexcept;

        til::event<Microsoft::Terminal::TerminalConnection::TerminalOutputHandler> TerminalOutput;
        til::typed_event<Microsoft::Terminal::TerminalConnection::ITerminalConnection, Windows::Foundation::IInspectable> StateChanged;

    private:
        InputHandler _input;
        ResizeHandler _resize;
        winrt::guid _sessionId;
        std::atomic<Microsoft::Terminal::TerminalConnection::ConnectionState> _state{
            Microsoft::Terminal::TerminalConnection::ConnectionState::Connecting
        };
        std::mutex _outputMutex;
        std::mutex _inputMutex;
        til::u16state _encoder;
        til::u8state _decoder;
        std::wstring _pendingOutput;
        std::atomic<bool> _started = false;
        std::atomic<uint64_t> _viewportSize = 0;
    };
}
