// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - IInlineSuggestionProvider.h
//
// Abstract:
// - Interface for inline suggestion providers. Providers return suffix
//   suggestions given a cursor prefix and context.

#pragma once

#include <string>
#include <future>
#include <cstdint>

namespace winrt::Microsoft::Terminal::Control::implementation
{
    struct SuggestionRequest
    {
        std::wstring cursorPrefix;
        std::wstring cwd;
        std::wstring shell;
        uint64_t generationId;
    };

    enum class SuggestionKind
    {
        None,
        Suffix,
    };

    struct SuggestionResult
    {
        SuggestionKind kind = SuggestionKind::None;
        std::wstring suggestion;
        uint64_t generationId = 0;
    };

    struct IInlineSuggestionProvider
    {
        virtual ~IInlineSuggestionProvider() = default;

        // Returns a suggestion asynchronously. The provider should check
        // generationId on return to allow stale-result rejection.
        virtual std::future<SuggestionResult> SuggestAsync(SuggestionRequest request) = 0;

        // Whether this provider is available and functional.
        virtual bool IsAvailable() const noexcept = 0;
    };
}
