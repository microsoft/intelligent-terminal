// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "Formatting.h"

#include <cstdio>
#include <combaseapi.h>

// ── Helper ──

static std::string GuidToStr(const winrt::guid& g)
{
    wchar_t buf[40]{};
    StringFromGUID2(g, buf, ARRAYSIZE(buf));
    std::wstring ws(buf);
    if (ws.size() > 2 && ws.front() == L'{' && ws.back() == L'}')
        ws = ws.substr(1, ws.size() - 2);
    return winrt::to_string(winrt::hstring{ ws });
}

// ── JSON output ──

void PrintJson(const Json::Value& val)
{
    Json::StreamWriterBuilder wb;
    wb["indentation"] = "  ";
    printf("%s\n", Json::writeString(wb, val).c_str());
}

// ── Human-readable formatters ──

void FormatWindowsHuman(const winrt::com_array<Protocol::WindowInfo>& windows)
{
    if (windows.empty())
    {
        printf("No windows found.\n");
        return;
    }
    printf("%-12s %-30s %s\n", "WINDOW_ID", "TITLE", "FOCUSED");
    for (const auto& w : windows)
    {
        auto title = winrt::to_string(w.Title);
        printf("%-12llu %-30s %s\n", static_cast<unsigned long long>(w.WindowId), title.c_str(), w.IsFocused ? "*" : "");
    }
}

void FormatTabsHuman(const winrt::com_array<Protocol::TabInfo>& tabs)
{
    if (tabs.empty())
    {
        printf("No tabs found.\n");
        return;
    }
    printf("%-10s %-30s %s\n", "TAB_ID", "TITLE", "FOCUSED");
    for (const auto& t : tabs)
    {
        auto title = winrt::to_string(t.Title);
        printf("%-10u %-30s %s\n", t.TabId, title.c_str(), t.IsActive ? "*" : "");
    }
}

void FormatPanesHuman(const winrt::com_array<Protocol::PaneInfo>& panes)
{
    if (panes.empty())
    {
        printf("No panes found.\n");
        return;
    }
    printf("%-38s %-8s %-8s %-10s %s\n", "SESSION_ID", "PID", "ACTIVE", "ROWS", "COLS");
    for (const auto& p : panes)
    {
        auto sid = GuidToStr(p.SessionId);
        printf("%-38s %-8lu %-8s %-10d %d\n",
               sid.c_str(),
               p.Pid,
               p.IsActive ? "*" : "",
               p.Rows,
               p.Columns);
    }
}

void FormatActivePaneHuman(const Protocol::PaneInfo& info)
{
    printf("Active pane: %s (tab: %u, window: %llu)\n", GuidToStr(info.SessionId).c_str(), info.TabId, static_cast<unsigned long long>(info.WindowId));
}

void FormatPaneStatusHuman(const Protocol::ProcessStatus& status)
{
    auto state = winrt::to_string(status.State);
    printf("State:     %s\n", state.c_str());
    printf("PID:       %lu\n", status.Pid);
    if (status.HasExitCode)
        printf("Exit code: %d\n", status.ExitCode);
}

void FormatCreatedTabHuman(const Protocol::TabCreationResult& result)
{
    printf("Created tab %u (session %s)\n", result.TabId, GuidToStr(result.SessionId).c_str());
}

void FormatCreatedPaneHuman(const Protocol::TabCreationResult& result)
{
    printf("Created pane (session %s)\n", GuidToStr(result.SessionId).c_str());
}

// ── JSON serialization ──

Json::Value WindowInfoToJson(const Protocol::WindowInfo& w)
{
    Json::Value v;
    v["window_id"] = static_cast<Json::UInt64>(w.WindowId);
    v["title"] = winrt::to_string(w.Title);
    v["is_focused"] = w.IsFocused;
    v["tab_count"] = static_cast<Json::UInt>(w.TabCount);
    return v;
}

Json::Value TabInfoToJson(const Protocol::TabInfo& t)
{
    Json::Value v;
    v["tab_id"] = static_cast<Json::UInt>(t.TabId);
    v["window_id"] = static_cast<Json::UInt64>(t.WindowId);
    v["title"] = winrt::to_string(t.Title);
    v["is_active"] = t.IsActive;
    v["pane_count"] = static_cast<Json::UInt>(t.PaneCount);
    return v;
}

Json::Value PaneInfoToJson(const Protocol::PaneInfo& p)
{
    Json::Value v;
    v["session_id"] = GuidToStr(p.SessionId);
    v["tab_id"] = static_cast<Json::UInt>(p.TabId);
    v["window_id"] = static_cast<Json::UInt64>(p.WindowId);
    v["title"] = winrt::to_string(p.Title);
    v["profile"] = winrt::to_string(p.Profile);
    v["is_active"] = p.IsActive;
    v["is_agent_pane"] = p.IsAgentPane;
    v["pid"] = static_cast<Json::UInt>(p.Pid);
    v["size"]["rows"] = p.Rows;
    v["size"]["columns"] = p.Columns;
    v["cwd"] = winrt::to_string(p.Cwd);
    return v;
}

Json::Value PaneOutputToJson(const Protocol::PaneOutput& o)
{
    Json::Value v;
    v["session_id"] = GuidToStr(o.SessionId);
    v["content"] = winrt::to_string(o.Content);
    v["line_count"] = o.LineCount;
    v["truncated"] = o.Truncated;
    v["has_marks"] = o.HasMarks;
    return v;
}

Json::Value CreationResultToJson(const Protocol::TabCreationResult& r)
{
    Json::Value v;
    v["tab_id"] = static_cast<Json::UInt>(r.TabId);
    v["session_id"] = GuidToStr(r.SessionId);
    v["window_id"] = static_cast<Json::UInt64>(r.WindowId);
    v["pid"] = static_cast<Json::UInt>(r.Pid);
    return v;
}
