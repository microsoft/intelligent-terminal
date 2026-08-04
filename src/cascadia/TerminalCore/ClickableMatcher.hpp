// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstddef>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::Core
{
    enum class ClickAction
    {
        OpenUri,
        OpenFile,
    };

    enum class ClickResolveMode
    {
        Detect,
        Activate,
    };

    struct ClickableMatcher
    {
        size_t id;
        std::wstring_view pattern;
        ClickAction action;
    };

    struct ClickableMatch
    {
        size_t matcherId;
        ClickAction action;
        std::wstring target;
    };
}
