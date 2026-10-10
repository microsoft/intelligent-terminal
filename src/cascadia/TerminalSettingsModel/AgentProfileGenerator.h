// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once
#include "IDynamicProfileGenerator.h"
#include <chrono>
#include <filesystem>
#include <functional>
#include <map>
#include <optional>

namespace winrt::Microsoft::Terminal::Settings::Model
{
    class AgentProfileGenerator final : public IDynamicProfileGenerator
    {
    public:
        class DiscoveryCache
        {
        public:
            using Snapshot = std::map<std::wstring, std::filesystem::path>;
            const Snapshot& Get(std::chrono::steady_clock::time_point now, const std::function<Snapshot()>& probe);

        private:
            std::optional<Snapshot> _snapshot;
            std::chrono::steady_clock::time_point _lastProbe{};
        };

        std::wstring_view GetNamespace() const noexcept override;
        std::wstring_view GetDisplayName() const noexcept override;
        std::wstring_view GetIcon() const noexcept override;
        void GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles) const override;
    };
}
