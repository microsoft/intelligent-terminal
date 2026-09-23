// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentUsage.h"

#include <charconv>
#include <stdexcept>

namespace
{
    constexpr size_t MaxMetricIdLength = 64;
    constexpr size_t MaxDisplayKindLength = 16;
    constexpr size_t MaxDecimalTextLength = 64;
    constexpr size_t MaxDisplayTextLength = 64;
    constexpr size_t MaxUnitIdLength = 16;
    constexpr size_t MaxUnitDisplayTextLength = 64;
    constexpr size_t MaxScopeLength = 32;
    constexpr size_t MaxSourceLength = 32;
    constexpr int64_t MaxFormattedIntegerDigits = 512;

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

    TerminalApp::AgentUsage::DisplayKind parseDisplayKind(const Json::Value& item)
    {
        const auto text = requiredString(item, "display_kind", MaxDisplayKindLength);
        if (text == "context")
        {
            return TerminalApp::AgentUsage::DisplayKind::Context;
        }
        if (text == "billing")
        {
            return TerminalApp::AgentUsage::DisplayKind::Billing;
        }
        return TerminalApp::AgentUsage::DisplayKind::Other;
    }

    std::wstring formatBillingAmount(const std::string_view text)
    {
        const auto exponentMarker = text.find_first_of("eE");
        const auto mantissa = text.substr(0, exponentMarker);
        const auto decimalPoint = mantissa.find('.');
        const auto integerDigits = decimalPoint == std::string_view::npos ? mantissa.size() : decimalPoint;

        std::string digits;
        digits.reserve(mantissa.size());
        for (const auto ch : mantissa)
        {
            if (ch != '.')
            {
                digits.push_back(ch);
            }
        }

        int64_t exponent = 0;
        if (exponentMarker != std::string_view::npos)
        {
            auto index = exponentMarker + 1;
            const auto negative = index < text.size() && text[index] == '-';
            if (index < text.size() && (text[index] == '+' || text[index] == '-'))
            {
                ++index;
            }
            for (; index < text.size(); ++index)
            {
                const auto digit = static_cast<int64_t>(text[index] - '0');
                exponent = std::min<int64_t>(MaxFormattedIntegerDigits + 1, exponent * 10 + digit);
            }
            if (negative)
            {
                exponent = -exponent;
            }
        }

        const auto decimalPosition = static_cast<int64_t>(integerDigits) + exponent;
        const auto firstNonZero = digits.find_first_not_of('0');
        if (firstNonZero == std::string::npos)
        {
            return L"0.00";
        }

        // Positive values below one cent should not be visually rounded to zero.
        if (static_cast<int64_t>(firstNonZero) > decimalPosition + 1)
        {
            return L"<0.01";
        }

        // Keep the UI safe if a synthetic normalized event bypasses the
        // bounded projection contract with an extreme exponent.
        if (decimalPosition > MaxFormattedIntegerDigits)
        {
            return til::u8u16(text);
        }

        const auto digitAt = [&](const int64_t position) {
            return position >= 0 && position < static_cast<int64_t>(digits.size()) ?
                       digits[static_cast<size_t>(position)] :
                       '0';
        };

        std::string integer;
        if (decimalPosition > 0)
        {
            integer.reserve(static_cast<size_t>(decimalPosition));
            for (int64_t position = 0; position < decimalPosition; ++position)
            {
                integer.push_back(digitAt(position));
            }
            const auto firstSignificant = integer.find_first_not_of('0');
            integer.erase(0, firstSignificant == std::string::npos ? integer.size() - 1 : firstSignificant);
        }
        else
        {
            integer = "0";
        }

        std::string rounded = integer;
        rounded.push_back(digitAt(decimalPosition));
        rounded.push_back(digitAt(decimalPosition + 1));
        if (digitAt(decimalPosition + 2) >= '5')
        {
            auto position = rounded.size();
            while (position > 0 && rounded[position - 1] == '9')
            {
                rounded[--position] = '0';
            }
            if (position == 0)
            {
                rounded.insert(rounded.begin(), '1');
            }
            else
            {
                ++rounded[position - 1];
            }
        }

        rounded.insert(rounded.end() - 2, '.');
        return til::u8u16(rounded);
    }

