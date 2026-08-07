// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <string>
#include <string_view>

namespace Microsoft::Terminal::CustomModels
{
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

    inline bool IsCustomSelection(std::string_view selectionId)
    {
        constexpr std::string_view prefix{ "custom:" };
        if (!selectionId.starts_with(prefix))
        {
            return false;
        }

        const auto separator = selectionId.find(':', prefix.size());
        return separator != std::string_view::npos &&
               separator != prefix.size() &&
               separator + 1 < selectionId.size();
    }
}
