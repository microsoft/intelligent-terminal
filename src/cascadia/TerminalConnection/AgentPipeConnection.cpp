// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentPipeConnection.h"

#include "AgentPipeConnection.g.cpp"

#include <combaseapi.h>

namespace winrt::Microsoft::Terminal::TerminalConnection::implementation
{
    AgentPipeConnection::AgentPipeConnection(uint32_t rows, uint32_t columns) :
        _rows{ rows ? rows : 24 },
        _cols{ columns ? columns : 80 }
    {
        // Each side of the agent pane gets one anonymous pipe. Buffer
        // size 0 means "system default" — small enough that wta backs
        // off cooperatively when TermControl falls behind, large enough
        // that single-frame Ratatui writes don't block.
        SECURITY_ATTRIBUTES sa{};
        sa.nLength = sizeof(sa);
        sa.bInheritHandle = FALSE;
        sa.lpSecurityDescriptor = nullptr;

        HANDLE readT{};
        HANDLE writeW{};
        // Pipe1: wta writes VT → TermControl reads VT.
        THROW_IF_WIN32_BOOL_FALSE(CreatePipe(&readT, &writeW, &sa, 0));
        _terminalReadHandle.reset(readT);
        _wtaWriteHandle.reset(writeW);

        HANDLE readW{};
        HANDLE writeT{};
        // Pipe2: TermControl writes keystrokes → wta reads.
        THROW_IF_WIN32_BOOL_FALSE(CreatePipe(&readW, &writeT, &sa, 0));
        _wtaReadHandle.reset(readW);
        _terminalWriteHandle.reset(writeT);

        THROW_IF_FAILED(CoCreateGuid(reinterpret_cast<GUID*>(&_sessionId)));
    }

    AgentPipeConnection::~AgentPipeConnection()
    {
        Close();
    }

    ConnectionState AgentPipeConnection::State() const noexcept
    {
        return _state;
    }

    uint64_t AgentPipeConnection::WtaInputHandle() const noexcept
    {
        return reinterpret_cast<uint64_t>(_wtaReadHandle.get());
    }

    uint64_t AgentPipeConnection::WtaOutputHandle() const noexcept
    {
        return reinterpret_cast<uint64_t>(_wtaWriteHandle.get());
    }

    void AgentPipeConnection::Start()
    {
        if (!_terminalReadHandle)
        {
            _setState(ConnectionState::Failed);
            return;
        }

        // The reader pumps bytes from wta's VT write side. TermControl
        // expects UTF-8 -> UTF-16 conversion; we mirror the same
        // codepath ConptyConnection uses (chunked read, UTF-8 decode).
        _reader = std::thread([this]() {
            try
            {
                _readerLoop();
            }
            CATCH_LOG()
        });

        _setState(ConnectionState::Connected);
    }

    void AgentPipeConnection::WriteInput(const winrt::array_view<const char16_t> data)
    {
        if (!_terminalWriteHandle || _closing.load(std::memory_order_acquire))
        {
            return;
        }

        const auto wstr = std::wstring_view{
            reinterpret_cast<const wchar_t*>(data.data()),
            data.size()
        };
        if (wstr.empty())
        {
            return;
        }

        const auto needed = WideCharToMultiByte(CP_UTF8,
                                                0,
                                                wstr.data(),
                                                gsl::narrow_cast<int>(wstr.size()),
                                                nullptr,
                                                0,
                                                nullptr,
                                                nullptr);
        if (needed <= 0)
        {
            return;
        }

        std::string utf8;
        utf8.resize(static_cast<size_t>(needed));
        WideCharToMultiByte(CP_UTF8,
                            0,
                            wstr.data(),
                            gsl::narrow_cast<int>(wstr.size()),
                            utf8.data(),
                            needed,
                            nullptr,
                            nullptr);

        DWORD written{};
        // Synchronous write — anonymous pipes don't support overlapped
        // I/O, and the buffer is small enough that we don't worry about
        // blocking the dispatcher thread for a keystroke.
        WriteFile(_terminalWriteHandle.get(),
                  utf8.data(),
                  gsl::narrow_cast<DWORD>(utf8.size()),
                  &written,
                  nullptr);
    }

