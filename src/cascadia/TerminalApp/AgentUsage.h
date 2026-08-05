// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <json/json.h>

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace TerminalApp::AgentUsage
{
    inline constexpr size_t MaxItems = 8;
    inline constexpr size_t MaxPrimaryItems = 2;

    enum class DisplayKind
    {
        Other,
        Context,
        Billing,
    };

    struct Item
    {
        std::string metricId;
        DisplayKind displayKind{ DisplayKind::Other };
        std::string valueDecimalText;
        std::optional<std::string> limitDecimalText;
        std::optional<std::string> valueDisplayText;
        std::optional<std::string> limitDisplayText;
        std::optional<uint64_t> reportedPercent;
        std::string unitId;
        std::string unitDisplayText;
        std::string scope;
        std::string source;
        bool stale{ false };

        bool operator==(const Item&) const = default;
    };

    struct PrimaryDisplayItem
    {
        std::wstring text;
        std::wstring fullText;
    };

    struct PrimaryDisplay
    {
        std::vector<PrimaryDisplayItem> items;
        bool visible{ false };
    };

    std::vector<Item> Parse(const Json::Value& usage);
    void UpdateCache(std::vector<Item>& cache, const Json::Value& usage);
    [[nodiscard]] bool TryUpdateCache(std::vector<Item>& cache, const Json::Value& usage) noexcept;
    std::vector<std::wstring> BuildPrimaryDisplayTexts(
        const std::vector<Item>& items,
        std::wstring_view tokensUnit,
        std::wstring_view contextWindowLabel = L"Context Window");
    PrimaryDisplay BuildPrimaryDisplay(
        const std::vector<Item>& items,
        std::wstring_view tokensUnit,
        bool showUsageAndCost = true,
        std::wstring_view contextWindowLabel = L"Context Window");
}
