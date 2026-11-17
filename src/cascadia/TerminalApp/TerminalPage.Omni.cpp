// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TerminalPage.h"
#include "CommandPalette.h"
#include "AgentPaneLog.h"
#include "TerminalPaneContent.h"
#include "../inc/OmniPlannerProcess.h"
#include "../inc/OmniDirectoryUtils.h"
#include "../inc/OmniToolDefinitions.h"
#include "../inc/AgentSourceUtils.h"
#include "../../types/inc/utils.hpp"
#include <cwctype>

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace
{
    std::shared_ptr<Pane> findOmniControlPane(const std::shared_ptr<Pane>& root, const uint64_t contentId, const bool skipHidden = true)
    {
        // Omni exposes TermControl IDs; Pane IDs belong to a separate namespace.
        return root->WalkTree([&](const auto& pane) -> std::shared_ptr<Pane> {
            const auto control = pane->GetTerminalControl();
            return control && control.ContentId() == contentId ? pane : nullptr;
        },
                              skipHidden);
    }

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

    void trimOmniAppearanceMetadata(Json::Value& context, const size_t budget)
    {
        if (jsonString(context).size() > budget)
        {
            context["scheme_appearances"].clear();
            context["scheme_metadata_complete"] = false;
        }
        if (jsonString(context).size() > budget)
        {
            for (auto& pane : context["panes"])
                pane.removeMember("appearance");
            context["pane_appearance_complete"] = false;
        }
    }

    void rejectOmni(const bool rejected, const wchar_t* reason, const HRESULT hr = E_CHANGED_STATE)
    {
        if (rejected)
            throw winrt::hresult_error(hr, reason);
    }

    std::string colorText(const winrt::Microsoft::Terminal::Core::Color& color)
    {
        return fmt::format("#{:02X}{:02X}{:02X}", color.R, color.G, color.B);
    }

    Json::Value schemeColors(const ColorScheme& scheme)
    {
        Json::Value colors{ Json::arrayValue };
        for (const auto& color : { scheme.Foreground(), scheme.Background(), scheme.SelectionBackground(), scheme.CursorColor() })
            colors.append(colorText(color));
        for (const auto& color : scheme.Table())
            colors.append(colorText(color));
        return colors;
    }

    Json::Value currentAppearance(const winrt::Microsoft::Terminal::Control::TermControl& control, const CascadiaSettings& settings)
    {
        Json::Value result;
        result["opacity"] = control.BackgroundOpacity() * 100.f;
        result["acrylic"] = control.Settings().UseAcrylic();
        result["font_size"] = control.Settings().FontSize();
        result["colors"] = Json::Value{ Json::arrayValue };
        for (const auto& color : control.CurrentColorScheme())
            result["colors"].append(colorText(color));
        result["foreground"] = result["colors"][0];
        result["background"] = result["colors"][1];
        result["color_scheme"] = Json::nullValue;
        for (const auto& entry : settings.GlobalSettings().ColorSchemes())
        {
            if (schemeColors(entry.Value()) == result["colors"])
            {
                result["color_scheme"] = winrt::to_string(entry.Key());
                break;
            }
        }
        return result;
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
        target.settings = _settings;
        if (tab)
        {
            target.tabId = tab->StableId();
            if (const auto source = tab->GetActivePane())
            {
                target.sourceKind = source->IsAgentPane()                                                  ? "agent" :
                                    source->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>() ? "terminal" :
                                                                                                             "non_terminal";
                if (const auto control = source->GetTerminalControl())
                    target.sourceContentId = gsl::narrow<uint32_t>(control.ContentId());
            }
        }
        for (const auto& candidate : _tabs)
        {
            if (const auto impl = _GetTabImpl(candidate))
            {
                target.tabIds.emplace_back(impl->StableId());
            }
        }
        if (tab)
            tab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (const auto content = pane->GetContent().template try_as<winrt::TerminalApp::TerminalPaneContent>())
                {
                    const auto control = winrt::get_self<TerminalPaneContent>(content)->GetTermControl();
                    target.panes.emplace_back(control, control.Connection());
                }
            },
                                         true);
        return target;
    }

    Json::Value TerminalPage::_RefreshOmniContext(const Json::Value& captured, const OmniTarget& target)
    {
        auto context = captured;
        context.removeMember("capabilities");
        context.removeMember("current_appearance");
        context.removeMember("profile_default_appearance");
        context["snapshot"] = false;
        context["targets_bound_to_request"] = true;
        context["tab_layout"] = _isVerticalLayout ? "vertical" : "horizontal";
        context["tabs"] = Json::Value{ Json::arrayValue };
        context["panes"] = Json::Value{ Json::arrayValue };
        context["pane_appearance_complete"] = true;
        context["source_available"] = false;
        const auto active = _GetFocusedTabImpl();
        context["active_tab_id"] = active ? Json::Value{ winrt::to_string(active->StableId()) } : Json::Value{};
        for (uint32_t index = 0; index < _tabs.Size(); ++index)
        {
            const auto tab = _GetTabImpl(_tabs.GetAt(index));
            if (!tab)
                continue;
            if (std::find(target.tabIds.begin(), target.tabIds.end(), tab->StableId()) != target.tabIds.end())
            {
                Json::Value entry;
                entry["id"] = winrt::to_string(tab->StableId());
                entry["index"] = index + 1;
                entry["title"] = winrt::to_string(hstring{ std::wstring_view{ _tabs.GetAt(index).Title() }.substr(0, 128) });
                context["tabs"].append(std::move(entry));
            }
            if (tab->StableId() != target.tabId)
                continue;
            std::unordered_map<uint32_t, uint32_t> paneOrder;
            uint32_t paneIndex{};
            tab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (pane->GetContent().template try_as<winrt::TerminalApp::TerminalPaneContent>())
                    paneOrder.emplace(gsl::narrow<uint32_t>(pane->GetTerminalControl().ContentId()), ++paneIndex);
            },
                                         true);
            for (const auto& [control, connection] : target.panes)
            {
                const auto pane = findOmniControlPane(tab->GetRootPane(), control.ContentId());
                if (!pane || pane->GetTerminalControl() != control)
                    continue;
                Json::Value entry;
                entry["content_id"] = control.ContentId();
                entry["title"] = winrt::to_string(hstring{ std::wstring_view{ control.Title() }.substr(0, 128) });
                entry["appearance"] = currentAppearance(control, _settings);
                entry["appearance"].removeMember("colors");
                entry["index"] = paneOrder.at(gsl::narrow<uint32_t>(control.ContentId()));
                if (!tab->IsZoomed() && control.XamlRoot() && control.ActualWidth() > 0 && control.ActualHeight() > 0)
                {
                    const auto bounds = control.TransformToVisual(Root()).TransformBounds({ 0, 0, gsl::narrow_cast<float>(control.ActualWidth()), gsl::narrow_cast<float>(control.ActualHeight()) });
                    entry["bounds"]["x"] = bounds.X;
                    entry["bounds"]["y"] = bounds.Y;
                    entry["bounds"]["width"] = bounds.Width;
                    entry["bounds"]["height"] = bounds.Height;
                }
                context["panes"].append(entry);
                if (target.sourceKind == "terminal" && control.ContentId() == target.sourceContentId)
                {
                    context["source_available"] = true;
                    context["current_appearance"] = entry["appearance"];
                    const auto content = winrt::get_self<TerminalPaneContent>(pane->GetContent().as<winrt::TerminalApp::TerminalPaneContent>());
                    const auto defaults = winrt::Microsoft::Terminal::Settings::TerminalSettings::CreateWithProfile(_settings, content->GetProfile()).DefaultSettings();
                    auto& appearance = context["profile_default_appearance"];
                    appearance["opacity"] = defaults->Opacity() * 100.f;
                    appearance["acrylic"] = defaults->UseAcrylic();
                    appearance["font_size"] = defaults->FontSize();
                    appearance["color_scheme"] = Json::nullValue;
                    Json::Value colors{ Json::arrayValue };
                    for (const auto& color : { defaults->DefaultForeground(), defaults->DefaultBackground(), defaults->SelectionBackground(), defaults->CursorColor() })
                        colors.append(colorText(color));
                    winrt::com_array<winrt::Microsoft::Terminal::Core::Color> table;
                    defaults->GetColorTable(table);
                    for (const auto& color : table)
                        colors.append(colorText(color));
                    for (const auto& scheme : _settings.GlobalSettings().ColorSchemes())
                    {
                        if (schemeColors(scheme.Value()) == colors)
                        {
                            appearance["color_scheme"] = winrt::to_string(scheme.Key());
                            break;
                        }
                    }
                }
            }
        }
        context["profiles"] = Json::Value{ Json::arrayValue };
        for (const auto& profile : _settings.ActiveProfiles())
        {
            const auto id = winrt::to_string(::Microsoft::Console::Utils::GuidToString(profile.Guid()));
            for (const auto& old : captured["profiles"])
            {
                if (old["id"] == id)
                {
                    Json::Value entry;
                    entry["id"] = id;
                    entry["name"] = winrt::to_string(profile.Name());
                    context["profiles"].append(std::move(entry));
                }
            }
        }
        context["schemes"] = Json::Value{ Json::arrayValue };
        context["scheme_appearances"] = Json::Value{ Json::arrayValue };
        context["scheme_metadata_complete"] = true;
        size_t schemeBytes{};
        for (const auto& name : captured["schemes"])
        {
            if (const auto scheme = _settings.GlobalSettings().ColorSchemes().TryLookup(winrt::to_hstring(name.asString())))
            {
                context["schemes"].append(name);
                Json::Value appearance;
                appearance["name"] = name;
                appearance["foreground"] = colorText(scheme.Foreground());
                appearance["background"] = colorText(scheme.Background());
                schemeBytes += jsonString(appearance).size();
                if (schemeBytes <= 4 * 1024)
                    context["scheme_appearances"].append(std::move(appearance));
                else
                    context["scheme_metadata_complete"] = false;
            }
        }
        return context;
    }

    Json::Value TerminalPage::_ExecuteOmniOperation(const Json::Value& plan, const OmniTarget& target, Json::Value* receipt)
    {
        THROW_HR_IF(E_INVALIDARG, !plan["kind"].isString());
        const auto kind = plan["kind"].asString();
        rejectOmni(_windowCloseAccepted || _windowPanesShutdown, L"window_closing");
        const auto applying = [&]() {
            if (receipt)
                (*receipt)["effect"] = "partial_or_unknown";
        };
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
        rejectOmni(kind != "new_tab" && kind != "focus_tab" && !tab, L"target_tab_missing");
        std::shared_ptr<Pane> source;
        if (kind == "split" || (kind == "appearance" && plan["scope"] == "pane"))
        {
            rejectOmni(target.sourceKind != "terminal", L"current_pane_not_supported", E_NOTIMPL);
            source = findOmniControlPane(tab->GetRootPane(), target.sourceContentId);
            const auto captured = std::find_if(target.panes.begin(), target.panes.end(), [&](const auto& entry) {
                return entry.first.ContentId() == target.sourceContentId;
            });
            rejectOmni(!source || captured == target.panes.end() || source->GetTerminalControl() != captured->first, L"target_pane_changed");
        }
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
                rejectOmni(!appearance.scheme, L"color_scheme_removed");
            }
            THROW_HR_IF(E_INVALIDARG, appearance.Empty());
            std::vector<winrt::com_ptr<TerminalPaneContent>> contents;
            tab->GetRootPane()->WalkTree([&](const auto& pane) {
                if (scope == "pane" && pane != source)
                    return;
                const auto content = pane->GetContent().template try_as<winrt::TerminalApp::TerminalPaneContent>();
                if (content)
                    contents.emplace_back(winrt::get_self<TerminalPaneContent>(content)->get_strong());
            });
            rejectOmni(contents.empty(), L"no_supported_terminal_panes", E_NOTIMPL);
            for (const auto& content : contents)
                rejectOmni(!content->CanApplyRuntimeAppearance(), L"profile_settings_unavailable");
            applying();
            if (scope == "tab")
            {
                rejectOmni(!tab->ApplyRuntimeAppearance(appearance), L"appearance_not_applied");
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
                auto actual = currentAppearance(control, _settings);
                actual["content_id"] = control.ContentId();
                result["panes"].append(actual);
                if (receipt)
                    (*receipt)["data"] = result;
                rejectOmni((appearance.opacity && std::abs(actual["opacity"].asFloat() / 100.f - *appearance.opacity) > 0.001f) ||
                               (appearance.acrylic && actual["acrylic"].asBool() != *appearance.acrylic) ||
                               (appearance.fontSize && actual["font_size"].asFloat() != *appearance.fontSize) ||
                               (appearance.scheme && actual["colors"] != schemeColors(appearance.scheme)),
                           L"appearance_readback_mismatch",
                           E_FAIL);
                result["panes"][result["panes"].size() - 1].removeMember("colors");
            }
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
            rejectOmni(!index, L"target_tab_missing");
            applying();
            rejectOmni(!_SelectTab(*index), L"tab_focus_failed", E_FAIL);
            const auto focused = _GetFocusedTabImpl();
            rejectOmni(!focused || focused->StableId() != id, L"tab_focus_readback_mismatch", E_FAIL);
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
            const auto pane = findOmniControlPane(tab->GetRootPane(), id);
            rejectOmni(!pane || !pane->Id() || pane->GetTerminalControl() != entry->first, L"target_pane_changed");
            uint32_t index{};
            rejectOmni(!_tabs.IndexOf(tab.as<winrt::TerminalApp::Tab>(), index), L"target_tab_missing");
            applying();
            rejectOmni(!_SelectTab(index), L"tab_focus_failed", E_FAIL);
            _UnZoomIfNeeded();
            tab->PreparePaneFocus(*pane->Id());
            rejectOmni(!tab->FocusPane(*pane->Id()), L"pane_focus_failed", E_FAIL);
            rejectOmni(tab->GetActivePane()->GetTerminalControl() != entry->first, L"pane_focus_readback_mismatch", E_FAIL);
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
            rejectOmni(!profile, L"profile_removed");
            NewTerminalArgs args;
            args.Profile(::Microsoft::Console::Utils::GuidToString(profileId));
            INewContentArgs contentArgs{ args };
            SplitDirection direction = SplitDirection::Right;
            const auto previousContentId = tab ? tab->GetActivePane()->ContentId() : std::nullopt;
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
                rejectOmni(!source->Id(), L"target_pane_changed");
                applying();
                tab->PreparePaneFocus(*source->Id());
                contentArgs = _PrepareSplitDirectory(tab, args, false);
            }
            applying();
            const auto pane = _MakePane(contentArgs, nullptr);
            rejectOmni(!pane || !pane->GetTerminalControl(), L"pane_creation_failed", E_FAIL);
            const auto control = pane->GetTerminalControl();
            if (kind == "split")
            {
                rejectOmni(!_SplitPane(tab, direction, 0.5f, pane, false), L"pane_split_failed", E_FAIL);
                rejectOmni(!findOmniControlPane(tab->GetRootPane(), control.ContentId()), L"pane_split_readback_mismatch", E_FAIL);
                result["direction"] = plan["direction"];
            }
            else
            {
                if (!_CreateNewTabFromPane(pane, -1, true))
                {
                    pane->Shutdown();
                    throw winrt::hresult_error(E_FAIL, L"tab_creation_failed");
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

    IAsyncAction TerminalPage::_WaitForOmniIdle(std::function<void()> validateRequest)
    {
        const auto lifetime = get_strong();
        const auto dispatcher = Dispatcher();
        THROW_HR_IF(E_INVALIDARG, !validateRequest);
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds{ 3 };
        for (;;)
        {
            validateRequest();
            if (_windowCloseAccepted || _windowPanesShutdown)
            {
                throw winrt::hresult_error(E_CHANGED_STATE, L"window_closing");
            }
            const auto busy = _changingTabLayout        ? L"layout_transition_busy" :
                              _rearranging              ? L"tab_drag_busy" :
                              _receivingContentTransfer ? L"content_transfer_busy" :
                              _pendingTabLayout         ? L"layout_request_pending" :
                                                          nullptr;
            if (std::chrono::steady_clock::now() >= deadline)
            {
                throw winrt::hresult_error(HRESULT_FROM_WIN32(ERROR_TIMEOUT), busy ? busy : L"ui_wait_timeout");
            }
            if (!busy)
                co_return;
            // Wait before submitting: the native pending-layout slot has no request
            // ownership and could otherwise execute after Omni has been cancelled.
            co_await winrt::resume_after(std::chrono::milliseconds{ 50 });
            co_await wil::resume_foreground(dispatcher, Windows::UI::Core::CoreDispatcherPriority::Low);
        }
    }

    IAsyncAction TerminalPage::_ApplyOmniTabLayout(const TabLayout layout, std::function<void()> validateRequest, Json::Value* receipt)
    {
        const auto lifetime = get_strong();
        const auto dispatcher = Dispatcher();
        THROW_HR_IF(E_INVALIDARG, layout != TabLayout::Horizontal && layout != TabLayout::Vertical);
        co_await _WaitForOmniIdle(validateRequest);
        validateRequest();
        const auto previousLayout = _runtimeTabLayout;
        if (receipt)
            (*receipt)["effect"] = "partial_or_unknown";
        if (!_ApplyTabLayout(layout))
        {
            throw winrt::hresult_error(E_FAIL, L"layout_apply_failed");
        }
        _runtimeTabLayout = layout;
        const auto generation = _tabLayoutGeneration;
        co_await wil::resume_foreground(dispatcher, Windows::UI::Core::CoreDispatcherPriority::Low);
        // Once submitted, report the actual outcome, even if the request was cancelled.
        // Never mistake acceptance of the asynchronous rebuild for completion.
        const auto superseded = _tabLayoutGeneration != generation || _pendingTabLayout.has_value();
        if (_windowCloseAccepted || _windowPanesShutdown || superseded || _changingTabLayout || _isVerticalLayout != (layout == TabLayout::Vertical))
        {
            if (_tabLayoutGeneration == generation && _runtimeTabLayout == layout)
            {
                _runtimeTabLayout = previousLayout;
            }
            throw winrt::hresult_error(E_FAIL, superseded ? L"layout_superseded" : L"layout_not_completed");
        }
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
        const auto endHostOperation = wil::scope_exit([&]() { palette->OmniHostOperation(cancellation, false); });
        hstring status;
        HRESULT failure = S_OK;
        bool clearPrompt = false;
        const auto requestId = winrt::to_string(::Microsoft::Console::Utils::GuidToString(::Microsoft::Console::Utils::CreateGuid()));
        std::string stage{ "capture" };
        std::string reason{ "operation_rejected" };
        uint64_t diagnosticContentId{};
        uint64_t diagnosticMatchId{};
        Json::Value lastMutation;
        _agentPaneLog("omni_started request=" + requestId);
        try
        {
            const auto target = _CaptureOmniTarget();
            struct DirectoryPane
            {
                hstring tabId;
                uint32_t contentId;
                winrt::Microsoft::Terminal::Control::TermControl control;
                winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection;
                hstring cwd;
                std::string provenance;
                Json::Value metadata;
            };
            const auto directoryFor = [&](const auto& paneControl) -> std::pair<hstring, std::string> {
                if (paneControl.ConnectionState() != winrt::Microsoft::Terminal::TerminalConnection::ConnectionState::Connected)
                    return {};
                const auto [agentOwned, agentCwd] = _RichTabAgentCwdForControl(paneControl);
                if (agentCwd)
                    return { winrt::to_hstring(*agentCwd), "agent" };
                // An owned agent with no live cwd must not inherit a stale shell cwd.
                if (agentOwned)
                    return {};
                const auto conpty = paneControl.Connection().template try_as<winrt::Microsoft::Terminal::TerminalConnection::ConptyConnection>();
                const auto reported = paneControl.WorkingDirectoryReportedByShell();
                return { hstring{ ::Microsoft::Terminal::AgentSource::ResolvePaneCwd(
                             reported ? paneControl.WorkingDirectory() : hstring{},
                             reported,
                             conpty ? conpty.Commandline() : hstring{},
                             conpty ? conpty.StartingDirectory() : hstring{}) },
                         reported ? "shell" : "launch" };
            };
            std::vector<DirectoryPane> directoryPanes;
            bool directoryContextComplete = true;
            Json::Value request;
            request["prompt"] = winrt::to_string(prompt);
            request["default_profile_id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToString(_settings.GlobalSettings().DefaultProfile()));
            request["current_tab_id"] = winrt::to_string(target.tabId);
            request["current_content_id"] = target.sourceKind == "terminal" ? Json::Value{ target.sourceContentId } : Json::Value{};
            request["current_pane_kind"] = target.sourceKind;
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
            const auto captureDirectories = [&]() {
                directoryPanes.clear();
                directoryContextComplete = true;
                request["directory_panes"] = Json::Value{ Json::arrayValue };
                size_t directoryBytes{};
                size_t directoryCount{};
                for (const auto& candidate : _tabs)
                {
                    if (const auto tab = _GetTabImpl(candidate))
                    {
                        tab->GetRootPane()->WalkTree([&](const auto& pane) {
                            const auto paneControl = pane->GetTerminalControl();
                            if (!paneControl)
                                return;
                            if (++directoryCount > 64)
                            {
                                directoryContextComplete = false;
                                return;
                            }
                            const auto [cwd, provenance] = directoryFor(paneControl);
                            Json::Value entry;
                            entry["tab_id"] = winrt::to_string(tab->StableId());
                            entry["content_id"] = paneControl.ContentId();
                            entry["tab_title"] = winrt::to_string(hstring{ std::wstring_view{ candidate.Title() }.substr(0, 128) });
                            entry["title"] = winrt::to_string(hstring{ std::wstring_view{ paneControl.Title() }.substr(0, 128) });
                            entry["cwd"] = winrt::to_string(cwd);
                            entry["provenance"] = provenance;
                            entry["path_kind"] = ::Microsoft::Terminal::Omni::IsPosixDirectory(cwd) ? "posix" : "windows";
                            if (const auto terminal = pane->GetContent().template try_as<winrt::TerminalApp::TerminalPaneContent>())
                            {
                                const auto profile = winrt::get_self<TerminalPaneContent>(terminal)->GetProfile();
                                entry["profile_name"] = winrt::to_string(hstring{ std::wstring_view{ profile.Name() }.substr(0, 128) });
                                entry["profile_id"] = winrt::to_string(::Microsoft::Console::Utils::GuidToString(profile.Guid()));
                            }
                            if (const auto connection = paneControl.Connection().template try_as<winrt::Microsoft::Terminal::TerminalConnection::ConptyConnection>())
                            {
                                int argc{};
                                const wil::unique_hlocal_ptr<PWSTR[]> argv{ ::CommandLineToArgvW(connection.Commandline().c_str(), &argc) };
                                if (argv && argc > 0)
                                    entry["launch_executable"] = winrt::to_string(hstring{ std::wstring_view{ argv.get()[0] }.substr(0, 256) });
                            }
                            if (const auto agent = _RichTabAgentInfoForControl(paneControl))
                            {
                                entry["agent"] = agent->providerId.substr(0, 128);
                                entry["session_id"] = agent->sessionId.substr(0, 128);
                                entry["activity"] = std::string{ _AgentStatusForPresentation(*agent) };
                                if (agent->lastActivityAtMs)
                                    entry["last_activity_ms"] = Json::UInt64{ *agent->lastActivityAtMs };
                            }
                            directoryBytes += jsonString(entry).size();
                            if (cwd.size() > 1024 || directoryBytes > 12 * 1024)
                            {
                                directoryContextComplete = false;
                                return;
                            }
                            directoryPanes.push_back({ tab->StableId(), gsl::narrow<uint32_t>(paneControl.ContentId()), paneControl, paneControl.Connection(), cwd, provenance, entry });
                            request["directory_panes"].append(std::move(entry));
                        },
                                                     true);
                    }
                }
                request["directory_context_complete"] = directoryContextComplete;
            };
            captureDirectories();
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
            request = _RefreshOmniContext(request, target);
            const auto tools = ::Microsoft::Terminal::Omni::ToolDefinitions(request);
            request["capabilities"] = tools;
            request["tool_protocol"] = 1;
            request["run_id"] = requestId;
            trimOmniAppearanceMetadata(request, 24 * 1024);
            if (jsonString(request).size() > 24 * 1024)
            {
                // Extra navigation metadata must not crowd out existing operations.
                request["directory_panes"].clear();
                directoryPanes.clear();
                directoryContextComplete = false;
                request["directory_context_complete"] = false;
            }
            auto requestJson = jsonString(request);
            THROW_HR_IF(E_INVALIDARG, requestJson.size() > 24 * 1024);
            const auto path = _DetectWtaPath();
            THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), path.empty());
            if (!_omniPlanner)
            {
                _omniPlanner = std::make_shared<::Microsoft::Terminal::WtaProcess::OmniPlannerProcess>();
            }
            const auto planner = _omniPlanner;
            bool runCompleted = false;
            const auto finishRun = wil::scope_exit([&]() { planner->FinishToolRun(requestId, runCompleted); });
            struct SearchPane
            {
                hstring tabId;
                hstring title;
                winrt::Microsoft::Terminal::Control::TermControl control;
                winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection;
                std::vector<winrt::Microsoft::Terminal::Control::TerminalTextMatch> matches;
                Json::Value metadata;
                bool metadataCandidate{};
                std::vector<std::pair<hstring, hstring>> sentPassages;
            };
            std::vector<SearchPane> panes;
            std::vector<std::pair<size_t, uint64_t>> candidates;
            std::set<std::string> calls;
            uint32_t searchCount{};
            uint32_t candidateBase{};
            bool mutationStarted = false;
            bool navigationCompleted = false;
            for (size_t step = 0; step <= 12; ++step)
            {
                stage = "planning";
                co_await winrt::resume_background();
                const auto capture = planner->Exchange(std::wstring{ path }, requestJson, cancellation.get(), requestId);
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
                THROW_HR_IF(E_CHANGED_STATE, plan["run_id"] != requestId);
                if (plan["kind"] == "tool_complete")
                {
                    THROW_HR_IF(E_INVALIDARG, !plan["message"].isString() || plan["message"].asString().size() > 16 * 1024);
                    runCompleted = true;
                    if (navigationCompleted)
                    {
                        palette->CompleteOmniNavigation(cancellation);
                        co_return;
                    }
                    status = winrt::to_hstring(plan["message"].asString());
                    if (!lastMutation.isNull())
                    {
                        status = winrt::to_hstring(jsonString(lastMutation));
                        if (lastMutation["status"] != "cancelled")
                        {
                            status = (lastMutation["status"] == "succeeded" ? RS_(L"FreOverlay_ProgressCompleted") : RS_(L"FreOverlay_ProgressFailed")) + L"\n" + status;
                        }
                    }
                    clearPrompt = lastMutation["status"] == "succeeded";
                    break;
                }
                THROW_HR_IF(E_INVALIDARG, step == 12 || plan["kind"] != "tool_call" || !plan["call_id"].isString() || !plan["name"].isString());
                const auto callId = plan["call_id"].asString();
                const auto toolName = plan["name"].asString();
                THROW_HR_IF(E_INVALIDARG, callId.empty() || callId.size() > 256 || !calls.insert(callId).second);
                Json::Value reply;
                reply["kind"] = "tool_result";
                reply["run_id"] = requestId;
                reply["call_id"] = callId;
                auto& receipt = reply["result"];
                receipt["status"] = "succeeded";
                receipt["effect"] = "not_applied";
                bool mutating = false;
                const auto finishCall = wil::scope_exit([&]() {
                    if (mutating)
                    {
                        if (receipt["status"] == "succeeded")
                            receipt["effect"] = "applied";
                        lastMutation = receipt;
                        if (palette->OmniCancellation() == cancellation && !cancellation->load())
                        {
                            palette->ParsedCommandLineText(winrt::to_hstring(jsonString(receipt)));
                        }
                    }
                    requestJson = jsonString(reply);
                    _agentPaneLog(fmt::format("omni_tool_result request={} tool={} status={} effect={}", requestId, toolName, receipt["status"].asString(), receipt["effect"].asString()));
                });
                try
                {
                    stage = "tool_validation";
                    reason = "invalid_tool_arguments";
                    plan = ::Microsoft::Terminal::Omni::ToolPlan(tools, toolName, plan["arguments"]);
                    const auto definition = std::find_if(tools.begin(), tools.end(), [&](const auto& entry) { return entry["name"] == toolName; });
                    const auto requestedMutation = (*definition)["effect"] == "mutation";
                    reason = "mutation_limit_reached";
                    THROW_HR_IF(E_CHANGED_STATE, requestedMutation && mutationStarted);
                    mutating = requestedMutation;
                    mutationStarted |= mutating;
                    const auto validateRequest = [cancellation, planner, requestId]() {
                        rejectOmni(cancellation->load(), L"request_cancelled", HRESULT_FROM_WIN32(ERROR_CANCELLED));
                        rejectOmni(!planner->IsToolRunCurrent(requestId), L"request_superseded");
                    };
                    if (mutating)
                    {
                        reason = "request_superseded";
                        THROW_HR_IF(E_CHANGED_STATE, !planner->IsToolRunCurrent(requestId));
                        palette->OmniHostOperation(cancellation, true);
                        if (plan["kind"] != "tab_layout")
                        {
                            stage = "wait_for_idle";
                            co_await page->_WaitForOmniIdle(validateRequest);
                            rejectOmni(cancellation->load(), L"request_cancelled", HRESULT_FROM_WIN32(ERROR_CANCELLED));
                            rejectOmni(!planner->IsToolRunCurrent(requestId), L"request_superseded");
                        }
                    }
                    reason = "operation_rejected";
                    if (plan["kind"] == "tab_layout")
                    {
                        stage = "tab_layout";
                        const auto layout = plan["layout"] == "vertical" ? TabLayout::Vertical : TabLayout::Horizontal;
                        co_await page->_ApplyOmniTabLayout(layout, validateRequest, &receipt);
                        receipt["data"]["kind"] = "tab_layout";
                        receipt["data"]["layout"] = plan["layout"];
                        _agentPaneLog("omni_executed request=" + requestId + " kind=tab_layout layout=" + plan["layout"].asString());
                        continue;
                    }
                    if (plan["kind"] == "get_context")
                    {
                        captureDirectories();
                        auto context = page->_RefreshOmniContext(request, target);
                        context["last_mutation"] = lastMutation;
                        trimOmniAppearanceMetadata(context, 20 * 1024);
                        if (jsonString(context).size() > 20 * 1024)
                        {
                            context["directory_panes"].clear();
                            context["directory_context_complete"] = false;
                            directoryPanes.clear();
                            directoryContextComplete = false;
                        }
                        rejectOmni(jsonString(context).size() > 22 * 1024, L"context_too_large", E_INVALIDARG);
                        receipt["data"] = std::move(context);
                        continue;
                    }
                    if (plan["kind"] == "close_pane")
                    {
                        stage = "close";
                        const auto id = plan["content_id"].asUInt();
                        const auto captured = std::find_if(target.panes.begin(), target.panes.end(), [&](const auto& entry) { return entry.first.ContentId() == id; });
                        THROW_HR_IF(E_INVALIDARG, captured == target.panes.end());
                        const auto resolve = [&]() -> winrt::com_ptr<Tab> {
                            rejectOmni(cancellation->load(), L"request_cancelled", HRESULT_FROM_WIN32(ERROR_CANCELLED));
                            rejectOmni(!planner->IsToolRunCurrent(requestId), L"request_superseded");
                            rejectOmni(page->_windowCloseAccepted || page->_windowPanesShutdown, L"window_closing");
                            rejectOmni(page->_changingTabLayout || page->_rearranging || page->_receivingContentTransfer, L"target_busy");
                            for (const auto& candidate : page->_tabs)
                            {
                                const auto tab = page->_GetTabImpl(candidate);
                                if (tab && tab->StableId() == target.tabId)
                                {
                                    const auto pane = findOmniControlPane(tab->GetRootPane(), id);
                                    rejectOmni(!pane || pane->GetTerminalControl() != captured->first || captured->first.Connection() != captured->second, L"target_pane_or_connection_changed");
                                    return tab;
                                }
                            }
                            throw winrt::hresult_error(E_CHANGED_STATE, L"target_tab_missing");
                        };
                        auto tab = resolve();
                        const auto paneContentId = findOmniControlPane(tab->GetRootPane(), id)->ContentId().value();
                        const auto pendingKey = std::wstring{ target.tabId } + L":" + std::to_wstring(paneContentId);
                        THROW_HR_IF(E_CHANGED_STATE, !page->_pendingTabStripPaneCloses.emplace(pendingKey).second);
                        const auto clearPending = wil::scope_exit([&]() { page->_pendingTabStripPaneCloses.erase(pendingKey); });
                        const auto closeScope = tab->GetPaneCloseScope(paneContentId);
                        THROW_HR_IF(E_CHANGED_STATE, closeScope.empty());
                        std::vector<std::pair<winrt::Microsoft::Terminal::Control::TermControl, winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection>> scopeConnections;
                        for (const auto& scopePane : closeScope)
                        {
                            scopePane->WalkTree([&](const auto& leaf) {
                                if (const auto terminal = leaf->GetTerminalControl())
                                    scopeConnections.emplace_back(terminal, terminal.Connection());
                            });
                        }
                        const auto revalidate = [&]() {
                            tab = resolve();
                            const auto currentScope = tab->GetPaneCloseScope(paneContentId);
                            THROW_HR_IF(E_CHANGED_STATE, currentScope.size() != closeScope.size());
                            for (size_t i = 0; i < closeScope.size(); ++i)
                            {
                                THROW_HR_IF(E_CHANGED_STATE, currentScope[i] != closeScope[i]);
                            }
                            for (const auto& [terminal, connection] : scopeConnections)
                                THROW_HR_IF(E_CHANGED_STATE, terminal.Connection() != connection);
                        };
                        if (page->_settings.GlobalSettings().ConfirmOnClose() == ConfirmOnClose::Always)
                        {
                            const auto wholeTab = closeScope.size() >= gsl::narrow_cast<size_t>(tab->GetLeafPaneCount());
                            const auto decision = co_await page->_ShowConfirmCloseDialog(wholeTab ? ConfirmCloseDialogKind::Tab : ConfirmCloseDialogKind::Pane);
                            if (decision != Windows::UI::Xaml::Controls::ContentDialogResult::Primary)
                            {
                                receipt["status"] = "cancelled";
                                receipt["reason"] = "native_confirmation_declined";
                                continue;
                            }
                            co_await page->_WaitForOmniIdle(validateRequest);
                            revalidate();
                        }
                        if (std::any_of(closeScope.begin(), closeScope.end(), [](const auto& pane) { return pane->ContainsReadOnly(); }))
                        {
                            const auto decision = co_await page->_ShowCloseReadOnlyDialog();
                            if (decision != Windows::UI::Xaml::Controls::ContentDialogResult::Primary)
                            {
                                receipt["status"] = "cancelled";
                                receipt["reason"] = "native_confirmation_declined";
                                continue;
                            }
                            co_await page->_WaitForOmniIdle(validateRequest);
                            revalidate();
                            receipt["effect"] = "partial_or_unknown";
                            for (const auto& pane : closeScope)
                            {
                                pane->WalkTree([](const auto& leaf) {
                                    if (const auto terminal = leaf->GetTerminalControl(); terminal && terminal.ReadOnly())
                                        terminal.ToggleReadOnly();
                                });
                            }
                        }
                        revalidate();
                        const auto pane = findOmniControlPane(tab->GetRootPane(), id);
                        receipt["effect"] = "partial_or_unknown";
                        page->_HandleClosePaneRequested(pane);
                        for (const auto& remaining : page->_tabs)
                        {
                            const auto remainingTab = page->_GetTabImpl(remaining);
                            THROW_HR_IF(E_FAIL, remainingTab && remainingTab->StableId() == target.tabId && findOmniControlPane(remainingTab->GetRootPane(), id, false));
                        }
                        receipt["data"]["kind"] = "close_pane";
                        receipt["data"]["content_id"] = id;
                        receipt["data"]["tab_id"] = winrt::to_string(target.tabId);
                        continue;
                    }
                    if (plan["kind"] == "focus_directory")
                    {
                        stage = "directory_resolve";
                        requireKeys(plan, { "kind", "directory" });
                        THROW_HR_IF(E_INVALIDARG, !plan["directory"].isString());
                        const auto selector = winrt::to_hstring(plan["directory"].asString());
                        THROW_HR_IF(E_INVALIDARG, selector.empty() || selector.size() > 1024 || std::any_of(selector.begin(), selector.end(), [](const auto ch) { return ch < 0x20 || (ch >= 0x7f && ch <= 0x9f); }) || !::Microsoft::Terminal::Omni::DirectorySelectorInPrompt(prompt, selector));
                        reason = "incomplete_directory_context";
                        THROW_HR_IF(E_CHANGED_STATE, !directoryContextComplete);
                        const DirectoryPane* destination = nullptr;
                        for (const auto& entry : directoryPanes)
                        {
                            if (::Microsoft::Terminal::Omni::DirectoryMatches(entry.cwd, selector) &&
                                ::Microsoft::Terminal::Omni::DirectorySelectorInPrompt(prompt, selector, ::Microsoft::Terminal::Omni::IsPosixDirectory(entry.cwd)))
                            {
                                reason = "ambiguous_directory";
                                THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_DUP_NAME), destination != nullptr);
                                destination = &entry;
                            }
                        }
                        reason = "directory_not_found";
                        THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_NOT_FOUND), !destination);
                        diagnosticContentId = destination->contentId;
                        stage = "directory_revalidate";
                        reason = "target_missing_or_busy";
                        rejectOmni(page->_windowCloseAccepted || page->_windowPanesShutdown, L"window_closing");
                        winrt::com_ptr<Tab> tab;
                        uint32_t index{};
                        for (; index < page->_tabs.Size(); ++index)
                        {
                            const auto candidate = page->_GetTabImpl(page->_tabs.GetAt(index));
                            if (candidate && candidate->StableId() == destination->tabId)
                            {
                                tab = candidate;
                                break;
                            }
                        }
                        THROW_HR_IF(E_CHANGED_STATE, !tab);
                        const auto pane = findOmniControlPane(tab->GetRootPane(), diagnosticContentId);
                        reason = "target_identity_changed";
                        THROW_HR_IF(E_CHANGED_STATE, !pane || !pane->Id() || pane->GetTerminalControl() != destination->control || destination->control.Connection() != destination->connection);
                        // Recheck every current pane: cwd changes or new panes can create
                        // ambiguity while the planner is running.
                        size_t currentMatches{};
                        size_t currentPaneCount{};
                        for (const auto& candidate : page->_tabs)
                        {
                            if (const auto currentTab = page->_GetTabImpl(candidate))
                            {
                                currentTab->GetRootPane()->WalkTree([&](const auto& currentPane) {
                                    if (const auto currentControl = currentPane->GetTerminalControl())
                                    {
                                        THROW_HR_IF(E_CHANGED_STATE, ++currentPaneCount > 64);
                                        const auto [cwd, provenance] = directoryFor(currentControl);
                                        if (::Microsoft::Terminal::Omni::DirectoryMatches(cwd, selector) &&
                                            ::Microsoft::Terminal::Omni::DirectorySelectorInPrompt(prompt, selector, ::Microsoft::Terminal::Omni::IsPosixDirectory(cwd)))
                                            ++currentMatches;
                                    }
                                },
                                                                    true);
                            }
                        }
                        const auto [cwd, provenance] = directoryFor(destination->control);
                        reason = "directory_metadata_changed";
                        THROW_HR_IF(E_CHANGED_STATE, cwd != destination->cwd || provenance != destination->provenance || currentMatches != 1);
                        stage = "directory_focus";
                        reason = "focus_failed";
                        receipt["effect"] = "partial_or_unknown";
                        THROW_HR_IF(E_FAIL, !page->_SelectTab(index));
                        page->_UnZoomIfNeeded();
                        tab->PreparePaneFocus(*pane->Id());
                        THROW_HR_IF(E_FAIL, !tab->FocusPane(*pane->Id()) || tab->GetActivePane()->GetTerminalControl() != destination->control);
                        navigationCompleted = true;
                        receipt["data"]["kind"] = "focus_directory";
                        receipt["data"]["content_id"] = diagnosticContentId;
                        receipt["data"]["tab_id"] = winrt::to_string(tab->StableId());
                        _agentPaneLog(fmt::format("omni_executed request={} kind=focus_directory content_id={} tab_id={}", requestId, diagnosticContentId, winrt::to_string(tab->StableId())));
                        continue;
                    }
                    if (plan["kind"] == "find_content")
                    {
                        stage = "content_search";
                        requireKeys(plan, { "kind", "queries", "metadata_pane_ids" });
                        THROW_HR_IF(E_INVALIDARG, !plan["queries"].isArray() || plan["queries"].empty() || plan["queries"].size() > 3);
                        std::vector<hstring> queries;
                        for (const auto& query : plan["queries"])
                        {
                            THROW_HR_IF(E_INVALIDARG, !query.isString());
                            auto text = winrt::to_hstring(query.asString());
                            THROW_HR_IF(E_INVALIDARG, text.empty() || text.size() > 128 || std::any_of(text.begin(), text.end(), [](const auto ch) { return ch < 0x20 || (ch >= 0x7f && ch <= 0x9f); }));
                            queries.push_back(std::move(text));
                        }
                        std::vector<uint32_t> metadataPaneIds;
                        if (plan.isMember("metadata_pane_ids"))
                        {
                            THROW_HR_IF(E_INVALIDARG, !plan["metadata_pane_ids"].isArray() || plan["metadata_pane_ids"].size() > 8);
                            for (const auto& value : plan["metadata_pane_ids"])
                            {
                                THROW_HR_IF(E_INVALIDARG, !value.isUInt());
                                const auto id = value.asUInt();
                                THROW_HR_IF(E_INVALIDARG, std::find(metadataPaneIds.begin(), metadataPaneIds.end(), id) != metadataPaneIds.end() || std::none_of(directoryPanes.begin(), directoryPanes.end(), [&](const auto& pane) { return pane.contentId == id; }));
                                metadataPaneIds.push_back(id);
                            }
                        }
                        THROW_HR_IF(E_INVALIDARG, searchCount >= 2);
                        const auto attempt = searchCount++;
                        candidateBase = attempt * 24;
                        const auto previousPanes = std::move(panes);
                        panes.clear();
                        candidates.clear();
                        bool truncated = false;
                        for (const auto& candidate : page->_tabs)
                        {
                            if (const auto tab = page->_GetTabImpl(candidate))
                            {
                                tab->GetRootPane()->WalkTree([&](const auto& pane) {
                                    if (const auto paneControl = pane->GetTerminalControl())
                                    {
                                        if (panes.size() == 64)
                                        {
                                            truncated = true;
                                            return;
                                        }
                                        SearchPane captured{ tab->StableId(), hstring{ std::wstring_view{ candidate.Title() }.substr(0, 128) }, paneControl, paneControl.Connection(), {}, {}, false, {} };
                                        const auto previous = std::find_if(previousPanes.begin(), previousPanes.end(), [&](const auto& entry) {
                                            return entry.control == captured.control && entry.connection == captured.connection;
                                        });
                                        if (previous != previousPanes.end())
                                            captured.sentPassages = previous->sentPassages;
                                        const auto metadata = std::find_if(directoryPanes.begin(), directoryPanes.end(), [&](const auto& previous) {
                                            return previous.tabId == captured.tabId && previous.control == captured.control && previous.connection == captured.connection;
                                        });
                                        if (metadata != directoryPanes.end())
                                        {
                                            captured.metadata = metadata->metadata;
                                            captured.metadataCandidate = std::find(metadataPaneIds.begin(), metadataPaneIds.end(), metadata->contentId) != metadataPaneIds.end();
                                        }
                                        panes.push_back(std::move(captured));
                                    }
                                },
                                                             true);
                            }
                        }
                        std::stable_partition(panes.begin(), panes.end(), [](const auto& pane) { return pane.metadataCandidate; });
                        const auto searchQueries = winrt::single_threaded_vector<hstring>(std::move(queries)).GetView();
                        for (auto& pane : panes)
                        {
                            pane.matches.clear();
                            if (cancellation->load())
                                co_return;
                            try
                            {
                                const auto operation = pane.control.FindTextMatchesWithContextAsync(searchQueries, pane.metadataCandidate ? (attempt ? 512 : 128) : 0);
                                palette->OmniAsyncOperation(operation.as<IAsyncInfo>());
                                const auto matches = co_await operation;
                                pane.matches.assign(matches.begin(), matches.end());
                            }
                            catch (const winrt::hresult_error& e)
                            {
                                LOG_HR(e.code());
                                truncated = true;
                            }
                        }
                        palette->OmniAsyncOperation(nullptr);
                        if (cancellation->load())
                            co_return;
                        Json::Value selection;
                        selection["prompt"] = winrt::to_string(prompt);
                        selection["content_matches"] = Json::Value{ Json::arrayValue };
                        selection["pane_metadata"] = Json::Value{ Json::arrayValue };
                        candidates.clear();
                        size_t bytes = winrt::to_string(prompt).size();
                        std::vector<bool> metadataIncluded(panes.size());
                        // Interleave panes so a noisy pane cannot monopolize the context.
                        for (size_t depth = 0; std::any_of(panes.begin(), panes.end(), [&](const auto& pane) { return depth < pane.matches.size(); }); ++depth)
                        {
                            for (size_t p = 0; p < panes.size(); ++p)
                            {
                                auto& pane = panes[p];
                                if (depth >= pane.matches.size())
                                    continue;
                                const auto& match = pane.matches[depth];
                                const auto passage = std::pair{ match.AnchorText, match.Text };
                                if (std::find(pane.sentPassages.begin(), pane.sentPassages.end(), passage) != pane.sentPassages.end())
                                    continue;
                                const std::wstring_view text{ match.Text };
                                Json::Value entry;
                                entry["id"] = candidateBase + gsl::narrow<uint32_t>(candidates.size());
                                entry["tab_id"] = winrt::to_string(pane.tabId);
                                entry["content_id"] = pane.control.ContentId();
                                entry["title"] = winrt::to_string(pane.title);
                                entry["text"] = winrt::to_string(text);
                                auto anchor = std::wstring_view{ match.AnchorText };
                                anchor = anchor.substr(0, 512);
                                if (!anchor.empty() && anchor.back() >= 0xd800 && anchor.back() <= 0xdbff)
                                    anchor.remove_suffix(1);
                                entry["anchor_text"] = winrt::to_string(anchor);
                                const auto size = jsonString(entry).size() + (!metadataIncluded[p] && !pane.metadata.isNull() ? jsonString(pane.metadata).size() : 0);
                                if (candidates.size() >= 24 || bytes + size > 20 * 1024)
                                {
                                    truncated = true;
                                    continue;
                                }
                                bytes += size;
                                if (!metadataIncluded[p] && !pane.metadata.isNull())
                                {
                                    selection["pane_metadata"].append(pane.metadata);
                                    metadataIncluded[p] = true;
                                }
                                candidates.emplace_back(p, match.Id);
                                pane.sentPassages.push_back(passage);
                                selection["content_matches"].append(std::move(entry));
                            }
                        }
                        selection["search_truncated"] = truncated || !directoryContextComplete;
                        const auto selectionJson = jsonString(selection);
                        stage = "content_selection";
                        _agentPaneLog(fmt::format("omni_retrieved request={} attempt={} panes={} metadata_candidates={} passages={} truncated={}", requestId, attempt + 1, panes.size(), metadataPaneIds.size(), candidates.size(), truncated));
                        THROW_HR_IF(E_INVALIDARG, selectionJson.size() > 24 * 1024);
                        receipt["data"] = std::move(selection);
                        continue;
                    }
                    if (plan["kind"] == "reveal_content")
                    {
                        requireKeys(plan, { "kind", "match_id" });
                        THROW_HR_IF(E_INVALIDARG, !plan["match_id"].isUInt() || plan["match_id"].asUInt() < candidateBase || plan["match_id"].asUInt() - candidateBase >= candidates.size());
                        const auto [paneIndex, matchId] = candidates[plan["match_id"].asUInt() - candidateBase];
                        const auto& destination = panes[paneIndex];
                        stage = "content_target";
                        reason = "target_missing_or_busy";
                        diagnosticContentId = destination.control.ContentId();
                        diagnosticMatchId = matchId;
                        winrt::com_ptr<Tab> tab;
                        uint32_t index = 0;
                        for (; index < page->_tabs.Size(); ++index)
                        {
                            const auto candidate = page->_GetTabImpl(page->_tabs.GetAt(index));
                            if (candidate && candidate->StableId() == destination.tabId)
                            {
                                tab = candidate;
                                break;
                            }
                        }
                        THROW_HR_IF(E_CHANGED_STATE, !tab || page->_changingTabLayout || page->_rearranging || page->_receivingContentTransfer);
                        const auto id = gsl::narrow<uint32_t>(destination.control.ContentId());
                        const auto pane = findOmniControlPane(tab->GetRootPane(), id);
                        reason = "target_identity_changed";
                        THROW_HR_IF(E_CHANGED_STATE, !pane || !pane->Id() || pane->GetTerminalControl() != destination.control || destination.control.Connection() != destination.connection);
                        receipt["effect"] = "partial_or_unknown";
                        THROW_HR_IF(E_FAIL, !page->_SelectTab(index));
                        page->_UnZoomIfNeeded();
                        tab->PreparePaneFocus(*pane->Id());
                        receipt["data"]["focused_tab_id"] = winrt::to_string(tab->StableId());
                        receipt["data"]["activated_content_id"] = id;
                        // Keep keyboard focus in Omni until verification completes.
                        // Closing the palette restores focus to this active pane.
                        destination.control.UpdateLayout();
                        stage = "content_prepare";
                        reason = "anchor_expired_or_changed";
                        const auto operation = destination.control.PrepareTextMatchAsync(matchId);
                        palette->OmniAsyncOperation(operation.as<IAsyncInfo>());
                        const auto prepared = co_await operation;
                        palette->OmniAsyncOperation(nullptr);
                        rejectOmni(cancellation->load(), L"request_cancelled", HRESULT_FROM_WIN32(ERROR_CANCELLED));
                        // Re-resolve after the await: splits replace leaf Pane objects.
                        THROW_HR_IF(E_CHANGED_STATE, !planner->IsToolRunCurrent(requestId));
                        const auto current = findOmniControlPane(tab->GetRootPane(), id);
                        THROW_HR_IF(E_CHANGED_STATE, !prepared);
                        stage = "content_revalidate";
                        reason = "target_identity_or_focus_changed";
                        THROW_HR_IF(E_CHANGED_STATE, page->_GetFocusedTabImpl() != tab || !current || current->GetTerminalControl() != destination.control || destination.control.Connection() != destination.connection || tab->GetActivePane()->GetTerminalControl() != destination.control);
                        stage = "content_reveal";
                        reason = "anchor_expired_or_changed";
                        THROW_HR_IF(E_CHANGED_STATE, !destination.control.RevealTextMatch(matchId));
                        navigationCompleted = true;
                        receipt["data"]["kind"] = "reveal_content";
                        receipt["data"]["content_id"] = id;
                        receipt["data"]["match_id"] = matchId;
                        receipt["data"]["tab_id"] = winrt::to_string(tab->StableId());
                        _agentPaneLog(fmt::format("omni_executed request={} kind=reveal_content content_id={} match_id={} tab_id={}", requestId, id, matchId, winrt::to_string(tab->StableId())));
                        continue;
                    }
                    stage = "execute";
                    auto actual = page->_ExecuteOmniOperation(plan, target, &receipt);
                    _agentPaneLog("omni_executed request=" + requestId + " kind=" + actual["kind"].asString());
                    receipt["data"] = std::move(actual);
                }
                catch (...)
                {
                    const auto hr = winrt::to_hresult();
                    try
                    {
                        throw;
                    }
                    catch (const winrt::hresult_error& error)
                    {
                        const auto detail = winrt::to_string(error.message());
                        if (!detail.empty() && detail.size() <= 64 && std::all_of(detail.begin(), detail.end(), [](const auto ch) { return (ch >= 'a' && ch <= 'z') || ch == '_'; }))
                            reason = detail;
                    }
                    catch (...)
                    {
                    }
                    LOG_HR(hr);
                    receipt["status"] = hr == HRESULT_FROM_WIN32(ERROR_CANCELLED) ? "cancelled" : "failed";
                    receipt["reason"] = reason;
                    receipt["stage"] = stage;
                    receipt["hresult"] = fmt::format("0x{:08X}", static_cast<uint32_t>(hr));
                    _agentPaneLog(fmt::format("omni_tool_failed request={} tool={} stage={} reason={} effect={} hr=0x{:08X}", requestId, toolName, stage, reason, receipt["effect"].asString(), static_cast<uint32_t>(hr)));
                }
            }
            THROW_HR_IF(E_FAIL, !runCompleted);
        }
        catch (...)
        {
            failure = winrt::to_hresult();
            LOG_HR(failure);
        }
        co_await wil::resume_foreground(dispatcher);
        if (FAILED(failure))
        {
            _agentPaneLog(fmt::format("omni_failed request={} stage={} reason={} content_id={} match_id={} hr=0x{:08X}", requestId, stage, reason, diagnosticContentId, diagnosticMatchId, static_cast<uint32_t>(failure)));
            status = RS_(L"FreOverlay_ProgressFailed") + L" (" + winrt::to_hstring(fmt::format("0x{:08X}", static_cast<uint32_t>(failure))) + L")";
            if (lastMutation["status"] == "succeeded")
            {
                // A failed model acknowledgement must not turn a verified mutation into a retry.
                status = RS_(L"FreOverlay_ProgressCompleted") + L"\n" + winrt::to_hstring(jsonString(lastMutation));
                clearPrompt = true;
            }
            else if (!lastMutation.isNull())
            {
                status = status + L"\n" + winrt::to_hstring(jsonString(lastMutation));
            }
        }
        palette->CompleteOmniRequest(cancellation, status, clearPrompt);
        _agentPaneLog(fmt::format("omni_feedback request={} cancelled={} visible={}", requestId, cancellation->load(), palette->Visibility() == Windows::UI::Xaml::Visibility::Visible));
    }
}
