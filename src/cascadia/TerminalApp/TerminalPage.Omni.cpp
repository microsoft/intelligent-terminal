// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TerminalPage.h"
#include "CommandPalette.h"
#include "AgentPaneLog.h"
#include "TerminalPaneContent.h"
#include "../inc/OmniPlannerProcess.h"
#include "../../types/inc/utils.hpp"

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace
{
    void requireKeys(const Json::Value& value, std::initializer_list<std::string_view> allowed)
    {
        THROW_HR_IF(E_INVALIDARG, !value.isObject());
        for (const auto& key : value.getMemberNames())
        {
            THROW_HR_IF(E_INVALIDARG, std::find(allowed.begin(), allowed.end(), key) == allowed.end());
        }
    }

    std::optional<float> optionalNumber(const Json::Value& value, const char* key, float minimum, float maximum)
    {
        if (!value.isMember(key))
        {
            return std::nullopt;
        }
        THROW_HR_IF(E_INVALIDARG, !value[key].isNumeric());
        const auto number = value[key].asFloat();
        THROW_HR_IF(E_INVALIDARG, !std::isfinite(number) || number < minimum || number > maximum);
        return number;
    }

    std::string jsonString(const Json::Value& value)
    {
        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        return Json::writeString(writer, value);
    }
}

namespace winrt::TerminalApp::implementation
{
    safe_void_coroutine TerminalPage::_WarmOmniPlanner()
    {
        if (!_omniPlanner)
        {
            _omniPlanner = std::make_shared<::Microsoft::Terminal::WtaProcess::OmniPlannerProcess>();
        }
        const auto planner = _omniPlanner;
        if (!planner->BeginWarmup())
        {
            co_return;
        }
        const auto finish = wil::scope_exit([&]() { planner->EndWarmup(); });
        const auto path = std::wstring{ _DetectWtaPath() };
        if (path.empty())
        {
            co_return;
        }
        co_await winrt::resume_background();
        const auto result = planner->Exchange(path, "null");
        _agentPaneLog(result.completed && result.exitCode == 0 ? "omni_planner_warmed" : "omni_planner_warmup_failed");
    }

    TerminalPage::OmniTarget TerminalPage::_CaptureOmniTarget() const
    {
        OmniTarget target;
        const auto tab = _GetFocusedTabImpl();
        const auto source = _SourceTerminalPaneForTab(tab);
        THROW_HR_IF(E_ILLEGAL_METHOD_CALL, !tab || !source || !source->GetTerminalControl());
        target.tabId = tab->StableId();
        target.settings = _settings;
        target.sourceContentId = gsl::narrow<uint32_t>(source->GetTerminalControl().ContentId());
        for (const auto& candidate : _tabs)
        {
            if (const auto impl = _GetTabImpl(candidate))
            {
                target.tabIds.emplace_back(impl->StableId());
            }
        }
        tab->GetRootPane()->WalkTree([&](const auto& pane) {
            if (const auto content = pane->GetContent().template try_as<winrt::TerminalApp::TerminalPaneContent>())
            {
                const auto control = winrt::get_self<TerminalPaneContent>(content)->GetTermControl();
                target.panes.emplace_back(control, control.Connection());
            }
        });
        return target;
    }

