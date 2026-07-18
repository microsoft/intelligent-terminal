// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "PsmuxConnection.g.h"
#include "BaseTerminalConnection.h"

#include <til/env.h>
#include <til/ticket_lock.h>
#include <til/unicode.h>

namespace winrt::Microsoft::Terminal::TerminalConnection::implementation
{
    struct PsmuxConnection : PsmuxConnectionT<PsmuxConnection>, BaseTerminalConnection<PsmuxConnection>
    {
        PsmuxConnection() = default;
        void Initialize(const Windows::Foundation::Collections::ValueSet& settings);

        static safe_void_coroutine final_release(std::unique_ptr<PsmuxConnection> connection);

        void Start();
        void WriteInput(const winrt::array_view<const char16_t> buffer);
        void Resize(uint32_t rows, uint32_t columns);
        void Close() noexcept;

        til::event<TerminalOutputHandler> TerminalOutput;

    private:
        enum class CaptureState
        {
            None,
            BeginMarkerResponse,
            AwaitingCaptureResponse,
            CaptureResponse,
            AwaitingEndMarkerResponse,
            EndMarkerResponse,
        };

        void _launchClient();
        void _writeControlCommand(std::string_view command);
        DWORD _outputThread();
        void _processProtocolLine(std::string_view line, til::u8state& u8State);
        void _raiseUtf8(std::string_view value, til::u8state& u8State);
        static std::string _decodeOutput(std::string_view value);
        static std::wstring _formatStatus(uint32_t status);

        uint32_t _rows{ 30 };
        uint32_t _cols{ 120 };
        hstring _commandline{};
        hstring _startingDirectory{};
        hstring _startingTitle{};
        Windows::Foundation::Collections::ValueSet _environment{ nullptr };
        bool _reloadEnvironmentVariables{ false };
        hstring _initialEnvironment{};
        guid _profileGuid{};
        bool _reattaching{ false };

        til::env _initialEnv{};
        wil::unique_hfile _input;
        wil::unique_hfile _output;
        wil::unique_process_information _piClient;
        wil::unique_handle _hOutputThread;
        til::ticket_lock _writeLock;

        std::string _protocolBuffer;
        std::string _captureContents;
        std::string _captureResponseEnd;
        std::string _captureResponseError;
        std::chrono::steady_clock::time_point _deduplicateCaptureUntil{};
        CaptureState _captureState{ CaptureState::None };
    };
}

namespace winrt::Microsoft::Terminal::TerminalConnection::factory_implementation
{
    BASIC_FACTORY(PsmuxConnection);
}
