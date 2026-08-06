// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ContentManager.h"
#include "ContentManager.g.cpp"

#include <wil/token_helpers.h>

#include "../../types/inc/utils.hpp"

using namespace winrt::Windows::ApplicationModel;
using namespace winrt::Windows::ApplicationModel::DataTransfer;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Windows::System;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::TerminalApp::implementation
{
    ContentManager::ContentManager(winrt::Windows::System::DispatcherQueue ownerDispatcher) :
        _ownerDispatcher{ std::move(ownerDispatcher) },
        _ownerThreadId{ GetCurrentThreadId() }
    {
        THROW_HR_IF(E_INVALIDARG, !_ownerDispatcher);
        _scheduleOnOwner = [dispatcher = _ownerDispatcher](winrt::Windows::System::DispatcherQueueHandler callback) {
            return dispatcher.TryEnqueue(std::move(callback));
        };
    }

    ContentManager::ContentManager(winrt::Windows::System::DispatcherQueue ownerDispatcher,
                                   std::function<bool(winrt::Windows::System::DispatcherQueueHandler)> scheduleOnOwner) :
        _ownerDispatcher{ std::move(ownerDispatcher) },
        _ownerThreadId{ GetCurrentThreadId() },
        _scheduleOnOwner{ std::move(scheduleOnOwner) }
    {
        THROW_HR_IF(E_INVALIDARG, !_ownerDispatcher || !_scheduleOnOwner);
    }

    static ITerminalConnection _getDetachedSessionConnection(const ControlInteractivity& content)
    {
        const auto core{ content ? content.Core() : nullptr };
        return core ? core.Connection() : nullptr;
    }

    static bool _isLiveDetachedSession(const ControlInteractivity& content)
    {
        const auto connection{ _getDetachedSessionConnection(content) };
        return connection && connection.State() < ConnectionState::Closed;
    }

    static winrt::hstring _getDetachedSessionEndedState(const ControlInteractivity& content)
    {
        const auto connection{ _getDetachedSessionConnection(content) };
        return connection && connection.State() == ConnectionState::Failed ? L"failed" : L"closed";
    }

    static winrt::TerminalApp::DetachedSessionEndedArgs _makeDetachedSessionEndedArgs(const winrt::guid& sessionId,
                                                                                       const winrt::hstring& state)
    {
        return winrt::make<winrt::TerminalApp::implementation::DetachedSessionEndedArgs>(sessionId, state);
    }

    static uint32_t _getDetachedSessionPid(const ControlInteractivity& content)
    {
        const auto connection{ _getDetachedSessionConnection(content) };
        if (const auto conpty{ connection.try_as<ConptyConnection>() })
        {
            if (const auto handle = reinterpret_cast<HANDLE>(conpty.RootProcessHandle()))
            {
                return static_cast<uint32_t>(GetProcessId(handle));
            }
        }

        return 0;
    }

    ControlInteractivity ContentManager::CreateCore(const Microsoft::Terminal::Control::IControlSettings& settings,
                                                    const IControlAppearance& unfocusedAppearance,
                                                    const TerminalConnection::ITerminalConnection& connection)
    {
        ControlInteractivity content{ settings, unfocusedAppearance, connection };
        content.Closed({ get_weak(), &ContentManager::_closedHandler });

        _invokeOnOwnerThread([&]() {
            _content.emplace(content.Id(), content);
            _drainPendingOwnerWork();
        });

        return content;
    }

    ControlInteractivity ContentManager::TryLookupCore(uint64_t id)
    {
        ControlInteractivity content{ nullptr };
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            const auto it = _content.find(id);
            if (it != _content.end())
            {
                content = it->second;
            }
        });
        return content;
    }

    void ContentManager::Detach(const Microsoft::Terminal::Control::TermControl& control)
    {
        const auto contentId{ control.ContentId() };
        if (const auto& content{ TryLookupCore(contentId) })
        {
            control.Detach();
        }
    }

    bool ContentManager::_isOwnerThread() const noexcept
    {
        return GetCurrentThreadId() == _ownerThreadId;
    }

    void ContentManager::_assertIsOwnerThread() const noexcept
    {
        WI_ASSERT_MSG(_isOwnerThread(), "Keep-running sessions must be accessed on the ContentManager owner thread");
    }

    bool ContentManager::_tryScheduleOnOwner(winrt::Windows::System::DispatcherQueueHandler callback) noexcept
    {
        try
        {
            return _scheduleOnOwner && _scheduleOnOwner(std::move(callback));
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            return false;
        }
    }

    void ContentManager::_invokeOnOwnerThread(const std::function<void()>& callback)
    {
        if (_isOwnerThread())
        {
            callback();
            return;
        }

        wil::unique_event completed{ wil::EventOptions::ManualReset };
        std::exception_ptr exception;
        const auto scheduled = _tryScheduleOnOwner([&]() {
            const auto signal = wil::scope_exit([&]() noexcept {
                completed.SetEvent();
            });
            try
            {
                callback();
            }
            catch (...)
            {
                exception = std::current_exception();
            }
        });
        THROW_HR_IF(E_ABORT, !scheduled);

        completed.wait();
        if (exception)
        {
            std::rethrow_exception(exception);
        }
    }

    void ContentManager::_queuePendingReap(const winrt::guid& sessionId)
    {
        std::scoped_lock lock{ _pendingOwnerWorkMutex };
        _pendingReaps.emplace(sessionId);
    }

    void ContentManager::_queuePendingClosedContent(const uint64_t contentId, const winrt::hstring& state)
    {
        std::scoped_lock lock{ _pendingOwnerWorkMutex };
        auto& pendingState = _pendingClosedContent[contentId];
        if (pendingState.empty() || state == L"failed")
        {
            pendingState = state;
        }
    }

    void ContentManager::_drainPendingOwnerWork()
    {
        _assertIsOwnerThread();

        std::unordered_set<winrt::guid> pendingReaps;
        std::unordered_map<uint64_t, winrt::hstring> pendingClosedContent;
        {
            std::scoped_lock lock{ _pendingOwnerWorkMutex };
            pendingReaps.swap(_pendingReaps);
            pendingClosedContent.swap(_pendingClosedContent);
        }

        // A Closed callback may have already removed the content. Apply its
        // captured terminal state before a pending reap observes that absence.
        for (const auto& [contentId, state] : pendingClosedContent)
        {
            _processClosedContent(contentId, state);
        }
        for (const auto& sessionId : pendingReaps)
        {
            _reapDetachedSessionIfDead(sessionId);
        }
    }

    // Detaches `control` and remembers its content as a session that must stay
    // alive with no window, as part of the tab identified by `groupId`. The
    // content keeps its ConptyConnection — and therefore its output thread and
    // its Terminal buffer — so the shell keeps running and its scrollback keeps
    // filling while nothing is attached. Returns true only if the detached
    // session is still retained after immediate liveness reaping.
    bool ContentManager::DetachForKeepRunning(const winrt::guid& groupId,
                                              const winrt::guid& sessionId,
                                              const winrt::hstring& title,
                                              const winrt::hstring& shellSessionId,
                                              const int64_t shellSessionRevision,
                                              const Microsoft::Terminal::Control::TermControl& control)
    {
        if (!control || sessionId == winrt::guid{} || groupId == winrt::guid{})
        {
            return false;
        }

        const auto contentId{ control.ContentId() };
        const auto content{ TryLookupCore(contentId) };
        if (!content)
        {
            return false;
        }

        const auto core{ content.Core() };
        control.Detach();

        auto retained = false;
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            retained = _detachForKeepRunningOnOwner(groupId, sessionId, title, shellSessionId, shellSessionRevision, contentId, core);
        });
        return retained;
    }

    bool ContentManager::_detachForKeepRunningOnOwner(const winrt::guid& groupId,
                                                      const winrt::guid& sessionId,
                                                      const winrt::hstring& title,
                                                      const winrt::hstring& shellSessionId,
                                                      const int64_t shellSessionRevision,
                                                      const uint64_t contentId,
                                                      const Microsoft::Terminal::Control::ControlCore& core)
    {
        _assertIsOwnerThread();

        KeptSession kept;
        kept.contentId = contentId;
        kept.groupId = groupId;

        // Detaching severed the only thing that was watching this connection —
        // TermControl::Detach() clears its revokers. Without our own watch, a
        // shell that exits while detached would go unnoticed forever: the entry
        // would linger, and the process would never find a reason to quit.
        //
        // The connection raises StateChanged from its output thread, so hop
        // back to the UI thread before touching our maps or tearing anything
        // down.
        if (core)
        {
            kept.connectionStateRevoker = core.ConnectionStateChanged(
                winrt::auto_revoke,
                [weakThis = get_weak(), sessionId](auto&&, auto&&) {
                    if (const auto self{ weakThis.get() })
                    {
                        const auto scheduled = self->_tryScheduleOnOwner([weakThis, sessionId]() {
                            if (const auto owner{ weakThis.get() })
                            {
                                owner->_drainPendingOwnerWork();
                                owner->_reapDetachedSessionIfDead(sessionId);
                            }
                        });
                        if (!scheduled)
                        {
                            self->_queuePendingReap(sessionId);
                        }
                    }
                });
        }

        _keptSessions.insert_or_assign(sessionId, std::move(kept));

        auto& group = _keptGroups[groupId];
        if (group.title.empty())
        {
            group.title = title;
        }
        if (group.shellSessionId.empty())
        {
            group.shellSessionId = shellSessionId;
        }
        else
        {
            WI_ASSERT(group.shellSessionId == shellSessionId || shellSessionId.empty());
        }
        if (group.shellSessionRevision == 0)
        {
            group.shellSessionRevision = shellSessionRevision;
        }
        else
        {
            WI_ASSERT(group.shellSessionRevision == shellSessionRevision || shellSessionRevision == 0);
        }
        group.sessionIds.push_back(sessionId);

        // The connection may already have transitioned to Closed/Failed before
        // we finished recording the detached session, or a queued callback may
        // have run before the session was visible in _keptSessions. Re-check
        // once the maps are populated so either race still gets reaped.
        _reapDetachedSessionIfDead(sessionId);
        if (const auto it = _keptSessions.find(sessionId); it != _keptSessions.end())
        {
            it->second.raiseDetachedCloseEvent = true;
            KeptSessionsChanged.raise(*this, nullptr);
            return true;
        }

        return false;
    }

    // Returns the ContentId of a previously detached session, or 0 if there is
    // no live session with that id — which is the ordinary "restore from the
    // saved snapshot instead" path, and is also what happens after the terminal
    // has been restarted.
    uint64_t ContentManager::TryReattachKeptSession(const winrt::guid& sessionId)
    {
        auto contentId = 0ull;
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            contentId = _tryReattachKeptSessionOnOwner(sessionId);
        });
        return contentId;
    }

    uint64_t ContentManager::_tryReattachKeptSessionOnOwner(const winrt::guid& sessionId)
    {
        _assertIsOwnerThread();

        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return 0;
        }

        const auto contentId = it->second.contentId;
        const auto content{ TryLookupCore(contentId) };
        if (!content)
        {
            const auto raiseDetachedCloseEvent = it->second.raiseDetachedCloseEvent;
            const auto detachedEndState = it->second.detachedEndState;
            _dropKeptSession(sessionId);
            if (raiseDetachedCloseEvent)
            {
                DetachedSessionClosed.raise(*this, _makeDetachedSessionEndedArgs(sessionId, detachedEndState));
            }
            KeptSessionsChanged.raise(*this, nullptr);
            return 0;
        }

        if (!_isLiveDetachedSession(content))
        {
            // The shell exited while detached after it was listed but before the
            // caller asked to reattach it. Finish the reap here so the caller
            // falls back to the snapshot/new-shell path, and the queued reap can
            // later observe that the bookkeeping is already gone and no-op.
            it->second.detachedEndState = _getDetachedSessionEndedState(content);
            content.Close();
            return 0;
        }

        _dropKeptSession(sessionId);
        KeptSessionsChanged.raise(*this, nullptr);
        return contentId;
    }

    winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::DetachedSessionInfo> ContentManager::DetachedSessions()
    {
        winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::DetachedSessionInfo> rows{ nullptr };
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            rows = _detachedSessionsOnOwner();
        });
        return rows;
    }

    winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::DetachedSessionInfo> ContentManager::_detachedSessionsOnOwner()
    {
        _assertIsOwnerThread();

        std::vector<winrt::TerminalApp::DetachedSessionInfo> rows;
        rows.reserve(_keptSessions.size());

        for (const auto& [sessionId, kept] : _keptSessions)
        {
            const auto group = _keptGroups.find(kept.groupId);
            if (group == _keptGroups.end())
            {
                continue;
            }

            const auto content{ TryLookupCore(kept.contentId) };
            if (!content)
            {
                continue;
            }
            if (!_isLiveDetachedSession(content))
            {
                continue;
            }

            rows.emplace_back(winrt::make<winrt::TerminalApp::implementation::DetachedSessionInfo>(
                sessionId,
                kept.groupId,
                group->second.title,
                group->second.shellSessionId,
                _getDetachedSessionPid(content)));
        }

        return winrt::single_threaded_vector<winrt::TerminalApp::DetachedSessionInfo>(std::move(rows)).GetView();
    }

    bool ContentManager::HasKeptSessions()
    {
        auto hasKeptSessions = false;
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            hasKeptSessions = !_keptSessions.empty();
        });
        return hasKeptSessions;
    }

    // What is still running with no window, one entry per detached tab.
    winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> ContentManager::KeptGroups()
    {
        winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> groups{ nullptr };
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            groups = _keptGroupsOnOwner();
        });
        return groups;
    }

    winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> ContentManager::_keptGroupsOnOwner()
    {
        _assertIsOwnerThread();

        auto map = winrt::single_threaded_map<winrt::guid, winrt::hstring>();
        for (const auto& [groupId, group] : _keptGroups)
        {
            map.Insert(groupId, group.title);
        }
        return map.GetView();
    }

    // Takes a whole detached tab at once. Returning the content ids together
    // with the durable shell-session metadata keeps this class out of the
    // business of composing tab layouts while still making the take atomic.
    winrt::TerminalApp::KeptGroupRestoreResult ContentManager::TryReattachKeptGroup(const winrt::guid& groupId)
    {
        winrt::TerminalApp::KeptGroupRestoreResult result{ nullptr };
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            result = _tryReattachKeptGroupOnOwner(groupId);
        });
        return result;
    }

    winrt::TerminalApp::KeptGroupRestoreResult ContentManager::_tryReattachKeptGroupOnOwner(const winrt::guid& groupId)
    {
        _assertIsOwnerThread();

        const auto it = _keptGroups.find(groupId);
        if (it == _keptGroups.end())
        {
            return nullptr;
        }

        std::vector<uint64_t> contentIds;
        auto changed = false;

        const auto shellSessionId = it->second.shellSessionId;
        const auto shellSessionRevision = it->second.shellSessionRevision;

        // Copy first: dropping the sessions mutates the group.
        const auto sessionIds = it->second.sessionIds;
        for (const auto& sessionId : sessionIds)
        {
            const auto session = _keptSessions.find(sessionId);
            if (session == _keptSessions.end())
            {
                continue;
            }
            const auto contentId = session->second.contentId;
            const auto content{ TryLookupCore(contentId) };
            if (!content)
            {
                const auto raiseDetachedCloseEvent = session->second.raiseDetachedCloseEvent;
                const auto detachedEndState = session->second.detachedEndState;
                _dropKeptSession(sessionId);
                if (raiseDetachedCloseEvent)
                {
                    DetachedSessionClosed.raise(*this, _makeDetachedSessionEndedArgs(sessionId, detachedEndState));
                }
                changed = true;
                continue;
            }

            if (!_isLiveDetachedSession(content))
            {
                // Reattach each pane independently: dead members are reaped
                // here, while live members still come back in the rebuilt tab.
                session->second.detachedEndState = _getDetachedSessionEndedState(content);
                content.Close();
                continue;
            }

            _dropKeptSession(sessionId);
            changed = true;
            contentIds.push_back(contentId);
        }
        if (changed)
        {
            KeptSessionsChanged.raise(*this, nullptr);
        }
        if (contentIds.empty())
        {
            return nullptr;
        }

        return winrt::make<winrt::TerminalApp::implementation::KeptGroupRestoreResult>(
            std::move(contentIds),
            shellSessionId,
            shellSessionRevision);
    }

    bool ContentManager::DiscardKeptSession(const winrt::guid& sessionId)
    {
        auto discarded = false;
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            discarded = _discardKeptSessionOnOwner(sessionId);
        });
        return discarded;
    }

    bool ContentManager::_discardKeptSessionOnOwner(const winrt::guid& sessionId)
    {
        _assertIsOwnerThread();

        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return false;
        }

        const auto contentId = it->second.contentId;
        const auto content{ TryLookupCore(contentId) };
        const auto liveDetachedSession = content && _isLiveDetachedSession(content);
        if (content)
        {
            // DetachedSessions() already filters out dead sessions. If the shell
            // exited after the caller listed the session but before it asked us
            // to discard it, finish the reap here and report "not found" to the
            // caller after emitting the one close notification.
            it->second.detachedEndState = _getDetachedSessionEndedState(content);
            content.Close();
        }
        else
        {
            const auto raiseDetachedCloseEvent = it->second.raiseDetachedCloseEvent;
            const auto detachedEndState = it->second.detachedEndState;
            _dropKeptSession(sessionId);
            if (raiseDetachedCloseEvent)
            {
                DetachedSessionClosed.raise(*this, _makeDetachedSessionEndedArgs(sessionId, detachedEndState));
            }
            KeptSessionsChanged.raise(*this, nullptr);
        }

        return liveDetachedSession;
    }

    // Ends a detached tab on purpose. Closing each content is what tears its
    // shell down, and _closedHandler drops it from our maps and tells the
    // emperor it may now have one fewer reason to stay alive.
    void ContentManager::DiscardKeptGroup(const winrt::guid& groupId)
    {
        _invokeOnOwnerThread([&]() {
            _drainPendingOwnerWork();
            _discardKeptGroupOnOwner(groupId);
        });
    }

    void ContentManager::_discardKeptGroupOnOwner(const winrt::guid& groupId)
    {
        _assertIsOwnerThread();

        const auto it = _keptGroups.find(groupId);
        if (it == _keptGroups.end())
        {
            return;
        }

        const auto sessionIds = it->second.sessionIds;
        auto changed = false;
        for (const auto& sessionId : sessionIds)
        {
            const auto session = _keptSessions.find(sessionId);
            if (session == _keptSessions.end())
            {
                continue;
            }
            const auto contentId = session->second.contentId;
            if (const auto content{ TryLookupCore(contentId) })
            {
                // Drops through _closedHandler, which also raises the event.
                session->second.detachedEndState = _getDetachedSessionEndedState(content);
                content.Close();
            }
            else
            {
                const auto raiseDetachedCloseEvent = session->second.raiseDetachedCloseEvent;
                const auto detachedEndState = session->second.detachedEndState;
                _dropKeptSession(sessionId);
                if (raiseDetachedCloseEvent)
                {
                    DetachedSessionClosed.raise(*this, _makeDetachedSessionEndedArgs(sessionId, detachedEndState));
                }
                changed = true;
            }
        }

        if (changed)
        {
            KeptSessionsChanged.raise(*this, nullptr);
        }
    }

    // Removes one session and, once its tab has no members left, the tab.
    // Deliberately silent: callers batch several drops behind one event.
    void ContentManager::_dropKeptSession(const winrt::guid& sessionId)
    {
        _assertIsOwnerThread();

        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return;
        }

        const auto groupId = it->second.groupId;
        _keptSessions.erase(it);

        const auto group = _keptGroups.find(groupId);
        if (group == _keptGroups.end())
        {
            return;
        }

        auto& members = group->second.sessionIds;
        members.erase(std::remove(members.begin(), members.end(), sessionId), members.end());
        if (members.empty())
        {
            _keptGroups.erase(group);
        }
    }

    // Runs on the process-lifetime owner thread after a detached session's
    // connection changed state.
    void ContentManager::_reapDetachedSessionIfDead(const winrt::guid& sessionId)
    {
        _assertIsOwnerThread();

        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return;
        }

        const auto contentId = it->second.contentId;
        const auto content{ TryLookupCore(contentId) };
        if (!content)
        {
            const auto raiseDetachedCloseEvent = it->second.raiseDetachedCloseEvent;
            const auto detachedEndState = it->second.detachedEndState;
            _dropKeptSession(sessionId);
            if (raiseDetachedCloseEvent)
            {
                DetachedSessionClosed.raise(*this, _makeDetachedSessionEndedArgs(sessionId, detachedEndState));
            }
            KeptSessionsChanged.raise(*this, nullptr);
            return;
        }

        if (_isLiveDetachedSession(content))
        {
            return;
        }

        // The shell exited while detached. Closing the content drops it from
        // both _content and our maps via _closedHandler, which is also what
        // eventually lets the emperor quit. `content` keeps it alive across the
        // call.
        it->second.detachedEndState = _getDetachedSessionEndedState(content);
        content.Close();
    }

    void ContentManager::_forgetKeptSession(uint64_t contentId, const winrt::hstring& fallbackDetachedEndState)
    {
        _assertIsOwnerThread();

        for (const auto& [sessionId, kept] : _keptSessions)
        {
            if (kept.contentId == contentId)
            {
                const auto id = sessionId;
                const auto raiseDetachedCloseEvent = kept.raiseDetachedCloseEvent;
                const auto detachedEndState = kept.detachedEndState.empty() ? fallbackDetachedEndState : kept.detachedEndState;
                _dropKeptSession(id);
                if (raiseDetachedCloseEvent)
                {
                    DetachedSessionClosed.raise(*this, _makeDetachedSessionEndedArgs(id, detachedEndState));
                }
                KeptSessionsChanged.raise(*this, nullptr);
                return;
            }
        }
    }

    bool ContentManager::_processClosedContent(const uint64_t contentId, const winrt::hstring& detachedEndState)
    {
        _assertIsOwnerThread();

        // The Closed event can be queued more than once when a dispatcher
        // callback and fallback owner work race. The map is the authoritative
        // lifetime record, so only the invocation that removes it may emit
        // the corresponding detached-session events.
        if (_content.erase(contentId) != 0)
        {
            _forgetKeptSession(contentId, detachedEndState);
            return true;
        }

        return false;
    }

    void ContentManager::_closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                                        const winrt::Windows::Foundation::IInspectable&)
    {
        if (const auto& content{ sender.try_as<winrt::Microsoft::Terminal::Control::ControlInteractivity>() })
        {
            const auto contentId{ content.Id() };
            const auto detachedEndState = _getDetachedSessionEndedState(content);

            // A kept session whose shell just exited stops being a reason to
            // keep this process alive. `content` is agile, but the manager's
            // maps and events are owned by its configured dispatcher thread.
            if (_isOwnerThread())
            {
                if (!_processClosedContent(contentId, detachedEndState))
                {
                    _queuePendingClosedContent(contentId, detachedEndState);
                }
            }
            else
            {
                const auto weakThis = get_weak();
                if (!_tryScheduleOnOwner([weakThis, contentId, detachedEndState]() {
                        if (const auto self{ weakThis.get() })
                        {
                            self->_drainPendingOwnerWork();
                            if (!self->_processClosedContent(contentId, detachedEndState))
                            {
                                self->_queuePendingClosedContent(contentId, detachedEndState);
                            }
                        }
                    }))
                {
                    _queuePendingClosedContent(contentId, detachedEndState);
                }
            }
        }
    }
}
