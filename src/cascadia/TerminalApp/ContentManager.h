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
#include "DetachedSessionEndedArgs.g.h"
#include "DetachedSessionInfo.g.h"
#include "KeptGroupRestoreResult.g.h"

#include <inc/cppwinrt_utils.h>

#include <functional>
#include <mutex>
#include <unordered_set>

namespace winrt::TerminalApp::implementation
{
    struct DetachedSessionInfo : DetachedSessionInfoT<DetachedSessionInfo>
    {
    public:
        DetachedSessionInfo(const winrt::guid& sessionId,
                            const winrt::guid& groupId,
                            winrt::hstring tabTitle,
                            winrt::hstring shellSessionId,
                            const uint32_t pid) :
            _sessionId{ sessionId },
            _groupId{ groupId },
            _tabTitle{ std::move(tabTitle) },
            _shellSessionId{ std::move(shellSessionId) },
            _pid{ pid }
        {
        }

        winrt::guid SessionId() const noexcept { return _sessionId; }
        winrt::guid GroupId() const noexcept { return _groupId; }
        winrt::hstring TabTitle() const noexcept { return _tabTitle; }
        winrt::hstring ShellSessionId() const noexcept { return _shellSessionId; }
        uint32_t Pid() const noexcept { return _pid; }

    private:
        winrt::guid _sessionId{};
        winrt::guid _groupId{};
        winrt::hstring _tabTitle;
        winrt::hstring _shellSessionId;
        uint32_t _pid{ 0 };
    };

    struct DetachedSessionEndedArgs : DetachedSessionEndedArgsT<DetachedSessionEndedArgs>
    {
    public:
        DetachedSessionEndedArgs(const winrt::guid& sessionId, winrt::hstring state) :
            _sessionId{ sessionId },
            _state{ std::move(state) }
        {
        }

        winrt::guid SessionId() const noexcept { return _sessionId; }
        winrt::hstring State() const noexcept { return _state; }

    private:
        winrt::guid _sessionId{};
        winrt::hstring _state{ L"closed" };
    };

    struct KeptGroupRestoreResult : KeptGroupRestoreResultT<KeptGroupRestoreResult>
    {
    public:
        KeptGroupRestoreResult(std::vector<winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs> restoreArgs,
                               winrt::hstring shellSessionId,
                               const int64_t shellSessionRevision);

        winrt::Windows::Foundation::Collections::IVectorView<uint64_t> ContentIds() const noexcept { return _contentIds; }
        winrt::Windows::Foundation::Collections::IVectorView<winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs> RestoreArgs() const noexcept { return _restoreArgs; }
        winrt::hstring ShellSessionId() const noexcept { return _shellSessionId; }
        int64_t ShellSessionRevision() const noexcept { return _shellSessionRevision; }

    private:
        winrt::Windows::Foundation::Collections::IVectorView<uint64_t> _contentIds{ nullptr };
        winrt::Windows::Foundation::Collections::IVectorView<winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs> _restoreArgs{ nullptr };
        winrt::hstring _shellSessionId;
        int64_t _shellSessionRevision{ 0 };
    };

    struct ContentManager : ContentManagerT<ContentManager>
    {
    public:
        explicit ContentManager(winrt::Windows::System::DispatcherQueue ownerDispatcher);
        ContentManager(winrt::Windows::System::DispatcherQueue ownerDispatcher,
                       std::function<bool(winrt::Windows::System::DispatcherQueueHandler)> scheduleOnOwner);
        Microsoft::Terminal::Control::ControlInteractivity CreateCore(const Microsoft::Terminal::Control::IControlSettings& settings,
                                                                      const Microsoft::Terminal::Control::IControlAppearance& unfocusedAppearance,
                                                                      const Microsoft::Terminal::TerminalConnection::ITerminalConnection& connection);
        Microsoft::Terminal::Control::ControlInteractivity TryLookupCore(uint64_t id);

        void Detach(const Microsoft::Terminal::Control::TermControl& control);

        bool DetachForKeepRunning(const winrt::guid& groupId,
                                  const winrt::guid& sessionId,
                                  const winrt::hstring& title,
                                  const winrt::hstring& shellSessionId,
                                  int64_t shellSessionRevision,
                                  const Microsoft::Terminal::Settings::Model::NewTerminalArgs& restoreArgs,
                                  const Microsoft::Terminal::Control::TermControl& control);
        uint64_t TryReattachKeptSession(const winrt::guid& sessionId);
        void CancelKeptSessionReattach(const winrt::guid& sessionId);
        bool ConfirmReattachedContent(uint64_t contentId);
        bool IsReattachPendingContent(uint64_t contentId);
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::DetachedSessionInfo> DetachedSessions();
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> KeptGroups();
        winrt::TerminalApp::KeptGroupRestoreResult BeginReattachKeptGroup(const winrt::guid& groupId);
        void CancelKeptGroupReattach(const winrt::guid& groupId);
        bool DiscardKeptSession(const winrt::guid& sessionId);
        void DiscardKeptGroup(const winrt::guid& groupId);
        bool HasKeptSessions();

