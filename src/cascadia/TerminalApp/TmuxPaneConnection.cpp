// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TmuxPaneConnection.h"
#include "../../types/inc/utils.hpp"

using namespace winrt::Microsoft::Terminal::TerminalConnection;

namespace winrt::TerminalApp::implementation
{
    TmuxPaneConnection::TmuxPaneConnection(InputHandler input, ResizeHandler resize) :
        _input{ std::move(input) },
        _resize{ std::move(resize) },
        _sessionId{ ::Microsoft::Console::Utils::CreateGuid() }
    {
    }

    void TmuxPaneConnection::Initialize(const Windows::Foundation::Collections::ValueSet&) noexcept
    {
    }

    void TmuxPaneConnection::Start()
    {
        {
            std::lock_guard lock{ _outputMutex };
            if (_started || State() == ConnectionState::Closed)
            {
                return;
            }
            _started = true;
            if (!_pendingOutput.empty())
            {
                TerminalOutput.raise(winrt_wstring_to_array_view(_pendingOutput));
                _pendingOutput.clear();
            }
        }
    }

    void TmuxPaneConnection::WriteInput(const winrt::array_view<const char16_t> data)
    {
        if (State() == ConnectionState::Connected && _input)
        {
            std::lock_guard lock{ _inputMutex };
            std::string bytes;
            THROW_IF_FAILED(til::u16u8(winrt_array_to_wstring_view(data), bytes, _encoder));
            if (!bytes.empty())
            {
                _input(bytes);
            }
        }
    }

    void TmuxPaneConnection::Resize(const uint32_t rows, const uint32_t columns)
    {
        if (State() < ConnectionState::Closing && _resize && rows && columns)
        {
            _resize(rows, columns);
        }
    }

    void TmuxPaneConnection::Close()
    {
        if (_state.exchange(ConnectionState::Closed) != ConnectionState::Closed)
        {
            StateChanged.raise(*this, nullptr);
        }
        std::lock_guard lock{ _outputMutex };
        _pendingOutput.clear();
    }

    winrt::guid TmuxPaneConnection::SessionId() const noexcept
    {
        return _sessionId;
    }

    ConnectionState TmuxPaneConnection::State() const noexcept
    {
        return _state.load();
    }

    void TmuxPaneConnection::WriteOutput(const std::string_view bytes)
    {
        std::lock_guard lock{ _outputMutex };
        if (State() == ConnectionState::Closed)
        {
            return;
        }
        std::wstring text;
        THROW_IF_FAILED(til::u8u16(bytes, text, _decoder));
        if (_started)
        {
            if (!text.empty())
            {
                TerminalOutput.raise(winrt_wstring_to_array_view(text));
            }
        }
        else
        {
            constexpr size_t maximumPendingCharacters = 8 * 1024 * 1024;
            THROW_HR_IF_MSG(HRESULT_FROM_WIN32(ERROR_BUFFER_OVERFLOW),
                            text.size() > maximumPendingCharacters - _pendingOutput.size(),
                            "tmux pane output exceeded the initialization buffer limit");
            _pendingOutput.append(text);
        }
    }

    void TmuxPaneConnection::SetState(const ConnectionState state)
    {
        auto previous = _state.load();
        while (previous < ConnectionState::Closing && previous < state)
        {
            if (_state.compare_exchange_weak(previous, state))
            {
                StateChanged.raise(*this, nullptr);
                return;
            }
        }
    }
}
