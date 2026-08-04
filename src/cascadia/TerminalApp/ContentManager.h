/*++
Copyright (c) Microsoft Corporation
Licensed under the MIT license.

Class Name:
- ContentManager.h

Abstract:
- This is a helper class for tracking all of the terminal "content" instances of
  the Terminal. These are all the ControlInteractivity & ControlCore's of each
  of our TermControls. These are each assigned a GUID on creation, and stored in
  a map for later lookup.
- This is used to enable moving panes between windows. TermControl's are not
  thread-agile, so they cannot be reused on other threads. However, the content
  is. This helper, which exists as a singleton across all the threads in the
  Terminal app, allows each thread to create content, assign it to a
  TermControl, detach it from that control, and reattach to new controls on
  other threads.
- When you want to create a new TermControl, call CreateCore to instantiate a
  new content with a GUID for later reparenting.
- Detach can be used to temporarily remove a content from its hosted
  TermControl. After detaching, you can still use LookupCore &
  TermControl::AttachContent to re-attach to the content.
- The same detached state is what powers "keep running" durable sessions: a
  detached content has no window and no TermControl, but its ConptyConnection and
  Terminal buffer are untouched, so the shell keeps running and its scrollback
  keeps filling. See doc/specs/keep-running-sessions.md.
--*/
#pragma once

#include "ContentManager.g.h"

#include <inc/cppwinrt_utils.h>
namespace winrt::TerminalApp::implementation
{
    struct ContentManager : ContentManagerT<ContentManager>
    {
    public:
        ContentManager() = default;
        Microsoft::Terminal::Control::ControlInteractivity CreateCore(const Microsoft::Terminal::Control::IControlSettings& settings,
                                                                      const Microsoft::Terminal::Control::IControlAppearance& unfocusedAppearance,
                                                                      const Microsoft::Terminal::TerminalConnection::ITerminalConnection& connection);
        Microsoft::Terminal::Control::ControlInteractivity TryLookupCore(uint64_t id);

        void Detach(const Microsoft::Terminal::Control::TermControl& control);

        void DetachForKeepRunning(const winrt::guid& groupId, const winrt::guid& sessionId, const winrt::hstring& title, const Microsoft::Terminal::Control::TermControl& control);
        uint64_t TryReattachKeptSession(const winrt::guid& sessionId);
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> KeptGroups();
        winrt::Windows::Foundation::Collections::IVectorView<uint64_t> TryReattachKeptGroup(const winrt::guid& groupId);
        void DiscardKeptGroup(const winrt::guid& groupId);
        bool HasKeptSessions() const noexcept;

        til::typed_event<winrt::TerminalApp::ContentManager, winrt::Windows::Foundation::IInspectable> KeptSessionsChanged;

    private:
        std::unordered_map<uint64_t, Microsoft::Terminal::Control::ControlInteractivity> _content;

        struct KeptSession
        {
            uint64_t contentId{ 0 };
            // The tab this pane was detached from.
            winrt::guid groupId{};
            // A detached content has no TermControl, so nothing else is left
            // watching its connection. This is how we notice a shell that exits
            // while detached.
            Microsoft::Terminal::Control::ControlCore::ConnectionStateChanged_revoker connectionStateRevoker;
        };

        // One detached tab. Its members are listed in detach order so a restore
        // rebuilds the panes in the order they appeared.
        struct KeptGroup
        {
            winrt::hstring title;
            std::vector<winrt::guid> sessionIds;
        };

        // Sessions deliberately kept alive with no window, keyed by the
        // connection's session GUID — the same id the durable session record is
        // persisted under, so a restore can find its way back without relying on
        // a ContentId that is only meaningful inside this process instance.
        std::unordered_map<winrt::guid, KeptSession> _keptSessions;
        std::unordered_map<winrt::guid, KeptGroup> _keptGroups;

        void _dropKeptSession(const winrt::guid& sessionId);
        void _reapDetachedSessionIfDead(const winrt::guid& sessionId);
        void _forgetKeptSession(uint64_t contentId);

        void _closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                            const winrt::Windows::Foundation::IInspectable& e);
    };
}