        til::typed_event<winrt::TerminalApp::ContentManager, winrt::Windows::Foundation::IInspectable> KeptSessionsChanged;
        til::typed_event<winrt::TerminalApp::ContentManager, winrt::TerminalApp::DetachedSessionEndedArgs> DetachedSessionClosed;

    private:
        winrt::Windows::System::DispatcherQueue _ownerDispatcher{ nullptr };
        DWORD _ownerThreadId{ 0 };
        std::function<bool(winrt::Windows::System::DispatcherQueueHandler)> _scheduleOnOwner;

        std::mutex _pendingOwnerWorkMutex;
        std::unordered_set<winrt::guid> _pendingReaps;
        std::unordered_map<uint64_t, winrt::hstring> _pendingClosedContent;

        std::unordered_map<uint64_t, Microsoft::Terminal::Control::ControlInteractivity> _content;

        struct KeptSession
        {
            uint64_t contentId{ 0 };
            // The tab this pane was detached from.
            winrt::guid groupId{};
            // Immutable restore configuration captured at detach time.
            Microsoft::Terminal::Settings::Model::NewTerminalArgs restoreArgs{ nullptr };
            // A session remains owned by ContentManager until an attach has
            // completed. Pending sessions are hidden from duplicate restore
            // requests but keep their liveness revoker armed.
            bool reattachPending{ false };
            // A detached content has no TermControl, so nothing else is left
            // watching its connection. This is how we notice a shell that exits
            // while detached.
            Microsoft::Terminal::Control::ControlCore::ConnectionStateChanged_revoker connectionStateRevoker;
            // Armed only after DetachForKeepRunning confirms the session
            // survived immediate liveness reaping. If the session was already
            // dead by then, the ordinary tab-close path emits the one and only
            // close signal instead.
            bool raiseDetachedCloseEvent{ false };
            // Captured before content.Close() tears the connection down so an
            // already-failed detached session stays failed in the one end event
            // we forward to WindowEmperor.
            winrt::hstring detachedEndState{ L"closed" };
        };

        // One detached tab. Its members are listed in detach order so a restore
        // rebuilds the panes in the order they appeared.
        struct KeptGroup
        {
            winrt::hstring title;
            winrt::hstring shellSessionId;
            int64_t shellSessionRevision{ 0 };
            bool reattachPending{ false };
            std::vector<winrt::guid> sessionIds;
        };

        // Sessions deliberately kept alive with no window, keyed by the
        // connection's live session GUID. The detached tab's durable
        // shell_sessions.id is tracked separately on the group, because one
        // detached tab may have several live panes sharing the same durable
        // record.
        std::unordered_map<winrt::guid, KeptSession> _keptSessions;
        std::unordered_map<winrt::guid, KeptGroup> _keptGroups;

        void _invokeOnOwnerThread(const std::function<void()>& callback);
        bool _tryScheduleOnOwner(winrt::Windows::System::DispatcherQueueHandler callback) noexcept;
        bool _isOwnerThread() const noexcept;
        void _assertIsOwnerThread() const noexcept;
        void _queuePendingReap(const winrt::guid& sessionId);
        void _queuePendingClosedContent(uint64_t contentId, const winrt::hstring& state);
        void _drainPendingOwnerWork();
        Microsoft::Terminal::Control::ControlInteractivity _tryLookupCoreOnOwner(uint64_t id);

        bool _detachForKeepRunningOnOwner(const winrt::guid& groupId,
                                          const winrt::guid& sessionId,
                                          const winrt::hstring& title,
                                          const winrt::hstring& shellSessionId,
                                          int64_t shellSessionRevision,
                                          uint64_t contentId,
                                          const Microsoft::Terminal::Settings::Model::NewTerminalArgs& restoreArgs,
                                          const Microsoft::Terminal::Control::ControlCore& core);
        uint64_t _tryReattachKeptSessionOnOwner(const winrt::guid& sessionId);
        void _cancelKeptSessionReattachOnOwner(const winrt::guid& sessionId);
        bool _confirmReattachedContentOnOwner(uint64_t contentId);
        bool _isReattachPendingContentOnOwner(uint64_t contentId) const;
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::DetachedSessionInfo> _detachedSessionsOnOwner();
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> _keptGroupsOnOwner();
        winrt::TerminalApp::KeptGroupRestoreResult _beginReattachKeptGroupOnOwner(const winrt::guid& groupId);
        void _cancelKeptGroupReattachOnOwner(const winrt::guid& groupId);
        bool _discardKeptSessionOnOwner(const winrt::guid& sessionId);
        void _discardKeptGroupOnOwner(const winrt::guid& groupId);

        void _dropKeptSession(const winrt::guid& sessionId);
        void _reapDetachedSessionIfDead(const winrt::guid& sessionId);
        void _forgetKeptSession(uint64_t contentId, const winrt::hstring& fallbackDetachedEndState);
        bool _processClosedContent(uint64_t contentId, const winrt::hstring& detachedEndState);

        void _closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                            const winrt::Windows::Foundation::IInspectable& e);
    };
}
