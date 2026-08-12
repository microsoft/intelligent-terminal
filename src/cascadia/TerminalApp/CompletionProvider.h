// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

namespace winrt::TerminalApp::implementation
{
    struct CompletionProviderResult
    {
        std::wstring completionText;
        std::wstring displayText;
        std::wstring description;
        int32_t resultType{ 0 };
        uint32_t replacementIndex{ 0 };
        uint32_t replacementLength{ 0 };
        uint32_t cursorIndex{ 0 };
    };
}
