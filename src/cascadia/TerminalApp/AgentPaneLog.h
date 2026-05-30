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
// Output: the WTA log directory + `wta-agent-pane.log`, one ISO8601 UTC line
// per call with millisecond precision so timestamps correlate with
// `wta-main_*.log` down to the millisecond. The log directory is resolved by
// `_intelligentTerminalLogDir()` below to match wta's Rust
// `runtime_paths::intelligent_terminal_local_root()`.
//
// Header-only `inline` so each translation unit that includes this picks
// up its own copy of the symbol without ODR conflicts.

#pragma once

#include <windows.h>

#include <chrono>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <string>
#include <system_error>

#include "../inc/IntelligentTerminalPaths.h"

namespace winrt::TerminalApp::implementation
{
    // The WTA log directory, resolved by the shared
    // `IntelligentTerminal::LogDir()` (package LocalCache\Local when packaged)
    // so this logger, the bug-report-zip action, and the Rust side all agree.
    inline std::filesystem::path _intelligentTerminalLogDir()
    {
        return ::IntelligentTerminal::LogDir();
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

        const auto logPath = logDir / L"wta-agent-pane.log";
        std::ofstream f{ logPath, std::ios::app };
        if (!f)
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
        char ts[32];
        std::strftime(ts, sizeof(ts), "%Y-%m-%dT%H:%M:%S", &tmUtc);
        f << '[' << ts << '.' << std::setw(3) << std::setfill('0') << ms
          << "Z] " << msg << '\n';
    }
}
