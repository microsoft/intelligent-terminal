// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <atomic>
#include <cstdint>
#include <chrono>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <unordered_map>

#include <winrt/base.h>
#include "SharedWta.h"

namespace winrt::TerminalApp::implementation
{
    // Process-wide handoff for an agent pane moving between window UI threads.
    // ContentId preserves the live TermControl/conpty; this stash preserves the
    // wrapper identity and old tab StableId needed to rekey WTA routing.
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
            AttachDisposition attachDisposition;
            std::shared_ptr<details::SharedWtaPaneReferenceToken> paneReference;
            uint64_t generation;
        };

        static void Stash(uint64_t contentId,
                          const winrt::hstring& originalTabId,
                          const std::optional<winrt::guid>& sourceProfileGuid,
                          AttachDisposition attachDisposition,
                          std::shared_ptr<details::SharedWtaPaneReferenceToken> paneReference) noexcept
        {
            if (contentId == 0 || !paneReference)
            {
                return;
            }

            const auto generation = _NextGeneration().fetch_add(1, std::memory_order_relaxed) + 1;
            std::shared_ptr<details::SharedWtaPaneReferenceToken> replacedReference;
            bool releaseReplacedReference = false;
            {
                std::lock_guard lock{ _Mutex() };
                auto& entry = _Map()[contentId];
                replacedReference = std::move(entry.paneReference);
                releaseReplacedReference = replacedReference && replacedReference != paneReference;
                entry = Entry{
                    std::wstring{ originalTabId },
                    sourceProfileGuid,
                    attachDisposition,
                    std::move(paneReference),
                    generation,
                };
            }
            if (releaseReplacedReference)
            {
                _Release(std::move(replacedReference));
            }
            _Expire(contentId, generation);
        }

        static bool Take(uint64_t contentId,
                         winrt::hstring& outOriginalTabId,
                         std::optional<winrt::guid>& outSourceProfileGuid,
                         AttachDisposition& outAttachDisposition,
                         std::shared_ptr<details::SharedWtaPaneReferenceToken>& outPaneReference) noexcept
        {
            outPaneReference.reset();
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
            outAttachDisposition = it->second.attachDisposition;
            outPaneReference = std::move(it->second.paneReference);
            map.erase(it);
            return true;
        }

        static bool Discard(uint64_t contentId) noexcept
        {
            std::shared_ptr<details::SharedWtaPaneReferenceToken> paneReference;
            {
                std::lock_guard lock{ _Mutex() };
                auto& map = _Map();
                const auto it = map.find(contentId);
                if (it == map.end())
                {
                    return false;
                }
                paneReference = std::move(it->second.paneReference);
                map.erase(it);
            }
            _Release(std::move(paneReference));
            return true;
        }

    private:
        static void _Release(std::shared_ptr<details::SharedWtaPaneReferenceToken> paneReference) noexcept
        {
            if (paneReference && paneReference->ClaimRelease())
            {
                SharedWta::ReleasePaneAfterSessionClose();
            }
        }

        static winrt::fire_and_forget _Expire(const uint64_t contentId, const uint64_t generation) noexcept
        {
            try
            {
                co_await winrt::resume_after(std::chrono::minutes{ 1 });
                std::shared_ptr<details::SharedWtaPaneReferenceToken> paneReference;
                {
                    std::lock_guard lock{ _Mutex() };
                    auto& map = _Map();
                    const auto it = map.find(contentId);
                    if (it == map.end() || it->second.generation != generation)
                    {
                        co_return;
                    }
                    paneReference = std::move(it->second.paneReference);
                    map.erase(it);
                }
                _Release(std::move(paneReference));
            }
            CATCH_LOG();
        }

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

        static std::atomic_uint64_t& _NextGeneration() noexcept
        {
            static std::atomic_uint64_t generation{ 0 };
            return generation;
        }
    };
}
