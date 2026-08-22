// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>

#include <winrt/base.h>

namespace winrt::TerminalApp::implementation
{
    // Process-wide handoff for an agent pane moving between window UI threads.
    // ContentId preserves the live TermControl/conpty; this stash preserves the
    // wrapper identity and old tab StableId needed to rekey WTA routing.
    struct AgentPaneDragStash
    {
        struct Entry
        {
            std::wstring originalTabId;
            std::optional<winrt::guid> sourceProfileGuid;
        };

        static void Stash(uint64_t contentId,
                          const winrt::hstring& originalTabId,
                          const std::optional<winrt::guid>& sourceProfileGuid) noexcept
        {
            if (contentId == 0)
            {
                return;
            }

            std::lock_guard lock{ _Mutex() };
            _Map()[contentId] = Entry{
                std::wstring{ originalTabId },
                sourceProfileGuid,
            };
        }

        static bool Take(uint64_t contentId,
                         winrt::hstring& outOriginalTabId,
                         std::optional<winrt::guid>& outSourceProfileGuid) noexcept
        {
            if (contentId == 0)
            {
                return false;
            }

            std::lock_guard lock{ _Mutex() };
            auto& map = _Map();
            const auto it = map.find(contentId);
            if (it == map.end())
            {
                return false;
            }

            outOriginalTabId = winrt::hstring{ it->second.originalTabId };
            outSourceProfileGuid = it->second.sourceProfileGuid;
            map.erase(it);
            return true;
        }

    private:
        static std::mutex& _Mutex() noexcept
        {
            static std::mutex mutex;
            return mutex;
        }

        static std::unordered_map<uint64_t, Entry>& _Map() noexcept
        {
            static std::unordered_map<uint64_t, Entry> map;
            return map;
        }
    };
}
