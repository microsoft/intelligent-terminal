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
        Microsoft::Terminal::Control::ControlInteractivity CreateAgentCliCore(const Microsoft::Terminal::Control::IControlSettings& settings,
                                                                              const Microsoft::Terminal::Control::IControlAppearance& unfocusedAppearance,
                                                                              const Microsoft::Terminal::TerminalConnection::ITerminalConnection& connection,
                                                                              const winrt::hstring& providerId,
                                                                              const std::string& nativeAgentLaunchLocation = {});
        winrt::hstring NativeAgentProviderId(uint64_t contentId) const;
        winrt::hstring NativeAgentProviderIdForPane(const winrt::guid& paneId) const;
        struct NativeAgentObservationIdentity
        {
            winrt::hstring providerId;
            uint64_t modeEpoch{ 0 };
            uint64_t windowId{ 0 };
            std::string tabId;
            std::string agentSessionId;
            std::string location;
            bool initialAdmission{ false };
            uint64_t receiverGeneration{ 0 };
            std::string qualifiedSessionId;
            std::string qualifiedLocation;
            uint64_t paneCreatedAtMs{ 0 };
            uint64_t paneCreationSequence{ 0 };
        };
        void SetExplicitNativeAgentProvider(uint64_t contentId, const winrt::hstring& providerId);
        bool RefineNativeAgentLaunchLocation(uint64_t contentId, const winrt::hstring& providerId, const std::string& location);
        std::optional<NativeAgentObservationIdentity> NativeAgentObservationForContent(uint64_t contentId) const;
        NativeAgentObservationIdentity AdmitNativeAgentObservation(uint64_t contentId, uint64_t windowId, const std::string& tabId, const std::string& agentSessionId, const std::string& location, bool reset, uint64_t receiverGeneration = 0);
        void RetainQualifiedNativeAgentIdentity(uint64_t contentId, uint64_t modeEpoch, const std::string& sessionId, const std::string& location);
        void RemoveNativeAgentObservation(uint64_t contentId);
        void RestartNativeAgentConnectionTracking(uint64_t contentId);
        void SetNativeAgentSessionEventsEnabled(bool enabled);
        void MarkAgentHelper(uint64_t contentId);
        bool AcceptsAgentSessionEvents(const winrt::guid& paneId) const;

        struct PaneAgentSession
        {
            winrt::hstring sessionId;
            winrt::hstring agent;
            winrt::hstring resumeCommandline;
            winrt::hstring backend;
            winrt::hstring cwd;
        };
        enum class PaneAgentState
        {
            Unknown,
            Starting,
            Running,
            Ended,
            Failed,
        };
        struct PaneSessionIdentity
        {
            winrt::hstring sessionId;
            winrt::hstring agent;
            winrt::hstring source;
            winrt::hstring distro;
            winrt::hstring universe;

            bool Matches(const PaneSessionIdentity& other) const noexcept
            {
                return sessionId == other.sessionId &&
                       (agent.empty() || other.agent.empty() || agent == other.agent) &&
                       (source.empty() || other.source.empty() ||
                        (source == other.source && distro == other.distro && universe == other.universe));
            }
        };
        struct PaneMetadata
        {
            uint64_t contentId{};
            winrt::hstring launchProvider;
            PaneAgentState state{ PaneAgentState::Unknown };
            PaneAgentSession session;
            winrt::hstring activity;
            winrt::hstring source;
            winrt::hstring wslDistro;
            winrt::hstring universe;
            bool identityQualified{};
            bool interactiveResume{};
            bool explicitNativeAgent{};
            bool resumeCommandlineTrusted{};
            bool awaitingSessionStart{};
            bool sessionStartConfirmed{};
            bool connectionClosed{};
            winrt::hstring eventJson;
            std::vector<PaneSessionIdentity> supersededSessions;

            PaneSessionIdentity Identity() const
            {
                return { session.sessionId, session.agent, source, wslDistro, universe };
            }
            bool IsSuperseded(const PaneSessionIdentity& identity) const
            {
                return std::ranges::any_of(supersededSessions, [&](const auto& previous) {
                    return previous.Matches(identity);
                });
            }

            bool HasAgent() const noexcept
            {
                return state == PaneAgentState::Starting ||
                       state == PaneAgentState::Running ||
                       state == PaneAgentState::Failed;
            }
            bool HasSession() const noexcept
            {
                return HasAgent() && !session.sessionId.empty();
            }
        };
        std::optional<PaneMetadata> MetadataForPane(const winrt::guid& paneId) const;
        void ResetPaneConnection(uint64_t contentId, const winrt::guid& paneId, const std::optional<PaneMetadata>& resumeBinding = std::nullopt);
        void BindPaneSession(const winrt::guid& paneId, const PaneAgentSession& session, const winrt::hstring& source = {}, const winrt::hstring& distro = {}, const winrt::hstring& universe = {}, bool interactiveResume = false);
        void EndPaneSession(const winrt::guid& paneId, const winrt::hstring& sessionId = {}, const winrt::hstring& agent = {}, const winrt::hstring& source = {}, const winrt::hstring& distro = {}, std::optional<winrt::hstring> universe = winrt::hstring{}, bool connectionStillRunning = false);
        void SetPaneConnectionState(const winrt::guid& paneId, bool failed);
        void UpdatePaneSessionIdentity(const winrt::guid& paneId, const PaneAgentSession& session, const winrt::hstring& source, const winrt::hstring& distro, const winrt::hstring& universe);
        til::typed_event<winrt::TerminalApp::ContentManager, winrt::guid> PaneMetadataChanged;

        void Detach(const Microsoft::Terminal::Control::TermControl& control);

        // The projected entry point is out-of-band protocol ingress. VT output
        // must use the implementation overload with command trust disabled.
        void OnPaneAgentSessionChanged(const winrt::hstring& eventJson);
        void OnPaneAgentSessionChanged(const winrt::hstring& eventJson, bool trustResumeCommandline);
        winrt::hstring AgentSessionEvent(uint64_t contentId);
        void UpdateKeepRunningTab(const winrt::TerminalApp::Tab& tab);
        bool IsPaneKeepRunning(const winrt::guid& paneId) const;
        void KeepTab(const winrt::TerminalApp::TerminalPage& owner, const winrt::TerminalApp::Tab& tab);
        bool HasKeptSessions();
        bool IsKeptContent(uint64_t contentId);
        winrt::guid KeptGroupForPane(const winrt::guid& sessionId);
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> KeptGroups();
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::TerminalPage> KeptPages();
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::Tab> KeptTabs(const winrt::TerminalApp::TerminalPage& owner);
        winrt::TerminalApp::TerminalPage KeptGroupOwner(const winrt::guid& groupId);
        winrt::Windows::Foundation::Rect KeptGroupBounds(const winrt::guid& groupId);
        winrt::TerminalApp::Tab BeginReattachKeptGroup(const winrt::guid& groupId);
        void CompleteKeptGroupReattach(const winrt::guid& groupId, bool committed);
        void NotifyPaneWindowChanged(const winrt::guid& paneId, uint64_t windowId);
        void DiscardKeptGroup(const winrt::guid& groupId);
        void DiscardAllKeptGroups();
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::IPaneContent> KeptPanes();

        til::typed_event<winrt::TerminalApp::ContentManager, winrt::Windows::Foundation::IInspectable> KeptSessionsChanged;
        til::typed_event<winrt::TerminalApp::ContentManager, winrt::Windows::Foundation::IInspectable> KeepRunningTabsChanged;
        til::typed_event<winrt::TerminalApp::ContentManager, winrt::hstring> DetachedSessionEvent;
        til::event<winrt::delegate<winrt::guid, uint64_t>> PaneWindowChanged;

    private:
        mutable std::mutex _mutex;
        struct TerminalContent
        {
            Microsoft::Terminal::Control::ControlInteractivity core;
            winrt::hstring nativeAgentProviderId;
            uint64_t paneCreatedAtMs;
            uint64_t paneCreationSequence;
            std::string nativeAgentLaunchLocation;
            NativeAgentObservationIdentity observation;
            bool observationAdmitted{ false };
            bool agentHelper{ false };
        };
        std::unordered_map<uint64_t, TerminalContent> _content;
        std::unordered_map<winrt::guid, PaneMetadata> _paneMetadata;
        bool _nativeAgentSessionEventsEnabled{ true };
        uint64_t _nativeAgentObservationGeneration{ 0 };
        uint64_t _nativeAgentPaneCreationSequence{ 0 };

        struct KeptGroup
        {
            winrt::TerminalApp::TerminalPage owner{ nullptr };
            winrt::TerminalApp::Tab tab{ nullptr };
            winrt::Windows::Foundation::Rect bounds{};
            bool restoring{ false };
            SharedWtaLease lease;
        };
        // All windows share this dispatcher. Retaining the tab and its event owner
        // preserves the live pane tree, helper lifetime and background protocol routing.
        winrt::Windows::System::DispatcherQueue _dispatcher{ winrt::Windows::System::DispatcherQueue::GetForCurrentThread() };
        std::unordered_map<winrt::guid, KeptGroup> _keptGroups;
        // Only kept groups own retention; the sidebar index must not keep tabs alive.
        std::unordered_map<winrt::hstring, winrt::weak_ref<winrt::TerminalApp::Tab>> _keepRunningTabs;
        void _CheckThread() const;
        void _NotifyKeptSessionsChanged() noexcept;

        void _closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                            const winrt::Windows::Foundation::IInspectable& e);
    };
}
