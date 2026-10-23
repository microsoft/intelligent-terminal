// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ContentManager.h"
#include "ContentManager.g.cpp"
#include "TerminalPage.h"
#include "../inc/AgentPaneBackend.h"
#include "../inc/AgentPaneRestore.h"
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
using ::Microsoft::Terminal::Settings::Model::AgentPaneBackend;
using ::Microsoft::Terminal::Settings::Model::AgentPaneBackendSource;
namespace AgentRegistry = ::Microsoft::Terminal::Settings::Model::AgentRegistry;

namespace winrt::TerminalApp::implementation
{
    static ContentManager::PaneSessionIdentity _PaneSessionIdentity(
        const ContentManager::PaneAgentSession& session,
        const winrt::hstring& source,
        const winrt::hstring& distro,
        const winrt::hstring& universe)
    {
        ContentManager::PaneSessionIdentity identity{ session.sessionId, session.agent, source, distro, universe };
        if (const auto backend = AgentPaneBackend::Parse(std::wstring_view{ session.backend }))
        {
            if (identity.source.empty())
            {
                identity.source = backend->source == AgentPaneBackendSource::Wsl ? L"wsl" : L"host";
                identity.distro = winrt::hstring{ backend->wslDistro };
            }
            else if (identity.source == L"wsl" && identity.distro.empty() && backend->source == AgentPaneBackendSource::Wsl)
            {
                identity.distro = winrt::hstring{ backend->wslDistro };
            }
        }
        return identity;
    }

    static void _ResolvePaneHookScope(const ContentManager::PaneMetadata& metadata,
                                      ContentManager::PaneSessionIdentity& identity,
                                      const bool universeProvided,
                                      const bool started = false)
    {
        if (!metadata.HasSession() || !metadata.identityQualified || metadata.session.agent != identity.agent ||
            (metadata.session.sessionId != identity.sessionId &&
             !(started && identity.sessionId.empty() && metadata.awaitingSessionStart)))
        {
            return;
        }
        if (identity.source.empty() && identity.distro.empty())
        {
            identity.source = metadata.source;
            identity.distro = metadata.wslDistro;
        }
        if (!universeProvided && identity.source == metadata.source && identity.distro == metadata.wslDistro)
        {
            identity.universe = metadata.universe;
        }
    }

    static void _ClearPaneSession(ContentManager::PaneMetadata& metadata)
    {
        metadata.session = {};
        metadata.source.clear();
        metadata.wslDistro.clear();
        metadata.universe.clear();
        metadata.identityQualified = false;
        metadata.interactiveResume = false;
        metadata.explicitNativeAgent = false;
        metadata.resumeCommandlineTrusted = false;
        metadata.awaitingSessionStart = false;
        metadata.sessionStartConfirmed = false;
        metadata.eventJson.clear();
    }

