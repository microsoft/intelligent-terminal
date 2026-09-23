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

        bool CanKeepRunning(uint64_t contentId);
        bool IsKeepRunning(uint64_t contentId);
        void SetKeepRunning(uint64_t contentId, bool enabled);
        void OnPaneAgentSessionChanged(const winrt::hstring& eventJson);
        winrt::hstring AgentSessionEvent(uint64_t contentId);
        bool DetachForKeepRunning(const winrt::guid& groupId, const winrt::hstring& title, const Microsoft::Terminal::Settings::Model::NewTerminalArgs& args, const Microsoft::Terminal::Control::TermControl& control);
        bool HasKeptSessions();
        bool IsKeptContent(uint64_t contentId);
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> KeptGroups();
        winrt::Windows::Foundation::Collections::IVectorView<Microsoft::Terminal::Settings::Model::NewTerminalArgs> BeginReattachKeptGroup(const winrt::guid& groupId);
        void CompleteKeptGroupReattach(const winrt::guid& groupId, bool committed);
        void DiscardKeptGroup(const winrt::guid& groupId);

        til::typed_event<winrt::TerminalApp::ContentManager, winrt::Windows::Foundation::IInspectable> KeptSessionsChanged;
        til::typed_event<winrt::TerminalApp::ContentManager, winrt::hstring> DetachedSessionEvent;

    private:
        std::mutex _mutex;
        std::unordered_map<uint64_t, Microsoft::Terminal::Control::ControlInteractivity> _content;

        // Runtime policy follows the content through tab/pane moves, not its XAML control.
        struct PanePolicy
        {
            winrt::hstring agentSessionId;
            winrt::hstring eventJson;
            bool keepRunning{ false };
        };
        struct KeptPane
        {
            uint64_t contentId{};
            winrt::guid sessionId{};
            Microsoft::Terminal::Settings::Model::NewTerminalArgs args{ nullptr };
            Microsoft::Terminal::Control::ControlCore::ConnectionStateChanged_revoker stateChanged;
            Microsoft::Terminal::Control::ControlCore::VtSequenceReceived_revoker vtSequence;
        };
        struct KeptGroup
        {
            winrt::hstring title;
            bool restoring{ false };
            std::vector<KeptPane> panes;
            SharedWtaLease lease;
        };
        // Policy and detached metadata belong to the process-lifetime UI dispatcher.
        // Connection callbacks only enqueue work; they never destroy XAML-bound args.
        winrt::Windows::System::DispatcherQueue _dispatcher{ winrt::Windows::System::DispatcherQueue::GetForCurrentThread() };
        std::unordered_map<uint64_t, PanePolicy> _panePolicies;
        std::unordered_map<winrt::guid, KeptGroup> _keptGroups;
        void _CheckThread() const;
        void _QueueReap();
        void _ReapClosedSessions();
        void _CloseKeptPane(KeptPane pane);
        void _NotifyKeptSessionsChanged() noexcept;

        void _closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                            const winrt::Windows::Foundation::IInspectable& e);
    };
}
