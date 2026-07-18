// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "PsmuxConnection.h"

#include "../inc/IntelligentTerminalPaths.h"
#include "../../types/inc/utils.hpp"

#include "PsmuxConnection.g.cpp"

using namespace ::Microsoft::Console;
using namespace std::string_view_literals;

namespace winrt::Microsoft::Terminal::TerminalConnection::implementation
{
    static constexpr std::string_view CaptureBegin{ "__WT_PSMUX_CAPTURE_BEGIN__" };
    static constexpr std::string_view CaptureEnd{ "__WT_PSMUX_CAPTURE_END__" };

    void PsmuxConnection::Initialize(const Windows::Foundation::Collections::ValueSet& settings)
    {
        if (settings)
        {
            _commandline = unbox_prop_or<hstring>(settings, L"commandline", _commandline);
            _startingDirectory = unbox_prop_or<hstring>(settings, L"startingDirectory", _startingDirectory);
            _startingTitle = unbox_prop_or<hstring>(settings, L"startingTitle", _startingTitle);
            _rows = unbox_prop_or<uint32_t>(settings, L"initialRows", _rows);
            _cols = unbox_prop_or<uint32_t>(settings, L"initialCols", _cols);
            _sessionId = unbox_prop_or<guid>(settings, L"sessionId", _sessionId);
            _reattaching = _sessionId != guid{};
            _environment = settings.TryLookup(L"environment").try_as<Windows::Foundation::Collections::ValueSet>();
            _reloadEnvironmentVariables = unbox_prop_or<bool>(settings, L"reloadEnvironmentVariables", false);
            _initialEnvironment = unbox_prop_or<hstring>(settings, L"initialEnvironment", L"");
            _profileGuid = unbox_prop_or<guid>(settings, L"profileGuid", _profileGuid);
        }

        if (_sessionId == guid{})
        {
            _sessionId = Utils::CreateGuid();
        }
    }

