// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// This file contains the protocol bridge methods for TerminalPage.
// These methods are called by the TerminalProtocolComServer to query
// and mutate terminal state. They return typed WinRT structs across
// the DLL boundary.
//
// IMPORTANT: These methods are called from background threads (COM).
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
namespace Protocol = winrt::Microsoft::Terminal::Protocol;

namespace winrt::TerminalApp::implementation
{
    // Cross-thread state for ShowProtocolQuickPick.
    struct QuickPickState
    {
        HANDLE completedEvent = nullptr;
        winrt::hstring result;
    };

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
    static uint32_t _getPidFromPane(const std::shared_ptr<Pane>& pane)
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
                        return static_cast<uint32_t>(GetProcessId(reinterpret_cast<HANDLE>(handle)));
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

    // ============================================================================
    // Queries — return typed WinRT structs
    // ============================================================================

    Protocol::PaneInfo TerminalPage::GetProtocolActivePane()
    {
        return _runOnUIThread(*this, [&]() -> Protocol::PaneInfo {
            Protocol::PaneInfo result{};

            const auto focusedTabIdx = _GetFocusedTabIndex();
            if (!focusedTabIdx.has_value())
                return result;

            const auto tab = _tabs.GetAt(focusedTabIdx.value());
            const auto tabImpl = _GetTabImpl(tab);
            if (!tabImpl)
                return result;

            const auto activePane = tabImpl->GetActivePane();
            if (!activePane)
                return result;

            result.PaneId = winrt::to_hstring(std::to_string(activePane->ContentId().value()));
            result.TabId = winrt::to_hstring(std::to_string(focusedTabIdx.value()));
            result.IsActive = true;

            if (const auto termContent = activePane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
            {
                result.Title = termContent.Title();
                const auto profile = termContent.GetProfile();
                result.Profile = profile ? profile.Name() : L"";
            }

            result.Pid = _getPidFromPane(activePane);
            return result;
        });
    }

    Windows::Foundation::Collections::IVector<Protocol::TabInfo> TerminalPage::GetProtocolTabs()
    {
        return _runOnUIThread(*this, [&]() -> Windows::Foundation::Collections::IVector<Protocol::TabInfo> {
            auto tabs = winrt::single_threaded_vector<Protocol::TabInfo>();
            const auto focusedIdx = _GetFocusedTabIndex();

            for (uint32_t i = 0; i < _tabs.Size(); ++i)
            {
                const auto tab = _tabs.GetAt(i);
                const auto tabImpl = _GetTabImpl(tab);
                if (!tabImpl)
                    continue;

                Protocol::TabInfo info{};
                info.TabId = winrt::to_hstring(std::to_string(i));
                info.Title = tab.Title();
                info.IsActive = focusedIdx.has_value() && (focusedIdx.value() == i);
                info.PaneCount = tabImpl->GetLeafPaneCount();
                tabs.Append(info);
            }

            return tabs;
        });
    }

    Windows::Foundation::Collections::IVector<Protocol::PaneInfo> TerminalPage::GetProtocolPanes(hstring tabIdFilter)
    {
        return _runOnUIThread(*this, [&]() -> Windows::Foundation::Collections::IVector<Protocol::PaneInfo> {
            auto panes = winrt::single_threaded_vector<Protocol::PaneInfo>();
            const auto tabIdFilterStr = winrt::to_string(tabIdFilter);

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabIdStr = std::to_string(tabIdx);
                if (!tabIdFilterStr.empty() && tabIdStr != tabIdFilterStr)
                    continue;

                const auto tab = _tabs.GetAt(tabIdx);
                const auto tabImpl = _GetTabImpl(tab);
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto activePane = tabImpl->GetActivePane();

                rootPane->WalkTree([&](const auto& pane) {
                    if (!pane->GetContent())
                        return; // Skip branch nodes

                    Protocol::PaneInfo info{};
                    info.PaneId = winrt::to_hstring(std::to_string(pane->ContentId().value()));
                    info.TabId = winrt::to_hstring(tabIdStr);
                    info.IsActive = (activePane == pane);
                    info.Pid = _getPidFromPane(pane);

                    if (const auto termContent = pane->GetContent().try_as<TerminalApp::TerminalPaneContent>())
                    {
                        info.Title = termContent.Title();
                        const auto profile = termContent.GetProfile();
                        info.Profile = profile ? profile.Name() : L"";

                        if (const auto termControl = pane->GetTerminalControl())
                        {
                            info.Rows = termControl.ViewHeight();
                            info.Columns = 0;
                        }
                    }
                    else
                    {
                        info.Title = pane->GetContent().Title();
                        info.Profile = L"";
                    }

                    panes.Append(info);
                });
            }

            return panes;
        });
    }

