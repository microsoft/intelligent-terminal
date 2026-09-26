// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ContentManager.h"
#include "ContentManager.g.cpp"
#include "TerminalPage.h"

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
        auto& policy = _agentBindings[contentId];
        if (ended)
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
        }
    }

    void ContentManager::KeepTab(const winrt::TerminalApp::TerminalPage& owner, const winrt::TerminalApp::Tab& tab)
    {
        _CheckThread();
        THROW_HR_IF(E_INVALIDARG, !owner || !tab);
        const auto impl = winrt::get_self<Tab>(tab);
        const winrt::guid id{ impl->StableId() };
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, !impl->KeepRunning() || _keptGroups.contains(id));
        _keptGroups.emplace(id, KeptGroup{ owner, tab, false, SharedWta::Instance().AcquireKeepRunningLease() });
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

    winrt::TerminalApp::Tab ContentManager::BeginReattachKeptGroup(const winrt::guid& groupId)
    {
        _CheckThread();
        const auto it = _keptGroups.find(groupId);
        THROW_HR_IF(E_INVALIDARG, it == _keptGroups.end());
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, it->second.restoring);
        it->second.restoring = true;
        return it->second.tab;
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
        const auto notify = wil::scope_exit([&]() noexcept {
            group.lease.Retire();
            _NotifyKeptSessionsChanged();
        });
        group.tab.Shutdown();
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
