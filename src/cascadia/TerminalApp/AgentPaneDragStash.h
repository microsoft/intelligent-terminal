// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>

#include <winrt/base.h>
#include "AgentPaneLifetime.h"
#include "AgentPaneLog.h"

namespace winrt::TerminalApp::implementation
{
    // A claim removes the entry atomically. The claimant owns cleanup until it
    // moves the lifetime into destination content; expiry can no longer touch it.
    struct AgentPaneDragStash
    {
        enum class AttachDisposition
        {
            FirstPaneOfNewTab,
            ExistingTabSplit,
        };

        struct Entry
        {
            std::wstring originalTabId;
            std::optional<winrt::guid> sourceProfileGuid;
            AttachDisposition attachDisposition{ AttachDisposition::ExistingTabSplit };
            bool hidden{ false };
            bool sessionsView{ false };
            std::wstring panePosition;
            AgentPaneLifetime lifetime;
            uint64_t transferId{ 0 };
        };

        static AgentPaneDragStash& Instance()
        {
            // Timers may outlive the last window's UI thread.
            static auto* const instance = new AgentPaneDragStash;
            return *instance;
        }

        uint64_t Store(uint64_t contentId, Entry entry)
        {
            THROW_HR_IF(E_INVALIDARG, contentId == 0 || entry.transferId == 0);
            std::lock_guard lock{ _mutex };
            THROW_HR_IF(E_ILLEGAL_METHOD_CALL, _entries.contains(contentId));
            const auto transferId = entry.transferId;
            _entries.emplace(contentId, std::move(entry));
            return transferId;
        }

        std::optional<Entry> Take(uint64_t contentId, uint64_t expectedTransferId)
        {
            std::lock_guard lock{ _mutex };
            const auto it = _entries.find(contentId);
            if (it == _entries.end() ||
                it->second.transferId != expectedTransferId)
            {
                return std::nullopt;
            }

            auto entry = std::move(it->second);
            _entries.erase(it);
            return entry;
        }

        static winrt::fire_and_forget ExpireAfterTimeout(uint64_t contentId, uint64_t transferId)
        {
            try
            {
                co_await winrt::resume_after(std::chrono::minutes{ 2 });
            }
            CATCH_LOG()
            if (auto abandoned = Instance().Take(contentId, transferId))
            {
                _agentPaneLog("abandoning unclaimed agent pane transfer content=" + std::to_string(contentId));
                // Destruction retires the helper/core off the window UI thread.
            }
        }

    private:
        std::mutex _mutex;
        std::unordered_map<uint64_t, Entry> _entries;
    };
}