    Protocol::PaneOutput TerminalPage::ReadProtocolPaneOutput(hstring paneId, hstring source, int32_t maxLines)
    {
        return _runOnUIThread(*this, [&]() -> Protocol::PaneOutput {
            Protocol::PaneOutput result{};
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));
            const auto sourceStr = winrt::to_string(source);
            if (maxLines <= 0)
                maxLines = 200;

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
                if (!foundPane)
                    continue;

                const auto termControl = foundPane->GetTerminalControl();
                if (!termControl)
                    return result; // empty PaneId signals not-ready

                hstring fullBuffer;
                try
                {
                    fullBuffer = termControl.ReadEntireBuffer();
                }
                catch (...)
                {
                    return result; // empty PaneId signals error
                }

                auto fullBufferStr = winrt::to_string(fullBuffer);
                std::vector<std::string> lines;
                std::istringstream iss(fullBufferStr);
                std::string line;
                while (std::getline(iss, line))
                {
                    if (!line.empty() && line.back() == '\r')
                        line.pop_back();
                    lines.push_back(line);
                }

                result.PaneId = paneId;

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

                    result.Content = winrt::to_hstring(content);
                    result.LineCount = lineCount;
                    result.Truncated = false;
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

                    result.Content = winrt::to_hstring(content);
                    result.LineCount = lineCount;
                    result.Truncated = truncated;
                }

