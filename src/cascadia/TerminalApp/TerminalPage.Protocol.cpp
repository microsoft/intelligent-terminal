// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// This file contains the protocol bridge methods for TerminalPage.
// These methods are called by the TerminalProtocolServer to query
// and mutate terminal state. They return JSON strings to avoid
// complex WinRT type definitions across the DLL boundary.
//
// IMPORTANT: These methods are called from background threads (pipe I/O).
// All access to UI state must be marshaled to the UI thread via Dispatcher().

#include "pch.h"
#include "TerminalPage.h"
#include "../../types/inc/utils.hpp"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"

#include <json/json.h>

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::TerminalApp::implementation
{
    // Helper: run a function on the UI thread and block until it completes.
    // If already on the UI thread, runs directly.
    template<typename F>
    static auto _runOnUIThread(const TerminalPage& page, F&& func) -> decltype(func())
    {
        using R = decltype(func());

        if (page.Dispatcher().HasThreadAccess())
        {
            return func();
        }

        R result{};
        std::exception_ptr exPtr;
        HANDLE completedEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);

        page.Dispatcher().RunAsync(CoreDispatcherPriority::Normal, [&]() {
            try
            {
                result = func();
            }
            catch (...)
            {
                exPtr = std::current_exception();
            }
            SetEvent(completedEvent);
        });

        WaitForSingleObject(completedEvent, INFINITE);
        CloseHandle(completedEvent);

        if (exPtr)
        {
            std::rethrow_exception(exPtr);
        }
        return result;
    }

    // Specialization for void return type
    template<typename F>
    static void _runOnUIThreadVoid(const TerminalPage& page, F&& func)
    {
        if (page.Dispatcher().HasThreadAccess())
        {
            func();
            return;
        }

        std::exception_ptr exPtr;
        HANDLE completedEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);

        page.Dispatcher().RunAsync(CoreDispatcherPriority::Normal, [&]() {
            try
            {
                func();
            }
            catch (...)
            {
                exPtr = std::current_exception();
            }
            SetEvent(completedEvent);
        });

        WaitForSingleObject(completedEvent, INFINITE);
        CloseHandle(completedEvent);

        if (exPtr)
        {
            std::rethrow_exception(exPtr);
        }
    }

    // Helper to get PID from a pane's terminal control connection.
    static Json::UInt _getPidFromPane(const std::shared_ptr<Pane>& pane)
    {
        if (const auto termControl = pane->GetTerminalControl())
        {
            const auto conn = termControl.Connection();
            if (conn)
            {
                if (const auto conpty = conn.try_as<ConptyConnection>())
                {
                    const auto handle = conpty.RootProcessHandle();
                    if (handle)
                    {
                        return static_cast<Json::UInt>(GetProcessId(reinterpret_cast<HANDLE>(handle)));
                    }
                }
            }
        }
        return 0;
    }

    uint32_t TerminalPage::TabCount() const
    {
        return _runOnUIThread(*this, [&]() -> uint32_t {
            return NumberOfTabs();
        });
    }

    Windows::Foundation::IReference<uint32_t> TerminalPage::FocusedTabIndex() const
    {
        return _runOnUIThread(*this, [&]() -> Windows::Foundation::IReference<uint32_t> {
            const auto idx = _GetFocusedTabIndex();
            if (idx.has_value())
            {
                return Windows::Foundation::IReference<uint32_t>(idx.value());
            }
            return nullptr;
        });
    }

    hstring TerminalPage::GetProtocolActivePaneJson()
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            const auto focusedTabIdx = _GetFocusedTabIndex();
            if (!focusedTabIdx.has_value())
            {
                return L"";
            }

            const auto tab = _tabs.GetAt(focusedTabIdx.value());
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
            {
                return L"";
            }

            const auto activePane = tabImpl->GetActivePane();
            if (!activePane)
            {
                return L"";
            }

            Json::Value result;
            result["pane_id"] = std::to_string(activePane->ProtocolId());
            result["tab_id"] = std::to_string(focusedTabIdx.value());

            if (const auto termContent = activePane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
            {
                result["title"] = winrt::to_string(termContent.Title());
                const auto profile = termContent.GetProfile();
                result["profile"] = profile ? winrt::to_string(profile.Name()) : "";
            }

            const auto pid = _getPidFromPane(activePane);
            if (pid != 0)
            {
                result["pid"] = pid;
            }

            Json::StreamWriterBuilder writerBuilder;
            writerBuilder["indentation"] = "";
            return winrt::to_hstring(Json::writeString(writerBuilder, result));
        });
    }

    hstring TerminalPage::GetProtocolTabsJson()
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            Json::Value tabs(Json::arrayValue);
            const auto focusedIdx = _GetFocusedTabIndex();

            for (uint32_t i = 0; i < _tabs.Size(); ++i)
            {
                const auto tab = _tabs.GetAt(i);
                const auto tabImpl = _GetTabImpl(tab);
                if (!tabImpl)
                {
                    continue;
                }

                Json::Value t;
                t["tab_id"] = std::to_string(i);
                t["title"] = winrt::to_string(tab.Title());
                t["is_active"] = focusedIdx.has_value() && (focusedIdx.value() == i);
                t["pane_count"] = tabImpl->GetLeafPaneCount();
                tabs.append(t);
            }

            Json::StreamWriterBuilder writerBuilder;
            writerBuilder["indentation"] = "";
            return winrt::to_hstring(Json::writeString(writerBuilder, tabs));
        });
    }

    hstring TerminalPage::GetProtocolPanesJson(hstring tabIdFilter)
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            Json::Value panes(Json::arrayValue);
            const auto tabIdFilterStr = winrt::to_string(tabIdFilter);

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabIdStr = std::to_string(tabIdx);
                if (!tabIdFilterStr.empty() && tabIdStr != tabIdFilterStr)
                {
                    continue;
                }

                const auto tab = _tabs.GetAt(tabIdx);
                const auto tabImpl = _GetTabImpl(tab);
                if (!tabImpl)
                {
                    continue;
                }

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                {
                    continue;
                }

                const auto activePane = tabImpl->GetActivePane();

                rootPane->WalkTree([&](const auto& pane) {
                    if (!pane->GetContent())
                    {
                        return; // Skip branch nodes
                    }

                    Json::Value p;
                    p["pane_id"] = std::to_string(pane->ProtocolId());
                    p["tab_id"] = tabIdStr;

                    if (const auto termContent = pane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
                    {
                        p["title"] = winrt::to_string(termContent.Title());
                        const auto profile = termContent.GetProfile();
                        p["profile"] = profile ? winrt::to_string(profile.Name()) : "";

                        if (const auto termControl = pane->GetTerminalControl())
                        {
                            p["size"]["rows"] = termControl.ViewHeight();
                            p["size"]["columns"] = 0;
                        }
                    }
                    else
                    {
                        p["title"] = winrt::to_string(pane->GetContent().Title());
                        p["profile"] = "";
                    }

                    p["is_active"] = (activePane == pane);

                    const auto pid = _getPidFromPane(pane);
                    if (pid != 0)
                    {
                        p["pid"] = pid;
                    }
                    else
                    {
                        p["pid"] = Json::nullValue;
                    }
                    p["process"] = "";

                    panes.append(p);
                });
            }

            Json::StreamWriterBuilder writerBuilder;
            writerBuilder["indentation"] = "";
            return winrt::to_hstring(Json::writeString(writerBuilder, panes));
        });
    }

    hstring TerminalPage::ReadProtocolPaneOutput(hstring paneId, hstring source, int32_t maxLines)
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));
            const auto sourceStr = winrt::to_string(source);
            if (maxLines <= 0)
            {
                maxLines = 200;
            }

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                {
                    continue;
                }

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                {
                    continue;
                }

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                {
                    continue;
                }

                const auto termControl = foundPane->GetTerminalControl();
                if (!termControl)
                {
                    return L"";
                }

                std::string fullBuffer;
                try
                {
                    fullBuffer = winrt::to_string(termControl.ReadEntireBuffer());
                }
                catch (...)
                {
                    Json::Value err;
                    err["pane_id"] = winrt::to_string(paneId);
                    err["error"] = "Terminal not yet initialized. Try again shortly.";
                    Json::StreamWriterBuilder wb;
                    wb["indentation"] = "";
                    return winrt::to_hstring(Json::writeString(wb, err));
                }

                std::vector<std::string> lines;
                std::istringstream iss(fullBuffer);
                std::string line;
                while (std::getline(iss, line))
                {
                    if (!line.empty() && line.back() == '\r')
                    {
                        line.pop_back();
                    }
                    lines.push_back(line);
                }

                Json::Value result;
                result["pane_id"] = winrt::to_string(paneId);

                if (sourceStr == "screen")
                {
                    const auto viewHeight = termControl.ViewHeight();
                    const auto startIdx = lines.size() > static_cast<size_t>(viewHeight)
                                              ? lines.size() - viewHeight
                                              : 0;

                    std::string content;
                    int lineCount = 0;
                    for (size_t i = startIdx; i < lines.size(); ++i)
                    {
                        if (!content.empty())
                            content += "\n";
                        content += lines[i];
                        lineCount++;
                    }

                    result["content"] = content;
                    result["line_count"] = lineCount;
                    result["truncated"] = false;
                }
                else
                {
                    const auto truncated = (static_cast<int32_t>(lines.size()) > maxLines);
                    const auto startIdx = truncated ? lines.size() - maxLines : 0;

                    std::string content;
                    int lineCount = 0;
                    for (size_t i = startIdx; i < lines.size(); ++i)
                    {
                        if (!content.empty())
                            content += "\n";
                        content += lines[i];
                        lineCount++;
                    }

                    result["content"] = content;
                    result["line_count"] = lineCount;
                    result["truncated"] = truncated;
                }

                Json::StreamWriterBuilder writerBuilder;
                writerBuilder["indentation"] = "";
                return winrt::to_hstring(Json::writeString(writerBuilder, result));
            }

            return L"";
        });
    }

    hstring TerminalPage::GetProtocolProcessStatus(hstring paneId)
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                    continue;

                Json::Value result;
                result["pane_id"] = winrt::to_string(paneId);

                const auto termControl = foundPane->GetTerminalControl();
                if (!termControl)
                {
                    result["state"] = "unknown";
                    Json::StreamWriterBuilder writerBuilder;
                    writerBuilder["indentation"] = "";
                    return winrt::to_hstring(Json::writeString(writerBuilder, result));
                }

                const auto conn = termControl.Connection();
                if (!conn)
                {
                    result["state"] = "exited";
                    result["exit_code"] = Json::nullValue;
                    result["pid"] = Json::nullValue;
                    Json::StreamWriterBuilder writerBuilder;
                    writerBuilder["indentation"] = "";
                    return winrt::to_hstring(Json::writeString(writerBuilder, result));
                }

                const auto connState = termControl.ConnectionState();

                if (connState == ConnectionState::Connected)
                {
                    result["state"] = "running";
                    const auto pid = _getPidFromPane(foundPane);
                    if (pid != 0)
                        result["pid"] = pid;
                }
                else
                {
                    result["state"] = "exited";
                    if (const auto conpty = conn.try_as<ConptyConnection>())
                    {
                        const auto handle = conpty.RootProcessHandle();
                        if (handle)
                        {
                            DWORD exitCode = 0;
                            if (GetExitCodeProcess(reinterpret_cast<HANDLE>(handle), &exitCode))
                            {
                                if (exitCode != STILL_ACTIVE)
                                    result["exit_code"] = static_cast<Json::Int>(exitCode);
                            }
                            const auto pid = GetProcessId(reinterpret_cast<HANDLE>(handle));
                            if (pid != 0)
                                result["pid"] = static_cast<Json::UInt>(pid);
                        }
                    }
                }

                Json::StreamWriterBuilder writerBuilder;
                writerBuilder["indentation"] = "";
                return winrt::to_hstring(Json::writeString(writerBuilder, result));
            }

            return L"";
        });
    }

    hstring TerminalPage::GetProtocolSessionVariable(hstring paneId, hstring name)
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                    continue;

                Json::Value result;
                result["pane_id"] = winrt::to_string(paneId);
                result["name"] = winrt::to_string(name);

                const auto value = foundPane->GetSessionVariable(name);
                if (value.has_value())
                {
                    result["value"] = winrt::to_string(value.value());
                    result["exists"] = true;
                }
                else
                {
                    result["value"] = Json::nullValue;
                    result["exists"] = false;
                }

                Json::StreamWriterBuilder writerBuilder;
                writerBuilder["indentation"] = "";
                return winrt::to_hstring(Json::writeString(writerBuilder, result));
            }

            return L"";
        });
    }

    bool TerminalPage::SetProtocolSessionVariable(hstring paneId, hstring name, hstring value)
    {
        return _runOnUIThread(*this, [&]() -> bool {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                    continue;

                if (value.empty())
                    foundPane->RemoveSessionVariable(name);
                else
                    foundPane->SetSessionVariable(name, value);
                return true;
            }

            return false;
        });
    }

    void TerminalPage::SetPendingProtocolEnv(hstring key, hstring value)
    {
        _runOnUIThreadVoid(*this, [&]() {
            if (!_pendingProtocolEnvVars.has_value())
            {
                _pendingProtocolEnvVars.emplace();
            }
            _pendingProtocolEnvVars.value()[std::wstring{ key }] = std::wstring{ value };
        });
    }

    void TerminalPage::ClearPendingProtocolEnv()
    {
        _runOnUIThreadVoid(*this, [&]() {
            _pendingProtocolEnvVars.reset();
        });
    }

    void TerminalPage::InitializeCoordinator(NewTerminalArgs args)
    {
        _runOnUIThreadVoid(*this, [&]() {
            if (_coordinatorInitialized)
            {
                return;
            }

            // Resolve the profile and create terminal settings.
            const auto profile = _settings.GetProfileForArgs(args);
            const auto controlSettings = winrt::Microsoft::Terminal::Settings::TerminalSettings::CreateWithNewTerminalArgs(_settings, args);

            // Create the connection (this picks up _pendingProtocolEnvVars).
            auto connection = _CreateConnectionFromSettings(profile, *controlSettings.DefaultSettings(), false);
            _pendingProtocolEnvVars.reset();

            // Create the TermControl.
            _coordinatorControl = _CreateNewControlAndContent(controlSettings, connection);

            // Host the control in the sidecar container.
            CoordinatorContainer().Children().Clear();
            CoordinatorContainer().Children().Append(_coordinatorControl);

            _coordinatorInitialized = true;
        });
    }

    void TerminalPage::ToggleCoordinator()
    {
        _runOnUIThreadVoid(*this, [&]() {
            // Lazily initialize the coordinator panel if it hasn't been set up yet.
            // This allows the toggle command to work even when the coordinator
            // wasn't started automatically (e.g. no commandline configured).
            if (!_coordinatorInitialized)
            {
                const auto& globals = _settings.GlobalSettings();
                auto commandline = std::wstring{ globals.AiCoordinatorCommandline() };

                // Fall back to the default shell if no coordinator CLI is configured.
                if (commandline.empty())
                {
                    commandline = L"cmd.exe";
                }

                // No cmd.exe wrapper needed — WT_PIPE_NAME is inherited from
                // the process environment, set in WindowEmperor::_startProtocolServer().
                NewTerminalArgs newTermArgs;
                newTermArgs.Commandline(winrt::hstring{ commandline });

                const auto profile = globals.AiCoordinatorProfile();
                if (!profile.empty())
                {
                    newTermArgs.Profile(profile);
                }

                newTermArgs.TabTitle(L"AI Assistant");
                newTermArgs.SuppressApplicationTitle(true);

                InitializeCoordinator(newTermArgs);
            }

            const auto border = CoordinatorBorder();
            if (border.Visibility() == winrt::Windows::UI::Xaml::Visibility::Visible)
            {
                border.Visibility(winrt::Windows::UI::Xaml::Visibility::Collapsed);
                // Return focus to the active pane in the current tab.
                if (const auto tab = _GetFocusedTab())
                {
                    tab.Focus(winrt::Windows::UI::Xaml::FocusState::Programmatic);
                }
            }
            else
            {
                border.Visibility(winrt::Windows::UI::Xaml::Visibility::Visible);
                // Focus the coordinator when showing it.
                if (_coordinatorControl)
                {
                    _coordinatorControl.Focus(winrt::Windows::UI::Xaml::FocusState::Programmatic);
                }
            }
        });
    }

    bool TerminalPage::CoordinatorVisible()
    {
        return _runOnUIThread(*this, [&]() -> bool {
            if (!_coordinatorInitialized)
            {
                return false;
            }
            return CoordinatorBorder().Visibility() == winrt::Windows::UI::Xaml::Visibility::Visible;
        });
    }

    hstring TerminalPage::CreateProtocolTab(NewTerminalArgs args, bool background)
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            auto pane = _MakePane(args, nullptr);
            _pendingProtocolEnvVars.reset();
            if (!pane)
            {
                return L"";
            }

            _CreateNewTabFromPane(pane, -1, /*openInBackground=*/background);
            _tabContent.UpdateLayout(); // Force synchronous terminal initialization

            if (_tabs.Size() == 0)
            {
                return L"";
            }

            const auto newTabIdx = _tabs.Size() - 1;
            const auto newTab = _tabs.GetAt(newTabIdx);
            const auto tabImpl = _GetTabImpl(newTab);

            Json::Value result;
            result["tab_id"] = std::to_string(newTabIdx);

            if (tabImpl)
            {
                const auto rootPane = tabImpl->GetRootPane();
                if (rootPane)
                {
                    result["pane_id"] = std::to_string(rootPane->ProtocolId());

                    const auto pid = _getPidFromPane(rootPane);
                    if (pid != 0)
                    {
                        result["pid"] = pid;
                    }
                }
            }

            Json::StreamWriterBuilder writerBuilder;
            writerBuilder["indentation"] = "";
            return winrt::to_hstring(Json::writeString(writerBuilder, result));
        });
    }

    hstring TerminalPage::SplitProtocolPane(hstring paneId, SplitDirection direction, float size, NewTerminalArgs args, bool background)
    {
        return _runOnUIThread(*this, [&]() -> hstring {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tab = _tabs.GetAt(tabIdx);
                const auto tabImpl = _GetTabImpl(tab);
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                    continue;

                if (const auto id = foundPane->Id())
                {
                    tabImpl->FocusPane(id.value());
                }

                auto newPane = _MakePane(args, nullptr);
                _pendingProtocolEnvVars.reset();
                if (!newPane)
                {
                    return L"";
                }

                // Capture new pane info before moving it into the split.
                const auto newPaneProtocolId = newPane->ProtocolId();
                const auto newPanePid = _getPidFromPane(newPane);

                _SplitPane(tabImpl, direction, size, std::move(newPane), /*focusNewPane=*/!background);
                _tabContent.UpdateLayout(); // Force synchronous terminal initialization

                Json::Value result;
                result["tab_id"] = std::to_string(tabIdx);
                result["pane_id"] = std::to_string(newPaneProtocolId);
                if (newPanePid != 0)
                {
                    result["pid"] = newPanePid;
                }

                Json::StreamWriterBuilder writerBuilder;
                writerBuilder["indentation"] = "";
                return winrt::to_hstring(Json::writeString(writerBuilder, result));
            }

            _pendingProtocolEnvVars.reset();
            return L"";
        });
    }

    bool TerminalPage::CloseProtocolPane(hstring paneId)
    {
        return _runOnUIThread(*this, [&]() -> bool {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                    continue;

                foundPane->Close();
                return true;
            }

            return false;
        });
    }

    bool TerminalPage::SendProtocolInput(hstring paneId, hstring text)
    {
        return _runOnUIThread(*this, [&]() -> bool {
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByProtocolId(paneIdVal);
                if (!foundPane)
                    continue;

                const auto termControl = foundPane->GetTerminalControl();
                if (!termControl)
                    return false;

                // Replace \n with \r — shells expect carriage return (Enter key)
                // rather than line feed to execute commands.
                std::wstring input{ text };
                std::replace(input.begin(), input.end(), L'\n', L'\r');
                termControl.SendInput(winrt::hstring{ input });
                return true;
            }

            return false;
        });
    }

    hstring TerminalPage::ShowProtocolQuickPick(hstring title, hstring choicesJson, bool allowFreeInput)
    {
        // We can't use _runOnUIThread here because ContentDialog::ShowAsync()
        // needs to pump UI messages. Instead, dispatch to the UI thread,
        // show the dialog async, and signal completion via an event.

        // Parse choices on the calling thread (no UI needed).
        Json::Value choices;
        {
            Json::CharReaderBuilder rb;
            std::string errors;
            auto choicesStr = winrt::to_string(choicesJson);
            std::istringstream stream(choicesStr);
            if (!Json::parseFromStream(rb, stream, &choices, &errors) || !choices.isArray())
            {
                return L"";
            }
        }

        // Shared state for cross-thread result passing.
        struct SharedState
        {
            HANDLE completedEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
            hstring result;
        };
        auto state = std::make_shared<SharedState>();

        // Capture what we need for the UI thread lambda.
        auto capturedTitle = title;
        auto capturedChoices = std::move(choices);
        auto capturedAllowFreeInput = allowFreeInput;
        auto weakThis = get_weak();

        Dispatcher().RunAsync(winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
            [state, capturedTitle, capturedChoices = std::move(capturedChoices), capturedAllowFreeInput, weakThis]() {
                auto strongThis = weakThis.get();
                if (!strongThis)
                {
                    SetEvent(state->completedEvent);
                    return;
                }

                // Build UI elements on the UI thread.
                auto panel = winrt::Windows::UI::Xaml::Controls::StackPanel();
                panel.Spacing(8);

                std::vector<winrt::Windows::UI::Xaml::Controls::RadioButton> radioButtons;
                for (Json::ArrayIndex i = 0; i < capturedChoices.size(); ++i)
                {
                    auto rb = winrt::Windows::UI::Xaml::Controls::RadioButton();
                    rb.Content(winrt::box_value(winrt::to_hstring(capturedChoices[i].asString())));
                    rb.GroupName(L"QuickPick");
                    if (i == 0)
                    {
                        rb.IsChecked(true);
                    }
                    panel.Children().Append(rb);
                    radioButtons.push_back(rb);
                }

                winrt::Windows::UI::Xaml::Controls::TextBox freeInputBox{ nullptr };
                winrt::Windows::UI::Xaml::Controls::RadioButton freeInputRadio{ nullptr };
                if (capturedAllowFreeInput)
                {
                    freeInputRadio = winrt::Windows::UI::Xaml::Controls::RadioButton();
                    freeInputRadio.Content(winrt::box_value(winrt::hstring{ L"Other (type below):" }));
                    freeInputRadio.GroupName(L"QuickPick");
                    panel.Children().Append(freeInputRadio);

                    freeInputBox = winrt::Windows::UI::Xaml::Controls::TextBox();
                    freeInputBox.PlaceholderText(L"Type your response...");
                    freeInputBox.Margin({ 24, 0, 0, 0 });
                    freeInputBox.GotFocus([freeInputRadio](auto&&, auto&&) {
                        freeInputRadio.IsChecked(true);
                    });
                    panel.Children().Append(freeInputBox);
                }

                winrt::Windows::UI::Xaml::Controls::ContentDialog dialog;
                dialog.Title(winrt::box_value(capturedTitle));
                dialog.Content(panel);
                dialog.PrimaryButtonText(L"OK");
                dialog.CloseButtonText(L"Cancel");
                dialog.DefaultButton(winrt::Windows::UI::Xaml::Controls::ContentDialogButton::Primary);

                if (const auto root = strongThis->XamlRoot())
                {
                    dialog.XamlRoot(root);
                }

                // ShowAsync returns asynchronously — does NOT block the UI thread.
                auto asyncOp = dialog.ShowAsync();
                asyncOp.Completed([state, radioButtons, freeInputRadio, freeInputBox, capturedChoices](
                    auto&& operation, auto&& status) {
                    Json::Value result;

                    if (status != winrt::Windows::Foundation::AsyncStatus::Completed ||
                        operation.GetResults() != winrt::Windows::UI::Xaml::Controls::ContentDialogResult::Primary)
                    {
                        result["cancelled"] = true;
                        result["selected"] = "";
                    }
                    else
                    {
                        std::string selectedText;
                        if (freeInputRadio && freeInputRadio.IsChecked().Value())
                        {
                            selectedText = freeInputBox ? winrt::to_string(freeInputBox.Text()) : "";
                        }
                        else
                        {
                            for (size_t i = 0; i < radioButtons.size(); ++i)
                            {
                                if (radioButtons[i].IsChecked().Value())
                                {
                                    selectedText = capturedChoices[static_cast<Json::ArrayIndex>(i)].asString();
                                    break;
                                }
                            }
                        }
                        result["cancelled"] = false;
                        result["selected"] = selectedText;
                    }

                    Json::StreamWriterBuilder wb;
                    wb["indentation"] = "";
                    state->result = winrt::to_hstring(Json::writeString(wb, result));
                    SetEvent(state->completedEvent);
                });
            });

        // Block the pipe I/O thread until the dialog completes.
        WaitForSingleObject(state->completedEvent, INFINITE);
        CloseHandle(state->completedEvent);
        return state->result;
    }
}
