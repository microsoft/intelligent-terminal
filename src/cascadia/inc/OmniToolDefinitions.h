// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <json/json.h>
#include <algorithm>
#include <cmath>
#include <sstream>

namespace Microsoft::Terminal::Omni
{
    inline Json::Value ToolDefinitions(const Json::Value& context)
    {
        std::istringstream input{ R"json([
            {"name":"terminal_get_context","description":"Refresh current-window state, appearance and bounded metadata for request-bound targets. Removed targets disappear; registered profile, scheme, tab and pane choices do not expand. Not complete history.","effect":"read","parameters":{"type":"object","properties":{},"additionalProperties":false}},
            {"name":"terminal_appearance","description":"Change runtime appearance of the original ordinary terminal pane or all ordinary terminal panes in its tab. Tab scope overwrites requested pane fields and preserves all other overrides. color_scheme changes terminal colors, not application theme. No persistent settings.","effect":"mutation","parameters":{"type":"object","properties":{"scope":{"type":"string","enum":["pane","tab"],"default":"pane"},"opacity":{"type":"number","minimum":0,"maximum":100},"acrylic":{"type":"boolean"},"font_size":{"type":"number","minimum":1,"maximum":128},"color_scheme":{"type":"string"}},"additionalProperties":false}},
            {"name":"terminal_split","description":"Split the source pane using a discovered profile. This version supports direction and profile only; no custom size, command or background creation.","effect":"mutation","parameters":{"type":"object","properties":{"direction":{"type":"string","enum":["auto","left","right","up","down"],"default":"auto"},"profile_id":{"type":"string"}},"additionalProperties":false}},
            {"name":"terminal_new_tab","description":"Create a tab in this window using a discovered profile. This version does not support custom cwd, title, command or background creation.","effect":"mutation","parameters":{"type":"object","properties":{"profile_id":{"type":"string"}},"additionalProperties":false}},
            {"name":"terminal_tab_layout","description":"Change this window's runtime tab-strip layout, not pane split direction.","effect":"mutation","parameters":{"type":"object","properties":{"layout":{"type":"string","enum":["horizontal","vertical"]}},"required":["layout"],"additionalProperties":false}},
            {"name":"terminal_focus_tab","description":"Focus a captured tab in this window using its stable tab ID.","effect":"mutation","parameters":{"type":"object","properties":{"tab_id":{"type":"string"}},"required":["tab_id"],"additionalProperties":false}},
            {"name":"terminal_focus_pane","description":"Focus a captured pane in the source tab. Use content search or directory navigation for other tabs.","effect":"mutation","parameters":{"type":"object","properties":{"content_id":{"type":"integer","minimum":0}},"required":["content_id"],"additionalProperties":false}},
            {"name":"terminal_focus_directory","description":"Focus the unique pane whose cwd matches the literal folder selector from the user. Searches this window's captured panes; no filesystem access or directory change.","effect":"mutation","parameters":{"type":"object","properties":{"directory":{"type":"string"}},"required":["directory"],"additionalProperties":false}},
            {"name":"terminal_find_content","description":"Retrieve bounded real buffer passages across this window. Supply 1-3 literal queries and optional metadata-nominated pane IDs for nearby conversation even without keyword hits. One expanded retry is allowed. Each search retires previous match IDs; this does not focus a pane.","effect":"read","parameters":{"type":"object","properties":{"queries":{"type":"array","minItems":1,"maxItems":3,"items":{"type":"string"}},"metadata_pane_ids":{"type":"array","maxItems":8,"items":{"type":"integer","minimum":0}}},"required":["queries"],"additionalProperties":false}},
            {"name":"terminal_reveal_content","description":"Focus and scroll to an exact match ID returned by the latest content search. Select actual discussion supported by metadata and surrounding text; never a bare directory prompt or equally ambiguous passages.","effect":"mutation","parameters":{"type":"object","properties":{"match_id":{"type":"integer","minimum":0}},"required":["match_id"],"additionalProperties":false}},
            {"name":"terminal_close_pane","description":"Close one captured pane in the source tab, preserving native close confirmation and read-only protection. The last pane also closes its tab. Requires an explicit user close request; never use for cleanup after another operation.","effect":"mutation","parameters":{"type":"object","properties":{"content_id":{"type":"integer","minimum":0}},"required":["content_id"],"additionalProperties":false}}
        ])json" };
        Json::Value tools;
        Json::CharReaderBuilder reader;
        std::string errors;
        THROW_HR_IF(E_UNEXPECTED, !Json::parseFromStream(reader, input, &tools, &errors));
        for (auto& tool : tools)
        {
            tool["hasHandler"] = true;
            // The native host, not the model or SDK permission dialog, owns confirmation.
            tool["skipPermission"] = true;
            auto& properties = tool["parameters"]["properties"];
            const auto bindEnum = [&](const char* property, const Json::Value& entries, const char* key) {
                if (properties.isMember(property))
                {
                    auto& values = properties[property]["enum"];
                    values = Json::Value{ Json::arrayValue };
                    for (const auto& entry : entries)
                        values.append(key ? entry[key] : entry);
                }
            };
            bindEnum("profile_id", context["profiles"], "id");
            bindEnum("color_scheme", context["schemes"], nullptr);
            bindEnum("tab_id", context["tabs"], "id");
            bindEnum("content_id", context["panes"], "content_id");
            if (properties.isMember("profile_id"))
                properties["profile_id"]["default"] = context["default_profile_id"];
        }
        return tools;
    }

    inline void ValidateToolValue(const Json::Value& value, const Json::Value& schema)
    {
        const auto type = schema["type"].asString();
        THROW_HR_IF(E_INVALIDARG,
                    (type == "string" && !value.isString()) ||
                        (type == "boolean" && !value.isBool()) ||
                        (type == "number" && (!value.isNumeric() || !std::isfinite(value.asDouble()))) ||
                        (type == "integer" && !value.isUInt()) ||
                        (type == "array" && !value.isArray()));
        if (schema.isMember("enum"))
        {
            const auto& values = schema["enum"];
            THROW_HR_IF(E_INVALIDARG, std::none_of(values.begin(), values.end(), [&](const auto& option) {
                            return option.isUInt64() && value.isUInt64() ? option.asUInt64() == value.asUInt64() : option == value;
                        }));
        }
        if (value.isNumeric())
        {
            THROW_HR_IF(E_INVALIDARG, schema.isMember("minimum") && value.asDouble() < schema["minimum"].asDouble());
            THROW_HR_IF(E_INVALIDARG, schema.isMember("maximum") && value.asDouble() > schema["maximum"].asDouble());
        }
        if (value.isArray())
        {
            THROW_HR_IF(E_INVALIDARG, schema.isMember("minItems") && value.size() < schema["minItems"].asUInt());
            THROW_HR_IF(E_INVALIDARG, schema.isMember("maxItems") && value.size() > schema["maxItems"].asUInt());
            for (const auto& entry : value)
                ValidateToolValue(entry, schema["items"]);
        }
    }

    inline Json::Value ToolPlan(const Json::Value& tools, const std::string& name, const Json::Value& arguments)
    {
        const auto tool = std::find_if(tools.begin(), tools.end(), [&](const auto& entry) { return entry["name"] == name; });
        THROW_HR_IF(E_INVALIDARG, tool == tools.end() || !arguments.isObject());
        const auto& schema = (*tool)["parameters"];
        const auto& properties = schema["properties"];
        auto plan = arguments;
        for (const auto& key : arguments.getMemberNames())
        {
            THROW_HR_IF(E_INVALIDARG, !properties.isMember(key));
            ValidateToolValue(arguments[key], properties[key]);
        }
        for (const auto& key : properties.getMemberNames())
        {
            if (!plan.isMember(key) && properties[key].isMember("default"))
            {
                plan[key] = properties[key]["default"];
                ValidateToolValue(plan[key], properties[key]);
            }
        }
        for (const auto& key : schema["required"])
            THROW_HR_IF(E_INVALIDARG, !plan.isMember(key.asString()));
        plan["kind"] = name.substr(std::string_view{ "terminal_" }.size());
        return plan;
    }
}
