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

        _content.emplace(content.Id(), content);

        return content;
    }

    ControlInteractivity ContentManager::TryLookupCore(uint64_t id)
    {
        const auto it = _content.find(id);
        return it != _content.end() ? it->second : ControlInteractivity{ nullptr };
    }

    void ContentManager::Detach(const Microsoft::Terminal::Control::TermControl& control)
    {
        const auto contentId{ control.ContentId() };
        if (const auto& content{ TryLookupCore(contentId) })
        {
            control.Detach();
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

        KeptSession kept;
        kept.contentId = contentId;
        kept.groupId = groupId;
        const auto core{ content.Core() };

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
                [weakThis = get_weak(),
                 dispatcher = winrt::Windows::System::DispatcherQueue::GetForCurrentThread(),
                 sessionId](auto&&, auto&&) {
                    if (!dispatcher)
                    {
                        return;
                    }
                    dispatcher.TryEnqueue([weakThis, sessionId]() {
                        if (const auto self{ weakThis.get() })
                        {
                            self->_reapDetachedSessionIfDead(sessionId);
                        }
                    });
                });
        }

        control.Detach();

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
        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return 0;
        }

        const auto contentId = it->second.contentId;
        const auto content{ TryLookupCore(contentId) };
        if (!content)
        {
            _dropKeptSession(sessionId);
            KeptSessionsChanged.raise(*this, nullptr);
            return 0;
        }

        if (!_isLiveDetachedSession(content))
        {
            // The shell exited while detached after it was listed but before the
            // caller asked to reattach it. Finish the reap here so the caller
            // falls back to the snapshot/new-shell path, and the queued reap can
            // later observe that the bookkeeping is already gone and no-op.
            content.Close();
            return 0;
        }

        _dropKeptSession(sessionId);
        KeptSessionsChanged.raise(*this, nullptr);
        return contentId;
    }

    winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::DetachedSessionInfo> ContentManager::DetachedSessions()
    {
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

    bool ContentManager::HasKeptSessions() const noexcept
    {
        return !_keptSessions.empty();
    }

    // What is still running with no window, one entry per detached tab.
    winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> ContentManager::KeptGroups()
    {
        auto map = winrt::single_threaded_map<winrt::guid, winrt::hstring>();
        for (const auto& [groupId, group] : _keptGroups)
        {
            map.Insert(groupId, group.title);
        }
        return map.GetView();
    }

    // Takes a whole detached tab at once. Returning the content ids rather than
    // rebuilding here keeps this class out of the business of composing tab
    // layouts; the caller turns them into a new tab.
    winrt::Windows::Foundation::Collections::IVectorView<uint64_t> ContentManager::TryReattachKeptGroup(const winrt::guid& groupId)
    {
        std::vector<uint64_t> contentIds;
        auto changed = false;

        const auto it = _keptGroups.find(groupId);
        if (it != _keptGroups.end())
        {
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
                    _dropKeptSession(sessionId);
                    changed = true;
                    continue;
                }

                if (!_isLiveDetachedSession(content))
                {
                    // Reattach each pane independently: dead members are reaped
                    // here, while live members still come back in the rebuilt tab.
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
        }

        return winrt::single_threaded_vector<uint64_t>(std::move(contentIds)).GetView();
    }

    bool ContentManager::DiscardKeptSession(const winrt::guid& sessionId)
    {
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
            content.Close();
        }
        else
        {
            const auto raiseDetachedCloseEvent = it->second.raiseDetachedCloseEvent;
            _dropKeptSession(sessionId);
            if (raiseDetachedCloseEvent)
            {
                DetachedSessionClosed.raise(*this, winrt::box_value(sessionId));
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
                content.Close();
            }
            else
            {
                _dropKeptSession(sessionId);
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

    // Runs on the UI thread after a detached session's connection changed state.
    void ContentManager::_reapDetachedSessionIfDead(const winrt::guid& sessionId)
    {
        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return;
        }

        const auto contentId = it->second.contentId;
        const auto content{ TryLookupCore(contentId) };
        if (!content)
        {
            _dropKeptSession(sessionId);
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
        content.Close();
    }

    void ContentManager::_forgetKeptSession(uint64_t contentId)
    {
        for (const auto& [sessionId, kept] : _keptSessions)
        {
            if (kept.contentId == contentId)
            {
                const auto id = sessionId;
                const auto raiseDetachedCloseEvent = kept.raiseDetachedCloseEvent;
                _dropKeptSession(id);
                if (raiseDetachedCloseEvent)
                {
                    DetachedSessionClosed.raise(*this, winrt::box_value(id));
                }
                KeptSessionsChanged.raise(*this, nullptr);
                return;
            }
        }
    }

    void ContentManager::_closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                                        const winrt::Windows::Foundation::IInspectable&)
    {
        if (const auto& content{ sender.try_as<winrt::Microsoft::Terminal::Control::ControlInteractivity>() })
        {
            const auto& contentId{ content.Id() };
            _content.erase(contentId);

            // A kept session whose shell just exited stops being a reason to
            // keep this process alive.
            _forgetKeptSession(contentId);
        }
    }
}
