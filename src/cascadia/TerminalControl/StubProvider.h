// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - StubProvider.h
//
// Abstract:
// - A stub inline suggestion provider that returns canned suffixes
//   for common command prefixes. Used for development, testing, and
//   offline fallback.

#pragma once

#include "IInlineSuggestionProvider.h"

#include <unordered_map>
#include <chrono>
#include <thread>

namespace winrt::Microsoft::Terminal::Control::implementation
{
    class StubProvider final : public IInlineSuggestionProvider
    {
    public:
        StubProvider() = default;
        ~StubProvider() override = default;

        std::future<SuggestionResult> SuggestAsync(SuggestionRequest request) override
        {
            return std::async(std::launch::async, [req = std::move(request), this]() -> SuggestionResult {
                // Simulate network/model latency
                std::this_thread::sleep_for(_fakeDelay);

                // Find the longest matching prefix in our canned data
                std::wstring_view bestMatch;
                std::wstring_view bestSuffix;

                for (const auto& [prefix, suffix] : _cannedSuffixes)
                {
                    if (req.cursorPrefix.size() >= prefix.size() &&
                        req.cursorPrefix.compare(0, prefix.size(), prefix) == 0 &&
                        prefix.size() > bestMatch.size())
                    {
                        bestMatch = prefix;
                        bestSuffix = suffix;
                    }
                }

                if (bestSuffix.empty())
                {
                    return SuggestionResult{ SuggestionKind::None, L"", req.generationId };
                }

                // The suggestion is only the part after what's already typed
                // e.g., cursorPrefix="git sta", bestMatch="git st", bestSuffix="atus"
                // We want to return "tus" (the remaining suffix after accounting for extra typed chars)
                const auto extraTyped = req.cursorPrefix.size() - bestMatch.size();
                if (extraTyped >= bestSuffix.size())
                {
                    // User has typed past the suggestion
                    return SuggestionResult{ SuggestionKind::None, L"", req.generationId };
                }

                // Check that the extra typed chars match the beginning of the suffix
                if (bestSuffix.compare(0, extraTyped, req.cursorPrefix, bestMatch.size(), extraTyped) != 0)
                {
                    // Typed chars diverge from the suffix
                    return SuggestionResult{ SuggestionKind::None, L"", req.generationId };
                }

                std::wstring remainingSuffix{ bestSuffix.substr(extraTyped) };
                return SuggestionResult{ SuggestionKind::Suffix, std::move(remainingSuffix), req.generationId };
            });
        }

        bool IsAvailable() const noexcept override
        {
            return true;
        }

        void SetFakeDelay(std::chrono::milliseconds delay) noexcept
        {
            _fakeDelay = delay;
        }

    private:
        std::chrono::milliseconds _fakeDelay{ 50 };

        // Canned suffixes: prefix → suffix after that prefix
        static inline const std::vector<std::pair<std::wstring_view, std::wstring_view>> _cannedSuffixes = {
            { L"git status", L"" },
            { L"git st", L"atus" },
            { L"git ch", L"eckout " },
            { L"git co", L"mmit -m \"" },
            { L"git pu", L"sh origin main" },
            { L"git pull", L" --rebase" },
            { L"git lo", L"g --oneline" },
            { L"git di", L"ff --cached" },
            { L"git br", L"anch" },
            { L"cd ", L".." },
            { L"ls -", L"la" },
            { L"docker ", L"compose up -d" },
            { L"docker ps", L" --format \"table {{.Names}}\\t{{.Status}}\"" },
            { L"npm ", L"install" },
            { L"npm ru", L"n dev" },
            { L"npm run b", L"uild" },
            { L"cargo ", L"build" },
            { L"cargo t", L"est" },
            { L"cargo r", L"un" },
            { L"mkdir ", L"-p " },
            { L"curl ", L"-s " },
            { L"python ", L"-m " },
            { L"pip ", L"install " },
        };
    };
}
