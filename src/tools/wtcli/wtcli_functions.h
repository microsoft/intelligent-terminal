// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Extracted pure functions from wtcli for fuzzing and testability.
// These functions have no COM/WinRT dependencies and can be called
// from a LibFuzzer harness.

#pragma once

#include <string>
#include <sstream>
#include <vector>

#include <Windows.h>
#include <json/json.h>

namespace wtcli
{
    // Convert a UTF-8 string to a wide string using Win32 API.
    // Equivalent to winrt::to_hstring() but without WinRT dependency.
    inline std::wstring Utf8ToWide(const std::string& str)
    {
        if (str.empty())
        {
            return {};
        }
        const auto size = MultiByteToWideChar(
            CP_UTF8, 0,
            str.data(), static_cast<int>(str.size()),
            nullptr, 0);
        std::wstring result(size, 0);
        MultiByteToWideChar(
            CP_UTF8, 0,
            str.data(), static_cast<int>(str.size()),
            result.data(), size);
        return result;
    }

    // Translate tmux-style key names to actual characters.
    // Each entry in |keys| is either a named key (e.g. "Enter", "C-c")
    // or literal text that is converted from UTF-8 to wide.
    inline std::wstring TranslateKeys(const std::vector<std::string>& keys)
    {
        std::wstring result;
        for (const auto& key : keys)
        {
            if (key == "Enter" || key == "enter")
            {
                result += L"\r\n";
            }
            else if (key == "Space" || key == "space")
            {
                result += L" ";
            }
            else if (key == "Tab" || key == "tab")
            {
                result += L"\t";
            }
            else if (key == "Escape" || key == "escape" || key == "Esc")
            {
                result += L"\x1b";
            }
            else if (key == "BSpace" || key == "bspace")
            {
                result += L"\b";
            }
            else if (key == "C-c")
            {
                result += L"\x03";
            }
            else if (key == "C-d")
            {
                result += L"\x04";
            }
            else if (key == "C-z")
            {
                result += L"\x1a";
            }
            else if (key == "C-l")
            {
                result += L"\x0c";
            }
            else if (key.size() == 3 && key[0] == 'C' && key[1] == '-' && key[2] >= 'a' && key[2] <= 'z')
            {
                result += static_cast<wchar_t>(key[2] - 'a' + 1);
            }
            else
            {
                result += Utf8ToWide(key);
            }
        }
        return result;
    }

    // Build the JSON envelope for a send-event command.
    // Returns true on success (outEvt is populated), false if paramsJson
    // is non-empty but not a valid JSON object.
    //
    // |eventType|  — required event name (e.g. "pane.output.changed")
    // |paramsJson| — optional JSON object string with extra params
    // |paneId|     — source pane ID as a string (already resolved by caller)
    inline bool BuildSendEventJson(
        const std::string& eventType,
        const std::string& paramsJson,
        const std::string& paneId,
        Json::Value& outEvt)
    {
        outEvt["type"] = "event";
        outEvt["method"] = "send_event";

        Json::Value params;
        if (!paramsJson.empty())
        {
            Json::CharReaderBuilder rb;
            std::string errs;
            std::istringstream ss(paramsJson);
            if (!Json::parseFromStream(rb, ss, &params, &errs) || !params.isObject())
            {
                return false;
            }
        }

        params["event"] = eventType;
        params["pane_id"] = paneId;
        outEvt["params"] = params;
        return true;
    }

    // Check whether an event JSON string passes the pane_id and event type
    // filters used by the "listen" command.
    //
    // Returns true if the event should be emitted (matches filters or filters
    // are empty). Returns true on parse failure to match original behavior
    // (unparseable events are passed through).
    //
    // |eventTypeFilter| supports a trailing wildcard: "pane.*" matches
    // "pane.output.changed".
    inline bool MatchesEventFilter(
        const std::string& eventJson,
        const std::string& paneIdFilter,
        const std::string& eventTypeFilter)
    {
        if (paneIdFilter.empty() && eventTypeFilter.empty())
        {
            return true;
        }

        Json::Value ev;
        Json::CharReaderBuilder rb;
        std::string errs;
        std::istringstream ss(eventJson);
        if (!Json::parseFromStream(rb, ss, &ev, &errs))
        {
            return true;
        }

        // Event JSON must be an object with a "params" object inside.
        if (!ev.isObject() || !ev.isMember("params") || !ev["params"].isObject())
        {
            return true;
        }

        if (!paneIdFilter.empty())
        {
            auto paneId = ev["params"].get("pane_id", "").asString();
            if (paneId != paneIdFilter)
            {
                return false;
            }
        }

        if (!eventTypeFilter.empty())
        {
            auto eventType = ev["params"].get("event", "").asString();
            if (eventTypeFilter.back() == '*')
            {
                auto prefix = eventTypeFilter.substr(0, eventTypeFilter.size() - 1);
                if (eventType.substr(0, prefix.size()) != prefix)
                {
                    return false;
                }
            }
            else if (eventType != eventTypeFilter)
            {
                return false;
            }
        }

        return true;
    }
}
