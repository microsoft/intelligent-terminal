// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <chrono>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>
#include <vector>

#include <winrt/base.h>

namespace winrt::TerminalApp::implementation
{
    struct TabDragStash
    {
        struct MetadataOverride
        {
            std::wstring text;
            std::wstring tooltip;
            std::wstring accessibilityText;
            std::optional<std::chrono::steady_clock::time_point> expiresAt;
        };

        struct Entry
        {
            std::wstring stableId;
            std::optional<MetadataOverride> metadataOverride;
        };

        static void Stash(const std::vector<uint64_t>& contentIds, Entry entry)
        {
            if (contentIds.empty())
            {
                return;
            }

            std::lock_guard lock{ _Mutex() };
            auto sharedEntry = std::make_shared<Entry>(std::move(entry));
            for (const auto contentId : contentIds)
            {
                if (contentId != 0)
                {
                    _Map()[contentId] = sharedEntry;
                }
            }
        }

        static std::optional<Entry> Take(const uint64_t contentId)
        {
            if (contentId == 0)
            {
                return std::nullopt;
            }

            std::lock_guard lock{ _Mutex() };
            auto& map = _Map();
            const auto it = map.find(contentId);
            if (it == map.end())
            {
                return std::nullopt;
            }

            const auto sharedEntry = it->second;
            Entry entry = *sharedEntry;
            std::erase_if(map, [&](const auto& pair) {
                return pair.second == sharedEntry;
            });
            return entry;
        }

    private:
        static std::mutex& _Mutex() noexcept
        {
            static std::mutex mutex;
            return mutex;
        }

        static std::unordered_map<uint64_t, std::shared_ptr<Entry>>& _Map() noexcept
        {
            static std::unordered_map<uint64_t, std::shared_ptr<Entry>> map;
            return map;
        }
    };
}