    void PsmuxConnection::_launchClient()
    {
        SECURITY_ATTRIBUTES securityAttributes{
            .nLength = sizeof(SECURITY_ATTRIBUTES),
            .lpSecurityDescriptor = nullptr,
            .bInheritHandle = TRUE,
        };

        wil::unique_hfile childInput;
        wil::unique_hfile parentInput;
        THROW_IF_WIN32_BOOL_FALSE(CreatePipe(childInput.addressof(), parentInput.addressof(), &securityAttributes, 0));
        THROW_IF_WIN32_BOOL_FALSE(SetHandleInformation(parentInput.get(), HANDLE_FLAG_INHERIT, 0));

        wil::unique_hfile parentOutput;
        wil::unique_hfile childOutput;
        THROW_IF_WIN32_BOOL_FALSE(CreatePipe(parentOutput.addressof(), childOutput.addressof(), &securityAttributes, 0));
        THROW_IF_WIN32_BOOL_FALSE(SetHandleInformation(parentOutput.get(), HANDLE_FLAG_INHERIT, 0));

        STARTUPINFOEX startupInfo{};
        startupInfo.StartupInfo.cb = sizeof(startupInfo);
        startupInfo.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startupInfo.StartupInfo.hStdInput = childInput.get();
        startupInfo.StartupInfo.hStdOutput = childOutput.get();
        startupInfo.StartupInfo.hStdError = childOutput.get();

        SIZE_T attributeListSize{};
        InitializeProcThreadAttributeList(nullptr, 1, 0, &attributeListSize);
        auto attributeList = std::make_unique<std::byte[]>(attributeListSize);
#pragma warning(suppress : 26490)
        startupInfo.lpAttributeList = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(attributeList.get());
        THROW_IF_WIN32_BOOL_FALSE(InitializeProcThreadAttributeList(startupInfo.lpAttributeList, 1, 0, &attributeListSize));
        const auto deleteAttributeList = wil::scope_exit([&]() {
            DeleteProcThreadAttributeList(startupInfo.lpAttributeList);
        });
        HANDLE inheritedHandles[]{ childInput.get(), childOutput.get() };
        THROW_IF_WIN32_BOOL_FALSE(UpdateProcThreadAttribute(
            startupInfo.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            inheritedHandles,
            sizeof(inheritedHandles),
            nullptr,
            nullptr));

        if (!_startingTitle.empty())
        {
            startupInfo.StartupInfo.lpTitle = const_cast<wchar_t*>(_startingTitle.c_str());
        }

        if (_reloadEnvironmentVariables)
        {
            _initialEnv.regenerate();
        }
        else if (!_initialEnvironment.empty())
        {
            _initialEnv = til::env{ _initialEnvironment.c_str() };
        }
        else
        {
            _initialEnv = til::env::from_current_environment();
        }

        auto& environment = _initialEnv.as_map();
        environment.insert_or_assign(L"WT_SESSION", Utils::GuidToPlainString(_sessionId));
        environment.insert_or_assign(L"WT_PROFILE_ID", Utils::GuidToString(_profileGuid));

        wchar_t value[512];
        if (GetEnvironmentVariableW(L"WT_COM_CLSID", value, ARRAYSIZE(value)))
        {
            environment.insert_or_assign(L"WT_COM_CLSID", value);
        }

        const auto wtaLogDir = ::IntelligentTerminal::LogDirVersioned();
        if (!wtaLogDir.empty())
        {
            environment.insert_or_assign(L"WTA_HOOK_LOG_DIR", wtaLogDir.wstring());
        }

        auto& wslEnv = environment[L"WSLENV"];
        std::wstring additionalWslEnv;
        std::unordered_set<std::wstring> wslEnvVars{ L"PATH" };
        for (const auto& part : til::split_iterator{ std::wstring_view{ wslEnv }, L':' })
        {
            const auto key = til::safe_slice_len(part, 0, part.rfind(L'/'));
            wslEnvVars.emplace(key);
        }

        const auto addToWslEnv = [&](const std::wstring_view key) {
            if (wslEnvVars.emplace(key).second)
            {
                additionalWslEnv.append(key);
                additionalWslEnv.push_back(L':');
            }
        };
        for (const auto variable : { L"WT_SESSION"sv, L"WT_PROFILE_ID"sv, L"WT_COM_CLSID"sv, L"WTA_HOOK_LOG_DIR"sv })
        {
            addToWslEnv(variable);
        }

        if (_environment)
        {
            for (const auto item : _environment)
            {
                const auto key = item.Key();
                const auto itemValue = item.Value().try_as<Windows::Foundation::IPropertyValue>();
                if (itemValue && itemValue.Type() == Windows::Foundation::PropertyType::String)
                {
                    _initialEnv.set_user_environment_var(key, itemValue.GetString());
                    addToWslEnv(key);
                }
            }
        }

        if (!additionalWslEnv.empty())
        {
            const auto hasColon = additionalWslEnv.ends_with(L':');
            const auto needsColon = !wslEnv.starts_with(L':');
            if (hasColon != needsColon)
            {
                if (hasColon)
                {
                    additionalWslEnv.pop_back();
                }
                else
                {
                    additionalWslEnv.push_back(L':');
                }
            }
            wslEnv.insert(0, additionalWslEnv);
        }

        auto expandedCommandline = wil::ExpandEnvironmentStringsW<std::wstring>(_commandline.c_str());
        auto [shellCommandline, psmuxWorkingDirectory] = Utils::MangleStartingDirectoryForWSL(expandedCommandline, _startingDirectory);
        const auto sessionName = fmt::format(FMT_COMPILE(L"wt-{}"), Utils::GuidToPlainString(_sessionId));
        auto clientCommandline = fmt::format(
            FMT_COMPILE(L"psmux.exe -L windows-terminal -CC new-session -A -s {} -x {} -y {} -- {}"),
            sessionName,
            _cols,
            _rows,
            shellCommandline);

        auto environmentBlock = _initialEnv.to_string();
        const auto startingDirectory = psmuxWorkingDirectory.empty() ? nullptr : psmuxWorkingDirectory.c_str();
        THROW_IF_WIN32_BOOL_FALSE(CreateProcessW(
            nullptr,
            clientCommandline.data(),
            nullptr,
            nullptr,
            TRUE,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            environmentBlock.empty() ? nullptr : environmentBlock.data(),
            startingDirectory,
            &startupInfo.StartupInfo,
            &_piClient));

        childInput.reset();
        childOutput.reset();
        _input = std::move(parentInput);
        _output = std::move(parentOutput);
    }

