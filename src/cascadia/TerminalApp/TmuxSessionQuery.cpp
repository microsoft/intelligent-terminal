// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxSessionQuery.cpp
//
// Abstract:
// - Bounded, cancellable one-shot tmux session and pane discovery.

#include "pch.h"
#include "TmuxSessionQuery.h"
#include "TmuxProcess.h"
#include "../inc/TmuxSshCommand.h"

#include <algorithm>
#include <chrono>
#include <memory>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::Tmux
{
    namespace
    {
        constexpr size_t MaxOutputBytes = 64 * 1024;
        constexpr size_t MaxErrorBytes = 16 * 1024;

        struct QueryState
        {
            QueryState() :
                ready{ CreateEventW(nullptr, TRUE, FALSE, nullptr) }
            {
                THROW_LAST_ERROR_IF(!ready);
            }

            wil::unique_handle ready;
            // Written only by serialized transport callbacks; read after Close
            // has joined that worker, including its failure and exit callbacks.
            std::string output;
            std::string error;
            uint32_t exitCode{};
            bool exited{};
        };

        void AppendBounded(std::string& destination, const std::string_view bytes, const size_t limit, const wchar_t* message)
        {
            if (bytes.size() > limit - destination.size())
            {
                throw winrt::hresult_error{ HRESULT_FROM_WIN32(ERROR_BUFFER_OVERFLOW), message };
            }
            destination.append(bytes);
        }

        std::wstring DecodeUtf8(const std::string_view bytes, const wchar_t* message)
        {
            if (bytes.empty())
            {
                return {};
            }
            const auto size = static_cast<int>(bytes.size());
            const auto count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, bytes.data(), size, nullptr, 0);
            if (count == 0)
            {
                throw winrt::hresult_error{ HRESULT_FROM_WIN32(GetLastError()), message };
            }
            std::wstring text(static_cast<size_t>(count), L'\0');
            if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, bytes.data(), size, text.data(), count) != count)
            {
                throw winrt::hresult_error{ HRESULT_FROM_WIN32(GetLastError()), message };
            }
            return text;
        }

        bool IsMissingServer(std::wstring_view error, const std::wstring_view expectedSocketPath) noexcept
        {
            if (error.ends_with(L"\r\n"))
            {
                error.remove_suffix(2);
            }
            else if (error.ends_with(L"\n"))
            {
                error.remove_suffix(1);
            }
            // Do not trim or search within diagnostics: an extra SSH warning,
            // permission error, or second line must not become a false success.
            if (std::any_of(error.begin(), error.end(), [](const wchar_t ch) { return ch < L' ' || ch == L'\x7f'; }))
            {
                return false;
            }
            constexpr std::wstring_view noServer{ L"no server running on " };
            constexpr std::wstring_view connecting{ L"error connecting to " };
            constexpr std::wstring_view absent{ L" (No such file or directory)" };
            if (error.starts_with(noServer))
            {
                error.remove_prefix(noServer.size());
            }
            else if (error.starts_with(connecting) && error.ends_with(absent))
            {
                error.remove_prefix(connecting.size());
                error.remove_suffix(absent.size());
            }
            else
            {
                return false;
            }
            return expectedSocketPath.empty() ?
                       error.starts_with(L"/") && error.ends_with(L"/default") && error.size() > 8 :
                       error == expectedSocketPath;
        }
    }

    winrt::Windows::Foundation::IAsyncOperation<winrt::hstring> QuerySessionListAsync(winrt::hstring commandline, winrt::hstring workingDirectory, winrt::hstring expectedSocketPath)
    {
        const auto cancellation = co_await winrt::get_cancellation_token();
        co_await winrt::resume_background();

        const auto state = std::make_shared<QueryState>();
        cancellation.callback([state]() noexcept {
            // Cancel may run on the UI thread. Retaining the event (not the
            // process) also makes a late callback safe after coroutine cleanup.
            SetEvent(state->ready.get());
        });
        if (cancellation())
        {
            throw winrt::hresult_canceled{};
        }
        THROW_HR_IF(E_INVALIDARG, !expectedSocketPath.empty() && !IsValidSshSocketPath(expectedSocketPath));

        TmuxProcess process{ {
            [state](const std::string_view bytes) {
                AppendBounded(state->output, bytes, MaxOutputBytes, L"Tmux session query stdout exceeded 65536 bytes.");
            },
            [state](const std::string_view bytes) {
                AppendBounded(state->error, bytes, MaxErrorBytes, L"Tmux session query stderr exceeded 16384 bytes.");
            },
            [state](const uint32_t code) {
                state->exitCode = code;
                state->exited = true;
                SetEvent(state->ready.get());
            },
            [state](std::exception_ptr) {
                SetEvent(state->ready.get());
            },
        } };

        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds{ 10 };
        try
        {
            process.Start(std::wstring{ commandline }, std::wstring{ workingDirectory });
        }
        catch (...)
        {
            const auto message = L"Unable to launch tmux session query: " + winrt::to_message();
            throw winrt::hresult_error{ winrt::to_hresult(), message };
        }

        auto signaled = false;
        const auto remaining = deadline - std::chrono::steady_clock::now();
        if (remaining > std::chrono::steady_clock::duration::zero())
        {
            signaled = co_await winrt::resume_on_signal(state->ready.get(), std::chrono::duration_cast<winrt::Windows::Foundation::TimeSpan>(remaining));
        }
        // Both explicit cleanup and stack unwinding occur after resume_background.
        // In particular, a cancellation thrown while entering the wait cannot
        // move this blocking join onto the thread that called Cancel.
        process.Close();
        if (cancellation())
        {
            throw winrt::hresult_canceled{};
        }
        if (!signaled)
        {
            throw winrt::hresult_error{ HRESULT_FROM_WIN32(ERROR_TIMEOUT), L"Tmux session query timed out after 10 seconds." };
        }
        try
        {
            process.RethrowFailure();
        }
        catch (...)
        {
            const auto message = L"Tmux session query transport failed: " + winrt::to_message();
            throw winrt::hresult_error{ winrt::to_hresult(), message };
        }
        if (!state->exited)
        {
            throw winrt::hresult_error{ E_UNEXPECTED, L"Tmux session query completed without a process exit." };
        }

        const auto output = DecodeUtf8(state->output, L"Tmux session query returned invalid UTF-8 on stdout.");
        const auto error = DecodeUtf8(state->error, L"Tmux session query returned invalid UTF-8 on stderr.");
        if (state->exitCode != 0)
        {
            if (state->exitCode == 1 && output.empty() && IsMissingServer(error, expectedSocketPath))
            {
                co_return winrt::hstring{};
            }
            auto message = L"Tmux session query failed (exit code " + std::to_wstring(state->exitCode) + L").";
            if (!error.empty())
            {
                message.append(L" ").append(error);
            }
            throw winrt::hresult_error{ E_FAIL, message };
        }
        co_return winrt::hstring{ output };
    }
}
