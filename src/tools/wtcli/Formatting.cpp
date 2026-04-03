// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "Formatting.h"

#include <cstdio>

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
        auto id = winrt::to_string(w.WindowId);
        auto title = winrt::to_string(w.Title);
        printf("%-12s %-30s %s\n", id.c_str(), title.c_str(), w.IsFocused ? "*" : "");
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
        auto id = winrt::to_string(t.TabId);
        auto title = winrt::to_string(t.Title);
        printf("%-10s %-30s %s\n", id.c_str(), title.c_str(), t.IsActive ? "*" : "");
    }
}

void FormatPanesHuman(const winrt::com_array<Protocol::PaneInfo>& panes)
{
    if (panes.empty())
    {
        printf("No panes found.\n");
        return;
    }
    printf("%-10s %-8s %-8s %-10s %s\n", "PANE_ID", "PID", "ACTIVE", "ROWS", "COLS");
    for (const auto& p : panes)
    {
        auto id = winrt::to_string(p.PaneId);
        printf("%-10s %-8lu %-8s %-10d %d\n",
               id.c_str(),
               p.Pid,
               p.IsActive ? "*" : "",
               p.Rows,
               p.Columns);
    }
}

void FormatActivePaneHuman(const Protocol::PaneInfo& info)
{
    auto pane = winrt::to_string(info.PaneId);
    auto tab = winrt::to_string(info.TabId);
    auto win = winrt::to_string(info.WindowId);
    printf("Active pane: %s (tab: %s, window: %s)\n", pane.c_str(), tab.c_str(), win.c_str());
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
    auto tabId = winrt::to_string(result.TabId);
    auto paneId = winrt::to_string(result.PaneId);
    printf("Created tab %s (pane %s)\n", tabId.c_str(), paneId.c_str());
}

void FormatCreatedPaneHuman(const Protocol::TabCreationResult& result)
{
    auto paneId = winrt::to_string(result.PaneId);
    printf("Created pane %s\n", paneId.c_str());
}

// ── JSON serialization ──

Json::Value WindowInfoToJson(const Protocol::WindowInfo& w)
{
    Json::Value v;
    v["window_id"] = winrt::to_string(w.WindowId);
    v["title"] = winrt::to_string(w.Title);
    v["is_focused"] = w.IsFocused;
    v["tab_count"] = static_cast<Json::UInt>(w.TabCount);
    return v;
}

Json::Value TabInfoToJson(const Protocol::TabInfo& t)
{
    Json::Value v;
    v["tab_id"] = winrt::to_string(t.TabId);
    v["window_id"] = winrt::to_string(t.WindowId);
    v["title"] = winrt::to_string(t.Title);
    v["is_active"] = t.IsActive;
    v["pane_count"] = static_cast<Json::UInt>(t.PaneCount);
    return v;
}

Json::Value PaneInfoToJson(const Protocol::PaneInfo& p)
{
    Json::Value v;
    v["pane_id"] = winrt::to_string(p.PaneId);
    v["tab_id"] = winrt::to_string(p.TabId);
    v["window_id"] = winrt::to_string(p.WindowId);
    v["title"] = winrt::to_string(p.Title);
    v["profile"] = winrt::to_string(p.Profile);
    v["is_active"] = p.IsActive;
    v["pid"] = static_cast<Json::UInt>(p.Pid);
    v["size"]["rows"] = p.Rows;
    v["size"]["columns"] = p.Columns;
    return v;
}

Json::Value PaneOutputToJson(const Protocol::PaneOutput& o)
{
    Json::Value v;
    v["pane_id"] = winrt::to_string(o.PaneId);
    v["content"] = winrt::to_string(o.Content);
    v["line_count"] = o.LineCount;
    v["truncated"] = o.Truncated;
    return v;
}

Json::Value CreationResultToJson(const Protocol::TabCreationResult& r)
{
    Json::Value v;
    v["tab_id"] = winrt::to_string(r.TabId);
    v["pane_id"] = winrt::to_string(r.PaneId);
    v["window_id"] = winrt::to_string(r.WindowId);
    v["pid"] = static_cast<Json::UInt>(r.Pid);
    return v;
}
