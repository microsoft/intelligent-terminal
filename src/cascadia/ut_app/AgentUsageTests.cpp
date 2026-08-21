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
        const std::string& displayKind,
        const std::optional<std::string>& limit = std::nullopt)
    {
        Json::Value item{ Json::objectValue };
        item["metric_id"] = metricId;
        item["display_kind"] = displayKind;
        item["value_decimal_text"] = value;
        if (limit)
        {
            item["limit_decimal_text"] = *limit;
        }
        item["unit_id"] = unitId;
        item["unit_display_text"] = unitId;
        item["scope"] = "session";
        item["source"] = "acp_standard";
        item["stale"] = false;
        return item;
    }

    class AgentUsageTests
    {
        TEST_CLASS(AgentUsageTests);

        TEST_METHOD(ParseValidItems);
        TEST_METHOD(ParseProviderDisplayMetadata);
        TEST_METHOD(ParseNullAndEmptyClear);
        TEST_METHOD(ParseRejectsMalformedItemAtomically);
        TEST_METHOD(ParseRejectsInvalidDecimalText);
        TEST_METHOD(ParseRejectsExcessiveItems);
        TEST_METHOD(UpdateCacheReplacesAndClears);
        TEST_METHOD(UpdateCacheDropsInvalidContext);
        TEST_METHOD(TryUpdateCacheContainsMalformedUsage);
        TEST_METHOD(BuildPrimaryDisplayTextsFormatsContextPercentageAndCost);
        TEST_METHOD(BuildPrimaryDisplayTextsIgnoresInputOutput);
        TEST_METHOD(BuildPrimaryDisplayTextsCapsMainBarItems);
        TEST_METHOD(BuildPrimaryDisplayShowsCostWithoutTokens);
        TEST_METHOD(BuildPrimaryDisplayRoundsCostAndPreservesFullText);
        TEST_METHOD(BuildPrimaryDisplayShowsTokensWithoutCost);
        TEST_METHOD(BuildPrimaryDisplayShowsOnlyValidContext);
        TEST_METHOD(BuildPrimaryDisplayShowsProviderContextAndAic);
        TEST_METHOD(BuildPrimaryDisplayUsesFirstBillingItem);
        TEST_METHOD(BuildPrimaryDisplayHidesStaleMetrics);
        TEST_METHOD(BuildPrimaryDisplayHidesInputOutputOnly);
        TEST_METHOD(BuildPrimaryDisplayHidesAfterContainedError);
        TEST_METHOD(BuildPrimaryDisplayHidesWhenNothingReported);
        TEST_METHOD(BuildPrimaryDisplayHidesUsageAndCostWhenDisabled);
    };

    void AgentUsageTests::ParseValidItems()
    {
        const auto usage = Json::Value{ Json::objectValue };
        auto input = usage;
        input["items"] = Json::Value{ Json::arrayValue };
        input["items"].append(makeUsageItem("acp.context.window", "1024", "token", "context", "8192"));
        input["items"].append(makeUsageItem("acp.billing.cost", "0.004", "USD", "billing"));

        const auto parsed = TerminalApp::AgentUsage::Parse(input);

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), parsed.size());
        VERIFY_ARE_EQUAL(std::string{ "acp.context.window" }, parsed[0].metricId);
        VERIFY_ARE_EQUAL(std::string{ "1024" }, parsed[0].valueDecimalText);
        VERIFY_ARE_EQUAL(std::string{ "8192" }, parsed[0].limitDecimalText.value());
        VERIFY_ARE_EQUAL(std::string{ "USD" }, parsed[1].unitId);
        VERIFY_IS_FALSE(parsed[1].limitDecimalText.has_value());
    }

    void AgentUsageTests::ParseProviderDisplayMetadata()
    {
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        auto context = makeUsageItem("acp.context.window", "30000", "token", "context", "264000");
        context["source"] = "provider_reported";
        context["value_display_text"] = "30k";
        context["limit_display_text"] = "264k";
        context["reported_percent"] = Json::UInt64{ 11 };
        usage["items"].append(std::move(context));

        const auto parsed = TerminalApp::AgentUsage::Parse(usage);

        VERIFY_ARE_EQUAL(std::string{ "30k" }, parsed[0].valueDisplayText.value());
        VERIFY_ARE_EQUAL(std::string{ "264k" }, parsed[0].limitDisplayText.value());
        VERIFY_ARE_EQUAL(static_cast<uint64_t>(11), parsed[0].reportedPercent.value());
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
        input["items"].append(makeUsageItem("acp.context.window", "20", "token", "context", "100"));
        auto malformed = makeUsageItem("acp.billing.cost", "1.0", "USD", "billing");
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
        input["items"].append(makeUsageItem("acp.billing.cost", "NaN", "USD", "billing"));

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
            input["items"].append(makeUsageItem("acp.context.window", "20", "token", "context"));
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
        usage["items"].append(makeUsageItem("acp.context.window", "20", "token", "context", "100"));

        TerminalApp::AgentUsage::UpdateCache(cache, usage);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), cache.size());

        TerminalApp::AgentUsage::UpdateCache(cache, Json::Value::nullSingleton());
        VERIFY_IS_TRUE(cache.empty());
    }

    void AgentUsageTests::UpdateCacheDropsInvalidContext()
    {
        const auto previous = makeUsageItem("acp.context.window", "20", "token", "context", "100");
        Json::Value valid{ Json::objectValue };
        valid["items"] = Json::Value{ Json::arrayValue };
        valid["items"].append(previous);
        std::vector<TerminalApp::AgentUsage::Item> cache;
        TerminalApp::AgentUsage::UpdateCache(cache, valid);
        auto malformed = previous;
        malformed["value_decimal_text"] = "not-a-number";
        Json::Value invalid{ Json::objectValue };
        invalid["items"] = Json::Value{ Json::arrayValue };
        invalid["items"].append(std::move(malformed));

        TerminalApp::AgentUsage::UpdateCache(cache, invalid);
        VERIFY_IS_TRUE(cache.empty());
    }

    void AgentUsageTests::TryUpdateCacheContainsMalformedUsage()
    {
        const auto validItem = makeUsageItem("acp.context.window", "20", "token", "context", "100");
        Json::Value validUsage{ Json::objectValue };
        validUsage["items"] = Json::Value{ Json::arrayValue };
        validUsage["items"].append(validItem);
        const auto expected = TerminalApp::AgentUsage::Parse(validUsage);
        std::vector<TerminalApp::AgentUsage::Item> cache{ expected };

        VERIFY_IS_FALSE(TerminalApp::AgentUsage::TryUpdateCache(cache, Json::Value{ "malformed" }));
        VERIFY_IS_TRUE(cache.empty());

        Json::Value malformedSchema{ Json::objectValue };
        malformedSchema["items"] = Json::Value{ Json::arrayValue };
        auto malformedItem = validItem;
        malformedItem["stale"] = "false";
        malformedSchema["items"].append(std::move(malformedItem));
        cache = expected;

        VERIFY_IS_FALSE(TerminalApp::AgentUsage::TryUpdateCache(cache, malformedSchema));
        VERIFY_IS_TRUE(cache.empty());
    }

    void AgentUsageTests::BuildPrimaryDisplayTextsFormatsContextPercentageAndCost()
    {
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        usage["items"].append(makeUsageItem("acp.context.window", "1024", "token", "context", "8192"));
        usage["items"].append(makeUsageItem("acp.billing.cost", "0.004", "USD", "billing"));

        const auto texts = TerminalApp::AgentUsage::BuildPrimaryDisplayTexts(
            TerminalApp::AgentUsage::Parse(usage),
            L"tokens");

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), texts.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 13%" }, texts[0]);
        VERIFY_ARE_EQUAL(std::wstring{ L"<0.01 USD" }, texts[1]);
    }

    void AgentUsageTests::BuildPrimaryDisplayTextsIgnoresInputOutput()
    {
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        usage["items"].append(makeUsageItem("acp.tokens.input", "12341", "token", "other"));
        usage["items"].append(makeUsageItem("acp.tokens.output", "23", "token", "other"));
        usage["items"].append(makeUsageItem("acp.context.window", "1024", "token", "context", "8192"));
        usage["items"].append(makeUsageItem("acp.billing.cost", "0.004", "USD", "billing"));

        const auto texts = TerminalApp::AgentUsage::BuildPrimaryDisplayTexts(
            TerminalApp::AgentUsage::Parse(usage),
            L"tokens");

        VERIFY_ARE_EQUAL(static_cast<size_t>(2), texts.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 13%" }, texts[0]);
        VERIFY_ARE_EQUAL(std::wstring{ L"<0.01 USD" }, texts[1]);
    }

    void AgentUsageTests::BuildPrimaryDisplayTextsCapsMainBarItems()
    {
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), TerminalApp::AgentUsage::MaxPrimaryItems);

        const std::vector<TerminalApp::AgentUsage::Item> items{
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.context.window",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                .valueDecimalText = "20",
                .limitDecimalText = "100",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "acp_standard",
            },
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.billing.cost",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Billing,
                .valueDecimalText = "0.004",
                .unitId = "USD",
                .unitDisplayText = "USD",
                .scope = "session",
                .source = "acp_standard",
            },
            TerminalApp::AgentUsage::Item{
                .metricId = "provider.other",
                .valueDecimalText = "7",
                .unitId = "unit",
                .unitDisplayText = "unit",
                .scope = "session",
                .source = "provider_reported",
            },
        };

        const auto texts = TerminalApp::AgentUsage::BuildPrimaryDisplayTexts(items, L"Tokens");

        VERIFY_ARE_EQUAL(TerminalApp::AgentUsage::MaxPrimaryItems, texts.size());
    }

    void AgentUsageTests::BuildPrimaryDisplayShowsCostWithoutTokens()
    {
        const std::vector<TerminalApp::AgentUsage::Item> items{
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.billing.cost",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Billing,
                .valueDecimalText = "0.004",
                .unitId = "USD",
                .unitDisplayText = "USD",
                .scope = "session",
                .source = "provider_reported",
            },
        };

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(items, L"Tokens");

        VERIFY_IS_TRUE(display.visible);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), display.items.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"<0.01 USD" }, display.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"0.004 USD" }, display.items[0].fullText);
    }

    void AgentUsageTests::BuildPrimaryDisplayRoundsCostAndPreservesFullText()
    {
        const auto build = [](const std::string& value) {
            return TerminalApp::AgentUsage::BuildPrimaryDisplay(
                { TerminalApp::AgentUsage::Item{
                    .metricId = "acp.billing.cost",
                    .displayKind = TerminalApp::AgentUsage::DisplayKind::Billing,
                    .valueDecimalText = value,
                    .unitId = "USD",
                    .unitDisplayText = "USD",
                    .scope = "session",
                    .source = "acp_standard",
                } },
                L"Tokens");
        };

        const auto roundsUp = build("1.235");
        VERIFY_ARE_EQUAL(std::wstring{ L"1.24 USD" }, roundsUp.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"1.235 USD" }, roundsUp.items[0].fullText);

        const auto roundsDown = build("1.234");
        VERIFY_ARE_EQUAL(std::wstring{ L"1.23 USD" }, roundsDown.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"1.234 USD" }, roundsDown.items[0].fullText);

        const auto zero = build("0");
        VERIFY_ARE_EQUAL(std::wstring{ L"0.00 USD" }, zero.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"0 USD" }, zero.items[0].fullText);
    }

    void AgentUsageTests::BuildPrimaryDisplayShowsTokensWithoutCost()
    {
        const std::vector<TerminalApp::AgentUsage::Item> items{
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.context.window",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                .valueDecimalText = "1024",
                .limitDecimalText = "8192",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "acp_standard",
            },
        };

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(items, L"tokens");

        VERIFY_IS_TRUE(display.visible);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), display.items.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 13%" }, display.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window:\n1024 / 8192 tokens (13%)" }, display.items[0].fullText);
    }

    void AgentUsageTests::BuildPrimaryDisplayShowsOnlyValidContext()
    {
        const auto build = [](const std::string& used, const std::string& size) {
            return TerminalApp::AgentUsage::BuildPrimaryDisplay(
                { TerminalApp::AgentUsage::Item{
                    .metricId = "acp.context.window",
                    .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                    .valueDecimalText = used,
                    .limitDecimalText = size,
                    .unitId = "token",
                    .unitDisplayText = "token",
                    .scope = "session",
                    .source = "acp_standard",
                } },
                L"tokens");
        };

        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 43%" }, build("43", "100").items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 43%" }, build("425", "1000").items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 42%" }, build("424", "1000").items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 0%" }, build("0", "100").items[0].text);
        VERIFY_IS_TRUE(build("100", "100").visible);
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 100%" }, build("100", "100").items[0].text);
        VERIFY_IS_FALSE(build("101", "100").visible);
        VERIFY_IS_FALSE(build("1", "0").visible);

        Json::Value negative{ Json::objectValue };
        negative["items"] = Json::Value{ Json::arrayValue };
        auto invalidContext = makeUsageItem("vendor.context", "-1", "token", "context", "100");
        negative["items"].append(std::move(invalidContext));
        negative["items"].append(makeUsageItem("vendor.billing", "1.235", "vendor.usd", "billing"));
        negative["items"][1]["unit_display_text"] = "USD";

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(
            TerminalApp::AgentUsage::Parse(negative),
            L"tokens");
        VERIFY_IS_TRUE(display.visible);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), display.items.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"1.24 USD" }, display.items[0].text);

        const std::vector<TerminalApp::AgentUsage::Item> duplicateContext{
            TerminalApp::AgentUsage::Item{
                .metricId = "invalid.context",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                .valueDecimalText = "101",
                .limitDecimalText = "100",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "invalid",
            },
            TerminalApp::AgentUsage::Item{
                .metricId = "valid.context",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                .valueDecimalText = "50",
                .limitDecimalText = "100",
                .reportedPercent = 900,
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "valid",
            },
        };
        const auto duplicateDisplay = TerminalApp::AgentUsage::BuildPrimaryDisplay(duplicateContext, L"tokens");
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 50%" }, duplicateDisplay.items[0].text);
    }

    void AgentUsageTests::BuildPrimaryDisplayShowsProviderContextAndAic()
    {
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        auto context = makeUsageItem("vendor.context.gauge", "30000", "token", "context", "264000");
        context["source"] = "provider_reported";
        context["value_display_text"] = "30k";
        context["limit_display_text"] = "264k";
        context["reported_percent"] = Json::UInt64{ 11 };
        context["unit_display_text"] = "tokens";
        usage["items"].append(std::move(context));
        auto aiCredits = makeUsageItem("vendor.billing.credits", "7.5539", "github.ai_credit", "billing");
        aiCredits["source"] = "provider_reported";
        aiCredits["unit_display_text"] = "AIC";
        usage["items"].append(std::move(aiCredits));

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(
            TerminalApp::AgentUsage::Parse(usage),
            L"tokens");

        VERIFY_IS_TRUE(display.visible);
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), display.items.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window: 11%" }, display.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Context Window:\n30k / 264k tokens (11%)" }, display.items[0].fullText);
        VERIFY_ARE_EQUAL(std::wstring{ L"7.55 AIC" }, display.items[1].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"7.5539 AIC" }, display.items[1].fullText);
    }

    void AgentUsageTests::BuildPrimaryDisplayUsesFirstBillingItem()
    {
        const std::vector<TerminalApp::AgentUsage::Item> items{
            TerminalApp::AgentUsage::Item{
                .metricId = "vendor.billing.primary",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Billing,
                .valueDecimalText = "1.235",
                .unitId = "vendor.currency.usd",
                .unitDisplayText = "USD",
                .scope = "session",
                .source = "first_source",
            },
            TerminalApp::AgentUsage::Item{
                .metricId = "vendor.billing.secondary",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Billing,
                .valueDecimalText = "7.5539",
                .unitId = "vendor.credit.aic",
                .unitDisplayText = "AIC",
                .scope = "session",
                .source = "second_source",
            },
        };

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(items, L"tokens");

        VERIFY_ARE_EQUAL(static_cast<size_t>(1), display.items.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"1.24 USD" }, display.items[0].text);
    }

    void AgentUsageTests::BuildPrimaryDisplayHidesUsageAndCostWhenDisabled()
    {
        const std::vector<TerminalApp::AgentUsage::Item> items{
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.context.window",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                .valueDecimalText = "1024",
                .limitDecimalText = "8192",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "acp_standard",
            },
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.billing.cost",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Billing,
                .valueDecimalText = "0.004",
                .unitId = "USD",
                .unitDisplayText = "USD",
                .scope = "session",
                .source = "acp_standard",
            },
        };

        const auto hidden = TerminalApp::AgentUsage::BuildPrimaryDisplay(items, L"Tokens", false);
        VERIFY_IS_FALSE(hidden.visible);
        VERIFY_IS_TRUE(hidden.items.empty());

        const auto visible = TerminalApp::AgentUsage::BuildPrimaryDisplay(items, L"Tokens", true);
        VERIFY_IS_TRUE(visible.visible);
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), visible.items.size());
    }

    void AgentUsageTests::BuildPrimaryDisplayHidesStaleMetrics()
    {
        Json::Value usage{ Json::objectValue };
        usage["items"] = Json::Value{ Json::arrayValue };
        auto staleContext = makeUsageItem("acp.context.window", "1024", "token", "context", "8192");
        staleContext["stale"] = true;
        usage["items"].append(std::move(staleContext));
        usage["items"].append(makeUsageItem("acp.billing.cost", "0.004", "USD", "billing"));

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(
            TerminalApp::AgentUsage::Parse(usage),
            L"Tokens");

        VERIFY_IS_TRUE(display.visible);
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), display.items.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"<0.01 USD" }, display.items[0].text);
        VERIFY_ARE_EQUAL(std::wstring{ L"0.004 USD" }, display.items[0].fullText);
    }

    void AgentUsageTests::BuildPrimaryDisplayHidesInputOutputOnly()
    {
        const std::vector<TerminalApp::AgentUsage::Item> items{
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.tokens.input",
                .valueDecimalText = "12341",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "acp_standard",
            },
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.tokens.output",
                .valueDecimalText = "23",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "acp_standard",
            },
        };

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(items, L"Tokens");

        VERIFY_IS_FALSE(display.visible);
        VERIFY_IS_TRUE(display.items.empty());
    }

    void AgentUsageTests::BuildPrimaryDisplayHidesAfterContainedError()
    {
        std::vector<TerminalApp::AgentUsage::Item> cache{
            TerminalApp::AgentUsage::Item{
                .metricId = "acp.context.window",
                .displayKind = TerminalApp::AgentUsage::DisplayKind::Context,
                .valueDecimalText = "20",
                .limitDecimalText = "100",
                .unitId = "token",
                .unitDisplayText = "token",
                .scope = "session",
                .source = "acp_standard",
            },
        };
        TerminalApp::AgentUsage::UpdateCache(cache, Json::Value::nullSingleton());

        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay(cache, L"Tokens");

        VERIFY_IS_FALSE(display.visible);
        VERIFY_IS_TRUE(display.items.empty());
    }

    void AgentUsageTests::BuildPrimaryDisplayHidesWhenNothingReported()
    {
        const auto display = TerminalApp::AgentUsage::BuildPrimaryDisplay({}, L"Tokens");

        VERIFY_IS_FALSE(display.visible);
        VERIFY_IS_TRUE(display.items.empty());
    }
}
