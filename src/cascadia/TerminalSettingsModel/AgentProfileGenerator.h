// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once
#include "IDynamicProfileGenerator.h"
#include "../inc/AgentAvailability.h"
#include <chrono>
#include <filesystem>
#include <functional>
#include <map>
#include <mutex>
#include <optional>

namespace winrt::Microsoft::Terminal::Settings::Model
{
    class AgentProfileGenerator final : public IDynamicProfileGenerator
    {
    public:
        template<typename T>
        class BasicDiscoveryCache
        {
        public:
            using Snapshot = T;
            const Snapshot* Previous() const noexcept { return _snapshot ? &*_snapshot : nullptr; }
            bool NeedsRefresh(const std::chrono::steady_clock::time_point now) const noexcept
            {
                return !_snapshot || now - _lastProbe >= std::chrono::seconds{ 30 };
            }
            void Store(const std::chrono::steady_clock::time_point now, Snapshot snapshot)
            {
                _snapshot = std::move(snapshot);
                _lastProbe = now;
            }
            const Snapshot& Get(const std::chrono::steady_clock::time_point now, const std::function<Snapshot()>& probe)
            {
                if (NeedsRefresh(now))
                {
                    try
                    {
                        Store(now, probe());
                    }
                    catch (...)
                    {
                        LOG_CAUGHT_EXCEPTION_MSG("Native agent profile discovery failed; retaining the last successful snapshot");
                        if (!_snapshot)
                        {
                            throw;
                        }
                    }
                }
                return *_snapshot;
            }

        private:
            std::optional<Snapshot> _snapshot;
            std::chrono::steady_clock::time_point _lastProbe{};
        };

        using DiscoveryCache = BasicDiscoveryCache<std::map<std::wstring, std::filesystem::path>>;
        using WslSnapshot = std::vector<::Microsoft::Terminal::AgentAvailability::AgentSourceSnapshot>;

        class WslSourceCache
        {
        public:
            using Snapshot = WslSnapshot;
            Snapshot Read();
            void Update(Snapshot sources);
            void RestoreSavedProfiles(const std::vector<winrt::com_ptr<implementation::Profile>>& profiles);

        private:
            std::mutex _mutex;
            Snapshot _sources;
            bool _discovered{ false };
        };

        static void UpdateWslSources(WslSnapshot sources);
        static void RestoreSavedWslProfiles(const std::vector<winrt::com_ptr<implementation::Profile>>& profiles);
        static void RetainKnownWslAgents(WslSnapshot& sources, const WslSnapshot* previous);
        std::wstring_view GetNamespace() const noexcept override;
        std::wstring_view GetDisplayName() const noexcept override;
        std::wstring_view GetIcon() const noexcept override;
        void GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles) const override;
        static void GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles,
                                     const DiscoveryCache::Snapshot& host,
                                     const WslSnapshot& wsl);
    };
}
