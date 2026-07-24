// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cwctype>
#include <string>
#include <string_view>

#include <gsl/narrow>
#include <winrt/Microsoft.Terminal.Settings.Model.h>
#include <winrt/base.h>

namespace Microsoft::Terminal::CustomModels
{
    inline winrt::hstring SelectionId(const winrt::hstring& providerId, const winrt::hstring& modelId)
    {
        std::wstring value{ L"custom:" };
        value.append(providerId);
        value.push_back(L':');
        value.append(modelId);
        return winrt::hstring{ value };
    }

    inline bool TryParseSelectionId(std::wstring_view selectionId, std::wstring& providerId, std::wstring& modelId)
    {
        constexpr std::wstring_view prefix{ L"custom:" };
        if (!selectionId.starts_with(prefix))
        {
            return false;
        }

        const auto separator = selectionId.find(L':', prefix.size());
        if (separator == std::wstring_view::npos || separator == prefix.size() || separator + 1 >= selectionId.size())
        {
            return false;
        }

        providerId.assign(selectionId.substr(prefix.size(), separator - prefix.size()));
        modelId.assign(selectionId.substr(separator + 1));
        return true;
    }

    inline bool IsCustomSelection(std::wstring_view selectionId)
    {
        std::wstring providerId;
        std::wstring modelId;
        return TryParseSelectionId(selectionId, providerId, modelId);
    }

    inline winrt::hstring ResolvedLocation(
        const winrt::Microsoft::Terminal::Settings::Model::CustomModelProvider& provider)
    {
        const auto configured = provider.Location();
        if (configured == L"local" || configured == L"cloud")
        {
            return configured;
        }

        auto url = std::wstring{ provider.BaseUrl() };
        std::transform(url.begin(), url.end(), url.begin(), [](const wchar_t ch) {
            return gsl::narrow_cast<wchar_t>(std::towlower(ch));
        });
        const bool local =
            url.find(L"://localhost") != std::wstring::npos ||
            url.find(L"://127.") != std::wstring::npos ||
            url.find(L"://[::1]") != std::wstring::npos ||
            url.find(L"://0.0.0.0") != std::wstring::npos ||
            url.find(L".local") != std::wstring::npos;
        return local ? L"local" : L"cloud";
    }
}
