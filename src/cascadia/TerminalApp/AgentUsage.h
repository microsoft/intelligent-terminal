// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <json/json.h>

#include <optional>
#include <string>
#include <vector>

namespace TerminalApp::AgentUsage
{
    inline constexpr size_t MaxItems = 8;

    struct Item
    {
        std::string metricId;
        std::string valueDecimalText;
        std::optional<std::string> limitDecimalText;
        std::string unitId;
        std::string scope;
        std::string source;
        bool stale{ false };

        bool operator==(const Item&) const = default;
    };

    std::vector<Item> Parse(const Json::Value& usage);
    void UpdateCache(std::vector<Item>& cache, const Json::Value& usage);
}
