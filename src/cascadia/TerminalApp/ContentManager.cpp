// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ContentManager.h"
#include "ContentManager.g.cpp"
#include "TerminalPage.h"
#include "../inc/AgentRegistry.h"

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
        return CreateAgentCliCore(settings, unfocusedAppearance, connection, {});
    }

    ControlInteractivity ContentManager::CreateAgentCliCore(const Microsoft::Terminal::Control::IControlSettings& settings,
                                                            const IControlAppearance& unfocusedAppearance,
                                                            const TerminalConnection::ITerminalConnection& connection,
                                                            const winrt::hstring& providerId,
                                                            const std::string& nativeAgentLaunchLocation)
    {
        const auto createdAtMs = static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
                                                          std::chrono::system_clock::now().time_since_epoch())
                                                          .count());
        ControlInteractivity content{ settings, unfocusedAppearance, connection };
        content.Closed({ get_weak(), &ContentManager::_closedHandler });

        {
            std::lock_guard lock{ _mutex };
            _content.emplace(content.Id(), TerminalContent{ content, providerId, createdAtMs, ++_nativeAgentPaneCreationSequence, nativeAgentLaunchLocation });
        }

        return content;
    }

    ControlInteractivity ContentManager::TryLookupCore(uint64_t id)
    {
        std::lock_guard lock{ _mutex };
        const auto it = _content.find(id);
        return it != _content.end() ? it->second.core : ControlInteractivity{ nullptr };
    }

    winrt::hstring ContentManager::NativeAgentProviderId(const uint64_t contentId) const
    {
        std::lock_guard lock{ _mutex };
        const auto it = _content.find(contentId);
        return it == _content.end() ? winrt::hstring{} :
               !it->second.explicitNativeAgentProviderId.empty() ?
                   it->second.explicitNativeAgentProviderId : it->second.nativeAgentProviderId;
    }

    winrt::hstring ContentManager::NativeAgentProviderIdForPane(const winrt::guid& paneId) const
    {
        std::lock_guard lock{ _mutex };
        for (const auto& [id, content] : _content)
        {
            const auto connection = content.core.Core().Connection();
            if (connection && connection.SessionId() == paneId)
            {
                return !content.explicitNativeAgentProviderId.empty() ?
                           content.explicitNativeAgentProviderId : content.nativeAgentProviderId;
            }
        }
        return {};
    }

    std::optional<ContentManager::NativeAgentObservationIdentity> ContentManager::NativeAgentObservationForContent(const uint64_t contentId) const
    {
        std::lock_guard lock{ _mutex };
        const auto it = _content.find(contentId);
        if (it == _content.end() || it->second.agentHelper || it->second.observation.modeEpoch == 0)
        {
            return std::nullopt;
        }
        return it->second.observation;
    }

    void ContentManager::SetExplicitNativeAgentProvider(const uint64_t contentId, const winrt::hstring& providerId)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId); it != _content.end())
        {
            it->second.explicitNativeAgentProviderId = providerId;
        }
    }

    bool ContentManager::RefineNativeAgentLaunchLocation(
        const uint64_t contentId, const winrt::hstring& providerId, const std::string& location)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId);
            it != _content.end() && !location.empty() && !it->second.agentHelper &&
            !it->second.nativeAgentProviderId.empty() &&
            ::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(it->second.nativeAgentProviderId, providerId) &&
            it->second.nativeAgentLaunchLocation.empty() && it->second.observation.agentSessionId.empty() &&
            it->second.observation.qualifiedSessionId.empty() &&
            (it->second.observation.location.empty() || it->second.observation.location == location))
        {
            it->second.nativeAgentLaunchLocation = location;
            if (it->second.observation.modeEpoch != 0)
            {
                it->second.observation.location = location;
            }
            return true;
        }
        return false;
    }

    ContentManager::NativeAgentObservationIdentity ContentManager::AdmitNativeAgentObservation(
        const uint64_t contentId, const uint64_t windowId, const std::string& tabId, const std::string& agentSessionId, const std::string& location, const bool reset, const uint64_t receiverGeneration)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId); it != _content.end() && !it->second.agentHelper)
        {
            const auto provider = it->second.explicitNativeAgentProviderId.empty() || agentSessionId.empty() ?
                                      it->second.nativeAgentProviderId : it->second.explicitNativeAgentProviderId;
            if (provider.empty())
            {
                return {};
            }
            auto& identity = it->second.observation;
            const auto& observationLocation = location.empty() ? it->second.nativeAgentLaunchLocation : location;
            if (reset || identity.modeEpoch == 0 || identity.providerId != provider ||
                identity.windowId != windowId || identity.tabId != tabId ||
                identity.agentSessionId != agentSessionId || identity.location != observationLocation ||
                identity.receiverGeneration != receiverGeneration)
            {
                const auto retainQualified = identity.providerId == provider &&
                    (agentSessionId.empty() ? identity.agentSessionId.empty() : identity.qualifiedSessionId == agentSessionId) &&
                    (observationLocation.empty() || identity.qualifiedLocation == observationLocation);
                auto qualifiedSessionId = retainQualified ? identity.qualifiedSessionId : std::string{};
                auto qualifiedLocation = retainQualified ? identity.qualifiedLocation : std::string{};
                identity = { provider, ++_nativeAgentObservationGeneration, windowId, tabId, agentSessionId, observationLocation, !it->second.observationAdmitted, receiverGeneration, std::move(qualifiedSessionId), std::move(qualifiedLocation), it->second.paneCreatedAtMs, it->second.paneCreationSequence };
                it->second.observationAdmitted = true;
            }
            return identity;
        }
        return {};
    }

    void ContentManager::RetainQualifiedNativeAgentIdentity(
        const uint64_t contentId, const uint64_t modeEpoch, const std::string& sessionId, const std::string& location)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId);
            it != _content.end() && !it->second.agentHelper && modeEpoch != 0 &&
            it->second.observation.modeEpoch == modeEpoch)
        {
            it->second.observation.qualifiedSessionId = sessionId;
            it->second.observation.qualifiedLocation = location;
        }
    }

    void ContentManager::RemoveNativeAgentObservation(const uint64_t contentId)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId); it != _content.end())
        {
            it->second.observation = {};
        }
    }

    void ContentManager::RestartNativeAgentConnectionTracking(const uint64_t contentId)
    {
        _CheckThread();
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId); it != _content.end() && !it->second.agentHelper)
        {
            it->second.paneCreatedAtMs = static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
                                                                   std::chrono::system_clock::now().time_since_epoch())
                                                                   .count());
            it->second.paneCreationSequence = ++_nativeAgentPaneCreationSequence;
            it->second.observation = {};
            it->second.observationAdmitted = false;
            if (!_nativeAgentSessionEventsEnabled)
            {
                it->second.explicitNativeAgentProviderId = winrt::hstring{};
                _agentBindings.erase(contentId);
            }
        }
    }

    void ContentManager::SetNativeAgentSessionEventsEnabled(const bool enabled)
    {
        _CheckThread();
        std::lock_guard lock{ _mutex };
        if (_nativeAgentSessionEventsEnabled != enabled)
        {
            for (auto& [id, content] : _content)
            {
                content.observation = {};
            }
        }
        _nativeAgentSessionEventsEnabled = enabled;
        if (!enabled)
        {
            std::erase_if(_agentBindings, [&](const auto& entry) {
                const auto content = _content.find(entry.first);
                return content == _content.end() || !content->second.agentHelper;
            });
        }
    }

    void ContentManager::MarkAgentHelper(const uint64_t contentId)
    {
        std::lock_guard lock{ _mutex };
        if (const auto content = _content.find(contentId); content != _content.end())
        {
            content->second.agentHelper = true;
        }
    }

    bool ContentManager::AcceptsAgentSessionEvents(const winrt::guid& paneId) const
    {
        std::lock_guard lock{ _mutex };
        if (_nativeAgentSessionEventsEnabled)
        {
            return true;
        }
        for (const auto& [id, content] : _content)
        {
            const auto connection = content.core.Core().Connection();
            if (connection && connection.SessionId() == paneId)
            {
                return content.agentHelper;
            }
        }
        return false;
    }

    void ContentManager::UpdateKeepRunningTab(const winrt::TerminalApp::Tab& tab)
    {
        _CheckThread();
        std::erase_if(_keepRunningTabs, [](const auto& entry) {
            return !entry.second.get();
        });
        const auto impl = winrt::get_self<implementation::Tab>(tab);
        if (impl->KeepRunning() && impl->CanKeepRunning())
        {
            _keepRunningTabs.insert_or_assign(impl->StableId(), winrt::make_weak(tab));
        }
        else
        {
            const auto it = _keepRunningTabs.find(impl->StableId());
            if (it != _keepRunningTabs.end() && it->second.get() == tab)
            {
                _keepRunningTabs.erase(it);
            }
        }
        try
        {
            KeepRunningTabsChanged.raise(*this, nullptr);
        }
        CATCH_LOG()
    }

    bool ContentManager::IsPaneKeepRunning(const winrt::guid& paneId) const
    {
        _CheckThread();
        if (paneId == winrt::guid{})
        {
            return false;
        }
        for (const auto& [_, weakTab] : _keepRunningTabs)
        {
            if (const auto tab = weakTab.get())
            {
                const auto impl = winrt::get_self<implementation::Tab>(tab);
                if (const auto root = impl->GetRootPane();
                    impl->KeepRunning() && root && root->FindPaneBySessionId(paneId))
                {
                    return true;
                }
            }
        }
        return false;
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
            std::optional<NativeAgentObservationIdentity> retired;
            {
                std::lock_guard lock{ _mutex };
                if (const auto found = _content.find(contentId);
                    found != _content.end() && !_nativeAgentSessionEventsEnabled &&
                    !found->second.agentHelper && found->second.observation.modeEpoch != 0)
                {
                    retired = found->second.observation;
                }
                _content.erase(contentId);
            }
            if (retired)
            {
                Json::Value event;
                event["type"] = "event";
                event["method"] = "native_agent_observation";
                auto& params = event["params"];
                params["kind"] = "removed";
                params["pane_id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToPlainString(content.Core().Connection().SessionId()));
                params["provider_id"] = winrt::to_string(retired->providerId);
                params["window_id"] = Json::UInt64{ retired->windowId };
                params["tab_id"] = retired->tabId;
                params["mode_epoch"] = Json::UInt64{ retired->modeEpoch };
                if (retired->receiverGeneration != 0)
                {
                    params["receiver_generation"] = Json::UInt64{ retired->receiverGeneration };
                }
                if (!retired->agentSessionId.empty())
                {
                    params["agent_session_id"] = retired->agentSessionId;
                    const auto& source = retired->location;
                    if (source == "Host")
                    {
                        params["location"] = "Host";
                    }
                    else if (source.starts_with("wsl:") && source.size() > 4)
                    {
                        params["location"]["Wsl"]["distro"] = source.substr(4);
                    }
                }
                Json::StreamWriterBuilder writer;
                writer["indentation"] = "";
                DetachedSessionEvent.raise(*this, winrt::to_hstring(Json::writeString(writer, event)));
            }
            LOG_HR_IF(E_ABORT, !_dispatcher.TryEnqueue([weak = get_weak(), contentId]() {
                if (const auto self = weak.get())
                {
                    self->_agentBindings.erase(contentId);
                }
            }));
        }
    }

    void ContentManager::_CheckThread() const
    {
        THROW_HR_IF(RPC_E_WRONG_THREAD, !_dispatcher || !_dispatcher.HasThreadAccess());
    }

    winrt::hstring ContentManager::AgentSessionEvent(const uint64_t contentId)
    {
        _CheckThread();
        const auto it = _agentBindings.find(contentId);
        return it == _agentBindings.end() ? winrt::hstring{} : it->second.eventJson;
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
        const auto unbound = name == "agent.session.unbind";
        if (!ended && !started && !prompt && !unbound)
        {
            return;
        }
        const auto& payload = params["payload"];
        const auto paneOwner = payload.isObject() ? payload.get("pane_owner", Json::Value{}) : Json::Value{};
        if (params.get("agent", params.get("cli_source", "")).asString() == "opencode" &&
            paneOwner.isBool() && !paneOwner.asBool())
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
        if (!AcceptsAgentSessionEvents(sessionId))
        {
            return;
        }
        const auto agentSessionId = winrt::to_hstring(params.get("agent_session_id", "").asString());
        const auto agent = winrt::to_hstring(params.get("agent", params.get("cli_source", "")).asString());
        if ((!ended || unbound) && (agentSessionId.empty() || std::wstring_view{ agentSessionId }.starts_with(L"sidekick-") || agent.empty()))
        {
            return;
        }

        uint64_t contentId{};
        {
            std::lock_guard lock{ _mutex };
            for (const auto& [id, content] : _content)
            {
                const auto connection = content.core.Core().Connection();
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
        auto& policy = _agentBindings[contentId];
        if (unbound)
        {
            if (agent == L"opencode" && policy.agent == agent && policy.agentSessionId == agentSessionId)
            {
                policy = {};
            }
        }
        else if (ended)
        {
            if (agentSessionId.empty() || agentSessionId == policy.agentSessionId)
            {
                policy = {};
            }
        }
        else if (!(prompt && agent == L"copilot" && !policy.agentSessionId.empty()))
        {
            policy.agentSessionId = agentSessionId;
            policy.eventJson = eventJson;
            policy.agent = agent;
            std::lock_guard lock{ _mutex };
            if (const auto content = _content.find(contentId);
                content != _content.end() && !content->second.nativeAgentProviderId.empty() &&
                !std::wstring_view{ content->second.nativeAgentProviderId }.starts_with(L"custom:"))
            {
                content->second.nativeAgentProviderId = agent;
            }
        }
    }

    void ContentManager::KeepTab(const winrt::TerminalApp::TerminalPage& owner, const winrt::TerminalApp::Tab& tab)
    {
        _CheckThread();
        THROW_HR_IF(E_INVALIDARG, !owner || !tab);
        const auto impl = winrt::get_self<Tab>(tab);
        const winrt::guid id{ impl->StableId() };
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, !impl->KeepRunning() || _keptGroups.contains(id));
        const winrt::Windows::Foundation::Rect bounds{
            0, 0, static_cast<float>(owner.ActualWidth()), static_cast<float>(owner.ActualHeight())
        };
        _keptGroups.emplace(id, KeptGroup{ owner, tab, bounds, false, SharedWta::Instance().AcquireKeepRunningLease() });
        _NotifyKeptSessionsChanged();
    }

    bool ContentManager::IsKeptContent(const uint64_t contentId)
    {
        _CheckThread();
        for (const auto& [id, group] : _keptGroups)
        {
            const auto root = winrt::get_self<Tab>(group.tab)->GetRootPane();
            if (root && root->WalkTree([&](const auto& pane) -> std::shared_ptr<Pane> {
                    const auto control = pane->GetTerminalControl();
                    return control && control.ContentId() == contentId ? pane : nullptr;
                }))
            {
                return true;
            }
        }
        return false;
    }

    bool ContentManager::HasKeptSessions()
    {
        _CheckThread();
        return !_keptGroups.empty();
    }

    winrt::guid ContentManager::KeptGroupForPane(const winrt::guid& sessionId)
    {
        _CheckThread();
        if (sessionId != winrt::guid{})
        {
            for (const auto& [id, group] : _keptGroups)
            {
                const auto root = winrt::get_self<Tab>(group.tab)->GetRootPane();
                if (root && root->FindPaneBySessionId(sessionId))
                {
                    return id;
                }
            }
        }
        return {};
    }

    winrt::Windows::Foundation::Collections::IMapView<winrt::guid, winrt::hstring> ContentManager::KeptGroups()
    {
        _CheckThread();
        auto result = winrt::single_threaded_map<winrt::guid, winrt::hstring>();
        for (const auto& [id, group] : _keptGroups)
        {
            if (!group.restoring)
            {
                result.Insert(id, group.tab.Title());
            }
        }
        return result.GetView();
    }

    winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::TerminalPage> ContentManager::KeptPages()
    {
        _CheckThread();
        std::vector<winrt::TerminalApp::TerminalPage> pages;
        for (const auto& [id, group] : _keptGroups)
        {
            if (std::ranges::find(pages, group.owner) == pages.end())
            {
                pages.emplace_back(group.owner);
            }
        }
        return winrt::single_threaded_vector(std::move(pages)).GetView();
    }

    winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::Tab> ContentManager::KeptTabs(const winrt::TerminalApp::TerminalPage& owner)
    {
        _CheckThread();
        std::vector<winrt::TerminalApp::Tab> tabs;
        for (const auto& [id, group] : _keptGroups)
        {
            if (group.owner == owner)
            {
                tabs.emplace_back(group.tab);
            }
        }
        return winrt::single_threaded_vector(std::move(tabs)).GetView();
    }

    winrt::TerminalApp::TerminalPage ContentManager::KeptGroupOwner(const winrt::guid& groupId)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        return it == _keptGroups.end() ? nullptr : it->second.owner;
    }

    winrt::Windows::Foundation::Collections::IVectorView<winrt::TerminalApp::IPaneContent> ContentManager::KeptPanes()
    {
        _CheckThread();
        std::vector<winrt::TerminalApp::IPaneContent> panes;
        for (const auto& [id, group] : _keptGroups)
        {
            if (!group.restoring)
            {
                winrt::get_self<Tab>(group.tab)->GetRootPane()->WalkTree([&](const auto& pane) {
                    if (auto content = pane->GetContent())
                    {
                        panes.emplace_back(std::move(content));
                    }
                });
            }
        }
        return winrt::single_threaded_vector(std::move(panes)).GetView();
    }

    winrt::TerminalApp::Tab ContentManager::BeginReattachKeptGroup(const winrt::guid& groupId)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end());
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, it->second.restoring);
        it->second.restoring = true;
        return it->second.tab;
    }

    winrt::Windows::Foundation::Rect ContentManager::KeptGroupBounds(const winrt::guid& groupId)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end());
        return it->second.bounds;
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
        }
        _NotifyKeptSessionsChanged();
    }

    void ContentManager::NotifyPaneWindowChanged(const winrt::guid& paneId, const uint64_t windowId)
    {
        _CheckThread();
        PaneWindowChanged.raise(paneId, windowId);
    }

    void ContentManager::DiscardKeptGroup(const winrt::guid& groupId)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end());
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, it->second.restoring);
        it->second.restoring = true;
        const auto owner = it->second.owner;
        const auto tab = it->second.tab;
        try
        {
            const auto page = winrt::get_self<TerminalPage>(owner);
            const auto impl = winrt::get_self<Tab>(tab);
            page->_NotifyPanesClosing(impl->GetRootPane());
            page->_NotifyAgentTabClosed(impl->StableId());
        }
        CATCH_LOG()
        auto group = std::move(_keptGroups.at(groupId));
        _keptGroups.erase(groupId);
        _keepRunningTabs.erase(winrt::get_self<Tab>(group.tab)->StableId());
        const auto notify = wil::scope_exit([&]() noexcept {
            group.lease.Retire();
            _NotifyKeptSessionsChanged();
        });
        group.tab.Shutdown();
    }

    void ContentManager::DiscardAllKeptGroups()
    {
        // Closing a tab can synchronously change other groups. Snapshot the IDs
        // and recheck ownership so restored or claimed tabs are never closed.
        for (const auto& group : KeptGroups())
        {
            try
            {
                const auto id = group.Key();
                const auto it = _keptGroups.find(id);
                if (it != _keptGroups.end() && !it->second.restoring)
                {
                    DiscardKeptGroup(id);
                }
            }
            CATCH_LOG()
        }
    }

    void ContentManager::_NotifyKeptSessionsChanged() noexcept
    {
        try
        {
            KeptSessionsChanged.raise(*this, nullptr);
        }
        CATCH_LOG()
        try
        {
            KeepRunningTabsChanged.raise(*this, nullptr);
        }
        CATCH_LOG()
    }
}
