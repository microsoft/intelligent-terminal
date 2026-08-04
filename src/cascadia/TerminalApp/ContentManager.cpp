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
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::TerminalApp::implementation
{
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
    // alive with no window. The content keeps its ConptyConnection — and
    // therefore its output thread and its Terminal buffer — so the shell keeps
    // running and its scrollback keeps filling while nothing is attached.
    void ContentManager::DetachForKeepRunning(const winrt::guid& sessionId, const Microsoft::Terminal::Control::TermControl& control)
    {
        if (!control || sessionId == winrt::guid{})
        {
            return;
        }

        const auto contentId{ control.ContentId() };
        const auto content{ TryLookupCore(contentId) };
        if (!content)
        {
            return;
        }

        control.Detach();

        KeptSession kept;
        kept.contentId = contentId;

        // Detaching severed the only thing that was watching this connection —
        // TermControl::Detach() clears its revokers. Without our own watch, a
        // shell that exits while detached would go unnoticed forever: the entry
        // would linger, and the process would never find a reason to quit.
        //
        // The connection raises StateChanged from its output thread, so hop
        // back to the UI thread before touching our maps or tearing anything
        // down.
        if (const auto core{ content.Core() })
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

        _keptSessions.insert_or_assign(sessionId, std::move(kept));
        KeptSessionsChanged.raise(*this, nullptr);
    }

    // Returns the ContentId of a previously kept session, or 0 if there is no
    // live session with that id — which is the ordinary "restore from the saved
    // snapshot instead" path, and is also what happens after the terminal has
    // been restarted.
    uint64_t ContentManager::TryReattachKeptSession(const winrt::guid& sessionId)
    {
        const auto it = _keptSessions.find(sessionId);
        if (it == _keptSessions.end())
        {
            return 0;
        }

        const auto contentId = it->second.contentId;
        _keptSessions.erase(it);
        KeptSessionsChanged.raise(*this, nullptr);

        // The shell may have exited while detached, in which case the content is
        // already gone and the caller should start a fresh one.
        return TryLookupCore(contentId) ? contentId : 0;
    }

    bool ContentManager::HasKeptSessions() const noexcept
    {
        return !_keptSessions.empty();
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
            _keptSessions.erase(it);
            KeptSessionsChanged.raise(*this, nullptr);
            return;
        }

        const auto core{ content.Core() };
        const auto connection{ core ? core.Connection() : nullptr };
        if (connection && connection.State() < winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Closed)
        {
            return;
        }

        // The shell exited while detached. Closing the content drops it from both
        // _content and _keptSessions via _closedHandler, which is also what
        // eventually lets the emperor quit. `content` keeps it alive across the
        // call.
        content.Close();
    }

    void ContentManager::_forgetKeptSession(uint64_t contentId)
    {
        for (auto it = _keptSessions.begin(); it != _keptSessions.end(); ++it)
        {
            if (it->second.contentId == contentId)
            {
                _keptSessions.erase(it);
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
