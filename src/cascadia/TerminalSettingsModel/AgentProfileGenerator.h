// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once
#include "IDynamicProfileGenerator.h"

namespace winrt::Microsoft::Terminal::Settings::Model
{
    class AgentProfileGenerator final : public IDynamicProfileGenerator
    {
    public:
        std::wstring_view GetNamespace() const noexcept override;
        std::wstring_view GetDisplayName() const noexcept override;
        std::wstring_view GetIcon() const noexcept override;
        void GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles) const override;
    };
}
