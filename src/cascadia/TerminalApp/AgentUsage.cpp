// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentUsage.h"

#include <stdexcept>

namespace
{
    constexpr size_t MaxMetricIdLength = 64;
    constexpr size_t MaxDecimalTextLength = 64;
    constexpr size_t MaxUnitIdLength = 16;
    constexpr size_t MaxScopeLength = 32;
    constexpr size_t MaxSourceLength = 32;

    std::string requiredString(const Json::Value& item, const char* key, const size_t maxLength)
    {
        const auto& value = item[key];
        if (!value.isString())
        {
            throw std::invalid_argument{ std::string{ "usage item requires string field: " } + key };
        }
        auto text = value.asString();
        if (text.empty() || text.size() > maxLength)
        {
            throw std::invalid_argument{ std::string{ "usage item string length is invalid: " } + key };
        }
        return text;
    }

    bool isDecimalText(const std::string_view text)
    {
        size_t index = 0;
        auto consumeDigits = [&]() {
            const auto start = index;
            while (index < text.size() && text[index] >= '0' && text[index] <= '9')
            {
                ++index;
            }
            return index > start;
        };

        if (!consumeDigits())
        {
            return false;
        }
        if (index < text.size() && text[index] == '.')
        {
            ++index;
            if (!consumeDigits())
            {
                return false;
            }
        }
        if (index < text.size() && (text[index] == 'e' || text[index] == 'E'))
        {
            ++index;
            if (index < text.size() && (text[index] == '+' || text[index] == '-'))
            {
                ++index;
            }
            if (!consumeDigits())
            {
                return false;
            }
        }
        return index == text.size();
    }
}

namespace TerminalApp::AgentUsage
{
    std::vector<Item> Parse(const Json::Value& usage)
    {
        if (usage.isNull())
        {
            return {};
        }
        if (!usage.isObject() || !usage.isMember("items") || !usage["items"].isArray())
        {
            throw std::invalid_argument{ "usage must be null or an object containing an items array" };
        }

        const auto& items = usage["items"];
        if (items.size() > MaxItems)
        {
            throw std::invalid_argument{ "usage contains too many items" };
        }

        std::vector<Item> parsed;
        parsed.reserve(items.size());
        for (const auto& item : items)
        {
            if (!item.isObject())
            {
                throw std::invalid_argument{ "usage item must be an object" };
            }

            Item result;
            result.metricId = requiredString(item, "metric_id", MaxMetricIdLength);
            result.valueDecimalText = requiredString(item, "value_decimal_text", MaxDecimalTextLength);
            if (!isDecimalText(result.valueDecimalText))
            {
                throw std::invalid_argument{ "usage value_decimal_text is invalid" };
            }
            if (item.isMember("limit_decimal_text"))
            {
                result.limitDecimalText = requiredString(item, "limit_decimal_text", MaxDecimalTextLength);
                if (!isDecimalText(*result.limitDecimalText))
                {
                    throw std::invalid_argument{ "usage limit_decimal_text is invalid" };
                }
            }
            result.unitId = requiredString(item, "unit_id", MaxUnitIdLength);
            result.scope = requiredString(item, "scope", MaxScopeLength);
            result.source = requiredString(item, "source", MaxSourceLength);
            if (!item.isMember("stale") || !item["stale"].isBool())
            {
                throw std::invalid_argument{ "usage item requires bool field: stale" };
            }
            result.stale = item["stale"].asBool();
            parsed.emplace_back(std::move(result));
        }
        return parsed;
    }

    void UpdateCache(std::vector<Item>& cache, const Json::Value& usage)
    {
        auto parsed = Parse(usage);
        cache = std::move(parsed);
    }
}
