// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CompletionProvider.h"
#include "PromptInputModel.h"

namespace winrt::TerminalApp::implementation
{
    class HistoryCompletionProvider
    {
    public:
        std::vector<CompletionProviderResult> Query(const PromptInputSnapshot& input,
                                                    std::span<const std::wstring> history) const;
    };
}
