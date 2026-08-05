// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "Formatting.h"

#include <algorithm>
#include <cstdio>
#include <string>
#include <vector>

namespace
{
    struct DetachedSessionRow
    {
        std::string sessionId;
        std::string pid;
        std::string tabTitle;
        std::string shellSessionId;
    };

    std::string DetachedSessionPidString(const Json::Value& detachedSession)
    {
        return std::to_string(static_cast<unsigned long long>(detachedSession["pid"].asUInt64()));
    }
}

void PrintJson(const Json::Value& val)
{
    Json::StreamWriterBuilder wb;
    wb["indentation"] = "  ";
    printf("%s\n", Json::writeString(wb, val).c_str());
}

// ── Human-readable formatters (read the server's JSON) ──

void FormatWindowsHuman(const Json::Value& windows)
{
    if (!windows.isArray() || windows.empty())
    {
        printf("No windows found.\n");
        return;
    }
    printf("%-12s %-30s %s\n", "WINDOW_ID", "TITLE", "FOCUSED");
    for (const auto& w : windows)
    {
        printf("%-12llu %-30s %s\n",
               static_cast<unsigned long long>(w["window_id"].asUInt64()),
               w["title"].asString().c_str(),
               w["is_focused"].asBool() ? "*" : "");
    }
}

void FormatTabsHuman(const Json::Value& tabs)
{
    if (!tabs.isArray() || tabs.empty())
    {
        printf("No tabs found.\n");
        return;
    }
    printf("%-10s %-30s %s\n", "TAB_ID", "TITLE", "FOCUSED");
    for (const auto& t : tabs)
    {
        printf("%-10u %-30s %s\n",
               t["tab_id"].asUInt(),
               t["title"].asString().c_str(),
               t["is_active"].asBool() ? "*" : "");
    }
}

void FormatPanesHuman(const Json::Value& panes)
{
    if (!panes.isArray() || panes.empty())
    {
        printf("No panes found.\n");
        return;
    }
    printf("%-38s %-8s %-8s %-10s %s\n", "SESSION_ID", "PID", "ACTIVE", "ROWS", "COLS");
    for (const auto& p : panes)
    {
        printf("%-38s %-8lu %-8s %-10d %d\n",
               p["session_id"].asString().c_str(),
               static_cast<unsigned long>(p["pid"].asUInt()),
               p["is_active"].asBool() ? "*" : "",
               p["size"]["rows"].asInt(),
               p["size"]["columns"].asInt());
    }
}

void FormatDetachedSessionsHuman(const Json::Value& detachedSessions)
{
    if (!detachedSessions.isArray() || detachedSessions.empty())
    {
        printf("No detached sessions found.\n");
        return;
    }

    constexpr char sessionIdHeader[] = "SESSION_ID";
    constexpr char pidHeader[] = "PID";
    constexpr char tabHeader[] = "TAB";
    constexpr char shellSessionIdHeader[] = "SHELL_SESSION_ID";

    size_t sessionIdWidth = std::char_traits<char>::length(sessionIdHeader);
    size_t pidWidth = std::char_traits<char>::length(pidHeader);
    size_t tabWidth = std::char_traits<char>::length(tabHeader);
    size_t shellSessionIdWidth = std::char_traits<char>::length(shellSessionIdHeader);

    std::vector<DetachedSessionRow> rows;
    rows.reserve(detachedSessions.size());

    for (const auto& detachedSession : detachedSessions)
    {
        DetachedSessionRow row{
            detachedSession["session_id"].asString(),
            DetachedSessionPidString(detachedSession),
            detachedSession["tab_title"].asString(),
            detachedSession["shell_session_id"].asString(),
        };

        sessionIdWidth = std::max(sessionIdWidth, row.sessionId.size());
        pidWidth = std::max(pidWidth, row.pid.size());
        tabWidth = std::max(tabWidth, row.tabTitle.size());
        shellSessionIdWidth = std::max(shellSessionIdWidth, row.shellSessionId.size());

        rows.emplace_back(row);
    }

    printf("%-*s %-*s %-*s %-*s\n",
           static_cast<int>(sessionIdWidth),
           sessionIdHeader,
           static_cast<int>(pidWidth),
           pidHeader,
           static_cast<int>(tabWidth),
           tabHeader,
           static_cast<int>(shellSessionIdWidth),
           shellSessionIdHeader);

    for (const auto& row : rows)
    {
        printf("%-*s %-*s %-*s %-*s\n",
               static_cast<int>(sessionIdWidth),
               row.sessionId.c_str(),
               static_cast<int>(pidWidth),
               row.pid.c_str(),
               static_cast<int>(tabWidth),
               row.tabTitle.c_str(),
               static_cast<int>(shellSessionIdWidth),
               row.shellSessionId.c_str());
    }
}

void FormatActivePaneHuman(const Json::Value& info)
{
    printf("Active pane: %s (tab: %u, window: %llu)\n",
           info["session_id"].asString().c_str(),
           info["tab_id"].asUInt(),
           static_cast<unsigned long long>(info["window_id"].asUInt64()));
}

void FormatPaneStatusHuman(const Json::Value& status)
{
    printf("State:     %s\n", status["state"].asString().c_str());
    printf("PID:       %lu\n", static_cast<unsigned long>(status["pid"].asUInt()));
    if (status.isMember("has_exit_code") ? status["has_exit_code"].asBool() : status.isMember("exit_code"))
        printf("Exit code: %d\n", status["exit_code"].asInt());
}

void FormatCreatedTabHuman(const Json::Value& result)
{
    printf("Created tab %u (session %s)\n",
           result["tab_id"].asUInt(),
           result["session_id"].asString().c_str());
}

void FormatCreatedPaneHuman(const Json::Value& result)
{
    printf("Created pane (session %s)\n", result["session_id"].asString().c_str());
}
