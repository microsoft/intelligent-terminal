// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CustomAgentId.h"
#include <algorithm>
#include <json/json.h>
#include <stdexcept>

namespace Microsoft::Terminal::Settings::Model
{
    inline Json::Value ProjectCustomAcpAgentCommands(const Json::Value& settings)
    {
        if (!settings.isObject())
        {
            throw std::invalid_argument("custom ACP settings root must be an object");
        }
        Json::Value commands{ Json::objectValue };
        const auto append = [&](const Json::Value& value) {
            if (!value.isNull() && !value.isString())
            {
                throw std::invalid_argument("custom ACP command must be a string or null");
            }
            if (value.isNull() || value.asString().empty())
            {
                return;
            }
            const auto command = winrt::to_hstring(value.asString());
            auto id = "custom:" + winrt::to_string(DeriveCustomAgentId(command));
            std::ranges::transform(id, id.begin(), [](const unsigned char ch) { return static_cast<char>(ch >= 'A' && ch <= 'Z' ? ch + ('a' - 'A') : ch); });
            if (id != "custom:" && !commands.isMember(id))
            {
                commands[id] = value;
            }
        };
        const auto& plural = settings["acpCustomCommands"];
        if (plural.isArray())
        {
            for (const auto& command : plural)
            {
                append(command);
            }
        }
        else
        {
            append(plural);
        }
        append(settings["acpCustomCommand"]);
        return commands;
    }
}
