// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ContentManager.h"
#include "ContentManager.g.cpp"

#include <wil/token_helpers.h>
#include <json/json.h>
#include <sstream>

#include "../../types/inc/utils.hpp"

using namespace winrt::Windows::ApplicationModel::DataTransfer;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Windows::System;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Microsoft::Terminal::TerminalConnection;

namespace winrt::TerminalApp::implementation
{
    ControlInteractivity ContentManager::CreateCore(const Microsoft::Terminal::Control::IControlSettings& settings,
                                                    const IControlAppearance& unfocusedAppearance,
                                                    const TerminalConnection::ITerminalConnection& connection)
    {
        ControlInteractivity content{ settings, unfocusedAppearance, connection };
        content.Closed({ get_weak(), &ContentManager::_closedHandler });

        {
            std::lock_guard lock{ _mutex };
            _content.emplace(content.Id(), content);
        }

        return content;
    }

    ControlInteractivity ContentManager::TryLookupCore(uint64_t id)
    {
        std::lock_guard lock{ _mutex };
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

    void ContentManager::_closedHandler(const winrt::Windows::Foundation::IInspectable& sender,
                                        const winrt::Windows::Foundation::IInspectable&)
    {
        if (const auto& content{ sender.try_as<winrt::Microsoft::Terminal::Control::ControlInteractivity>() })
        {
            const auto& contentId{ content.Id() };
            {
                std::lock_guard lock{ _mutex };
                _content.erase(contentId);
            }
            _QueueReap();
        }
    }

    void ContentManager::_CheckThread() const
    {
        THROW_HR_IF(RPC_E_WRONG_THREAD, !_dispatcher || !_dispatcher.HasThreadAccess());
    }

    bool ContentManager::CanKeepRunning(const uint64_t contentId)
    {
        _CheckThread();
        const auto policy = _panePolicies.find(contentId);
        const auto content = TryLookupCore(contentId);
        const auto connection = content ? content.Core().Connection() : nullptr;
        return policy != _panePolicies.end() && !policy->second.agentSessionId.empty() &&
               connection && connection.State() == ConnectionState::Connected;
    }

    bool ContentManager::IsKeepRunning(const uint64_t contentId)
    {
        _CheckThread();
        const auto policy = _panePolicies.find(contentId);
        return policy != _panePolicies.end() && policy->second.keepRunning && CanKeepRunning(contentId);
    }

    void ContentManager::SetKeepRunning(const uint64_t contentId, const bool enabled)
    {
        _CheckThread();
        THROW_HR_IF(E_INVALIDARG, !TryLookupCore(contentId) || IsKeptContent(contentId));
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, enabled && !CanKeepRunning(contentId));
        _panePolicies[contentId].keepRunning = enabled;
    }

    winrt::hstring ContentManager::AgentSessionEvent(const uint64_t contentId)
    {
        _CheckThread();
        const auto it = _panePolicies.find(contentId);
        return it == _panePolicies.end() ? winrt::hstring{} : it->second.eventJson;
    }

    void ContentManager::OnPaneAgentSessionChanged(const winrt::hstring& eventJson)
    {
        _CheckThread();
        Json::Value event;
        Json::CharReaderBuilder reader;
        std::string errors;
        std::istringstream stream{ winrt::to_string(eventJson) };
        if (!Json::parseFromStream(reader, stream, &event, &errors) || !event["params"].isObject())
        {
            THROW_HR(E_INVALIDARG);
        }
        const auto& params = event["params"];
        const auto name = params.get("event", "").asString();
        const auto ended = name == "agent.session.end" || name == "agent.session.stopped";
        const auto started = name == "agent.session.start" || name == "agent.session.started";
        const auto prompt = name == "agent.prompt.submit";
        if (!ended && !started && !prompt)
        {
            return;
        }
        const auto paneId = params.get("pane_id", "").asString();
        if (paneId.empty())
        {
            return; // Unattributed hooks must never bind the focused pane.
        }
        const auto text = winrt::to_hstring(paneId);
        const winrt::guid sessionId = paneId.starts_with('{') ?
                                          ::Microsoft::Console::Utils::GuidFromString(text.c_str()) :
                                          ::Microsoft::Console::Utils::GuidFromPlainString(text.c_str());
        const auto agentSessionId = winrt::to_hstring(params.get("agent_session_id", "").asString());
        const auto agent = winrt::to_hstring(params.get("agent", params.get("cli_source", "")).asString());
        if (!ended && (agentSessionId.empty() || std::wstring_view{ agentSessionId }.starts_with(L"sidekick-") || agent.empty()))
        {
            return;
        }

        uint64_t contentId{};
        {
            std::lock_guard lock{ _mutex };
            for (const auto& [id, content] : _content)
            {
                const auto connection = content.Core().Connection();
                if (connection && connection.SessionId() == sessionId)
                {
                    contentId = id;
                    break;
                }
            }
        }
        if (!contentId)
        {
            return;
        }
        auto& policy = _panePolicies[contentId];
        if (ended)
        {
            if (agentSessionId.empty() || agentSessionId == policy.agentSessionId)
            {
                // Ending the CLI cancels future detachment, not an already-kept shell.
                policy = {};
            }
        }
        else if (!(prompt && agent == L"copilot" && !policy.agentSessionId.empty()))
        {
            if (policy.agentSessionId != agentSessionId)
            {
                policy.keepRunning = false;
            }
            policy.agentSessionId = agentSessionId;
            policy.eventJson = eventJson;
        }
    }