    std::optional<uint64_t> parseContextCount(const std::string_view text)
    {
        uint64_t value = 0;
        const auto result = std::from_chars(text.data(), text.data() + text.size(), value);
        if (result.ec != std::errc{} || result.ptr != text.data() + text.size())
        {
            return std::nullopt;
        }
        return value;
    }

    std::wstring formatContextPercentage(const uint64_t used, const uint64_t size)
    {
        auto whole = used / size;
        const auto remainder = used % size;

        // Compute round(100 * remainder / size) without overflowing u64.
        // Each iteration maintains residual < size and adds one remainder.
        uint64_t fractional = 0;
        uint64_t residual = 0;
        for (size_t i = 0; i < 100; ++i)
        {
            if (residual >= size - remainder)
            {
                residual -= size - remainder;
                ++fractional;
            }
            else
            {
                residual += remainder;
            }
        }
        if (residual >= size - residual)
        {
            ++fractional;
        }
        if (fractional == 100)
        {
            ++whole;
            fractional = 0;
        }

        std::string percentage;
        if (whole == 0)
        {
            percentage = std::to_string(fractional);
        }
        else
        {
            percentage = std::to_string(whole);
            percentage.push_back(static_cast<char>('0' + fractional / 10));
            percentage.push_back(static_cast<char>('0' + fractional % 10));
        }
        percentage.push_back('%');
        return til::u8u16(percentage);
    }

    bool isValidContextItem(const TerminalApp::AgentUsage::Item& item)
    {
        if (item.displayKind != TerminalApp::AgentUsage::DisplayKind::Context ||
            item.stale || !item.limitDecimalText)
        {
            return false;
        }
        const auto used = parseContextCount(item.valueDecimalText);
        const auto size = parseContextCount(*item.limitDecimalText);
        return used && size && *size > 0 && *used <= *size;
    }

