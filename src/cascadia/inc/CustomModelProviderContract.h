// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string_view>

namespace Microsoft::Terminal::CustomModels
{
    // This identifies the shared OpenAI-compatible Chat Completions provider
    // shape consumed by supported agents.
    inline constexpr std::wstring_view CanonicalApiContract{ L"openai-compatible" };
    inline constexpr std::string_view CanonicalApiContractUtf8{ "openai-compatible" };

    inline constexpr bool IsContractWhitespace(const wchar_t value) noexcept
    {
        return value == L' ' || value == L'\t' || value == L'\r' || value == L'\n';
    }

    inline constexpr std::wstring_view NormalizeApiContract(const std::wstring_view value) noexcept
    {
        for (const auto ch : value)
        {
            if (!IsContractWhitespace(ch))
            {
                return value;
            }
        }
        return CanonicalApiContract;
    }

    inline constexpr bool IsSupportedApiContract(const std::wstring_view value) noexcept
    {
        return value == CanonicalApiContract;
    }

    inline constexpr bool IsSupportedApiContract(const std::string_view value) noexcept
    {
        return value == CanonicalApiContractUtf8;
    }
}