    bool ContentManager::DetachForKeepRunning(const winrt::guid& groupId, const winrt::hstring& title, const NewTerminalArgs& args, const TermControl& control)
    {
        _CheckThread();
        THROW_HR_IF(E_INVALIDARG, groupId == winrt::guid{} || !args || !control);
        const auto contentId = control.ContentId();
        if (!IsKeepRunning(contentId))
        {
            return false;
        }
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, IsKeptContent(contentId));
        const auto content = TryLookupCore(contentId);
        auto copiedArgs = args.Copy().as<NewTerminalArgs>();
        copiedArgs.ContentId(contentId);
        const auto sessionId = control.Connection().SessionId();
        copiedArgs.SessionId(sessionId);
        KeptPane pane;
        pane.contentId = contentId;
        pane.sessionId = sessionId;
        pane.args = std::move(copiedArgs);
        pane.stateChanged = content.Core().ConnectionStateChanged(winrt::auto_revoke, [weak = get_weak()](auto&&, auto&&) {
            if (const auto self = weak.get())
            {
                self->_QueueReap();
            }
        });
        pane.vtSequence = content.Core().VtSequenceReceived(winrt::auto_revoke, [weak = get_weak(), dispatcher = _dispatcher, sessionId, contentId](auto&&, const winrt::hstring& sequence) {
            if (!std::wstring_view{ sequence }.starts_with(L"AgentEvent;"))
            {
                return;
            }
            LOG_HR_IF(E_ABORT, !dispatcher.TryEnqueue([weak, sessionId, contentId, sequence]() {
                if (const auto self = weak.get())
                {
                    try
                    {
                        const ControlInteractivity liveContent{ self->TryLookupCore(contentId) };
                        if (!liveContent || liveContent.Core().ConnectionState() >= ConnectionState::Closed)
                        {
                            return;
                        }
                        Json::Value params;
                        Json::CharReaderBuilder reader;
                        std::string errors;
                        std::istringstream stream{ winrt::to_string(sequence).substr(11) };
                        THROW_HR_IF(E_INVALIDARG, !Json::parseFromStream(reader, stream, &params, &errors) || !params.isObject());
                        params["pane_id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToPlainString(sessionId));
                        Json::Value event;
                        event["method"] = "agent_event";
                        event["params"] = std::move(params);
                        const auto json = winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event));
                        self->OnPaneAgentSessionChanged(json);
                        self->DetachedSessionEvent.raise(*self, json);
                    }
                    CATCH_LOG()
                }
            }));
        });
        auto [it, inserted] = _keptGroups.try_emplace(groupId);
        auto& group = it->second;
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, group.restoring);
        auto rollback = wil::scope_exit([&]() noexcept {
            if (inserted)
            {
                _keptGroups.erase(groupId);
            }
        });
        if (inserted)
        {
            group.title = title;
            group.lease = SharedWta::Instance().AcquireKeepRunningLease();
        }
        group.panes.reserve(group.panes.size() + 1);
        control.Detach();
        group.panes.emplace_back(std::move(pane));
        rollback.release();
        _NotifyKeptSessionsChanged();
        return true;
    }

    bool ContentManager::IsKeptContent(const uint64_t contentId)
    {
        _CheckThread();
        for (const auto& [id, group] : _keptGroups)
        {
            for (const auto& pane : group.panes)
            {
                if (pane.contentId == contentId)
                {
                    return true;
                }
            }
        }
        return false;
    }

    bool ContentManager::HasKeptSessions()
    {
        _CheckThread();
        _ReapClosedSessions();
        return !_keptGroups.empty();
    }

    winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> ContentManager::KeptGroups()
    {
        _CheckThread();
        _ReapClosedSessions();
        auto result = winrt::single_threaded_map<winrt::guid, winrt::hstring>();
        for (const auto& [id, group] : _keptGroups)
        {
            if (!group.restoring)
            {
                result.Insert(id, group.title);
            }
        }
        return result.GetView();
    }

    winrt::Windows::Foundation::Collections::IVectorView<NewTerminalArgs> ContentManager::BeginReattachKeptGroup(const winrt::guid& groupId)
    {
        _CheckThread();
        _ReapClosedSessions();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end());
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, it->second.restoring);
        std::vector<NewTerminalArgs> result;
        for (const auto& pane : it->second.panes)
        {
            result.emplace_back(pane.args.Copy().as<NewTerminalArgs>());
        }
        const auto args = winrt::single_threaded_vector<NewTerminalArgs>(std::move(result)).GetView();
        it->second.restoring = true;
        return args;
    }

    void ContentManager::CompleteKeptGroupReattach(const winrt::guid& groupId, const bool committed)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end() || !it->second.restoring);
        if (committed)
        {
            auto lease = std::move(it->second.lease);
            _keptGroups.erase(it);
            // Allow queued hook events to drain and the restored tab's helper
            // to acquire its lease before the last master reference disappears.
            lease.Retire();
        }
        else
        {
            it->second.restoring = false;
            _ReapClosedSessions();
        }
        _NotifyKeptSessionsChanged();
    }

    void ContentManager::_CloseKeptPane(KeptPane pane)
    {
        pane.stateChanged.revoke();
        pane.vtSequence.revoke();
        const auto content = TryLookupCore(pane.contentId);
        const auto connection = content ? content.Core().Connection() : nullptr;
        Json::Value event;
        event["method"] = "connection_state";
        event["params"]["pane_id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToPlainString(pane.sessionId));
        event["params"]["state"] = connection && connection.State() == ConnectionState::Failed ? "failed" : "closed";
        _panePolicies.erase(pane.contentId);
        // Publish while a group's lease still keeps the master listening.
        try
        {
            DetachedSessionEvent.raise(*this, winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event)));
        }
        CATCH_LOG()
        if (content)
        {
            content.Close();
        }
    }

    void ContentManager::DiscardKeptGroup(const winrt::guid& groupId)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end());
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, it->second.restoring);
        auto group = std::move(it->second);
        _keptGroups.erase(it);
        for (auto& pane : group.panes)
        {
            _CloseKeptPane(std::move(pane));
        }
        group.lease.Retire();
        _NotifyKeptSessionsChanged();
    }

    void ContentManager::_QueueReap()
    {
        LOG_HR_IF(E_ABORT, !_dispatcher || !_dispatcher.TryEnqueue([weak = get_weak()]() {
            if (const auto self = weak.get())
            {
                try
                {
                    self->_ReapClosedSessions();
                }
                CATCH_LOG()
            }
        }));
    }

    void ContentManager::_ReapClosedSessions()
    {
        _CheckThread();
        std::vector<KeptPane> closed;
        std::vector<SharedWtaLease> leases;
        for (auto group = _keptGroups.begin(); group != _keptGroups.end();)
        {
            // A restore owns the borrowed controls until its synchronous commit/rollback.
            if (group->second.restoring)
            {
                ++group;
                continue;
            }
            auto& panes = group->second.panes;
            for (auto pane = panes.begin(); pane != panes.end();)
            {
                const auto content = TryLookupCore(pane->contentId);
                const auto connection = content ? content.Core().Connection() : nullptr;
                if (!connection || connection.State() >= ConnectionState::Closed)
                {
                    closed.emplace_back(std::move(*pane));
                    pane = panes.erase(pane);
                }
                else
                {
                    ++pane;
                }
            }
            if (panes.empty())
            {
                leases.emplace_back(std::move(group->second.lease));
                group = _keptGroups.erase(group);
            }
            else
            {
                ++group;
            }
        }
        for (auto& pane : closed)
        {
            _CloseKeptPane(std::move(pane));
        }
        for (auto& lease : leases)
        {
            lease.Retire();
        }
        std::erase_if(_panePolicies, [&](const auto& entry) { return !TryLookupCore(entry.first); });
        if (!closed.empty())
        {
            _NotifyKeptSessionsChanged();
        }
    }

    void ContentManager::_NotifyKeptSessionsChanged() noexcept
    {
        try
        {
            KeptSessionsChanged.raise(*this, nullptr);
        }
        CATCH_LOG()
    }
}
