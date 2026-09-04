// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "WtaProcess.h"

#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cwctype>
#include <exception>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <ranges>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_set>

#include <json/json.h>
#include <winrt/base.h>

namespace Microsoft::Terminal::AgentAvailability
{
    using AgentIds = std::unordered_set<std::wstring>;
    using ProbeFunction = std::function<AgentIds()>;

    inline std::unordered_set<std::wstring> ParseHostAgentIds(const std::string_view payload)
    {
        Json::Value root;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        if (!reader->parse(payload.data(), payload.data() + payload.size(), &root, &errors) ||
            !root.isObject() ||
            !root["agents"].isArray())
        {
            return {};
        }

        std::unordered_set<std::wstring> ids;
        for (const auto& agent : root["agents"])
        {
            const auto& id = agent["id"];
            if (id.isString())
            {
                const auto hstringId = winrt::to_hstring(id.asString());
                ids.emplace(hstringId.c_str(), hstringId.size());
            }
        }
        return ids;
    }

    namespace details
    {
        struct RefreshAttempt
        {
            explicit RefreshAttempt(const uint64_t generation) noexcept :
                generation{ generation }
            {
            }

            uint64_t generation;
            bool completed{ false };
            std::exception_ptr failure;
        };

        struct Cache
        {
            std::mutex mutex;
            std::condition_variable refreshed;
            AgentIds ids;
            bool valid{ false };
            uint32_t waitingCallers{ 0 };
            uint64_t generation{ 0 };
            std::shared_ptr<RefreshAttempt> refresh;
            ProbeFunction probeOverride;
        };

        inline Cache& HostAgentCache()
        {
            static Cache cache;
            return cache;
        }

        inline AgentIds ProbeHostAgentIds()
        {
            const auto wtaPath = WtaProcess::ResolveWtaExePath();
            if (wtaPath.empty())
            {
                return {};
            }

            const auto output = WtaProcess::RunWtaCaptureStdout(
                wtaPath,
                L"probe-host-agents",
                2'000);
            return ParseHostAgentIds(output);
        }
    }

    // UI-safe: this only copies the latest immutable detection result. It
    // never resolves or launches wta.exe.
    inline AgentIds GetCachedHostAgentIds()
    {
        auto& cache = details::HostAgentCache();
        const std::scoped_lock lock{ cache.mutex };
        return cache.ids;
    }

    inline bool AgentIdEquals(const std::wstring_view lhs, const std::wstring_view rhs) noexcept
    {
        return lhs.size() == rhs.size() &&
               std::equal(lhs.begin(), lhs.end(), rhs.begin(), [](const wchar_t left, const wchar_t right) {
                   return std::towlower(left) == std::towlower(right);
               });
    }

    inline std::optional<std::wstring> SelectAvailableAllowedAgentId(
        const std::wstring_view selectedAgentId,
        const std::span<const std::wstring> allowedAgentIds,
        const AgentIds& availableAgentIds)
    {
        const auto isAvailable = [&](const std::wstring_view candidate) {
            return std::ranges::any_of(availableAgentIds, [&](const auto& available) {
                return AgentIdEquals(candidate, available);
            });
        };
        const auto isAllowed = [&](const std::wstring_view candidate) {
            return std::ranges::any_of(allowedAgentIds, [&](const auto& allowed) {
                return AgentIdEquals(candidate, allowed);
            });
        };

        if (!selectedAgentId.empty() && isAllowed(selectedAgentId) && isAvailable(selectedAgentId))
        {
            return std::wstring{ selectedAgentId };
        }
        for (const auto& allowed : allowedAgentIds)
        {
            if (isAvailable(allowed))
            {
                return allowed;
            }
        }
        return std::nullopt;
    }

    // Mark the snapshot stale without clearing it. Policy is still applied by
    // each caller, while the last host detection remains usable until the
    // off-thread refresh completes.
    inline void InvalidateHostAgentIds()
    {
        auto& cache = details::HostAgentCache();
        const std::scoped_lock lock{ cache.mutex };
        ++cache.generation;
        cache.valid = false;
    }

