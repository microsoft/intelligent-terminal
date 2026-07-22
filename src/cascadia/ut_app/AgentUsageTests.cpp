// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/AgentUsage.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    Json::Value makeUsageItem(
        const std::string& metricId,
        const std::string& value,
        const std::string& unitId,
        const std::optional<std::string>& limit = std::nullopt)
    {
        Json::Value item{ Json::objectValue };
        item["metric_id"] = metricId;
        item["value_decimal_text"] = value;
        if (limit)
        {
            item["limit_decimal_text"] = *limit;
        }
        item["unit_id"] = unitId;
        item["scope"] = "session";
        item["source"] = "acp_standard";
        item["stale"] = false;
        return item;
    }

    class AgentUsageTests
    {
        TEST_CLASS(AgentUsageTests);

        TEST_METHOD(ParseValidItems);
        TEST_METHOD(ParseNullAndEmptyClear);
        TEST_METHOD(ParseRejectsMalformedItemAtomically);
        TEST_METHOD(ParseRejectsInvalidDecimalText);
        TEST_METHOD(ParseRejectsExcessiveItems);
        TEST_METHOD(UpdateCacheReplacesAndClears);
        TEST_METHOD(UpdateCachePreservesPreviousOnMalformedInput);
        TEST_METHOD(BuildPrimaryDisplayTextsFormatsContextAndCost);
        TEST_METHOD(BuildPrimaryDisplayTextsCapsMainBarItems);
    };

    void AgentUsageTests::ParseValidItems()
    {
        const auto usage = Json::Value{ Json::objectValue };
        auto input = usage;
        input["items"] = Json::Value{ Json::arrayValue };
        input["items"].append(makeUsageItem("acp.context.window", "1024", "token", "8192"));
        input["items"].append(makeUsageItem("acp.billing.cost", "0.004", "USD"));

        const auto parsed = TerminalApp::AgentUsage::Parse(input);

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), parsed.size());
        VERIFY_ARE_EQUAL(std::string{ "acp.context.window" }, parsed[0].metricId);
        VERIFY_ARE_EQUAL(std::string{ "1024" }, parsed[0].valueDecimalText);
        VERIFY_ARE_EQUAL(std::string{ "8192" }, parsed[0].limitDecimalText.value());
        VERIFY_ARE_EQUAL(std::string{ "USD" }, parsed[1].unitId);
        VERIFY_IS_FALSE(parsed[1].limitDecimalText.has_value());
    }

    void AgentUsageTests::ParseNullAndEmptyClear()
    {
        VERIFY_IS_TRUE(TerminalApp::AgentUsage::Parse(Json::Value::nullSingleton()).empty());

        Json::Value empty{ Json::objectValue };
        empty["items"] = Json::Value{ Json::arrayValue };
        VERIFY_IS_TRUE(TerminalApp::AgentUsage::Parse(empty).empty());
    }

    void AgentUsageTests::ParseRejectsMalformedItemAtomically()
    {
        Json::Value input{ Json::objectValue };
        input["items"] = Json::Value{ Json::arrayValue };
        input["items"].append(makeUsageItem("acp.context.window", "20", "token", "100"));
        auto malformed = makeUsageItem("acp.billing.cost", "1.0", "USD");
        malformed["stale"] = "false";
        input["items"].append(std::move(malformed));

        VERIFY_THROWS_SPECIFIC(
            TerminalApp::AgentUsage::Parse(input),
            std::invalid_argument,
            [](const std::invalid_argument&) { return true; });
    }

    void AgentUsageTests::ParseRejectsInvalidDecimalText()
    {
        Json::Value input{ Json::objectValue };
        input["items"] = Json::Value{ Json::arrayValue };
        input["items"].append(makeUsageItem("acp.billing.cost", "NaN", "USD"));

        VERIFY_THROWS_SPECIFIC(
            TerminalApp::AgentUsage::Parse(input),
            std::invalid_argument,
            [](const std::invalid_argument&) { return true; });
    }

    void AgentUsageTests::ParseRejectsExcessiveItems()
    {
        Json::Value input{ Json::objectValue };
        input["items"] = Json::Value{ Json::arrayValue };
        for (size_t i = 0; i < TerminalApp::AgentUsage::MaxItems + 1; ++i)
        {
            input["items"].append(makeUsageItem("acp.context.window", "20", "token"));
        }

        VERIFY_THROWS_SPECIFIC(
            TerminalApp::AgentUsage::Parse(input),
            std::invalid_argument,
            [](const std::invalid_argument&) { return true; });
    }

    void AgentUsageTests::UpdateCacheReplacesAndClears()
    {
        std::vector<TerminalApp::AgentUsage::Item> cache;
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        usage["items"].append(makeUsageItem("acp.context.window", "20", "token", "100"));

        TerminalApp::AgentUsage::UpdateCache(cache, usage);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), cache.size());

        TerminalApp::AgentUsage::UpdateCache(cache, Json::Value::nullSingleton());
        VERIFY_IS_TRUE(cache.empty());
    }

    void AgentUsageTests::UpdateCachePreservesPreviousOnMalformedInput()
    {
        const auto previous = makeUsageItem("acp.context.window", "20", "token", "100");
        Json::Value valid{ Json::objectValue };
        valid["items"] = Json::Value{ Json::arrayValue };
        valid["items"].append(previous);
        std::vector<TerminalApp::AgentUsage::Item> cache;
        TerminalApp::AgentUsage::UpdateCache(cache, valid);
        const auto before = cache;

        auto malformed = previous;
        malformed["value_decimal_text"] = "not-a-number";
        Json::Value invalid{ Json::objectValue };
        invalid["items"] = Json::Value{ Json::arrayValue };
        invalid["items"].append(std::move(malformed));

        VERIFY_THROWS_SPECIFIC(
            TerminalApp::AgentUsage::UpdateCache(cache, invalid),
            std::invalid_argument,
            [](const std::invalid_argument&) { return true; });
        VERIFY_IS_TRUE(cache == before);
    }

    void AgentUsageTests::BuildPrimaryDisplayTextsFormatsContextAndCost()
    {
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        usage["items"].append(makeUsageItem("acp.context.window", "1024", "token", "8192"));
        usage["items"].append(makeUsageItem("acp.billing.cost", "0.004", "USD"));

        const auto texts = TerminalApp::AgentUsage::BuildPrimaryDisplayTexts(
            TerminalApp::AgentUsage::Parse(usage),
            L"Tokens");

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), texts.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"1024 / 8192 Tokens" }, texts[0]);
        VERIFY_ARE_EQUAL(std::wstring{ L"0.004 USD" }, texts[1]);
    }

    void AgentUsageTests::BuildPrimaryDisplayTextsCapsMainBarItems()
    {
        std::vector<TerminalApp::AgentUsage::Item> items;
        for (size_t i = 0; i < 3; ++i)
        {
            items.push_back(TerminalApp::AgentUsage::Item{
                .metricId = "metric." + std::to_string(i),
                .valueDecimalText = std::to_string(i),
                .unitId = "unit",
                .scope = "session",
                .source = "acp_standard",
            });
        }

        const auto texts = TerminalApp::AgentUsage::BuildPrimaryDisplayTexts(items, L"Tokens");

        VERIFY_ARE_EQUAL(TerminalApp::AgentUsage::MaxPrimaryItems, texts.size());
    }
}