    Json::Value TerminalPage::_ExecuteOmniOperation(const Json::Value& plan, const OmniTarget& target)
    {
        THROW_HR_IF(E_CHANGED_STATE, _settings != target.settings);
        winrt::com_ptr<Tab> tab;
        for (const auto& candidate : _tabs)
        {
            const auto impl = _GetTabImpl(candidate);
            if (impl && impl->StableId() == target.tabId)
            {
                tab = impl;
                break;
            }
        }
        THROW_HR_IF(E_CHANGED_STATE, !tab);
        const auto source = tab->GetRootPane()->FindPaneByContentId(target.sourceContentId);
        THROW_HR_IF(E_CHANGED_STATE, !source);
        const auto captured = std::find_if(target.panes.begin(), target.panes.end(), [&](const auto& entry) {
            return entry.first.ContentId() == target.sourceContentId;
        });
        THROW_HR_IF(E_CHANGED_STATE, captured == target.panes.end() || source->GetTerminalControl() != captured->first || captured->first.Connection() != captured->second);
        THROW_HR_IF(E_INVALIDARG, !plan["kind"].isString());
        const auto kind = plan["kind"].asString();
        Json::Value result{ Json::objectValue };
        result["kind"] = kind;
        if (kind == "appearance")
        {
            requireKeys(plan, { "kind", "scope", "opacity", "acrylic", "font_size", "color_scheme" });
            THROW_HR_IF(E_INVALIDARG, !plan["scope"].isString());
            const auto scope = plan["scope"].asString();
            THROW_HR_IF(E_INVALIDARG, scope != "pane" && scope != "tab");
            winrt::Microsoft::Terminal::Settings::RuntimeAppearance appearance;
            if (const auto opacity = optionalNumber(plan, "opacity", 0.f, 100.f))
            {
                appearance.opacity = *opacity / 100.f;
            }
            appearance.fontSize = optionalNumber(plan, "font_size", 1.f, 128.f);
            if (plan.isMember("acrylic"))
            {
                THROW_HR_IF(E_INVALIDARG, !plan["acrylic"].isBool());
                appearance.acrylic = plan["acrylic"].asBool();
            }
            if (plan.isMember("color_scheme"))
            {
                THROW_HR_IF(E_INVALIDARG, !plan["color_scheme"].isString());
                appearance.scheme = _settings.GlobalSettings().ColorSchemes().TryLookup(winrt::to_hstring(plan["color_scheme"].asString()));
                THROW_HR_IF(E_INVALIDARG, !appearance.scheme);
            }
            THROW_HR_IF(E_INVALIDARG, appearance.Empty());
            std::vector<winrt::com_ptr<TerminalPaneContent>> contents;
            for (const auto& [control, connection] : target.panes)
            {
                if (scope == "pane" && control.ContentId() != target.sourceContentId)
                {
                    continue;
                }
                const auto pane = tab->GetRootPane()->FindPaneByContentId(gsl::narrow<uint32_t>(control.ContentId()));
                THROW_HR_IF(E_CHANGED_STATE, !pane || pane->GetTerminalControl() != control || control.Connection() != connection);
                const auto content = pane->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>();
                THROW_HR_IF(E_CHANGED_STATE, !content);
                contents.emplace_back(winrt::get_self<TerminalPaneContent>(content)->get_strong());
            }
            THROW_HR_IF(E_CHANGED_STATE, contents.empty());
            if (scope == "tab")
            {
                size_t currentCount = 0;
                tab->GetRootPane()->WalkTree([&](const auto& pane) {
                    if (pane->GetContent().template try_as<winrt::TerminalApp::TerminalPaneContent>())
                    {
                        ++currentCount;
                    }
                });
                THROW_HR_IF(E_CHANGED_STATE, currentCount != contents.size());
                THROW_HR_IF(E_CHANGED_STATE, !tab->ApplyRuntimeAppearance(appearance));
            }
            else
            {
                contents.front()->SetPaneRuntimeAppearance(appearance);
            }
            result["scope"] = scope;
            result["panes"] = Json::Value{ Json::arrayValue };
            for (const auto& content : contents)
            {
                const auto control = content->GetTermControl();
                const auto settings = control.Settings();
                THROW_HR_IF(E_FAIL, appearance.opacity && std::abs(control.BackgroundOpacity() - *appearance.opacity) > 0.001f);
                THROW_HR_IF(E_FAIL, appearance.acrylic && settings.UseAcrylic() != *appearance.acrylic);
                THROW_HR_IF(E_FAIL, appearance.fontSize && settings.FontSize() != *appearance.fontSize);
                Json::Value actual;
                actual["content_id"] = control.ContentId();
                actual["opacity"] = control.BackgroundOpacity() * 100.f;
                actual["acrylic"] = settings.UseAcrylic();
                actual["font_size"] = settings.FontSize();
                if (appearance.scheme)
                    actual["color_scheme"] = winrt::to_string(appearance.scheme.Name());
                result["panes"].append(std::move(actual));
            }
        }
        else if (kind == "tab_layout")
        {
            requireKeys(plan, { "kind", "layout" });
            THROW_HR_IF(E_INVALIDARG, !plan["layout"].isString());
            const auto layout = plan["layout"].asString();
            THROW_HR_IF(E_INVALIDARG, layout != "horizontal" && layout != "vertical");
            // Do not enqueue a mutation behind another layout/drag operation.
            THROW_HR_IF(E_CHANGED_STATE, _changingTabLayout || _pendingTabLayout || _rearranging || _receivingContentTransfer);
            const auto requested = layout == "vertical" ? TabLayout::Vertical : TabLayout::Horizontal;
            THROW_HR_IF(E_FAIL, !_ApplyTabLayout(requested));
            _runtimeTabLayout = requested;
            result["layout"] = layout;
            result["pending"] = _changingTabLayout;
        }
        else if (kind == "focus_tab")
        {
            requireKeys(plan, { "kind", "tab_id" });
            THROW_HR_IF(E_INVALIDARG, !plan["tab_id"].isString());
            const auto id = winrt::to_hstring(plan["tab_id"].asString());
            THROW_HR_IF(E_INVALIDARG, std::find(target.tabIds.begin(), target.tabIds.end(), id) == target.tabIds.end());
            std::optional<uint32_t> index;
            for (uint32_t i = 0; i < _tabs.Size(); ++i)
            {
                if (const auto candidate = _GetTabImpl(_tabs.GetAt(i)); candidate && candidate->StableId() == id)
                {
                    index = i;
                    break;
                }
            }
            THROW_HR_IF(E_CHANGED_STATE, !index || _changingTabLayout || _rearranging || _receivingContentTransfer);
            THROW_HR_IF(E_FAIL, !_SelectTab(*index));
            const auto focused = _GetFocusedTabImpl();
            THROW_HR_IF(E_FAIL, !focused || focused->StableId() != id);
            result["tab_id"] = winrt::to_string(focused->StableId());
        }
        else if (kind == "focus_pane")
        {
            requireKeys(plan, { "kind", "content_id" });
            THROW_HR_IF(E_INVALIDARG, !plan["content_id"].isUInt());
            const auto id = plan["content_id"].asUInt();
            const auto entry = std::find_if(target.panes.begin(), target.panes.end(), [&](const auto& pane) {
                return pane.first.ContentId() == id;
            });
            THROW_HR_IF(E_INVALIDARG, entry == target.panes.end());
            const auto pane = tab->GetRootPane()->FindPaneByContentId(id);
            THROW_HR_IF(E_CHANGED_STATE, !pane || !pane->Id() || pane->GetTerminalControl() != entry->first || entry->first.Connection() != entry->second);
            THROW_HR_IF(E_CHANGED_STATE, _changingTabLayout || _rearranging || _receivingContentTransfer);
            uint32_t index{};
            THROW_HR_IF(E_CHANGED_STATE, !_tabs.IndexOf(tab.as<winrt::TerminalApp::Tab>(), index));
            THROW_HR_IF(E_FAIL, !_SelectTab(index));
            _UnZoomIfNeeded();
            tab->PreparePaneFocus(*pane->Id());
            THROW_HR_IF(E_FAIL, !tab->FocusPane(*pane->Id()));
            THROW_HR_IF(E_FAIL, tab->GetActivePane()->ContentId() != id);
            result["tab_id"] = winrt::to_string(tab->StableId());
            result["content_id"] = id;
        }
        else if (kind == "split" || kind == "new_tab")
        {
            requireKeys(plan, kind == "split" ? std::initializer_list<std::string_view>{ "kind", "profile_id", "direction" } : std::initializer_list<std::string_view>{ "kind", "profile_id" });
            THROW_HR_IF(E_INVALIDARG, !plan["profile_id"].isString());
            const winrt::guid profileId{ winrt::to_hstring(plan["profile_id"].asString()) };
            Profile profile{ nullptr };
            for (const auto& candidate : _settings.ActiveProfiles())
            {
                if (candidate.Guid() == profileId)
                {
                    profile = candidate;
                    break;
                }
            }
            THROW_HR_IF(E_CHANGED_STATE, !profile);
            NewTerminalArgs args;
            args.Profile(::Microsoft::Console::Utils::GuidToString(profileId));
            INewContentArgs contentArgs{ args };
            SplitDirection direction = SplitDirection::Right;
            const auto previousContentId = tab->GetActivePane()->ContentId();
            const auto restoreFocus = wil::scope_exit([&]() noexcept {
                try
                {
                    if (kind == "split" && previousContentId)
                    {
                        if (const auto previous = tab->GetRootPane()->FindPaneByContentId(*previousContentId); previous && previous->Id())
                        {
                            tab->PreparePaneFocus(*previous->Id());
                        }
                    }
                }
                CATCH_LOG()
            });
            if (kind == "split")
            {
                THROW_HR_IF(E_INVALIDARG, !plan["direction"].isString());
                const auto requested = plan["direction"].asString();
                if (requested == "auto")
                    direction = SplitDirection::Automatic;
                else if (requested == "left")
                    direction = SplitDirection::Left;
                else if (requested == "right")
                    direction = SplitDirection::Right;
                else if (requested == "up")
                    direction = SplitDirection::Up;
                else if (requested == "down")
                    direction = SplitDirection::Down;
                else
                    THROW_HR(E_INVALIDARG);
                THROW_HR_IF(E_CHANGED_STATE, !source->Id());
                tab->PreparePaneFocus(*source->Id());
                contentArgs = _PrepareSplitDirectory(tab, args, false);
            }
            const auto pane = _MakePane(contentArgs, nullptr);
            THROW_HR_IF(E_FAIL, !pane || !pane->GetTerminalControl());
            const auto control = pane->GetTerminalControl();
            if (kind == "split")
            {
                THROW_HR_IF(E_FAIL, !_SplitPane(tab, direction, 0.5f, pane, false));
                THROW_HR_IF(E_FAIL, !tab->GetRootPane()->FindPaneByContentId(gsl::narrow<uint32_t>(control.ContentId())));
                result["direction"] = plan["direction"];
            }
            else
            {
                if (!_CreateNewTabFromPane(pane, -1, true))
                {
                    pane->Shutdown();
                    THROW_HR(E_FAIL);
                }
            }
            result["profile_id"] = plan["profile_id"];
            result["profile"] = winrt::to_string(profile.Name());
            result["content_id"] = control.ContentId();
        }
        else
        {
            THROW_HR(E_INVALIDARG);
        }
        return result;
    }