    // Blocking by design; callers must invoke this from a background thread.
    // Concurrent refreshes join the same generation. If invalidation races a
    // probe, every caller waits for a probe from the new generation.
    inline AgentIds RefreshHostAgentIds()
    {
        auto& cache = details::HostAgentCache();
        constexpr uint32_t maxInvalidatedRefreshes = 8;
        uint32_t invalidatedRefreshes = 0;

        const auto retryAfterInvalidation = [&]() {
            if (++invalidatedRefreshes >= maxInvalidatedRefreshes)
            {
                throw std::runtime_error{ "Agent availability refresh was repeatedly invalidated" };
            }
        };

        for (;;)
        {
            std::unique_lock lock{ cache.mutex };
            if (cache.valid)
            {
                return cache.ids;
            }
            if (cache.refresh && !cache.refresh->completed)
            {
                const auto refresh = cache.refresh;
                ++cache.waitingCallers;
                cache.refreshed.notify_all();
                cache.refreshed.wait(lock, [&refresh] { return refresh->completed; });
                --cache.waitingCallers;
                cache.refreshed.notify_all();

                if (refresh->generation != cache.generation)
                {
                    retryAfterInvalidation();
                    continue;
                }
                if (refresh->failure)
                {
                    std::rethrow_exception(refresh->failure);
                }
                if (cache.valid)
                {
                    return cache.ids;
                }
                throw std::runtime_error{ "Agent availability refresh completed without a result" };
            }

            const auto generation = cache.generation;
            const auto refresh = std::make_shared<details::RefreshAttempt>(generation);
            cache.refresh = refresh;
            const auto probeOverride = cache.probeOverride;
            lock.unlock();

            AgentIds ids;
            std::exception_ptr failure;
            try
            {
                ids = probeOverride ? probeOverride() : details::ProbeHostAgentIds();
            }
            catch (...)
            {
                failure = std::current_exception();
            }

            lock.lock();
            const bool current =
                cache.generation == generation &&
                cache.refresh == refresh;
            refresh->failure = failure;
            refresh->completed = true;
            if (current)
            {
                cache.valid = !failure;
                if (!failure)
                {
                    cache.ids = std::move(ids);
                }
            }
            const auto snapshot = current && !failure ? cache.ids : AgentIds{};
            lock.unlock();
            cache.refreshed.notify_all();

            if (!current)
            {
                retryAfterInvalidation();
                continue;
            }
            if (failure)
            {
                std::rethrow_exception(failure);
            }
            return snapshot;
        }
    }

    inline void SetProbeFunctionForTests(ProbeFunction probe)
    {
        auto& cache = details::HostAgentCache();
        const std::scoped_lock lock{ cache.mutex };
        cache.probeOverride = std::move(probe);
        cache.ids.clear();
        ++cache.generation;
        cache.valid = false;
    }

    inline bool WaitForRefreshCallersForTests(
        const uint32_t minimum,
        const std::chrono::milliseconds timeout)
    {
        auto& cache = details::HostAgentCache();
        std::unique_lock lock{ cache.mutex };
        return cache.refreshed.wait_for(lock, timeout, [&cache, minimum] {
            return cache.waitingCallers >= minimum;
        });
    }

    inline bool IsHostAgentCacheValidForTests()
    {
        auto& cache = details::HostAgentCache();
        const std::scoped_lock lock{ cache.mutex };
        return cache.valid;
    }

    inline void ResetCacheForTests()
    {
        auto& cache = details::HostAgentCache();
        std::unique_lock lock{ cache.mutex };
        cache.refreshed.wait(lock, [&cache] { return !cache.refresh || cache.refresh->completed; });
        cache.ids.clear();
        ++cache.generation;
        cache.valid = false;
        cache.refresh.reset();
        cache.probeOverride = {};
    }
}
