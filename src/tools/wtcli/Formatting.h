// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <winrt/Microsoft.Terminal.Protocol.h>
#include <json/json.h>

namespace Protocol = winrt::Microsoft::Terminal::Protocol;

// JSON output
void PrintJson(const Json::Value& val);

// Human-readable formatters
void FormatWindowsHuman(const winrt::com_array<Protocol::WindowInfo>& windows);
void FormatTabsHuman(const winrt::com_array<Protocol::TabInfo>& tabs);
void FormatPanesHuman(const winrt::com_array<Protocol::PaneInfo>& panes);
void FormatActivePaneHuman(const Protocol::PaneInfo& info);
void FormatPaneStatusHuman(const Protocol::ProcessStatus& status);
void FormatCreatedTabHuman(const Protocol::TabCreationResult& result);
void FormatCreatedPaneHuman(const Protocol::TabCreationResult& result);

// JSON serialization
Json::Value WindowInfoToJson(const Protocol::WindowInfo& w);
Json::Value TabInfoToJson(const Protocol::TabInfo& t);
Json::Value PaneInfoToJson(const Protocol::PaneInfo& p);
Json::Value PaneOutputToJson(const Protocol::PaneOutput& o);
Json::Value CreationResultToJson(const Protocol::TabCreationResult& r);
