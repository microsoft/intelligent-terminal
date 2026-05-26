// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <mutex>
#include <string>
#include <unordered_map>

// `winrt::hstring` lives in <winrt/base.h>. Include it explicitly so the
// header is self-contained and not dependent on PCH / include-order
// happenstance.
#include <winrt/base.h>

namespace winrt::TerminalApp::implementation
{
    // Cross-window agent-pane drag stash.
    //
    // Background: when the user drags an agent-bearing tab from one window
    // to another, WT's normal mechanism (Tab::BuildStartupActions on the
    // source side → action stream → target window's AttachContent →
    // _MakeTerminalPane reattaches by ContentId) preserves the live
    // TermControl but loses two things:
    //   1. the AgentPaneContent wrapper (the target side instantiates a
    //      plain TerminalPaneContent for ContentId reattach);
    //   2. the source tab's StableId (the target side's new Tab has a
    //      fresh GUID).
    // The wta-helper process behind the TermControl is still alive but
    // its `--owner-tab-id <stable>` argument is now stale — events from
    // WT carry the new StableId and the helper doesn't recognize them.
    //
    // To bridge the source → target windows we stash the original tab's
    // StableId keyed by the migrating ContentId. The source side populates
    // the stash from Tab::BuildStartupActions when an agent leaf pane is
    // about to be serialized for cross-window drag (Content kind). The
    // target side consumes the entry from TerminalPage::_MakeTerminalPane
    // when reattaching by ContentId, so it can:
    //   (a) re-wrap the new content in AgentPaneContent, and
    //   (b) emit a `tab_renamed` event with {old, new} StableIds so the
    //       wta-helper can rebind its `--owner-tab-id`.
    //
    // The map is mutex-guarded since the source-side populate and the
    // target-side consume run on independent UI threads (one per window).
    // Entries are removed on take(); there is no TTL — if a consume path
    // fails to run we leak one std::pair until process exit, which is
    // acceptable for a drag-drop edge case.
    //
    // This lives in TerminalApp (and not WindowEmperor in WindowsTerminal.exe)
    // because both producer and consumer (Tab.cpp + TerminalPage.cpp) link
    // against TerminalApp; TerminalApp.dll is loaded exactly once into
    // WindowsTerminal.exe, so a static inside it is process-wide.
    struct AgentPaneDragStash
    {
        static void Stash(uint64_t contentId, const winrt::hstring& originalTabId) noexcept
        {
            if (contentId == 0)
            {
                return;
            }
            std::lock_guard lock{ _Mutex() };
            _Map()[contentId] = std::wstring{ originalTabId };
        }

        static bool Take(uint64_t contentId, winrt::hstring& outOriginalTabId) noexcept
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
            outOriginalTabId = winrt::hstring{ it->second };
            map.erase(it);
            return true;
        }

    private:
        static std::mutex& _Mutex() noexcept
        {
            static std::mutex m;
            return m;
        }

        static std::unordered_map<uint64_t, std::wstring>& _Map() noexcept
        {
            static std::unordered_map<uint64_t, std::wstring> map;
            return map;
        }
    };
}
