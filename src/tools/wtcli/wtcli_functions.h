// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Extracted pure functions from wtcli for fuzzing and testability.
// These functions have no COM/WinRT dependencies and can be called
// from a LibFuzzer harness.

#pragma once

#include <string>
#include <sstream>

#include <json/json.h>

namespace wtcli
{
    // Build the JSON envelope for a send-event command.
    // Returns true on success (outEvt is populated), false if paramsJson
    // is non-empty but not a valid JSON object.
    //
    // |eventType|  — required event name (e.g. "agent.task.started")
    // |paramsJson| — optional JSON object string with extra params
    // |sessionId|  — source session ID as a string (already resolved by caller)
    inline bool BuildSendEventJson(
        const std::string& eventType,
        const std::string& paramsJson,
        const std::string& sessionId,
        Json::Value& outEvt)
    {
        outEvt["type"] = "event";
        outEvt["method"] = "agent_event";

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
        params["session_id"] = sessionId;
        outEvt["params"] = params;
        return true;
    }

    // Check whether an event JSON string passes the session_id and event type
    // filters used by the "listen" command.
    //
    // Returns true if the event should be emitted (matches filters or filters
    // are empty). Returns true on parse failure to match original behavior
    // (unparseable events are passed through).
    //
    // |eventTypeFilter| supports a trailing wildcard: "agent.*" matches
    // "agent.task.started".
    inline bool MatchesEventFilter(
        const std::string& eventJson,
        const std::string& sessionIdFilter,
        const std::string& eventTypeFilter)
    {
        if (sessionIdFilter.empty() && eventTypeFilter.empty())
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

        if (!sessionIdFilter.empty())
        {
            auto sessionId = ev["params"].get("session_id", "").asString();
            if (sessionId != sessionIdFilter)
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
