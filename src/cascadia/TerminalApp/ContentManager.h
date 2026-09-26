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
--*/
#pragma once

#include <mutex>
#include "ContentManager.g.h"
#include "SharedWta.h"

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

        void OnPaneAgentSessionChanged(const winrt::hstring& eventJson);
        winrt::hstring AgentSessionEvent(uint64_t contentId);
        void KeepTab(const winrt::TerminalApp::TerminalPage& owner, const winrt::TerminalApp::Tab& tab);
        bool HasKeptSessions();
        bool IsKeptContent(uint64_t contentId);
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> KeptGroups();
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::TerminalPage> KeptPages();
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::Tab> KeptTabs(const winrt::TerminalApp::TerminalPage& owner);
        winrt::TerminalApp::TerminalPage KeptGroupOwner(const winrt::guid& groupId);
        winrt::TerminalApp::Tab BeginReattachKeptGroup(const winrt::guid& groupId);
        void CompleteKeptGroupReattach(const winrt::guid& groupId, bool committed);
        void DiscardKeptGroup(const winrt::guid& groupId);

        til::typed_event<winrt::TerminalApp::ContentManager, winrt::Windows::Foundation::IInspectable> KeptSessionsChanged;
        til::typed_event<winrt::TerminalApp::ContentManager, winrt::hstring> DetachedSessionEvent;

    private:
        std::mutex _mutex;
        std::unordered_map<uint64_t, Microsoft::Terminal::Control::ControlInteractivity> _content;

        struct AgentBinding
        {
            winrt::hstring agentSessionId;
            winrt::hstring eventJson;
        };
        struct KeptGroup
        {
            winrt::TerminalApp::TerminalPage owner{ nullptr };
            winrt::TerminalApp::Tab tab{ nullptr };
            bool restoring{ false };
            SharedWtaLease lease;
        };
        // All windows share this dispatcher. Retaining the tab and its event owner
        // preserves the live pane tree, helper lifetime and background protocol routing.
        winrt::Windows::System::DispatcherQueue _dispatcher{ winrt::Windows::System::DispatcherQueue::GetForCurrentThread() };
        std::unordered_map<uint64_t, AgentBinding> _agentBindings;
        std::unordered_map<winrt::guid, KeptGroup> _keptGroups;
        void _CheckThread() const;
        void _NotifyKeptSessionsChanged() noexcept;

        void _closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                            const winrt::Windows::Foundation::IInspectable& e);
    };
}