    static void _RetirePaneSession(ContentManager::PaneMetadata& metadata)
    {
        if (!metadata.session.sessionId.empty())
        {
            metadata.supersededSessions.emplace_back(metadata.Identity());
        }
        _ClearPaneSession(metadata);
    }

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
            auto& metadata = _paneMetadata[connection.SessionId()];
            metadata = {};
            metadata.contentId = content.Id();
            metadata.launchProvider = providerId;
            metadata.session.agent = providerId;
            metadata.state = providerId.empty() ? PaneAgentState::Unknown : PaneAgentState::Starting;
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
        for (const auto& [_, metadata] : _paneMetadata)
        {
            if (metadata.contentId == contentId)
            {
                return metadata.explicitNativeAgent && metadata.HasAgent() ? metadata.session.agent : metadata.launchProvider;
            }
        }
        return {};
    }

    winrt::hstring ContentManager::NativeAgentProviderIdForPane(const winrt::guid& paneId) const
    {
        std::lock_guard lock{ _mutex };
        const auto it = _paneMetadata.find(paneId);
        return it == _paneMetadata.end() ? winrt::hstring{} :
               it->second.explicitNativeAgent && it->second.HasAgent() ? it->second.session.agent : it->second.launchProvider;
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
            const auto metadata = _paneMetadata.find(it->second.core.Core().Connection().SessionId());
            if (metadata != _paneMetadata.end())
            {
                metadata->second.explicitNativeAgent = !providerId.empty();
            }
        }
    }

    bool ContentManager::RefineNativeAgentLaunchLocation(
        const uint64_t contentId, const winrt::hstring& providerId, const std::string& location)
    {
        std::lock_guard lock{ _mutex };
        if (const auto it = _content.find(contentId);
            it != _content.end() && !location.empty() && !it->second.agentHelper &&
            _paneMetadata.contains(it->second.core.Core().Connection().SessionId()) &&
            ::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(
                _paneMetadata.at(it->second.core.Core().Connection().SessionId()).launchProvider, providerId) &&
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
            const auto metadata = _paneMetadata.find(it->second.core.Core().Connection().SessionId());
            const auto provider = metadata != _paneMetadata.end() && metadata->second.explicitNativeAgent &&
                                          metadata->second.HasAgent() && !agentSessionId.empty() ?
                                      metadata->second.session.agent :
                                      metadata != _paneMetadata.end() ? metadata->second.launchProvider : winrt::hstring{};
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
                if (const auto metadata = _paneMetadata.find(it->second.core.Core().Connection().SessionId());
                    metadata != _paneMetadata.end())
                {
                    _RetirePaneSession(metadata->second);
                    metadata->second.state = PaneAgentState::Ended;
                }
            }
        }
    }

    void ContentManager::SetNativeAgentSessionEventsEnabled(const bool enabled)
    {
        _CheckThread();
        std::lock_guard lock{ _mutex };
        if (_nativeAgentSessionEventsEnabled == enabled)
        {
            return;
        }
        for (auto& [id, content] : _content)
        {
            content.observation = {};
        }
        _nativeAgentSessionEventsEnabled = enabled;
        if (!enabled)
        {
            for (auto& [_, metadata] : _paneMetadata)
            {
                const auto content = _content.find(metadata.contentId);
                if (content == _content.end() || !content->second.agentHelper)
                {
                    metadata.eventJson.clear();
                    if (!metadata.explicitNativeAgent)
                    {
                        const auto recoverable = !metadata.connectionClosed && metadata.state != PaneAgentState::Ended;
                        const auto retainLaunch = recoverable && !metadata.launchProvider.empty();
                        _ClearPaneSession(metadata);
                        metadata.state = retainLaunch ? PaneAgentState::Starting :
                                         recoverable ? PaneAgentState::Unknown : PaneAgentState::Ended;
                        metadata.activity.clear();
                        if (retainLaunch)
                        {
                            metadata.session.agent = metadata.launchProvider;
                        }
                    }
                }
            }
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
            std::vector<winrt::guid> removed;
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
                std::erase_if(_paneMetadata, [&](const auto& item) {
                    if (item.second.contentId == contentId)
                    {
                        removed.emplace_back(item.first);
                        return true;
                    }
                    return false;
                });
            }
            for (const auto& paneId : removed)
            {
                PaneMetadataChanged.raise(*this, paneId);
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
        }
    }

    void ContentManager::_CheckThread() const
    {
        THROW_HR_IF(RPC_E_WRONG_THREAD, !_dispatcher || !_dispatcher.HasThreadAccess());
    }

    winrt::hstring ContentManager::AgentSessionEvent(const uint64_t contentId)
    {
        _CheckThread();
        std::lock_guard lock{ _mutex };
        for (const auto& [_, metadata] : _paneMetadata)
        {
            if (metadata.contentId == contentId)
            {
                return metadata.eventJson;
            }
        }
        return {};
    }

    std::optional<ContentManager::PaneMetadata> ContentManager::MetadataForPane(const winrt::guid& paneId) const
    {
        std::lock_guard lock{ _mutex };
        const auto it = _paneMetadata.find(paneId);
        return it == _paneMetadata.end() ? std::nullopt : std::optional{ it->second };
    }

    void ContentManager::ResetPaneConnection(const uint64_t contentId, const winrt::guid& paneId, const std::optional<PaneMetadata>& resumeBinding)
    {
        winrt::guid previousId{};
        {
            std::lock_guard lock{ _mutex };
            const auto it = std::ranges::find_if(_paneMetadata, [&](const auto& item) {
                return item.second.contentId == contentId;
            });
            THROW_HR_IF(E_INVALIDARG, it == _paneMetadata.end());
            previousId = it->first;
            PaneMetadata metadata;
            metadata.contentId = contentId;
            metadata.launchProvider = it->second.launchProvider;
            metadata.session.agent = metadata.launchProvider;
            metadata.state = metadata.launchProvider.empty() ? PaneAgentState::Unknown : PaneAgentState::Starting;
            if (resumeBinding && resumeBinding->contentId == contentId &&
                resumeBinding->explicitNativeAgent && !resumeBinding->session.sessionId.empty())
            {
                metadata.session = resumeBinding->session;
                metadata.source = resumeBinding->source;
                metadata.wslDistro = resumeBinding->wslDistro;
                metadata.universe = resumeBinding->universe;
                metadata.identityQualified = resumeBinding->identityQualified;
                metadata.interactiveResume = resumeBinding->interactiveResume;
                metadata.explicitNativeAgent = true;
                metadata.resumeCommandlineTrusted = resumeBinding->resumeCommandlineTrusted;
                metadata.awaitingSessionStart = true;
                metadata.state = PaneAgentState::Starting;
            }
            _paneMetadata.erase(it);
            _paneMetadata.insert_or_assign(paneId, std::move(metadata));
        }
        PaneMetadataChanged.raise(*this, previousId);
        if (previousId != paneId)
        {
            PaneMetadataChanged.raise(*this, paneId);
        }
    }

    void ContentManager::BindPaneSession(const winrt::guid& paneId, const PaneAgentSession& session, const winrt::hstring& source, const winrt::hstring& distro, const winrt::hstring& universe, const bool interactiveResume)
    {
        {
            std::lock_guard lock{ _mutex };
            const auto it = _paneMetadata.find(paneId);
            THROW_HR_IF(E_INVALIDARG, it == _paneMetadata.end());
            auto& metadata = it->second;
            if (metadata.connectionClosed)
            {
                return;
            }
            auto identity = _PaneSessionIdentity(session, source, distro, universe);
            if (identity.source.empty() && metadata.session.sessionId == session.sessionId &&
                metadata.session.agent == session.agent)
            {
                identity.source = metadata.source;
                identity.distro = metadata.wslDistro;
                if (universe.empty())
                {
                    identity.universe = metadata.universe;
                }
            }
            const auto sameIdentity = metadata.Identity().Matches(identity);
            if (!sameIdentity)
            {
                _RetirePaneSession(metadata);
            }
            auto resolved = session;
            if (resolved.resumeCommandline.empty() && sameIdentity)
            {
                resolved.resumeCommandline = metadata.session.resumeCommandline;
            }
            if (sameIdentity)
            {
                if (resolved.backend.empty())
                {
                    resolved.backend = metadata.session.backend;
                }
                if (resolved.cwd.empty())
                {
                    resolved.cwd = metadata.session.cwd;
                }
            }
            metadata.resumeCommandlineTrusted = !session.resumeCommandline.empty() ||
                                                (sameIdentity && metadata.resumeCommandlineTrusted);
            metadata.session = std::move(resolved);
            std::erase_if(metadata.supersededSessions, [&](const auto& previous) { return previous.Matches(identity); });
            if (metadata.state != PaneAgentState::Failed)
            {
                metadata.state = session.sessionId.empty() ? PaneAgentState::Starting : PaneAgentState::Running;
            }
            metadata.source = identity.source;
            metadata.wslDistro = identity.distro;
            metadata.universe = identity.universe;
            metadata.identityQualified = !identity.source.empty();
            metadata.interactiveResume = interactiveResume;
            metadata.explicitNativeAgent = true;
            metadata.awaitingSessionStart = !session.sessionId.empty();
            metadata.sessionStartConfirmed = false;
        }
        PaneMetadataChanged.raise(*this, paneId);
    }

    void ContentManager::EndPaneSession(const winrt::guid& paneId, const winrt::hstring& sessionId, const winrt::hstring& agent, const winrt::hstring& source, const winrt::hstring& distro, const std::optional<winrt::hstring> universe, const bool connectionStillRunning)
    {
        {
            std::lock_guard lock{ _mutex };
            const auto it = _paneMetadata.find(paneId);
            if (it == _paneMetadata.end())
            {
                return;
            }
            auto& metadata = it->second;
            if (connectionStillRunning &&
                (metadata.connectionClosed || !metadata.HasSession() || sessionId.empty() ||
                 metadata.session.sessionId != sessionId || metadata.session.agent != agent))
            {
                return;
            }
            PaneSessionIdentity identity{ sessionId, agent, source, distro, universe.value_or(winrt::hstring{}) };
            _ResolvePaneHookScope(metadata, identity, universe.has_value());
            if (!agent.empty() && !metadata.session.agent.empty() && metadata.session.agent != agent)
            {
                return;
            }
            if (!sessionId.empty() &&
                ((!metadata.session.sessionId.empty() && metadata.session.sessionId != sessionId) ||
                 metadata.IsSuperseded(identity)))
            {
                return;
            }
            if (!identity.source.empty() && metadata.identityQualified &&
                (metadata.source != identity.source || metadata.wslDistro != identity.distro || metadata.universe != identity.universe))
            {
                return;
            }
            const auto provider = metadata.launchProvider.empty() ? metadata.session.agent : metadata.launchProvider;
            const auto failed = metadata.state == PaneAgentState::Failed;
            _RetirePaneSession(metadata);
            metadata.state = connectionStillRunning ? (failed ? PaneAgentState::Failed : PaneAgentState::Starting) : PaneAgentState::Ended;
            if (connectionStillRunning)
            {
                metadata.session.agent = provider;
            }
            metadata.activity.clear();
        }
        PaneMetadataChanged.raise(*this, paneId);
    }

    void ContentManager::SetPaneConnectionState(const winrt::guid& paneId, const bool failed)
    {
        {
            std::lock_guard lock{ _mutex };
            const auto it = _paneMetadata.find(paneId);
            if (it == _paneMetadata.end() || it->second.connectionClosed)
            {
                return;
            }
            auto& metadata = it->second;
            if (!failed)
            {
                metadata.connectionClosed = true;
                _RetirePaneSession(metadata);
                metadata.state = PaneAgentState::Ended;
                metadata.activity.clear();
            }
            else
            {
                if (!metadata.HasAgent())
                {
                    return;
                }
                metadata.state = PaneAgentState::Failed;
                metadata.activity = L"Error";
            }
        }
        PaneMetadataChanged.raise(*this, paneId);
    }

    void ContentManager::UpdatePaneSessionIdentity(const winrt::guid& paneId, const PaneAgentSession& session, const winrt::hstring& source, const winrt::hstring& distro, const winrt::hstring& universe)
    {
        {
            std::lock_guard lock{ _mutex };
            const auto it = _paneMetadata.find(paneId);
            if (it == _paneMetadata.end())
            {
                return;
            }
            auto& metadata = it->second;
            const auto identity = _PaneSessionIdentity(session, source, distro, universe);
            if (metadata.connectionClosed ||
                (!metadata.HasAgent() && metadata.state != PaneAgentState::Unknown) ||
                metadata.IsSuperseded(identity) ||
                (!metadata.session.sessionId.empty() && metadata.session.sessionId != session.sessionId) ||
                (!metadata.session.agent.empty() && metadata.session.agent != session.agent) ||
                (metadata.identityQualified &&
                 (metadata.source != identity.source || metadata.wslDistro != identity.distro || metadata.universe != identity.universe)))
            {
                return;
            }
            auto resolved = session;
            if (resolved.resumeCommandline.empty())
            {
                resolved.resumeCommandline = metadata.session.resumeCommandline;
            }
            if (resolved.backend.empty())
            {
                resolved.backend = metadata.session.backend;
            }
            if (resolved.cwd.empty())
            {
                resolved.cwd = metadata.session.cwd;
            }
            if (!session.resumeCommandline.empty())
            {
                metadata.resumeCommandlineTrusted = true;
            }
            if (metadata.identityQualified && metadata.session.sessionId == resolved.sessionId &&
                metadata.session.agent == resolved.agent && metadata.session.resumeCommandline == resolved.resumeCommandline &&
                metadata.session.backend == resolved.backend && metadata.session.cwd == resolved.cwd)
            {
                return;
            }
            metadata.session = std::move(resolved);
            if (metadata.state != PaneAgentState::Failed)
            {
                metadata.state = PaneAgentState::Running;
            }
            metadata.source = identity.source;
            metadata.wslDistro = identity.distro;
            metadata.universe = identity.universe;
            metadata.identityQualified = !identity.source.empty();
        }
        PaneMetadataChanged.raise(*this, paneId);
    }

    void ContentManager::OnPaneAgentSessionChanged(const winrt::hstring& eventJson)
    {
        OnPaneAgentSessionChanged(eventJson, true);
    }

    void ContentManager::OnPaneAgentSessionChanged(const winrt::hstring& eventJson, const bool trustResumeCommandline)
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
        if (!trustResumeCommandline)
        {
            event["params"].removeMember("resume_commandline");
        }
        const auto bindingEventJson = trustResumeCommandline ? eventJson :
                                          winrt::to_hstring(Json::writeString(Json::StreamWriterBuilder{}, event));
        const auto& params = event["params"];
        const auto name = params.get("event", "").asString();
        const auto ended = name == "agent.session.end" || name == "agent.session.stopped";
        const auto started = name == "agent.session.start" || name == "agent.session.started";
        const auto prompt = name == "agent.prompt.submit";
        const auto unbound = name == "agent.session.unbind";
        const auto activity = name == "agent.stop"          ? L"Idle" :
                              name == "agent.error"         ? L"Error" :
                              name == "agent.notification"  ? L"Attention" :
                              name == "agent.tool.starting" ? L"Working" :
                                                              L"";
        if (!ended && !started && !prompt && !unbound && !*activity && !name.empty())
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
        if (std::wstring_view{ agentSessionId }.starts_with(L"sidekick-") ||
            (!ended && agent.empty()) ||
            (unbound && (agent != L"opencode" || agentSessionId.empty())))
        {
            return;
        }

        if (params.isMember("agent_source"))
        {
            THROW_HR_IF(E_INVALIDARG, !params["agent_source"].isString());
            const auto source = params["agent_source"].asString();
            THROW_HR_IF(E_INVALIDARG, source != "host" && source != "wsl");
        }
        if (params.isMember("wsl_distro"))
        {
            THROW_HR_IF(E_INVALIDARG, !params["wsl_distro"].isString());
        }
        if (params.isMember("session_universe"))
        {
            THROW_HR_IF(E_INVALIDARG, !params["session_universe"].isString());
        }

        winrt::hstring bindingBackend;
        if (params.isMember("agent_backend"))
        {
            THROW_HR_IF(E_INVALIDARG, !params["agent_backend"].isString());
            bindingBackend = winrt::to_hstring(params["agent_backend"].asString());
            const auto backend = AgentPaneBackend::Parse(std::wstring_view{ bindingBackend });
            THROW_HR_IF(E_INVALIDARG, !backend || !AgentRegistry::AgentIdEquals(backend->agentId, std::wstring_view{ agent }));
        }
        else if (params.isMember("wsl_distro"))
        {
            THROW_HR_IF(E_INVALIDARG, !params["wsl_distro"].isString());
            const auto distro = winrt::to_hstring(params["wsl_distro"].asString());
            if (!distro.empty())
            {
                bindingBackend = winrt::hstring{ AgentPaneBackend::Wsl(std::wstring_view{ distro }, std::wstring_view{ agent }) };
                THROW_HR_IF(E_INVALIDARG, !AgentPaneBackend::Parse(std::wstring_view{ bindingBackend }));
            }
        }
        auto identity = _PaneSessionIdentity(
            { agentSessionId, agent, {}, bindingBackend },
            winrt::to_hstring(params.get("agent_source", "").asString()),
            winrt::to_hstring(params.get("wsl_distro", "").asString()),
            winrt::to_hstring(params.get("session_universe", "").asString()));
        if (ended || unbound)
        {
            EndPaneSession(sessionId, agentSessionId, agent, identity.source, identity.distro, params.isMember("session_universe") ? std::optional<winrt::hstring>{ identity.universe } : std::nullopt, unbound);
            return;
        }
        const auto cwd = params.isMember("cwd") ? params["cwd"] :
                         payload.isObject()     ? payload.get("cwd", Json::Value{}) :
                                                  Json::Value{};
        const auto bindingCwd = cwd.isString() ? winrt::to_hstring(cwd.asString()) : winrt::hstring{};
        {
            std::lock_guard lock{ _mutex };
            const auto it = _paneMetadata.find(sessionId);
            if (it == _paneMetadata.end())
            {
                return;
            }
            auto& metadata = it->second;
            if (metadata.connectionClosed)
            {
                return;
            }
            _ResolvePaneHookScope(metadata, identity, params.isMember("session_universe"), started);
            if (!started &&
                ((!agentSessionId.empty() &&
                  ((!metadata.session.sessionId.empty() && metadata.session.sessionId != agentSessionId) ||
                   metadata.IsSuperseded(identity))) ||
                 (!metadata.session.sessionId.empty() && !metadata.session.agent.empty() && metadata.session.agent != agent)))
            {
                return;
            }
            const auto hadSession = !metadata.session.sessionId.empty();
            const auto& source = identity.source;
            const auto& distro = identity.distro;
            const auto& universe = identity.universe;
            if (!started && !source.empty() && metadata.identityQualified &&
                (metadata.source != source || metadata.wslDistro != distro || metadata.universe != universe))
            {
                return;
            }
            if (started || prompt || name.empty())
            {
                const auto preservesPendingResume = started && metadata.awaitingSessionStart &&
                                                    !metadata.session.sessionId.empty() && metadata.session.agent == agent &&
                                                    metadata.session.sessionId != agentSessionId &&
                                                    (source.empty() || !metadata.identityQualified ||
                                                     (metadata.source == source && metadata.wslDistro == distro)) &&
                                                    (!params.isMember("session_universe") || metadata.universe == universe);
                if (preservesPendingResume && !agentSessionId.empty())
                {
                    // Copilot may start a bootstrap session before switching to the selected resume target.
                    return;
                }
                if (started)
                {
                    std::erase_if(metadata.supersededSessions, [&](const auto& previous) { return previous.Matches(identity); });
                }
                const auto startsNewIdentity = started && !preservesPendingResume && !metadata.Identity().Matches(identity);
                if (startsNewIdentity)
                {
                    _RetirePaneSession(metadata);
                }
                if (!agentSessionId.empty() &&
                    !metadata.IsSuperseded(identity))
                {
                    metadata.session.sessionId = agentSessionId;
                    if (started)
                    {
                        metadata.awaitingSessionStart = false;
                        metadata.sessionStartConfirmed = true;
                    }
                    if (const auto resume = winrt::to_hstring(params.get("resume_commandline", "").asString()); trustResumeCommandline && !resume.empty())
                    {
                        metadata.session.resumeCommandline = resume;
                        metadata.resumeCommandlineTrusted = true;
                        if (name.empty())
                        {
                            metadata.awaitingSessionStart = !metadata.sessionStartConfirmed;
                        }
                    }
                    if (!(prompt && agent == L"copilot" && hadSession))
                    {
                        metadata.eventJson = bindingEventJson;
                    }
                }
                metadata.session.agent = agent;
                if (bindingBackend.empty())
                {
                    if (const auto content = _content.find(metadata.contentId); content != _content.end())
                    {
                        const auto shellName = content->second.core.Core().ShellName();
                        const std::wstring_view shell{ shellName };
                        if (shell.starts_with(L"wsl:"))
                        {
                            bindingBackend = winrt::hstring{ AgentPaneBackend::Wsl(shell.substr(4), std::wstring_view{ agent }) };
                        }
                    }
                }
                if (!bindingBackend.empty())
                {
                    metadata.session.backend = bindingBackend;
                }
                if (!bindingCwd.empty())
                {
                    metadata.session.cwd = bindingCwd;
                }
                if (metadata.session.resumeCommandline.empty() && !metadata.session.backend.empty())
                {
                    metadata.session.resumeCommandline = winrt::hstring{
                        ::Microsoft::Terminal::AgentPaneRestore::BuildResumeCommandline(
                            metadata.session.backend, metadata.session.sessionId, metadata.session.cwd)
                    };
                }
                if (!source.empty())
                {
                    metadata.source = source;
                    metadata.wslDistro = distro;
                    metadata.universe = universe;
                    metadata.identityQualified = true;
                }
                if (!metadata.session.backend.empty() &&
                    (!metadata.identityQualified || (metadata.source == L"wsl" && metadata.wslDistro.empty())))
                {
                    if (const auto backend = AgentPaneBackend::Parse(std::wstring_view{ metadata.session.backend }))
                    {
                        if (!metadata.identityQualified ||
                            (metadata.source == L"wsl" && metadata.wslDistro.empty() && backend->source == AgentPaneBackendSource::Wsl))
                        {
                            metadata.source = backend->source == AgentPaneBackendSource::Wsl ? L"wsl" : L"host";
                            metadata.wslDistro = winrt::hstring{ backend->wslDistro };
                            metadata.identityQualified = true;
                        }
                    }
                }
                if (metadata.state != PaneAgentState::Failed)
                {
                    metadata.state = metadata.session.sessionId.empty() ? PaneAgentState::Starting : PaneAgentState::Running;
                    if (prompt)
                    {
                        metadata.activity = L"Working";
                    }
                    else if (!hadSession || startsNewIdentity || metadata.activity.empty())
                    {
                        metadata.activity = L"Idle";
                    }
                }
                if (!metadata.launchProvider.empty() && !std::wstring_view{ metadata.launchProvider }.starts_with(L"custom:"))
                {
                    metadata.launchProvider = agent;
                }
            }
            else if (metadata.HasAgent())
            {
                if (name == "agent.notification" &&
                    params["payload"].get("notification_type", "").asString() == "idle_prompt")
                {
                    return;
                }
                if (name != "agent.stop" || metadata.activity != L"Error")
                {
                    metadata.activity = activity;
                }
            }
        }
        PaneMetadataChanged.raise(*this, sessionId);
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
