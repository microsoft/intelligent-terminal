// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::ProgramStatusPresentation
{
    enum class Category
    {
        None,
        Idle,
        Done,
        Working,
        Attention,
        Error,
        Blocked,
        InputRequest,
    };

    struct PaneStatus
    {
        bool observed{};
        uint32_t working{};
        uint32_t blocked{};
        uint32_t unseenDone{};
        uint32_t unseenError{};
        bool canonicalInput{};
        bool canonicalAttention{};
        bool connectionError{};
        std::optional<uint32_t> progress;
        bool canonicalWorking{};
        std::string taskState;
        std::string blockKind;
        std::wstring title;
        std::wstring message;
        std::wstring inputMessage;

        bool operator==(const PaneStatus&) const = default;
    };

    struct Summary
    {
        Category category{ Category::None };
        uint32_t workingPanes{};
        uint32_t blockedPanes{};
        uint32_t completedPanes{};
        uint32_t errorPanes{};
        std::optional<uint32_t> progress;
        std::string blockKind;
        std::wstring description;
    };

    struct AttentionTarget
    {
        uint32_t contentId{};
        Category category{};
        bool unseenDone{};
        bool unseenError{};
    };

    inline std::wstring DisplayText(const std::wstring_view text)
    {
        std::wstring result;
        result.reserve(text.size());
        for (const auto ch : text)
        {
            // Remove bidi controls, not language-shaping joiners or natural RTL text.
            if (ch >= 0x20 && !(ch >= 0x7f && ch <= 0x9f) && ch != 0x2028 && ch != 0x2029 &&
                ch != 0x061c && ch != 0x200e && ch != 0x200f &&
                !(ch >= 0x202a && ch <= 0x202e) && !(ch >= 0x2066 && ch <= 0x2069))
            {
                result.push_back(ch);
            }
        }
        return result;
    }

    inline std::optional<uint32_t> NextAttention(const std::span<const AttentionTarget> panes, const std::optional<uint32_t> currentContentId)
    {
        const auto attentionCategory = [](const AttentionTarget& pane) {
            return pane.category == Category::Working && pane.unseenError ? Category::Error :
                   pane.category == Category::Working && pane.unseenDone  ? Category::Done :
                                                                            pane.category;
        };
        auto priority = Category::None;
        size_t begin = 0;
        for (size_t i = 0; i < panes.size(); ++i)
        {
            const auto category = attentionCategory(panes[i]);
            if ((category == Category::Done || category >= Category::Attention) && category > priority)
            {
                priority = category;
            }
            if (currentContentId == panes[i].contentId)
            {
                begin = i + 1;
            }
        }
        if (priority != Category::None)
        {
            for (size_t offset = 0; offset < panes.size(); ++offset)
            {
                const auto& pane = panes[(begin + offset) % panes.size()];
                if (attentionCategory(pane) == priority)
                {
                    return pane.contentId;
                }
            }
        }
        return std::nullopt;
    }

    inline Summary Reduce(const std::span<const PaneStatus> panes)
    {
        Summary result;
        bool observed = false;
        bool input = false;
        bool attention = false;
        bool connectionError = false;
        bool unreportedWorking = false;
        uint32_t workingRecords = 0;
        for (const auto& pane : panes)
        {
            observed = observed || pane.observed;
            input = input || pane.canonicalInput;
            attention = attention || pane.canonicalAttention;
            connectionError = connectionError || pane.connectionError;
            unreportedWorking = unreportedWorking || (pane.canonicalWorking && pane.working == 0);
            result.workingPanes += pane.working != 0 || pane.canonicalWorking;
            result.blockedPanes += pane.blocked != 0;
            result.completedPanes += pane.unseenDone != 0;
            result.errorPanes += pane.unseenError != 0 || pane.connectionError;
            workingRecords += pane.working;
            if (pane.working == 1 && pane.progress && *pane.progress <= 100)
            {
                result.progress = pane.progress;
            }
        }
        if (workingRecords != 1 || unreportedWorking || result.blockedPanes != 0 || input || attention || connectionError || result.errorPanes != 0)
        {
            result.progress.reset();
        }
        result.category = input                 ? Category::InputRequest :
                          connectionError       ? Category::Error :
                          result.blockedPanes   ? Category::Blocked :
                          attention             ? Category::Attention :
                          result.workingPanes   ? Category::Working :
                          result.errorPanes     ? Category::Error :
                          result.completedPanes ? Category::Done :
                          observed              ? Category::Idle :
                                                  Category::None;
        for (const auto& pane : panes)
        {
            const auto request = result.category == Category::InputRequest && pane.canonicalInput;
            const auto genericAttention = result.category == Category::Attention && pane.canonicalAttention;
            if (request || genericAttention)
            {
                result.description = DisplayText(pane.inputMessage);
                break;
            }
            const auto matches = (result.category == Category::Blocked && pane.taskState == "blocked") ||
                                 (result.category == Category::Working && pane.taskState == "working") ||
                                 (result.category == Category::Error && !connectionError && pane.taskState == "error") ||
                                 (result.category == Category::Done && pane.taskState == "done") ||
                                 (result.category == Category::Idle && pane.taskState == "idle");
            if (matches)
            {
                result.blockKind = pane.blockKind;
                result.description = DisplayText(pane.message.empty() ? pane.title : pane.message);
                break;
            }
        }
        return result;
    }

    inline std::wstring FormatText(const Summary& summary, const std::wstring_view label, const std::wstring_view progress = {})
    {
        std::wstring text{ label };
        if (!progress.empty())
        {
            text.append(L" ");
            text.append(progress);
        }
        if (!summary.description.empty())
        {
            text.append(L" \u00b7 \u2068");
            text.append(summary.description);
            text.append(L"\u2069");
        }
        return text;
    }

    inline std::string_view StatusId(const Category category) noexcept
    {
        return category == Category::InputRequest || category == Category::Blocked || category == Category::Attention ? "Attention" :
               category == Category::Error                                                                            ? "Error" :
               category == Category::Working                                                                          ? "Working" :
                                                                                                                        "Idle";
    }
}