    safe_void_coroutine TerminalPage::_OnOmniPromptRequested(IInspectable sender, winrt::hstring prompt)
    {
        const auto weak = get_weak();
        const auto dispatcher = Dispatcher();
        const auto palette = winrt::get_self<CommandPalette>(sender.as<winrt::TerminalApp::CommandPalette>())->get_strong();
        const auto cancellation = palette->OmniCancellation();
        if (!cancellation)
        {
            co_return;
        }
        hstring status;
        HRESULT failure = S_OK;
        bool clearPrompt = false;
        const auto requestId = winrt::to_string(::Microsoft::Console::Utils::GuidToString(::Microsoft::Console::Utils::CreateGuid()));
        _agentPaneLog("omni_started request=" + requestId);
        try
        {
            const auto target = _CaptureOmniTarget();
            Json::Value request;
            request["prompt"] = winrt::to_string(prompt);
            request["default_profile_id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToString(_settings.GlobalSettings().DefaultProfile()));
            request["current_tab_id"] = winrt::to_string(target.tabId);
            request["current_content_id"] = target.sourceContentId;
            request["tab_layout"] = _isVerticalLayout ? "vertical" : "horizontal";
            request["tabs"] = Json::Value{ Json::arrayValue };
            for (uint32_t i = 0; i < _tabs.Size(); ++i)
            {
                const auto candidate = _tabs.GetAt(i);
                if (const auto impl = _GetTabImpl(candidate))
                {
                    Json::Value entry;
                    entry["id"] = winrt::to_string(impl->StableId());
                    entry["index"] = i + 1;
                    entry["title"] = winrt::to_string(candidate.Title());
                    request["tabs"].append(std::move(entry));
                }
            }
            request["panes"] = Json::Value{ Json::arrayValue };
            for (const auto& [paneControl, connection] : target.panes)
            {
                Json::Value entry;
                entry["content_id"] = paneControl.ContentId();
                entry["index"] = request["panes"].size() + 1;
                entry["title"] = winrt::to_string(paneControl.Title());
                try
                {
                    if (!_GetFocusedTabImpl()->IsZoomed() && paneControl.XamlRoot() && paneControl.ActualWidth() > 0 && paneControl.ActualHeight() > 0)
                    {
                        const auto bounds = paneControl.TransformToVisual(Root()).TransformBounds({ 0, 0, gsl::narrow_cast<float>(paneControl.ActualWidth()), gsl::narrow_cast<float>(paneControl.ActualHeight()) });
                        entry["bounds"]["x"] = bounds.X;
                        entry["bounds"]["y"] = bounds.Y;
                        entry["bounds"]["width"] = bounds.Width;
                        entry["bounds"]["height"] = bounds.Height;
                    }
                }
                CATCH_LOG()
                request["panes"].append(std::move(entry));
            }
            const auto source = _SourceTerminalPaneForTab(_GetFocusedTabImpl());
            const auto content = winrt::get_self<TerminalPaneContent>(source->GetContent().as<winrt::TerminalApp::TerminalPaneContent>());
            const auto control = content->GetTermControl();
            const auto defaults = winrt::Microsoft::Terminal::Settings::TerminalSettings::CreateWithProfile(_settings, content->GetProfile()).DefaultSettings();
            request["current_appearance"]["opacity"] = control.BackgroundOpacity() * 100.f;
            request["current_appearance"]["acrylic"] = control.Settings().UseAcrylic();
            request["current_appearance"]["font_size"] = control.Settings().FontSize();
            request["profile_default_appearance"]["opacity"] = defaults->Opacity() * 100.f;
            request["profile_default_appearance"]["acrylic"] = defaults->UseAcrylic();
            request["profile_default_appearance"]["font_size"] = defaults->FontSize();
            request["profiles"] = Json::Value{ Json::arrayValue };
            for (const auto& profile : _settings.ActiveProfiles())
            {
                Json::Value entry;
                entry["id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToString(profile.Guid()));
                entry["name"] = winrt::to_string(profile.Name());
                request["profiles"].append(std::move(entry));
            }
            request["schemes"] = Json::Value{ Json::arrayValue };
            for (const auto& scheme : _settings.GlobalSettings().ColorSchemes())
            {
                request["schemes"].append(winrt::to_string(scheme.Key()));
            }
            const auto requestJson = jsonString(request);
            THROW_HR_IF(E_INVALIDARG, requestJson.size() > 24 * 1024);
            const auto path = _DetectWtaPath();
            THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), path.empty());
            if (!_omniPlanner)
            {
                _omniPlanner = std::make_shared<::Microsoft::Terminal::WtaProcess::OmniPlannerProcess>();
            }
            const auto planner = _omniPlanner;
            co_await winrt::resume_background();
            const auto capture = planner->Exchange(std::wstring{ path }, requestJson, cancellation.get());
            co_await wil::resume_foreground(dispatcher);
            const auto page = weak.get();
            _agentPaneLog(fmt::format("omni_planned request={} completed={} exit={} cancelled={}", requestId, capture.completed, capture.exitCode, cancellation->load()));
            if (!page || cancellation->load())
            {
                co_return;
            }
            THROW_HR_IF(E_FAIL, !capture.completed || capture.exitCode != 0 || capture.output.size() > 64 * 1024);
            Json::CharReaderBuilder reader;
            reader["rejectDupKeys"] = true;
            reader["failIfExtra"] = true;
            std::istringstream input{ capture.output };
            Json::Value plan;
            std::string errors;
            THROW_HR_IF(E_INVALIDARG, !Json::parseFromStream(reader, input, &plan, &errors));
            THROW_HR_IF(E_FAIL, plan.isMember("error"));
            if (plan["kind"] == "unsupported")
            {
                requireKeys(plan, { "kind", "message" });
                THROW_HR_IF(E_INVALIDARG, !plan["message"].isString() || plan["message"].asString().size() > 4096);
                status = winrt::to_hstring(plan["message"].asString());
                _agentPaneLog("omni_unsupported request=" + requestId);
            }
            else
            {
                const auto previousLayout = page->_runtimeTabLayout;
                auto actual = page->_ExecuteOmniOperation(plan, target);
                if (plan["kind"] == "tab_layout")
                {
                    const auto generation = page->_tabLayoutGeneration;
                    const auto requested = plan["layout"] == "vertical" ? TabLayout::Vertical : TabLayout::Horizontal;
                    // _ApplyTabLayout completes on the low-priority dispatcher queue.
                    // Read back only after its queued rebuild, never report acceptance as success.
                    co_await wil::resume_foreground(dispatcher, Windows::UI::Core::CoreDispatcherPriority::Low);
                    const auto completed = !page->_changingTabLayout &&
                                           !page->_pendingTabLayout &&
                                           page->_tabLayoutGeneration == generation &&
                                           page->_isVerticalLayout == (requested == TabLayout::Vertical);
                    if (!completed && page->_tabLayoutGeneration == generation && page->_runtimeTabLayout == requested)
                    {
                        page->_runtimeTabLayout = previousLayout;
                    }
                    THROW_HR_IF(E_FAIL, !completed);
                    actual.removeMember("pending");
                }
                _agentPaneLog("omni_executed request=" + requestId + " kind=" + actual["kind"].asString());
                status = RS_(L"FreOverlay_ProgressCompleted") + L"\n" + winrt::to_hstring(jsonString(actual));
                clearPrompt = true;
            }
        }
        catch (...)
        {
            failure = winrt::to_hresult();
            LOG_HR(failure);
        }
        co_await wil::resume_foreground(dispatcher);
        if (FAILED(failure))
        {
            _agentPaneLog(fmt::format("omni_failed request={} hr=0x{:08X}", requestId, static_cast<uint32_t>(failure)));
            status = RS_(L"FreOverlay_ProgressFailed") + L" (" + winrt::to_hstring(fmt::format("0x{:08X}", static_cast<uint32_t>(failure))) + L")";
        }
        palette->CompleteOmniRequest(cancellation, status, clearPrompt);
        _agentPaneLog(fmt::format("omni_feedback request={} cancelled={} visible={}", requestId, cancellation->load(), palette->Visibility() == Windows::UI::Xaml::Visibility::Visible));
    }
}