    std::vector<TerminalApp::AgentUsage::PrimaryDisplayItem> buildPrimaryDisplayItems(
        const std::vector<TerminalApp::AgentUsage::Item>& items,
        const std::wstring_view tokensUnit,
        const std::wstring_view contextWindowLabel)
    {
        using namespace TerminalApp::AgentUsage;

        std::vector<PrimaryDisplayItem> displayItems;
        displayItems.reserve(std::min(items.size(), MaxPrimaryItems));
        for (const auto displayKind : { DisplayKind::Context, DisplayKind::Billing })
        {
            const auto item = std::ranges::find_if(items, [displayKind](const auto& candidate) {
                return displayKind == DisplayKind::Context ?
                           isValidContextItem(candidate) :
                           candidate.displayKind == displayKind && !candidate.stale;
            });
            if (item == items.end() || displayItems.size() == MaxPrimaryItems)
            {
                continue;
            }

            if (displayKind == DisplayKind::Context)
            {
                const auto used = parseContextCount(item->valueDecimalText);
                const auto size = parseContextCount(*item->limitDecimalText);
                const auto percentageText = item->reportedPercent && *item->reportedPercent <= 100 ?
                                                std::to_wstring(*item->reportedPercent) + L"%" :
                                                formatContextPercentage(*used, *size);

                std::wstring text{ contextWindowLabel };
                text += L": ";
                text += percentageText;

                std::wstring fullText{ contextWindowLabel };
                fullText += L":\n";
                fullText += til::u8u16(item->valueDisplayText.value_or(item->valueDecimalText));
                fullText += L" / ";
                fullText += til::u8u16(item->limitDisplayText.value_or(*item->limitDecimalText));
                fullText += L" ";
                fullText += tokensUnit;
                fullText += L" (";
                fullText += percentageText;
                fullText += L")";

                displayItems.emplace_back(PrimaryDisplayItem{
                    .text = std::move(text),
                    .fullText = std::move(fullText),
                });
            }
            else
            {
                auto fullText = til::u8u16(item->valueDecimalText);
                fullText += L" ";
                fullText += til::u8u16(item->unitDisplayText);
                auto text = formatBillingAmount(item->valueDecimalText);
                text += L" ";
                text += til::u8u16(item->unitDisplayText);
                displayItems.emplace_back(PrimaryDisplayItem{
                    .text = std::move(text),
                    .fullText = std::move(fullText),
                });
            }
        }
        return displayItems;
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
            result.displayKind = parseDisplayKind(item);
            result.valueDecimalText = requiredString(item, "value_decimal_text", MaxDecimalTextLength);
            if (!isDecimalText(result.valueDecimalText))
            {
                if (result.displayKind == DisplayKind::Context)
                {
                    continue;
                }
                throw std::invalid_argument{ "usage value_decimal_text is invalid" };
            }
            if (item.isMember("limit_decimal_text"))
            {
                result.limitDecimalText = requiredString(item, "limit_decimal_text", MaxDecimalTextLength);
                if (!isDecimalText(*result.limitDecimalText))
                {
                    if (result.displayKind == DisplayKind::Context)
                    {
                        continue;
                    }
                    throw std::invalid_argument{ "usage limit_decimal_text is invalid" };
                }
            }
            if (result.displayKind == DisplayKind::Context)
            {
                const auto used = parseContextCount(result.valueDecimalText);
                const auto size = result.limitDecimalText ?
                                      parseContextCount(*result.limitDecimalText) :
                                      std::nullopt;
                if (!used || !size || *size == 0 || *used > *size)
                {
                    continue;
                }
            }
            if (item.isMember("value_display_text"))
            {
                result.valueDisplayText = requiredString(item, "value_display_text", MaxDisplayTextLength);
            }
            if (item.isMember("limit_display_text"))
            {
                result.limitDisplayText = requiredString(item, "limit_display_text", MaxDisplayTextLength);
            }
            if (item.isMember("reported_percent"))
            {
                if (!item["reported_percent"].isUInt64())
                {
                    throw std::invalid_argument{ "usage reported_percent is invalid" };
                }
                result.reportedPercent = item["reported_percent"].asUInt64();
            }
            result.unitId = requiredString(item, "unit_id", MaxUnitIdLength);
            result.unitDisplayText = item.isMember("unit_display_text") ?
                                         requiredString(item, "unit_display_text", MaxUnitDisplayTextLength) :
                                         result.unitId;
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

    bool TryUpdateCache(std::vector<Item>& cache, const Json::Value& usage) noexcept
    {
        try
        {
            UpdateCache(cache, usage);
            return true;
        }
        catch (...)
        {
            cache.clear();
            return false;
        }
    }

    std::vector<std::wstring> BuildPrimaryDisplayTexts(
        const std::vector<Item>& items,
        const std::wstring_view tokensUnit,
        const std::wstring_view contextWindowLabel)
    {
        const auto displayItems = buildPrimaryDisplayItems(
            items,
            tokensUnit,
            contextWindowLabel);
        std::vector<std::wstring> texts;
        texts.reserve(displayItems.size());
        for (const auto& item : displayItems)
        {
            texts.emplace_back(item.text);
        }
        return texts;
    }

    PrimaryDisplay BuildPrimaryDisplay(
        const std::vector<Item>& items,
        const std::wstring_view tokensUnit,
        const bool showUsageAndCost,
        const std::wstring_view contextWindowLabel)
    {
        if (!showUsageAndCost)
        {
            return {};
        }

        auto displayItems = buildPrimaryDisplayItems(
            items,
            tokensUnit,
            contextWindowLabel);
        const auto visible = !displayItems.empty();
        return PrimaryDisplay{
            .items = std::move(displayItems),
            .visible = visible,
        };
    }
}
