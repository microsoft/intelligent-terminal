// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cstdint>
#include <map>
#include <string>
#include <unordered_set>
#include <vector>

namespace winrt::TerminalApp::implementation
{
    struct WorkspaceLiveBinding
    {
        std::wstring workspaceTabId;
        uint64_t currentWindowId;
        uint64_t homeWindowId;
    };

    struct WorkspaceRestorePlan
    {
        bool newWindow{ false };
        uint64_t windowId{ 0 };
        std::vector<std::wstring> missingTabIds;
    };

    inline WorkspaceRestorePlan DecideWorkspaceRestore(const std::vector<std::wstring>& savedTabIds,
                                                       const std::vector<WorkspaceLiveBinding>& live)
    {
        std::map<uint64_t, size_t> anchorCounts;
        for (const auto& binding : live)
        {
            if (binding.homeWindowId != 0 && binding.currentWindowId == binding.homeWindowId)
            {
                ++anchorCounts[binding.homeWindowId];
            }
        }

        if (anchorCounts.empty())
        {
            WorkspaceRestorePlan plan;
            plan.newWindow = true;
            return plan;
        }

        const auto targetWindow = std::max_element(anchorCounts.begin(), anchorCounts.end(), [](const auto& lhs, const auto& rhs) {
            if (lhs.second == rhs.second)
            {
                return lhs.first > rhs.first;
            }
            return lhs.second < rhs.second;
        })->first;

        std::unordered_set<std::wstring> anchoredIds;
        for (const auto& binding : live)
        {
            if (binding.homeWindowId == targetWindow && binding.currentWindowId == binding.homeWindowId)
            {
                anchoredIds.insert(binding.workspaceTabId);
            }
        }

        WorkspaceRestorePlan plan;
        plan.windowId = targetWindow;
        for (const auto& savedTabId : savedTabIds)
        {
            if (!anchoredIds.contains(savedTabId))
            {
                plan.missingTabIds.push_back(savedTabId);
            }
        }
        return plan;
    }
}