                return result;
            }

            return result; // empty PaneId = not found
        });
    }

    Protocol::ProcessStatus TerminalPage::GetProtocolProcessStatus(hstring paneId)
    {
        return _runOnUIThread(*this, [&]() -> Protocol::ProcessStatus {
            Protocol::ProcessStatus result{};
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
                if (!foundPane)
                    continue;

                result.PaneId = paneId;

                const auto termControl = foundPane->GetTerminalControl();
                if (!termControl)
                {
                    result.State = L"unknown";
                    return result;
                }

                const auto conn = termControl.Connection();
                if (!conn)
                {
                    result.State = L"exited";
                    return result;
                }

                const auto connState = termControl.ConnectionState();

                if (connState == ConnectionState::Connected)
                {
                    result.State = L"running";
                    result.Pid = _getPidFromPane(foundPane);
                }
                else
                {
                    result.State = L"exited";
                    if (const auto conpty = conn.try_as<ConptyConnection>())
                    {
                        const auto handle = conpty.RootProcessHandle();
                        if (handle)
                        {
                            DWORD exitCode = 0;
                            if (GetExitCodeProcess(reinterpret_cast<HANDLE>(handle), &exitCode))
                            {
                                if (exitCode != STILL_ACTIVE)
                                {
                                    result.ExitCode = static_cast<int32_t>(exitCode);
                                    result.HasExitCode = true;
                                }
                            }
                            result.Pid = static_cast<uint32_t>(GetProcessId(reinterpret_cast<HANDLE>(handle)));
                        }
                    }
                }

                return result;
            }

            return result; // empty PaneId = not found
        });
    }

    Protocol::SessionVariable TerminalPage::GetProtocolSessionVariable(hstring paneId, hstring name)
    {
        return _runOnUIThread(*this, [&]() -> Protocol::SessionVariable {
            Protocol::SessionVariable result{};
            const auto paneIdVal = static_cast<uint32_t>(std::stoul(winrt::to_string(paneId)));

            for (uint32_t tabIdx = 0; tabIdx < _tabs.Size(); ++tabIdx)
            {
                const auto tabImpl = _GetTabImpl(_tabs.GetAt(tabIdx));
                if (!tabImpl)
                    continue;

                const auto rootPane = tabImpl->GetRootPane();
                if (!rootPane)
                    continue;

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
                if (!foundPane)
                    continue;

                result.PaneId = paneId;
                result.Name = name;

                const auto value = foundPane->GetSessionVariable(name);
                if (value.has_value())
                {
                    result.Value = value.value();
                    result.Exists = true;
                }
                else
                {
                    result.Value = L"";
                    result.Exists = false;
                }

                return result;
            }

            return result; // empty PaneId = not found
        });
    }

    // ============================================================================
    // Mutations — return typed structs or bool
    // ============================================================================

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

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
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

    Protocol::TabCreationResult TerminalPage::CreateProtocolTab(NewTerminalArgs args, bool background)
    {
        return _runOnUIThread(*this, [&]() -> Protocol::TabCreationResult {
            Protocol::TabCreationResult result{};

            auto pane = _MakePane(args, nullptr);
            _pendingProtocolEnvVars.reset();
            if (!pane)
                return result;

            _CreateNewTabFromPane(pane, -1, /*openInBackground=*/background);
            _tabContent.UpdateLayout(); // Force synchronous terminal initialization

            if (_tabs.Size() == 0)
                return result;

            const auto newTabIdx = _tabs.Size() - 1;
            const auto newTab = _tabs.GetAt(newTabIdx);
            const auto tabImpl = _GetTabImpl(newTab);

            result.TabId = winrt::to_hstring(std::to_string(newTabIdx));

            if (tabImpl)
            {
                const auto rootPane = tabImpl->GetRootPane();
                if (rootPane)
                {
                    result.PaneId = winrt::to_hstring(std::to_string(rootPane->ContentId().value()));
                    result.Pid = _getPidFromPane(rootPane);
                }
            }

            return result;
        });
    }

    Protocol::TabCreationResult TerminalPage::SplitProtocolPane(hstring paneId, SplitDirection direction, float size, NewTerminalArgs args, bool background)
    {
        return _runOnUIThread(*this, [&]() -> Protocol::TabCreationResult {
            Protocol::TabCreationResult result{};
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

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
                if (!foundPane)
                    continue;

                if (const auto id = foundPane->Id())
                {
                    tabImpl->FocusPane(id.value());
                }

                auto newPane = _MakePane(args, nullptr);
                _pendingProtocolEnvVars.reset();
                if (!newPane)
                    return result;

                // Capture new pane info before moving it into the split.
                const auto newPaneContentId = newPane->ContentId().value();
                const auto newPanePid = _getPidFromPane(newPane);

                _SplitPane(tabImpl, direction, size, std::move(newPane), /*focusNewPane=*/!background);
                _tabContent.UpdateLayout(); // Force synchronous terminal initialization

                result.TabId = winrt::to_hstring(std::to_string(tabIdx));
                result.PaneId = winrt::to_hstring(std::to_string(newPaneContentId));
                result.Pid = newPanePid;
                return result;
            }

            _pendingProtocolEnvVars.reset();
            return result;
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

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
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

                const auto foundPane = rootPane->FindPaneByContentId(paneIdVal);
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

    // ============================================================================
    // QuickPick — still uses JSON for the choices parameter (UI-facing, not IPC)
    // ============================================================================

    hstring TerminalPage::ShowProtocolQuickPick(hstring /*title*/, hstring choicesJson, bool /*allowFreeInput*/)
    {
        // Parse choices on the calling (I/O) thread — no UI needed.
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
        auto state = std::make_shared<QuickPickState>();
        state->completedEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);

        auto capturedChoices = std::move(choices);
        auto weakThis = get_weak();

        Dispatcher().RunAsync(CoreDispatcherPriority::Normal,
            [state, capturedChoices = std::move(capturedChoices), weakThis]() {
                auto strongThis = weakThis.get();
                if (!strongThis)
                {
                    SetEvent(state->completedEvent);
                    return;
                }

                // Build Command objects for each choice (name only, no action needed).
                auto commands = winrt::single_threaded_vector<Command>();
                for (Json::ArrayIndex i = 0; i < capturedChoices.size(); ++i)
                {
                    auto choiceText = winrt::to_hstring(capturedChoices[i].asString());
                    auto cmd = Command{};
                    cmd.Name(choiceText);
                    commands.Append(cmd);
                }

                auto palette = strongThis->LoadCommandPalette();
                palette.SetQuickPickCommands(commands);

                // Subscribe to QuickPickCompleted for the selection path.
                // The event fires BEFORE _close() in _dispatchQuickPick,
                // so we can set the result and signal the I/O thread directly.
                auto qpToken = std::make_shared<winrt::event_token>();
                *qpToken = palette.QuickPickCompleted(
                    [state, weakThis, qpToken](auto&&, const winrt::hstring& selectedName) {
                        // Build properly-escaped JSON result.
                        Json::Value result;
                        result["cancelled"] = false;
                        result["selected"] = winrt::to_string(selectedName);
                        Json::StreamWriterBuilder wb;
                        wb["indentation"] = "";
                        state->result = winrt::to_hstring(Json::writeString(wb, result));
                        SetEvent(state->completedEvent);
                    });

                // Visibility callback handles cancellation (Escape / click-away)
                // and cleanup (unregister event handlers, restore action map).
                auto visToken = std::make_shared<int64_t>(0);
                *visToken = palette.RegisterPropertyChangedCallback(
                    winrt::Windows::UI::Xaml::UIElement::VisibilityProperty(),
                    [state, weakThis, visToken, qpToken](
                        winrt::Windows::UI::Xaml::DependencyObject const& sender,
                        winrt::Windows::UI::Xaml::DependencyProperty const&) {
                        auto vis = winrt::unbox_value<winrt::Windows::UI::Xaml::Visibility>(
                            sender.GetValue(winrt::Windows::UI::Xaml::UIElement::VisibilityProperty()));

                        if (vis != winrt::Windows::UI::Xaml::Visibility::Collapsed)
                        {
                            return;
                        }

                        // Unregister both callbacks immediately.
                        sender.UnregisterPropertyChangedCallback(
                            winrt::Windows::UI::Xaml::UIElement::VisibilityProperty(), *visToken);

                        if (auto page = weakThis.get())
                        {
                            auto palette2 = page->LoadCommandPalette();
                            palette2.QuickPickCompleted(*qpToken);
                            palette2.SetActionMap(page->_settings.ActionMap());
                        }

                        // If result is still empty, the user cancelled (Escape / click-away).
                        if (state->result.empty())
                        {
                            state->result = winrt::to_hstring("{\"cancelled\":true,\"selected\":\"\"}");
                            SetEvent(state->completedEvent);
                        }
                    });

                palette.Visibility(winrt::Windows::UI::Xaml::Visibility::Visible);
            });

        // Block the I/O thread until the palette completes.
        WaitForSingleObject(state->completedEvent, INFINITE);
        CloseHandle(state->completedEvent);
        return state->result;
    }

    // ============================================================================
    // Coordinator
    // ============================================================================

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
}