    void AgentPipeConnection::Resize(uint32_t rows, uint32_t columns)
    {
        _rows = rows ? rows : _rows;
        _cols = columns ? columns : _cols;
        // The actual resize is sent by the caller as `_internal.resize_pane`
        // over IProtocolServer; this connection only stashes the dimensions.
        // Anonymous pipes carry no out-of-band channel for SIGWINCH-style
        // signalling, so resize has to ride the COM control plane.
    }

    void AgentPipeConnection::Close()
    {
        if (_closing.exchange(true, std::memory_order_acq_rel))
        {
            return;
        }

        _setState(ConnectionState::Closing);

        // Closing the read handle on the reader side breaks the
        // blocking ReadFile and unwinds the reader thread.
        _terminalReadHandle.reset();
        _terminalWriteHandle.reset();
        _wtaReadHandle.reset();
        _wtaWriteHandle.reset();

        if (_reader.joinable())
        {
            _reader.join();
        }

        _setState(ConnectionState::Closed);
    }

    void AgentPipeConnection::_readerLoop()
    {
        // Reading from an anonymous pipe blocks until either bytes
        // arrive or the write end closes. We hand bytes off to the
        // TerminalOutput delegate after UTF-8 → UTF-16 decode. Partial
        // UTF-8 codepoints are kept in `pending` until the next read
        // completes them; otherwise a multi-byte glyph straddling a
        // ReadFile boundary would corrupt.
        std::array<char, 4096> chunk{};
        std::string pending;

        while (!_closing.load(std::memory_order_acquire))
        {
            DWORD bytesRead{};
            const auto ok = ReadFile(_terminalReadHandle.get(),
                                     chunk.data(),
                                     gsl::narrow_cast<DWORD>(chunk.size()),
                                     &bytesRead,
                                     nullptr);
            if (!ok || bytesRead == 0)
            {
                // Either wta closed its end (EOF) or we asked the
                // reader to stop. Either way: bail.
                break;
            }

            pending.append(chunk.data(), bytesRead);

            // Find the longest prefix of `pending` that ends on a
            // complete UTF-8 sequence. Any tail bytes that look like
            // an unfinished sequence stay in `pending` for the next
            // round.
            size_t boundary = pending.size();
            while (boundary > 0)
            {
                const auto b = static_cast<unsigned char>(pending[boundary - 1]);
                if ((b & 0x80) == 0)
                {
                    break;
                }
                if ((b & 0xC0) == 0xC0)
                {
                    --boundary;
                    break;
                }
                --boundary;
                if (pending.size() - boundary >= 4)
                {
                    boundary = pending.size();
                    break;
                }
            }

            if (boundary == 0)
            {
                continue;
            }

            const auto needed = MultiByteToWideChar(CP_UTF8,
                                                    0,
                                                    pending.data(),
                                                    gsl::narrow_cast<int>(boundary),
                                                    nullptr,
                                                    0);
            if (needed > 0)
            {
                std::wstring utf16;
                utf16.resize(static_cast<size_t>(needed));
                MultiByteToWideChar(CP_UTF8,
                                    0,
                                    pending.data(),
                                    gsl::narrow_cast<int>(boundary),
                                    utf16.data(),
                                    needed);

                try
                {
                    TerminalOutput.raise(winrt::array_view<const char16_t>{
                        reinterpret_cast<const char16_t*>(utf16.data()),
                        reinterpret_cast<const char16_t*>(utf16.data() + utf16.size()) });
                }
                CATCH_LOG()
            }

            pending.erase(0, boundary);
        }
    }

    void AgentPipeConnection::_setState(ConnectionState newState)
    {
        {
            std::lock_guard lock{ _stateMutex };
            if (_state == newState)
            {
                return;
            }
            _state = newState;
        }
        try
        {
            StateChanged.raise(*this, nullptr);
        }
        CATCH_LOG()
    }
}
