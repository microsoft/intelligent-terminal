// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Shared diagnostic logger for the agent-pane code paths spread across
// TerminalPage.cpp / TabManagement.cpp / AppActionHandlers.cpp. Three
// near-identical copies of this function used to live in those TUs; that
// drifted whenever one of them was tweaked. Centralized here so the
// timestamp format, log path, and error-handling semantics stay in lock-
// step.
//
// Output: the per-version WTA log directory + `terminal-agent-pane.log`, one
// ISO8601 UTC line per call with millisecond precision so timestamps correlate
// with `wta-main_*.log` down to the millisecond. The directory is resolved by
// `_intelligentTerminalLogDir()` below (the per-version `logs\<pkgver>\` folder)
// to match wta's Rust per-version logging.
//
// Header-only `inline` so each translation unit that includes this picks
// up its own copy of the symbol without ODR conflicts.

#pragma once

#include <windows.h>

#include <chrono>
#include <array>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <ctime>
#include <filesystem>
#include <string>
#include <system_error>
#include <utility>

#include "../inc/IntelligentTerminalPaths.h"

namespace winrt::TerminalApp::implementation
{
    // The per-version WTA log directory (`logs\<pkgver>\`), resolved by the
    // shared `IntelligentTerminal::LogDirVersioned()` so this logger and the
    // Rust processes write into the same per-version folder. (The
    // bug-report-zip action uses `LogDir()` — the root — so it can archive
    // every version at once.)
    inline std::filesystem::path _intelligentTerminalLogDir()
    {
        return ::IntelligentTerminal::LogDirVersioned();
    }

    inline void _agentPaneLog(const std::string& msg)
    {
        std::filesystem::path logDir = _intelligentTerminalLogDir();
        if (logDir.empty())
        {
            return;
        }

        // No-throw overload — this is a diagnostic logger; we never want
        // a filesystem hiccup (race with a concurrent rmdir, permission
        // change, disk full) to bubble out as an exception that kills the
        // caller. On failure we silently drop the log line.
        std::error_code ec;
        std::filesystem::create_directories(logDir, ec);
        if (ec)
        {
            return;
        }

        const auto nowMs = std::chrono::duration_cast<std::chrono::milliseconds>(
                               std::chrono::system_clock::now().time_since_epoch())
                               .count();
        const auto secs = static_cast<std::time_t>(nowMs / 1000);
        const int ms = static_cast<int>(nowMs % 1000);
        std::tm tmUtc{};
        ::gmtime_s(&tmUtc, &secs);
        char ts[24]{};
        std::strftime(ts, sizeof(ts), "%Y-%m-%dT%H:%M:%S", &tmUtc);

        // Format the whole line up front, then emit it with a SINGLE
        // FILE_APPEND_DATA WriteFile. Appends < 4 KB are atomic, so concurrent
        // writers (multiple WT threads, and the per-version log is shared) never
        // interleave a half-written line — which matters precisely for the
        // concurrent FRE activity this log is used to debug. (A buffered
        // std::ofstream `<<` chain can emit several writes per line and tear.)
        std::string line(72 + msg.size(), '\0');
        const int n = _snprintf_s(line.data(), line.size(), _TRUNCATE,
                                  "[%s.%03dZ] %s\n", ts, ms, msg.c_str());
        if (n <= 0)
        {
            return;
        }

        const auto logPath = (logDir / L"terminal-agent-pane.log").wstring();
        const HANDLE h = CreateFileW(logPath.c_str(),
                                     FILE_APPEND_DATA,
                                     FILE_SHARE_READ | FILE_SHARE_WRITE,
                                     nullptr,
                                     OPEN_ALWAYS,
                                     FILE_ATTRIBUTE_NORMAL,
                                     nullptr);
        if (h == INVALID_HANDLE_VALUE)
        {
            return;
        }
        DWORD written = 0;
        WriteFile(h, line.data(), static_cast<DWORD>(n), &written, nullptr);
        CloseHandle(h);
    }

    // Buffer checkpoints so tracing does not add file I/O between measured steps.
    class AgentStartupTiming
    {
    public:
        explicit AgentStartupTiming(const char* scope) noexcept :
            _scope{ scope },
            _enabled{ Enabled() }
        {
            if (_enabled)
            {
                _start = std::chrono::steady_clock::now();
                _unixUs = std::chrono::duration_cast<std::chrono::microseconds>(
                              std::chrono::system_clock::now().time_since_epoch())
                              .count();
                static std::atomic<uint64_t> nextId{ 0 };
                _id = ++nextId;
            }
        }

        ~AgentStartupTiming() noexcept
        {
            if (!_enabled)
            {
                return;
            }
            try
            {
                const auto elapsedUs = ElapsedUs();
                std::string message = "startup_timing pid=" + std::to_string(GetCurrentProcessId()) +
                                      " tid=" + std::to_string(GetCurrentThreadId()) +
                                      " span=" + std::to_string(_id) + " scope=" + _scope +
                                      " start_unix_us=" + std::to_string(_unixUs) +
                                      " elapsed_us=" + std::to_string(elapsedUs);
                for (size_t i = 0; i < _count; ++i)
                {
                    message += " ";
                    message += _checkpoints[i].first;
                    message += "=" + std::to_string(_checkpoints[i].second);
                }
                _agentPaneLog(message);
            }
            catch (...)
            {
                OutputDebugStringW(L"Failed to write agent startup timing\n");
            }
        }

        void Mark(const char* phase) noexcept
        {
            if (_enabled && _count < _checkpoints.size())
            {
                _checkpoints[_count++] = { phase, ElapsedUs() };
            }
        }

    private:
        static bool Enabled() noexcept
        {
            static const bool enabled = []() {
                wchar_t value[2]{};
                return GetEnvironmentVariableW(L"WTA_STARTUP_TIMING", value, ARRAYSIZE(value)) == 1 &&
                       value[0] == L'1';
            }();
            return enabled;
        }

        int64_t ElapsedUs() const noexcept
        {
            return std::chrono::duration_cast<std::chrono::microseconds>(
                       std::chrono::steady_clock::now() - _start)
                .count();
        }

        const char* _scope;
        bool _enabled;
        std::chrono::steady_clock::time_point _start{};
        int64_t _unixUs{};
        uint64_t _id{};
        std::array<std::pair<const char*, int64_t>, 32> _checkpoints{};
        size_t _count{};
    };
}
