// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "HistoryCompletionProvider.h"

namespace winrt::TerminalApp::implementation
{
    namespace
    {
        constexpr size_t MaxResults = 20;

        std::wstring _lowercase(std::wstring_view value)
        {
            std::wstring result{ value };
            std::transform(result.begin(), result.end(), result.begin(), towlower);
            return result;
        }
    }

    std::vector<CompletionProviderResult> HistoryCompletionProvider::Query(const PromptInputSnapshot& input,
                                                                           const std::span<const std::wstring> history) const
    {
        std::vector<CompletionProviderResult> results;
        if (!input.trusted || input.cursor != input.text.size())
        {
            return results;
        }

        struct RankedCommand
        {
            std::wstring command;
            size_t frequency{ 0 };
            size_t lastIndex{ 0 };
        };

        const auto prefix = _lowercase(input.text);
        std::unordered_map<std::wstring, RankedCommand> ranked;
        for (size_t index = 0; index < history.size(); ++index)
        {
            const auto& command = history[index];
            if (command.empty() || command == input.text)
            {
                continue;
            }

            const auto key = _lowercase(command);
            if (!key.starts_with(prefix))
            {
                continue;
            }

            auto& entry = ranked[key];
            entry.command = command;
            ++entry.frequency;
            entry.lastIndex = index;
        }

        std::vector<RankedCommand> matches;
        matches.reserve(ranked.size());
        for (auto& [_, entry] : ranked)
        {
            matches.emplace_back(std::move(entry));
        }
        std::sort(matches.begin(), matches.end(), [](const auto& lhs, const auto& rhs) {
            constexpr size_t FrequencyWeight = 4;
            const auto lhsScore = lhs.lastIndex + lhs.frequency * FrequencyWeight;
            const auto rhsScore = rhs.lastIndex + rhs.frequency * FrequencyWeight;
            if (lhsScore != rhsScore)
            {
                return lhsScore > rhsScore;
            }
            if (lhs.frequency != rhs.frequency)
            {
                return lhs.frequency > rhs.frequency;
            }
            if (lhs.lastIndex != rhs.lastIndex)
            {
                return lhs.lastIndex > rhs.lastIndex;
            }
            return _wcsicmp(lhs.command.c_str(), rhs.command.c_str()) < 0;
        });

        const auto resultCount = std::min(matches.size(), MaxResults);
        results.reserve(resultCount);
        for (size_t index = 0; index < resultCount; ++index)
        {
            auto& match = matches[index];
            results.push_back(CompletionProviderResult{
                .completionText = match.command,
                .displayText = std::move(match.command),
                .resultType = 1,
                .replacementIndex = 0,
                .replacementLength = gsl::narrow_cast<uint32_t>(input.text.size()),
                .cursorIndex = gsl::narrow_cast<uint32_t>(input.cursor),
            });
        }
        return results;
    }
}