    void PsmuxConnection::Start()
    try
    {
        _transitionToState(ConnectionState::Connecting);
        _launchClient();

        _hOutputThread.reset(CreateThread(
            nullptr,
            0,
            [](LPVOID context) noexcept {
                const auto connection = static_cast<PsmuxConnection*>(context);
                return connection ? connection->_outputThread() : gsl::narrow_cast<DWORD>(E_INVALIDARG);
            },
            this,
            0,
            nullptr));
        THROW_LAST_ERROR_IF_NULL(_hOutputThread);
        LOG_IF_FAILED(SetThreadDescription(_hOutputThread.get(), L"PsmuxConnection Output Thread"));

        _transitionToState(ConnectionState::Connected);
        _writeControlCommand(fmt::format(FMT_COMPILE("refresh-client -C {},{}\n"), _cols, _rows));
        if (_reattaching)
        {
            _writeControlCommand(fmt::format(
                FMT_COMPILE("display-message -p {}\ncapture-pane -peqJN -S -1000\ndisplay-message -p {}\n"),
                CaptureBegin,
                CaptureEnd));
        }
    }
    catch (...)
    {
        const auto hr = wil::ResultFromCaughtException();
        auto failureText = RS_fmt(L"ProcessFailedToLaunch", _formatStatus(hr), L"psmux.exe -CC");
        if (hr == HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND))
        {
            failureText.append(L"\r\n");
            failureText.append(RS_(L"PsmuxNotFound"));
        }
        TerminalOutput.raise(winrt_wstring_to_array_view(failureText));
        _transitionToState(ConnectionState::Failed);
        _input.reset();
        _output.reset();
        _piClient.reset();
    }

    void PsmuxConnection::_writeControlCommand(const std::string_view command)
    {
        std::lock_guard guard{ _writeLock };
        if (!_input)
        {
            return;
        }

        DWORD written;
        if (!WriteFile(_input.get(), command.data(), gsl::narrow_cast<DWORD>(command.size()), &written, nullptr))
        {
            const auto error = GetLastError();
            if (error != ERROR_BROKEN_PIPE)
            {
                LOG_WIN32(error);
            }
        }
    }

    void PsmuxConnection::WriteInput(const winrt::array_view<const char16_t> buffer)
    {
        if (!_isConnected())
        {
            return;
        }

        const auto input = winrt_array_to_wstring_view(buffer);
        if (input.empty())
        {
            return;
        }

        std::string command{ "send-keys -H" };
        command.reserve(command.size() + input.size() * 9 + 1);
        for (size_t i = 0; i < input.size(); ++i)
        {
            uint32_t codepoint = input[i];
            if (til::is_leading_surrogate(input[i]) &&
                i + 1 < input.size() &&
                til::is_trailing_surrogate(input[i + 1]))
            {
                codepoint = til::combine_surrogates(input[i], input[++i]);
            }
            else if (til::is_surrogate(input[i]))
            {
                codepoint = 0xfffd;
            }
            fmt::format_to(std::back_inserter(command), FMT_COMPILE(" 0x{:x}"), codepoint);
        }
        command.push_back('\n');
        _writeControlCommand(command);
    }

    void PsmuxConnection::Resize(const uint32_t rows, const uint32_t columns)
    {
        _rows = rows;
        _cols = columns;
        if (_isConnected())
        {
            _writeControlCommand(fmt::format(
                FMT_COMPILE("refresh-client -C {},{}\nresize-window -x {} -y {}\n"),
                columns,
                rows,
                columns,
                rows));
        }
    }

    std::string PsmuxConnection::_decodeOutput(const std::string_view value)
    {
        std::string result;
        result.reserve(value.size());
        for (size_t i = 0; i < value.size(); ++i)
        {
            if (value[i] != '\\')
            {
                result.push_back(value[i]);
                continue;
            }

            if (i + 1 < value.size() && value[i + 1] == '\\')
            {
                result.push_back('\\');
                ++i;
                continue;
            }

            if (i + 3 < value.size() &&
                value[i + 1] >= '0' && value[i + 1] <= '7' &&
                value[i + 2] >= '0' && value[i + 2] <= '7' &&
                value[i + 3] >= '0' && value[i + 3] <= '7')
            {
                const auto decoded = static_cast<char>(
                    ((value[i + 1] - '0') << 6) |
                    ((value[i + 2] - '0') << 3) |
                    (value[i + 3] - '0'));
                result.push_back(decoded);
                i += 3;
                continue;
            }

            result.push_back('\\');
        }
        return result;
    }

    void PsmuxConnection::_raiseUtf8(const std::string_view value, til::u8state& u8State)
    {
        std::wstring output;
        if (SUCCEEDED(til::u8u16(value, output, u8State)) && !output.empty())
        {
            TerminalOutput.raise(winrt_wstring_to_array_view(output));
        }
    }

    void PsmuxConnection::_processProtocolLine(const std::string_view line, til::u8state& u8State)
    {
        if (_captureState == CaptureState::CaptureResponse)
        {
            if (line == _captureResponseEnd || line == _captureResponseError)
            {
                if (line == _captureResponseError)
                {
                    _raiseUtf8("\r\n[psmux] control command failed\r\n", u8State);
                }
                _captureState = CaptureState::AwaitingEndMarkerResponse;
                _captureResponseEnd.clear();
                _captureResponseError.clear();
                _deduplicateCaptureUntil = std::chrono::steady_clock::now() + std::chrono::milliseconds{ 500 };
            }
            else
            {
                _captureContents.append(line);
                _captureContents.append("\r\n");
                _raiseUtf8(line, u8State);
                _raiseUtf8("\r\n", u8State);
            }
            return;
        }

        if (line.starts_with("%begin "))
        {
            if (_captureState == CaptureState::AwaitingCaptureResponse)
            {
                const auto suffix = line.substr(7);
                _captureResponseEnd = fmt::format(FMT_COMPILE("%end {}"), suffix);
                _captureResponseError = fmt::format(FMT_COMPILE("%error {}"), suffix);
                _captureState = CaptureState::CaptureResponse;
            }
            else if (_captureState == CaptureState::AwaitingEndMarkerResponse)
            {
                _captureState = CaptureState::EndMarkerResponse;
            }
            return;
        }

        const auto commandEnded = line.starts_with("%end ") || line.starts_with("%error ");
        if (commandEnded)
        {
            if (_captureState == CaptureState::BeginMarkerResponse)
            {
                _captureState = CaptureState::AwaitingCaptureResponse;
            }
            else if (_captureState == CaptureState::EndMarkerResponse)
            {
                _captureState = CaptureState::None;
            }

            if (line.starts_with("%error "))
            {
                _raiseUtf8("\r\n[psmux] control command failed\r\n", u8State);
            }
            return;
        }

        if (line == CaptureBegin)
        {
            _captureState = CaptureState::BeginMarkerResponse;
            _captureContents.clear();
            _raiseUtf8("\x1b[2J\x1b[H", u8State);
            return;
        }

        if (_captureState == CaptureState::EndMarkerResponse && line == CaptureEnd)
        {
            return;
        }

        if (line.starts_with("%output "))
        {
            const auto dataOffset = line.find(' ', 8);
            if (dataOffset != std::string_view::npos)
            {
                const auto decoded = _decodeOutput(line.substr(dataOffset + 1));
                if (std::chrono::steady_clock::now() <= _deduplicateCaptureUntil)
                {
                    const auto duplicate = _captureContents.find(decoded);
                    if (duplicate != std::string::npos)
                    {
                        _captureContents.erase(duplicate, decoded.size());
                        return;
                    }
                }
                else
                {
                    _captureContents.clear();
                }
                _raiseUtf8(decoded, u8State);
            }
        }
    }

    DWORD PsmuxConnection::_outputThread()
    {
        auto strongThis{ get_strong() };
        til::u8state u8State;
        char buffer[64 * 1024];

        for (;;)
        {
            DWORD read;
            if (!ReadFile(_output.get(), buffer, sizeof(buffer), &read, nullptr) || read == 0)
            {
                break;
            }
            if (_isStateAtOrBeyond(ConnectionState::Closing))
            {
                break;
            }

            _protocolBuffer.append(buffer, read);
            size_t newline;
            while ((newline = _protocolBuffer.find('\n')) != std::string::npos)
            {
                auto line = std::string_view{ _protocolBuffer }.substr(0, newline);
                if (!line.empty() && line.back() == '\r')
                {
                    line.remove_suffix(1);
                }
                try
                {
                    _processProtocolLine(line, u8State);
                }
                CATCH_LOG();
                _protocolBuffer.erase(0, newline + 1);
            }
        }

        if (!_isStateAtOrBeyond(ConnectionState::Closing))
        {
            DWORD exitCode{};
            GetExitCodeProcess(_piClient.hProcess, &exitCode);
            _transitionToState(exitCode == 0 || exitCode == STILL_ACTIVE ? ConnectionState::Closed : ConnectionState::Failed);
        }
        return 0;
    }

    void PsmuxConnection::Close() noexcept
    try
    {
        _transitionToState(ConnectionState::Closing);
        {
            std::lock_guard guard{ _writeLock };
            _input.reset();
        }

        if (_hOutputThread)
        {
            for (;;)
            {
                CancelSynchronousIo(_hOutputThread.get());
                if (WaitForSingleObject(_hOutputThread.get(), 1000) == WAIT_OBJECT_0)
                {
                    break;
                }
            }
        }

        _output.reset();
        _hOutputThread.reset();
        _piClient.reset();
        _transitionToState(ConnectionState::Closed);
    }
    CATCH_LOG()

    std::wstring PsmuxConnection::_formatStatus(const uint32_t status)
    {
        return fmt::format(FMT_COMPILE(L"{0} ({0:#010x})"), status);
    }

    safe_void_coroutine PsmuxConnection::final_release(std::unique_ptr<PsmuxConnection> connection)
    {
        co_await winrt::resume_background();
        connection.reset();
    }
}
