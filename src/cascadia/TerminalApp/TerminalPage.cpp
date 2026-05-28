
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TerminalPage.h"

#include <iomanip>

#include <json/json.h>
#include <TerminalCore/ControlKeyStates.hpp>
#include <TerminalThemeHelpers.h>
#include <til/hash.h>
#include <til/unicode.h>
#include <Utils.h>

#include "../../types/inc/ColorFix.hpp"
#include "../../types/inc/utils.hpp"
#include "../WinRTUtils/inc/WtExeUtils.h"
#include "../inc/AgentRegistry.h"
#include "../inc/AgentPolicy.h"
#include "../TerminalSettingsAppAdapterLib/TerminalSettings.h"
#include "AgentPaneContent.h"
#include "AgentPaneDragStash.h"
#include "AgentPaneLog.h"
#include "App.h"
#include "DebugTapConnection.h"
#include "FreOverlay.h"
#include "MarkdownPaneContent.h"
#include "Remoting.h"
#include "ScratchpadContent.h"
#include "SettingsPaneContent.h"
#include "SharedWta.h"
#include "SnippetsPaneContent.h"
#include "TabRowControl.h"
#include "TerminalSettingsCache.h"

#include "LaunchPositionRequest.g.cpp"
#include "RenameWindowRequestedArgs.g.cpp"
#include "RequestMoveContentArgs.g.cpp"
#include "TerminalPage.g.cpp"

using namespace winrt;
using namespace winrt::Microsoft::Management::Deployment;
using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using namespace winrt::Microsoft::Terminal;
using namespace winrt::Windows::ApplicationModel::DataTransfer;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Windows::System;
using namespace winrt::Windows::UI;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Windows::UI::Text;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Media;
namespace AgentPolicy = ::Microsoft::Terminal::Settings::Model::AgentPolicy;
using namespace ::TerminalApp;
using namespace ::Microsoft::Console;
using namespace ::Microsoft::Terminal::Core;
using namespace std::chrono_literals;

#define HOOKUP_ACTION(action) _actionDispatch->action({ this, &TerminalPage::_Handle##action });

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    namespace WUX = Windows::UI::Xaml;
    using IInspectable = Windows::Foundation::IInspectable;
    using VirtualKeyModifiers = Windows::System::VirtualKeyModifiers;
}

namespace clipboard
{
    static SRWLOCK lock = SRWLOCK_INIT;

    struct ClipboardHandle
    {
        explicit ClipboardHandle(bool open) :
            _open{ open }
        {
        }

        ~ClipboardHandle()
        {
            if (_open)
            {
                ReleaseSRWLockExclusive(&lock);
                CloseClipboard();
            }
        }

        explicit operator bool() const noexcept
        {
            return _open;
        }

    private:
        bool _open = false;
    };

    ClipboardHandle open(HWND hwnd)
    {
        // Turns out, OpenClipboard/CloseClipboard are not thread-safe whatsoever,
        // and on CloseClipboard, the GetClipboardData handle may get freed.
        // The problem is that WinUI also uses OpenClipboard (through WinRT which uses OLE),
        // and so even with this mutex we can still crash randomly if you copy something via WinUI.
        // Makes you wonder how many Windows apps are subtly broken, huh.
        AcquireSRWLockExclusive(&lock);

        bool success = false;

        // OpenClipboard may fail to acquire the internal lock --> retry.
        for (DWORD sleep = 10;; sleep *= 2)
        {
            if (OpenClipboard(hwnd))
            {
                success = true;
                break;
            }
            // 10 iterations
            if (sleep > 10000)
            {
                break;
            }
            Sleep(sleep);
        }

        if (!success)
        {
            ReleaseSRWLockExclusive(&lock);
        }

        return ClipboardHandle{ success };
    }

    void write(wil::zwstring_view text, std::string_view html, std::string_view rtf)
    {
        static const auto regular = [](const UINT format, const void* src, const size_t bytes) {
            wil::unique_hglobal handle{ THROW_LAST_ERROR_IF_NULL(GlobalAlloc(GMEM_MOVEABLE, bytes)) };

            const auto locked = GlobalLock(handle.get());
            memcpy(locked, src, bytes);
            GlobalUnlock(handle.get());

            THROW_LAST_ERROR_IF_NULL(SetClipboardData(format, handle.get()));
            handle.release();
        };
        static const auto registered = [](const wchar_t* format, const void* src, size_t bytes) {
            const auto id = RegisterClipboardFormatW(format);
            if (!id)
            {
                LOG_LAST_ERROR();
                return;
            }
            regular(id, src, bytes);
        };

        EmptyClipboard();

        if (!text.empty())
        {
            // As per: https://learn.microsoft.com/en-us/windows/win32/dataxchg/standard-clipboard-formats
            //   CF_UNICODETEXT: [...] A null character signals the end of the data.
            // --> We add +1 to the length. This works because .c_str() is null-terminated.
            regular(CF_UNICODETEXT, text.c_str(), (text.size() + 1) * sizeof(wchar_t));
        }

        if (!html.empty())
        {
            registered(L"HTML Format", html.data(), html.size());
        }

        if (!rtf.empty())
        {
            registered(L"Rich Text Format", rtf.data(), rtf.size());
        }
    }

    winrt::hstring read()
    {
        // This handles most cases of pasting text as the OS converts most formats to CF_UNICODETEXT automatically.
        if (const auto handle = GetClipboardData(CF_UNICODETEXT))
        {
            const wil::unique_hglobal_locked lock{ handle };
            const auto str = static_cast<const wchar_t*>(lock.get());
            if (!str)
            {
                return {};
            }

            const auto maxLen = GlobalSize(handle) / sizeof(wchar_t);
            const auto len = wcsnlen(str, maxLen);
            return winrt::hstring{ str, gsl::narrow_cast<uint32_t>(len) };
        }

        // We get CF_HDROP when a user copied a file with Ctrl+C in Explorer and pastes that into the terminal (among others).
        if (const auto handle = GetClipboardData(CF_HDROP))
        {
            const wil::unique_hglobal_locked lock{ handle };
            const auto drop = static_cast<HDROP>(lock.get());
            if (!drop)
            {
                return {};
            }

            const auto cap = DragQueryFileW(drop, 0, nullptr, 0);
            if (cap == 0)
            {
                return {};
            }

            auto buffer = winrt::impl::hstring_builder{ cap };
            const auto len = DragQueryFileW(drop, 0, buffer.data(), cap + 1);
            if (len == 0)
            {
                return {};
            }

            return buffer.to_hstring();
        }

        return {};
    }
} // namespace clipboard

namespace winrt::TerminalApp::implementation
{
    TerminalPage::TerminalPage(TerminalApp::WindowProperties properties, const TerminalApp::ContentManager& manager) :
        _tabs{ winrt::single_threaded_observable_vector<TerminalApp::Tab>() },
        _mruTabs{ winrt::single_threaded_observable_vector<TerminalApp::Tab>() },
        _manager{ manager },
        _hostingHwnd{},
        _WindowProperties{ std::move(properties) }
    {
        InitializeComponent();
        _WindowProperties.PropertyChanged({ get_weak(), &TerminalPage::_windowPropertyChanged });
    }

    TerminalPage::~TerminalPage()
    {
        // wta-helper processes are conpty children of TermControl and so
        // are torn down by the standard pane teardown path. No per-page
        // wta-process watch state to disarm here (removed in Phase 5).
    }

    // Method Description:
    // - implements the IInitializeWithWindow interface from shobjidl_core.
    // - We're going to use this HWND as the owner for the ConPTY windows, via
    //   ConptyConnection::ReparentWindow. We need this for applications that
    //   call GetConsoleWindow, and attempt to open a MessageBox for the
    //   console. By marking the conpty windows as owned by the Terminal HWND,
    //   the message box will be owned by the Terminal window as well.
    //   - see GH#2988
    HRESULT TerminalPage::Initialize(HWND hwnd)
    {
        if (!_hostingHwnd.has_value())
        {
            // GH#13211 - if we haven't yet set the owning hwnd, reparent all the controls now.
            for (const auto& tab : _tabs)
            {
                if (auto tabImpl{ _GetTabImpl(tab) })
                {
                    tabImpl->GetRootPane()->WalkTree([&](auto&& pane) {
                        if (const auto& term{ pane->GetTerminalControl() })
                        {
                            term.OwningHwnd(reinterpret_cast<uint64_t>(hwnd));
                        }
                    });
                }
                // We don't need to worry about resetting the owning hwnd for the
                // SUI here. GH#13211 only repros for a defterm connection, where
                // the tab is spawned before the window is created. It's not
                // possible to make a SUI tab like that, before the window is
                // created. The SUI could be spawned as a part of a window restore,
                // but that would still work fine. The window would be created
                // before restoring previous tabs in that scenario.
            }
        }

        _hostingHwnd = hwnd;
        return S_OK;
    }

    // INVARIANT: This needs to be called on OUR UI thread!
    void TerminalPage::SetSettings(CascadiaSettings settings, bool needRefreshUI)
    {
        assert(Dispatcher().HasThreadAccess());
        const bool firstLoad = (_settings == nullptr);
        if (firstLoad)
        {
            // Create this only on the first time we load the settings.
            _terminalSettingsCache = std::make_shared<TerminalSettingsCache>(settings);
        }
        _settings = settings;

        // Seed the agent-settings baseline on first load so that later
        // in-memory mutations (e.g. the bottom-bar agent selector click,
        // which mutates AcpAgent *before* calling _RebuildAgentStack) are
        // correctly diffed against the startup state. Without this, the
        // very first _RebuildAgentStack invocation would take the lazy
        // "seed-and-skip" branch with the *already-mutated* value and the
        // real rebuild would never run.
        if (firstLoad)
        {
            _lastAgentSettings = _CaptureAgentSettingsSnapshot();
            _agentSettingsSnapshotInitialized = true;
        }

        // Auto-suggest toggle hot-reload: when the effective auto-fix
        // value changes between settings reloads, push the new value
        // to WTA over the protocol. Tracks `EffectiveAutoFixEnabled`
        // (not the raw user pref) so GPO Forced/Blocked transitions
        // also propagate. WTA's `autofix_enabled` flag would
        // otherwise stay pinned to whatever `--no-autofix` value it
        // was launched with.
        {
            const bool currentAutoFix = _settings.GlobalSettings().EffectiveAutoFixEnabled();
            if (!_autoFixEnabledSnapshotInitialized)
            {
                _lastAutoFixEnabled = currentAutoFix;
                _autoFixEnabledSnapshotInitialized = true;
            }
            else if (_lastAutoFixEnabled != currentAutoFix)
            {
                _lastAutoFixEnabled = currentAutoFix;
                Json::Value evt;
                evt["type"] = "event";
                evt["method"] = "autofix_enabled_changed";
                Json::Value params;
                params["enabled"] = currentAutoFix;
                evt["params"] = params;
                Json::StreamWriterBuilder wb;
                wb["indentation"] = "";
                ProtocolVtSequenceReceived.raise(
                    *this,
                    winrt::to_hstring(Json::writeString(wb, evt)));
            }
        }

        // Make sure to call SetCommands before _RefreshUIForSettingsReload.
        // SetCommands will make sure the KeyChordText of Commands is updated, which needs
        // to happen before the Settings UI is reloaded and tries to re-read those values.
        if (const auto p = CommandPaletteElement())
        {
            p.SetActionMap(_settings.ActionMap());
        }

        if (needRefreshUI)
        {
            _RefreshUIForSettingsReload();
        }

        // Upon settings update we reload the system settings for scrolling as well.
        // TODO: consider reloading this value periodically.
        _systemRowsToScroll = _ReadSystemRowsToScroll();
    }

    bool TerminalPage::IsRunningElevated() const noexcept
    {
        // GH#2455 - Make sure to try/catch calls to Application::Current,
        // because that _won't_ be an instance of TerminalApp::App in the
        // LocalTests
        try
        {
            return Application::Current().as<TerminalApp::App>().Logic().IsRunningElevated();
        }
        CATCH_LOG();
        return false;
    }
    bool TerminalPage::CanDragDrop() const noexcept
    {
        try
        {
            return Application::Current().as<TerminalApp::App>().Logic().CanDragDrop();
        }
        CATCH_LOG();
        return true;
    }

    void TerminalPage::Create()
    {
        // Hookup the key bindings
        _HookupKeyBindings(_settings.ActionMap());

        _tabContent = this->TabContent();
        _tabRow = this->TabRow();
        _tabView = _tabRow.TabView();
        _rearranging = false;

        const auto canDragDrop = CanDragDrop();

        _tabView.CanReorderTabs(canDragDrop);
        _tabView.CanDragTabs(canDragDrop);
        _tabView.TabDragStarting({ get_weak(), &TerminalPage::_TabDragStarted });
        _tabView.TabDragCompleted({ get_weak(), &TerminalPage::_TabDragCompleted });

        auto tabRowImpl = winrt::get_self<implementation::TabRowControl>(_tabRow);
        _newTabButton = tabRowImpl->NewTabButton();

        if (_settings.GlobalSettings().ShowTabsInTitlebar())
        {
            // Remove the TabView from the page. We'll hang on to it, we need to
            // put it in the titlebar.
            uint32_t index = 0;
            if (this->Root().Children().IndexOf(_tabRow, index))
            {
                this->Root().Children().RemoveAt(index);
            }

            // Inform the host that our titlebar content has changed.
            SetTitleBarContent.raise(*this, _tabRow);

            // GH#13143 Manually set the tab row's background to transparent here.
            //
            // We're doing it this way because ThemeResources are tricky. We
            // default in XAML to using the appropriate ThemeResource background
            // color for our TabRow. When tabs in the titlebar are _disabled_,
            // this will ensure that the tab row has the correct theme-dependent
            // value. When tabs in the titlebar are _enabled_ (the default),
            // we'll switch the BG to Transparent, to let the Titlebar Control's
            // background be used as the BG for the tab row.
            //
            // We can't do it the other way around (default to Transparent, only
            // switch to a color when disabling tabs in the titlebar), because
            // looking up the correct ThemeResource from and App dictionary is a
            // HARD problem.
            const auto transparent = Media::SolidColorBrush();
            transparent.Color(Windows::UI::Colors::Transparent());
            _tabRow.Background(transparent);
        }
        _updateThemeColors();

        // Initialize the state of the CloseButtonOverlayMode property of
        // our TabView, to match the tab.showCloseButton property in the theme.
        if (const auto theme = _settings.GlobalSettings().CurrentTheme())
        {
            const auto visibility = theme.Tab() ? theme.Tab().ShowCloseButton() : Settings::Model::TabCloseButtonVisibility::Always;

            _tabItemMiddleClickHookEnabled = visibility == Settings::Model::TabCloseButtonVisibility::Never;

            switch (visibility)
            {
            case Settings::Model::TabCloseButtonVisibility::Never:
                _tabView.CloseButtonOverlayMode(MUX::Controls::TabViewCloseButtonOverlayMode::Auto);
                break;
            case Settings::Model::TabCloseButtonVisibility::Hover:
                _tabView.CloseButtonOverlayMode(MUX::Controls::TabViewCloseButtonOverlayMode::OnPointerOver);
                break;
            default:
                _tabView.CloseButtonOverlayMode(MUX::Controls::TabViewCloseButtonOverlayMode::Always);
                break;
            }
        }

        // Hookup our event handlers to the ShortcutActionDispatch
        _RegisterActionCallbacks();

        //Event Bindings (Early)
        _newTabButton.Click([weakThis{ get_weak() }](auto&&, auto&&) {
            if (auto page{ weakThis.get() })
            {
                TraceLoggingWrite(
                    g_hTerminalAppProvider,
                    "NewTabMenuDefaultButtonClicked",
                    TraceLoggingDescription("Event emitted when the default button from the new tab split button is invoked"),
                    TraceLoggingValue(page->NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
                    TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                    TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

                page->_OpenNewTerminalViaDropdown(NewTerminalArgs());
            }
        });
        _newTabButton.Drop({ get_weak(), &TerminalPage::_NewTerminalByDrop });
        _tabView.SelectionChanged({ this, &TerminalPage::_OnTabSelectionChanged });
        _tabView.TabCloseRequested({ this, &TerminalPage::_OnTabCloseRequested });
        _tabView.TabItemsChanged({ this, &TerminalPage::_OnTabItemsChanged });

        _tabView.TabDragStarting({ this, &TerminalPage::_onTabDragStarting });
        _tabView.TabStripDragOver({ this, &TerminalPage::_onTabStripDragOver });
        _tabView.TabStripDrop({ this, &TerminalPage::_onTabStripDrop });
        _tabView.TabDroppedOutside({ this, &TerminalPage::_onTabDroppedOutside });

        _CreateNewTabFlyout();

        _UpdateTabWidthMode();

        // Settings AllowDependentAnimations will affect whether animations are
        // enabled application-wide, so we don't need to check it each time we
        // want to create an animation.
        WUX::Media::Animation::Timeline::AllowDependentAnimations(!_settings.GlobalSettings().DisableAnimations());

        // Once the page is actually laid out on the screen, trigger all our
        // startup actions. Things like Panes need to know at least how big the
        // window will be, so they can subdivide that space.
        //
        // _OnFirstLayout will remove this handler so it doesn't get called more than once.
        _layoutUpdatedRevoker = _tabContent.LayoutUpdated(winrt::auto_revoke, { this, &TerminalPage::_OnFirstLayout });

        _isAlwaysOnTop = _settings.GlobalSettings().AlwaysOnTop();
        _showTabsFullscreen = _settings.GlobalSettings().ShowTabsFullscreen();

        // DON'T set up Toasts/TeachingTips here. They should be loaded and
        // initialized the first time they're opened, in whatever method opens
        // them.

        _tabRow.ShowElevationShield(IsRunningElevated() && _settings.GlobalSettings().ShowAdminShield());

        _adjustProcessPriorityThrottled = std::make_shared<ThrottledFunc<>>(
            DispatcherQueue::GetForCurrentThread(),
            til::throttled_func_options{
                .delay = std::chrono::milliseconds{ 100 },
                .debounce = true,
                .trailing = true,
            },
            [=]() {
                _adjustProcessPriority();
            });
    }

    Windows::UI::Xaml::Automation::Peers::AutomationPeer TerminalPage::OnCreateAutomationPeer()
    {
        return Automation::Peers::FrameworkElementAutomationPeer(*this);
    }

    // Method Description:
    // - This is a bit of trickiness: If we're running unelevated, and the user
    //   passed in only --elevate actions, the we don't _actually_ want to
    //   restore the layouts here. We're not _actually_ about to create the
    //   window. We're simply going to toss the commandlines
    // Arguments:
    // - <none>
    // Return Value:
    // - true if we're not elevated but all relevant pane-spawning actions are elevated
    bool TerminalPage::ShouldImmediatelyHandoffToElevated(const CascadiaSettings& settings) const
    {
        if (_startupActions.empty() || _startupConnection || IsRunningElevated())
        {
            // No point in handing off if we got no startup actions, or we're already elevated.
            // Also, we shouldn't need to elevate handoff ConPTY connections.
            assert(!_startupConnection);
            return false;
        }

        // Check that there's at least one action that's not just an elevated newTab action.
        for (const auto& action : _startupActions)
        {
            // Only new terminal panes will be requesting elevation.
            NewTerminalArgs newTerminalArgs{ nullptr };

            if (action.Action() == ShortcutAction::NewTab)
            {
                const auto& args{ action.Args().try_as<NewTabArgs>() };
                if (args)
                {
                    newTerminalArgs = args.ContentArgs().try_as<NewTerminalArgs>();
                }
                else
                {
                    // This was a nt action that didn't have any args. The default
                    // profile may want to be elevated, so don't just early return.
                }
            }
            else if (action.Action() == ShortcutAction::SplitPane)
            {
                const auto& args{ action.Args().try_as<SplitPaneArgs>() };
                if (args)
                {
                    newTerminalArgs = args.ContentArgs().try_as<NewTerminalArgs>();
                }
                else
                {
                    // This was a nt action that didn't have any args. The default
                    // profile may want to be elevated, so don't just early return.
                }
            }
            else
            {
                // This was not a new tab or split pane action.
                // This doesn't affect the outcome
                continue;
            }

            // It's possible that newTerminalArgs is null here.
            // GetProfileForArgs should be resilient to that.
            const auto profile{ settings.GetProfileForArgs(newTerminalArgs) };
            if (profile.Elevate())
            {
                continue;
            }

            // The profile didn't want to be elevated, and we aren't elevated.
            // We're going to open at least one tab, so return false.
            return false;
        }
        return true;
    }

    // Method Description:
    // - Escape hatch for immediately dispatching requests to elevated windows
    //   when first launched. At this point in startup, the window doesn't exist
    //   yet, XAML hasn't been started, but we need to dispatch these actions.
    //   We can't just go through ProcessStartupActions, because that processes
    //   the actions async using the XAML dispatcher (which doesn't exist yet)
    // - DON'T CALL THIS if you haven't already checked
    //   ShouldImmediatelyHandoffToElevated. If you're thinking about calling
    //   this outside of the one place it's used, that's probably the wrong
    //   solution.
    // Arguments:
    // - settings: the settings we should use for dispatching these actions. At
    //   this point in startup, we hadn't otherwise been initialized with these,
    //   so use them now.
    // Return Value:
    // - <none>
    void TerminalPage::HandoffToElevated(const CascadiaSettings& settings)
    {
        if (_startupActions.empty())
        {
            return;
        }

        // Hookup our event handlers to the ShortcutActionDispatch
        _settings = settings;
        _HookupKeyBindings(_settings.ActionMap());
        _RegisterActionCallbacks();

        for (const auto& action : _startupActions)
        {
            // only process new tabs and split panes. They're all going to the elevated window anyways.
            if (action.Action() == ShortcutAction::NewTab || action.Action() == ShortcutAction::SplitPane)
            {
                _actionDispatch->DoAction(action);
            }
        }
    }

    safe_void_coroutine TerminalPage::_NewTerminalByDrop(const Windows::Foundation::IInspectable&, winrt::Windows::UI::Xaml::DragEventArgs e)
    try
    {
        const auto data = e.DataView();
        if (!data.Contains(StandardDataFormats::StorageItems()))
        {
            co_return;
        }

        const auto weakThis = get_weak();
        const auto items = co_await data.GetStorageItemsAsync();
        const auto strongThis = weakThis.get();
        if (!strongThis)
        {
            co_return;
        }

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "NewTabByDragDrop",
            TraceLoggingDescription("Event emitted when the user drag&drops onto the new tab button"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

        for (const auto& item : items)
        {
            auto directory = item.Path();

            std::filesystem::path path(std::wstring_view{ directory });
            if (!std::filesystem::is_directory(path))
            {
                directory = winrt::hstring{ path.parent_path().native() };
            }

            NewTerminalArgs args;
            args.StartingDirectory(directory);
            _OpenNewTerminalViaDropdown(args);
        }
    }
    CATCH_LOG()

    // Method Description:
    // - This method is called once command palette action was chosen for dispatching
    //   We'll use this event to dispatch this command.
    // Arguments:
    // - command - command to dispatch
    // Return Value:
    // - <none>
    void TerminalPage::_OnDispatchCommandRequested(const IInspectable& sender, const Microsoft::Terminal::Settings::Model::Command& command)
    {
        const auto& actionAndArgs = command.ActionAndArgs();
        _actionDispatch->DoAction(sender, actionAndArgs);
    }

    // Method Description:
    // - This method is called once command palette command line was chosen for execution
    //   We'll use this event to create a command line execution command and dispatch it.
    // Arguments:
    // - command - command to dispatch
    // Return Value:
    // - <none>
    void TerminalPage::_OnCommandLineExecutionRequested(const IInspectable& /*sender*/, const winrt::hstring& commandLine)
    {
        ExecuteCommandlineArgs args{ commandLine };
        ActionAndArgs actionAndArgs{ ShortcutAction::ExecuteCommandline, args };
        _actionDispatch->DoAction(actionAndArgs);
    }

    // Method Description:
    // - This method is called when the user submits a foreground agent prompt
    //   from the command palette (? prefix). Opens or reuses an agent pane
    //   and sends the prompt to it.
    // Arguments:
    // - prompt - the user's agent prompt text
    // Return Value:
    // - <none>
    void TerminalPage::_OnAgentForegroundPromptRequested(const IInspectable& /*sender*/, const winrt::hstring& prompt)
    {
        _agentPaneLog("_OnAgentForegroundPromptRequested: prompt='" + winrt::to_string(prompt) + "' empty=" + (prompt.empty() ? "true" : "false"));
        if (!prompt.empty())
        {
            _DelegatePromptToAgent(prompt);
        }
        // Empty ? is a no-op. Use >Toggle AI assistant (openAgentPane action) to open the agent pane.
    }

    // Method Description:
    // - This method is called when the user submits a background agent task
    //   from the command palette (& prefix). Currently a stub that will be
    //   connected to the Background Task Manager (C9) in a future change.
    // Arguments:
    // - prompt - the user's background task prompt text
    // Return Value:
    // - <none>
    void TerminalPage::_OnAgentBackgroundTaskRequested(const IInspectable& /*sender*/, const winrt::hstring& /*prompt*/)
    {
        // TODO: Route to Background Task Manager (C9) when available.
        // For now, this is a no-op stub. Future implementation will:
        // 1. Submit the prompt to the Background Task Manager
        // 2. Show a status indicator in the tab bar
        // 3. Deliver results via toast notification on completion
    }

    // Method Description:
    // - Auto-detects an installed agent CLI by iterating the GPO-filtered
    //   built-in agent list and searching the system PATH for each.
    // Arguments:
    // - <none>
    // Return Value:
    // - The bare agent id (e.g. "copilot", "claude") of the first found
    //   CLI, or empty string if none found.  Callers must pass the result
    //   through _BuildAgentCommandLine to get a launchable command.
    winrt::hstring TerminalPage::_DetectAgentCli() const
    {
        wchar_t buffer[MAX_PATH];

        // Walk the policy-filtered agent list so we never auto-detect an
        // agent that is blocked by GPO.  FilteredAcpAgents() returns only
        // agents whose id passes AgentPolicy::IsAgentAllowed(); when no
        // AllowedAgents policy is configured it returns all built-in agents.
        namespace Reg = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
        for (const auto& agent : Reg::FilteredAcpAgents())
        {
            if (SearchPathW(nullptr, agent.id.data(), L".exe", MAX_PATH, buffer, nullptr) > 0 ||
                SearchPathW(nullptr, agent.id.data(), L".cmd", MAX_PATH, buffer, nullptr) > 0)
            {
                return winrt::hstring{ agent.id };
            }
        }
        return winrt::hstring{};
    }

    // Method Description:
    // - Auto-detects the WTA (Windows Terminal Agent) executable by searching
    //   the system PATH. WTA is the preferred way to launch agents because it
    //   handles protocol config injection, giving agents access to Windows Terminal
    //   tools (pane management, capture-pane, etc.) via the Terminal Protocol (wtcli).
    // Arguments:
    // - <none>
    // Return Value:
    // - The full path to wta.exe, or empty string if not found.
    winrt::hstring TerminalPage::_DetectWtaPath() const
    {
        const auto modulePath = std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>(nullptr) };
        const auto moduleDir = modulePath.parent_path();

        // 1. Check for wta.exe next to the running module (packaged / installed
        //    scenario).  A co-located wta.exe inherits the package identity of
        //    the Terminal process, which is required for COM activation.
        {
            const auto sibling = moduleDir / L"wta.exe";
            std::error_code ec;
            if (std::filesystem::exists(sibling, ec))
            {
                return winrt::hstring{ sibling.lexically_normal().wstring() };
            }
        }

        // 2. Walk up from the running module to find a local dev build.
        //    This is the fallback for development when wta.exe is not
        //    co-located with the Terminal binary.
        auto cursor = moduleDir;
        while (!cursor.empty())
        {
            for (const auto& relative : {
                     std::filesystem::path{ L"tools\\wta\\target\\debug\\wta.exe" },
                     std::filesystem::path{ L"tools\\wta\\target\\release\\wta.exe" },
                 })
            {
                const auto candidate = cursor / relative;
                std::error_code ec;
                if (std::filesystem::exists(candidate, ec))
                {
                    return winrt::hstring{ candidate.lexically_normal().wstring() };
                }
            }

            const auto parent = cursor.parent_path();
            if (parent == cursor)
            {
                break;
            }
            cursor = parent;
        }

        // 3. Fall back to system PATH.
        wchar_t buffer[MAX_PATH];
        if (SearchPathW(nullptr, L"wta", L".exe", MAX_PATH, buffer, nullptr) > 0)
        {
            return winrt::hstring{ buffer };
        }

        return winrt::hstring{};
    }

    // Look up a tab by its StableId. Linear scan over `_tabs`. Returns
    // nullptr if no tab has that id.
    winrt::com_ptr<Tab> TerminalPage::_FindTabByStableId(const winrt::hstring& stableId) const
    {
        if (stableId.empty())
        {
            return {};
        }
        for (const auto& tab : _tabs)
        {
            if (auto tabImpl = _GetTabImpl(tab))
            {
                if (tabImpl->StableId() == stableId)
                {
                    return tabImpl;
                }
            }
        }
        return {};
    }

    // Find the tab whose AgentPaneContent leaf == `content`. Used when an
    // AgentPaneContent click event fires; the sender doesn't know its tab.
    winrt::com_ptr<Tab> TerminalPage::_FindTabHostingAgentPaneContent(const winrt::TerminalApp::AgentPaneContent& content) const
    {
        if (!content)
        {
            return {};
        }
        for (const auto& tab : _tabs)
        {
            if (auto tabImpl = _GetTabImpl(tab))
            {
                if (tabImpl->FindAgentPaneContent() == content)
                {
                    return tabImpl;
                }
            }
        }
        return {};
    }

    SplitDirection TerminalPage::_AgentPanePositionToSplitDirection(const winrt::hstring& position)
    {
        if (position == L"bottom")
            return SplitDirection::Down;
        if (position == L"top")
            return SplitDirection::Up;
        if (position == L"left")
            return SplitDirection::Left;
        return SplitDirection::Right;
    }

    // ── First-run experience ──────────────────────────────────────────────

    bool TerminalPage::_IsFreRequired() const
    {
        return !ApplicationState::SharedInstance().AgentFreCompleted();
    }

    void TerminalPage::_ShowFreOverlay()
    {
        if (auto overlay = FindName(L"FreOverlayElement").try_as<winrt::TerminalApp::FreOverlay>())
        {
            overlay.Initialize(_settings);
            overlay.Completed({ get_weak(), &TerminalPage::_OnFreCompleted });
            overlay.Visibility(Visibility::Visible);

            // Focus the Next button so Enter triggers it immediately.
            // Also announce the page title to screen readers via
            // RaiseNotificationEvent so Narrator reads it on entry.
            // Dispatched at Low priority so it runs after all pending layout.
            Dispatcher().RunAsync(winrt::Windows::UI::Core::CoreDispatcherPriority::Low,
                [weak = get_weak()]() {
                    auto self = weak.get();
                    if (!self) return;
                    if (auto overlay = self->FreOverlayElement())
                    {
                        if (auto nextBtn = overlay.FindName(L"NextButton").try_as<Controls::Button>())
                        {
                            nextBtn.Focus(FocusState::Programmatic);

                            // Announce page title to screen readers
                            if (auto peer = winrt::Windows::UI::Xaml::Automation::Peers::FrameworkElementAutomationPeer::FromElement(nextBtn))
                            {
                                peer.RaiseNotificationEvent(
                                    winrt::Windows::UI::Xaml::Automation::Peers::AutomationNotificationKind::Other,
                                    winrt::Windows::UI::Xaml::Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                    RS_(L"FreOverlay_WelcomeTitle/Text"),
                                    L"FreWelcomeAnnouncement");
                            }
                        }
                    }
                });

            // Hide the tab bar during FRE — the full-screen wizard replaces
            // the entire window content. Restored in _OnFreCompleted.
            TabRow().Visibility(Visibility::Collapsed);
        }
    }

    void TerminalPage::_OnFreCompleted(const winrt::TerminalApp::FreOverlay& /*sender*/,
                                       const winrt::Windows::Foundation::IInspectable& /*args*/)
    {
        // Hide the FRE overlay
        if (auto overlay = FreOverlayElement())
        {
            overlay.Visibility(Visibility::Collapsed);
        }

        // Restore the tab bar
        TabRow().Visibility(Visibility::Visible);

        // Persist: never show FRE again
        ApplicationState::SharedInstance().AgentFreCompleted(true);

        // Flush settings to disk (FreOverlay already wrote to GlobalSettings
        // via _OnSaveButtonClick; if the user closed without saving, defaults
        // are used).
        _lastAgentSettings = _CaptureAgentSettingsSnapshot();
        try
        {
            _settings.WriteSettingsToDisk();
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
        }

        // Now create the agent pane — settings are already configured.
        // No --setup first-run needed; WTA starts in normal mode.
        // After FRE the pane should be visible so the user sees the
        // welcome hint immediately.
        if (const auto tab = _GetFocusedTabImpl())
        {
            _OpenOrReuseAgentPane(false, L"FirstRunExperience");
            // Focus is set in the Initialized callback once the pane is ready.
        }
    }

    // Repositions agent panes (one per tab) to match the current
    // AgentPanePosition setting. Walks every tab in this window.
    void TerminalPage::_RepositionAgentPanes()
    {
        const auto splitDirection = _AgentPanePositionToSplitDirection(
            _settings.GlobalSettings().AgentPanePosition());
        const auto position = _settings.GlobalSettings().AgentPanePosition();
        for (const auto& tab : _tabs)
        {
            if (auto tabImpl = _GetTabImpl(tab))
            {
                if (const auto rootPane = tabImpl->GetRootPane())
                {
                    rootPane->RepositionAgentPane(splitDirection);
                }
                if (const auto agentContent = tabImpl->FindAgentPaneContent())
                {
                    agentContent.SetAgentPanePosition(position);
                }
            }
        }
    }

    // Check if this is a custom agent ID (starts with "custom:").
    static bool _IsCustomAgentId(const winrt::hstring& id)
    {
        return winrt::to_string(id).starts_with("custom:");
    }

    // Build a launchable command line from a bare agent id (e.g. "copilot"
    // → "copilot --acp --stdio").  This is the single source of truth for
    // id-to-commandline mapping; both the settings path and the auto-detect
    // fallback route through here.
    static winrt::hstring _BuildAgentCommandLine(
        const winrt::hstring& agentId,
        const winrt::hstring& model)
    {
        const auto lower = winrt::to_string(agentId);

        // Adapter-style launches: claude/codex CLIs don't speak ACP themselves.
        if (lower == "claude")
        {
            return winrt::hstring{ L"npx -y @zed-industries/claude-code-acp" };
        }
        if (lower == "codex")
        {
            return winrt::hstring{ L"npx -y @zed-industries/codex-acp" };
        }

        std::wstring cmd{ agentId };
        if (lower == "copilot")
        {
            cmd += L" --acp --stdio";
        }
        else if (lower == "gemini")
        {
            cmd += L" --experimental-acp";
        }

        if (lower == "copilot" || lower == "gemini")
        {
            if (!model.empty())
            {
                cmd += L" --model ";
                cmd += std::wstring_view{ model };
            }
            return winrt::hstring{ cmd };
        }

        // Unknown agent — return the bare id as-is.
        return agentId;
    }

    static winrt::hstring _ResolveEffectiveAgentCliPath(
        const winrt::Microsoft::Terminal::Settings::Model::GlobalAppSettings& globals,
        const std::function<winrt::hstring()>& detectFallback)
    {
        // Use the policy-aware getter — returns empty if the selected agent
        // is blocked by GPO, ensuring we never launch a disallowed agent.
        const auto acpAgent = globals.EffectiveAcpAgent();
        if (acpAgent.empty())
        {
            // The user's selection is blocked or absent. Try auto-detection
            // which will itself only pick a policy-allowed agent.
            if (detectFallback)
            {
                const auto detected = detectFallback();
                if (!detected.empty())
                {
                    return _BuildAgentCommandLine(detected, globals.AcpModel());
                }
            }
            return winrt::hstring{};
        }

        // Custom agents: use the stored custom command directly.
        if (_IsCustomAgentId(acpAgent))
        {
            const auto customCmd = globals.AcpCustomCommand();
            if (!customCmd.empty()) return customCmd;
        }

        return _BuildAgentCommandLine(acpAgent, globals.AcpModel());
    }

    // Resolve the effective delegate agent name from structured settings.
    static winrt::hstring _ResolveEffectiveDelegateAgent(
        const winrt::Microsoft::Terminal::Settings::Model::GlobalAppSettings& globals)
    {
        // Use the policy-aware getter — returns empty if the selected agent
        // is blocked by GPO.
        const auto delegateAgent = globals.EffectiveDelegateAgent();
        if (delegateAgent.empty())
        {
            return winrt::hstring{};
        }
        // For custom agents, pass the full command so WTA can launch it.
        if (_IsCustomAgentId(delegateAgent))
        {
            const auto customCmd = globals.DelegateCustomCommand();
            if (!customCmd.empty()) return customCmd;
        }
        return delegateAgent;
    }

    // Resolve the effective UI language for wta.
    // Priority: explicit settings.json "language" override → MRT's resolved
    // language qualifier (matches what XAML actually renders) → empty (let
    // wta fall back to sys_locale).
    //
    // Without this, wta uses sys_locale::get_locale() (Windows
    // GetUserDefaultUILanguage), which returns just the UI culture (e.g.
    // "en-US"). The C++ XAML side uses MRT, which walks the preferred UI
    // languages list and may resolve to a different locale (e.g.
    // "zh-Hans-CN" / "zh-CN"). Passing the MRT-resolved language to wta
    // keeps both sides in sync.
    static winrt::hstring _ResolveEffectiveLanguage(
        const winrt::Microsoft::Terminal::Settings::Model::GlobalAppSettings& globals)
    {
        if (const auto lang = globals.Language(); !lang.empty())
        {
            return lang;
        }
        try
        {
            const auto context{ winrt::Windows::ApplicationModel::Resources::Core::ResourceContext::GetForViewIndependentUse() };
            const auto qualifiers{ context.QualifierValues() };
            if (const auto language{ qualifiers.TryLookup(L"language") })
            {
                return winrt::hstring{ *language };
            }
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
        }
        return winrt::hstring{};
    }

    void TerminalPage::_DelegatePromptToAgent(const winrt::hstring& prompt)
    {
        _agentPaneLog("_DelegatePromptToAgent called, prompt='" + winrt::to_string(prompt) + "'");

        // Find the WTA executable.
        const auto wtaPath = _DetectWtaPath();
        if (wtaPath.empty())
        {
            _agentPaneLog("ABORT: no WTA path found");
            return;
        }

        // Resolve agent CLI from structured settings (acpAgent/acpModel).
        const auto& globals = _settings.GlobalSettings();
        const auto agentCliPath = _ResolveEffectiveAgentCliPath(globals, [this]() { return _DetectAgentCli(); });

        // If no agent resolved and an AllowedAgents policy is active, bail out.
        // This covers both "policy blocks ALL agents" and "policy allows some
        // agents but none are installed" — in either case we must not launch
        // WTA without --agent, because WTA's own fallback detection would
        // bypass GPO and pick an unauthorized agent (e.g. copilot).
        if (agentCliPath.empty() && AgentPolicy::IsAllowedAgentsPolicyConfigured())
        {
            _agentPaneLog("ABORT: delegation blocked by GPO — no agents allowed");
            if (auto tip{ FindName(L"WindowIdToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _UpdateTeachingTipTheme(tip.try_as<winrt::Windows::UI::Xaml::FrameworkElement>());
                tip.Title(RS_(L"AgentBlockedByPolicyTitle"));
                tip.Subtitle(RS_(L"AgentBlockedByPolicySubtitle"));
                tip.IsOpen(true);
            }
            return;
        }

        // Helper: escape and quote an argument for the command line.
        auto quoteArg = [](std::wstring_view arg) -> std::wstring {
            std::wstring escaped{ arg };
            for (size_t pos = 0; (pos = escaped.find(L'"', pos)) != std::wstring::npos; pos += 2)
            {
                escaped.replace(pos, 1, L"\\\"");
            }
            return L"\"" + escaped + L"\"";
        };

        // Build: wta [--language <lang>] delegate --agent <agent> --delegate-agent <delegate> "<prompt>"
        //
        // `--language` is a top-level Cli flag, so it must appear *before* the
        // `delegate` subcommand — otherwise clap rejects it and the process
        // exits before `logging::init("delegate")` runs (silent failure, no
        // wta-delegate.log, no new tab).
        std::wstring cmdline = quoteArg(wtaPath);

        if (const auto lang = _ResolveEffectiveLanguage(globals); !lang.empty())
        {
            cmdline += L" --language " + quoteArg(std::wstring_view{ lang });
        }

        cmdline += L" delegate";

        if (!agentCliPath.empty())
        {
            cmdline += L" --agent " + quoteArg(std::wstring_view{ agentCliPath });
        }

        const auto delegateAgent = _ResolveEffectiveDelegateAgent(globals);
        if (!delegateAgent.empty())
        {
            cmdline += L" --delegate-agent " + quoteArg(std::wstring_view{ delegateAgent });
        }
        const auto delegateModel = globals.DelegateModel();
        if (!delegateModel.empty())
        {
            cmdline += L" --delegate-model " + quoteArg(std::wstring_view{ delegateModel });
        }

        // Pass CWD from the active pane.
        winrt::hstring activeCwd;
        if (const auto& activeControl = _GetActiveControl())
        {
            activeCwd = activeControl.WorkingDirectory();
        }
        if (activeCwd.empty())
        {
            wchar_t homePath[MAX_PATH];
            if (GetEnvironmentVariableW(L"USERPROFILE", homePath, MAX_PATH) > 0)
            {
                activeCwd = winrt::hstring{ homePath };
            }
        }
        if (!activeCwd.empty())
        {
            cmdline += L" --cwd " + quoteArg(std::wstring_view{ activeCwd });
        }

        // Append the prompt as a positional argument.
        std::wstring escapedPrompt{ prompt };
        for (size_t pos = 0; (pos = escapedPrompt.find(L'"', pos)) != std::wstring::npos; pos += 2)
        {
            escapedPrompt.replace(pos, 1, L"\"\"");
        }
        cmdline += fmt::format(FMT_COMPILE(L" \"{}\""), escapedPrompt);

        _agentPaneLog("launching: " + winrt::to_string(winrt::hstring{ cmdline }));

        // Launch as a hidden background process.
        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE;

        wil::unique_process_information pi;
        auto mutableCmdline = cmdline;
        if (!CreateProcessW(
                wtaPath.c_str(),
                mutableCmdline.data(),
                nullptr,
                nullptr,
                FALSE,
                CREATE_NO_WINDOW,
                nullptr,
                nullptr,
                &si,
                &pi))
        {
            const auto err = GetLastError();
            _agentPaneLog("FAILED to launch delegate process: GetLastError=" +
                          std::to_string(err) +
                          " cmdline=" + winrt::to_string(winrt::hstring{ cmdline }));
            return;
        }

        // pi destructor closes hProcess + hThread on scope exit.
        _agentPaneLog("delegate process launched OK");
    }

    // --- Hot-reload of agent/model settings -------------------------------
    //
    // When any agent-identity setting changes, the single shared wta pane must
    // be torn down and recreated with the updated flags baked into argv.
    // `_RebuildAgentStack` is the single entry point; it is called from
    // SetSettings (settings.json reload + Settings UI writes) and from the
    // bottom-bar selector Click handler.
    TerminalPage::AgentSettingsSnapshot TerminalPage::_CaptureAgentSettingsSnapshot() const
    {
        const auto& globals = _settings.GlobalSettings();
        return AgentSettingsSnapshot{
            std::wstring{ globals.AcpAgent() },
            std::wstring{ globals.AcpModel() },
            std::wstring{ globals.AcpCustomCommand() },
            std::wstring{ globals.DelegateAgent() },
            std::wstring{ globals.DelegateModel() },
            std::wstring{ globals.DelegateCustomCommand() },
        };
    }

    bool TerminalPage::_AgentSettingsChanged(const AgentSettingsSnapshot& a, const AgentSettingsSnapshot& b)
    {
        return a.acpAgent != b.acpAgent ||
               a.acpModel != b.acpModel ||
               a.acpCustomCommand != b.acpCustomCommand ||
               a.delegateAgent != b.delegateAgent ||
               a.delegateModel != b.delegateModel ||
               a.delegateCustomCommand != b.delegateCustomCommand;
    }

    // Close the agent pane in a specific tab, if it has one.
    //
    // Under the helper+master architecture, wta-helper processes are
    // ordinary conpty children of TermControl — the standard pane teardown
    // path (Pane::Close → ConptyConnection::Close → conpty pipes closed →
    // helper exits) is enough. Each tab has at most one agent pane.
    void TerminalPage::_TeardownAgentPane(const winrt::com_ptr<Tab>& tab)
    {
        if (!tab)
        {
            return;
        }
        if (const auto pane = tab->FindAgentPane())
        {
            _agentPaneLog("_TeardownAgentPane: closing agent pane on tab");
            pane->Close();
        }
        // Refresh the window-level bottom bar if this tab was the active
        // one — its agent-pane state just transitioned to "absent".
        if (const auto activeTab = _GetFocusedTabImpl(); activeTab && activeTab == tab)
        {
            _UpdateBottomBarState();
        }
    }

    std::shared_ptr<Pane> TerminalPage::_WrapInAgentPaneContent(std::shared_ptr<Pane> rawPane)
    {
        if (!rawPane)
        {
            return rawPane;
        }
        const auto innerTerm = rawPane->GetContent().try_as<winrt::TerminalApp::TerminalPaneContent>();
        if (!innerTerm)
        {
            // Defensive fallback — shouldn't happen for a terminal-content pane.
            _agentPaneLog("_WrapInAgentPaneContent: rawPane content is not TerminalPaneContent — using unwrapped pane");
            return rawPane;
        }
        // The raw Pane's first border currently parents the TermControl;
        // release it before AgentPaneContent re-parents it as the wrapper's
        // ContentPresenter child. Without this, XAML throws because the
        // TermControl would be in two visual trees at once.
        if (const auto rootGrid = rawPane->GetRootElement())
        {
            if (rootGrid.Children().Size() > 0)
            {
                if (const auto border = rootGrid.Children().GetAt(0).try_as<winrt::Windows::UI::Xaml::Controls::Border>())
                {
                    border.Child(nullptr);
                }
            }
        }
        auto agentContent = winrt::make<winrt::TerminalApp::implementation::AgentPaneContent>(innerTerm);
        return std::make_shared<Pane>(agentContent);
    }

    // Tells wta to clear a tab's session (conversation history + ACP
    // SessionId binding) WITHOUT dropping the TabSession key. "Tab is
    // still around, but the user wants a clean slate for it."
    void TerminalPage::_NotifyAgentTabReset(const winrt::hstring& tabId)
    {
        if (tabId.empty())
        {
            return;
        }

        Json::Value tabEvt;
        tabEvt["type"] = "event";
        tabEvt["method"] = "reset_tab_session";
        Json::Value tabParams;
        tabParams["tab_id"] = winrt::to_string(tabId);
        tabEvt["params"] = tabParams;
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        ProtocolVtSequenceReceived.raise(
            *this,
            winrt::to_hstring(Json::writeString(wb, tabEvt)));
    }

    // Tells wta that a tab has been destroyed so it can drop the per-tab
    // TabSession and any session_to_tab routing keyed to it.
    void TerminalPage::_NotifyAgentTabClosed(const winrt::hstring& tabId)
    {
        if (tabId.empty())
        {
            return;
        }

        Json::Value tabEvt;
        tabEvt["type"] = "event";
        tabEvt["method"] = "tab_closed";
        Json::Value tabParams;
        tabParams["tab_id"] = winrt::to_string(tabId);
        tabParams["window_id"] = std::to_string(_WindowProperties.WindowId());
        tabEvt["params"] = tabParams;
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        ProtocolVtSequenceReceived.raise(
            *this,
            winrt::to_hstring(Json::writeString(wb, tabEvt)));
    }

    // Tells wta that the focused tab changed so it can re-project per-tab
    // agent-pane state (view, pane_open, autofix snapshot). wta echoes the
    // authoritative state via `agent_state_changed`, which `OnAgentStateChanged`
    // applies to the local AgentPaneContent mirror and refreshes the bottom bar.
    // wta is the single source of truth for these fields, so the C++ side
    // never reads its own cached mirror on tab switch — it just asks wta.
    void TerminalPage::_NotifyAgentTabChanged(const winrt::hstring& tabId)
    {
        if (tabId.empty())
        {
            return;
        }

        Json::Value tabEvt;
        tabEvt["type"] = "event";
        tabEvt["method"] = "tab_changed";
        Json::Value tabParams;
        tabParams["tab_id"] = winrt::to_string(tabId);
        tabParams["window_id"] = std::to_string(_WindowProperties.WindowId());
        tabEvt["params"] = tabParams;
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        ProtocolVtSequenceReceived.raise(
            *this,
            winrt::to_hstring(Json::writeString(wb, tabEvt)));
    }

    // C++ → wta request for changing per-tab agent-pane UI state. The target
    // tab carries its own StableId. wta echoes back via `agent_state_changed`
    // which lands in `OnAgentStateChanged`, which routes by `tab_id` to the
    // specific AgentPaneContent.
    void TerminalPage::_RequestAgentStateForTab(const winrt::com_ptr<Tab>& tab,
                                                std::optional<std::string_view> view,
                                                std::optional<bool> paneOpen)
    {
        Json::Value evt;
        evt["type"] = "event";
        evt["method"] = "set_agent_state";
        Json::Value params;
        params["window_id"] = std::to_string(_WindowProperties.WindowId());

        if (tab)
        {
            const auto stableId = tab->StableId();
            if (!stableId.empty())
            {
                params["tab_id"] = winrt::to_string(stableId);
            }
        }

        std::string logSuffix;
        if (view.has_value())
        {
            params["view"] = std::string{ *view };
            logSuffix += " view=" + std::string{ *view };
        }
        if (paneOpen.has_value())
        {
            params["pane_open"] = *paneOpen;
            logSuffix += " pane_open=" + std::string{ *paneOpen ? "true" : "false" };
        }
        evt["params"] = params;
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        _agentPaneLog(std::string{ "requesting set_agent_state:" } + logSuffix);
        ProtocolVtSequenceReceived.raise(
            *this,
            winrt::to_hstring(Json::writeString(wb, evt)));
    }

    // Helper+master agent-pane creation. The C++ side spawns one wta-helper as
    // a regular conpty child per agent pane; SharedWta owns the singleton
    // wta-master process that the helpers connect to over a named pipe (helper
    // ↔ master speaks ACP JSON-RPC, master owns the single agent CLI subprocess).
    // See doc/specs/Multi-window-agent-pane.md.
    bool TerminalPage::_AutoCreateHiddenAgentPaneShared(winrt::com_ptr<Tab> tab,
                                                        bool intoSessionsView,
                                                        bool autoStash,
                                                        std::string_view initialLoadSessionId,
                                                        std::string_view initialLoadCwd)
    {
        if (!tab || !tab->GetActiveTerminalControl())
        {
            return false;
        }
        // Refuse if this tab already has an agent pane — caller should
        // have decided to focus / toggle / etc instead of creating again.
        if (tab->FindAgentPane())
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: tab already has an agent pane");
            return false;
        }

        const auto wtaPath = _DetectWtaPath();
        if (wtaPath.empty())
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: no WTA path");
            return false;
        }

        const auto& globals = _settings.GlobalSettings();

        // GPO `AllowedAgents` enforcement — mirror the legacy path so a
        // managed environment that blocks all agents doesn't get a
        // working pane via the shared master.
        const auto agentCliPath = _ResolveEffectiveAgentCliPath(globals, [this]() { return _DetectAgentCli(); });
        if (agentCliPath.empty() && AgentPolicy::IsAllowedAgentsPolicyConfigured())
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: ABORT — all agents blocked by GPO policy");
            return false;
        }

        // Build the per-process settings the master will inherit.
        // These are baked at first-spawn time only; subsequent acquires
        // reuse the same master. Runtime changes flow over event channels
        // (`autofix_enabled_changed` is the existing one).
        //
        // Push as tokenized flag/value pairs; SharedWta handles all
        // Windows command-line quoting via QuoteAndEscapeCommandlineArg.
        std::vector<std::wstring> extraArgs;
        const auto pushFlagValue = [&extraArgs](const std::wstring_view flag, const std::wstring_view value) {
            if (value.empty())
            {
                return;
            }
            extraArgs.emplace_back(flag);
            extraArgs.emplace_back(value);
        };
        // `agentCliPath` was resolved above for the GPO policy check.
        // It may be empty when no agent CLI is detected but policy
        // doesn't actively block (e.g. fresh install with no agents
        // installed) — pass through anyway; wta will surface the
        // failure to spawn the child as an ACP error.
        pushFlagValue(L"--agent", agentCliPath);
        pushFlagValue(L"--agent-id", globals.EffectiveAcpAgent());
        if (!globals.EffectiveAutoFixEnabled())
        {
            extraArgs.emplace_back(L"--no-autofix");
        }
        if (const auto lang = _ResolveEffectiveLanguage(globals); !lang.empty())
        {
            pushFlagValue(L"--language", lang);
        }
        pushFlagValue(L"--acp-model", globals.AcpModel());
        pushFlagValue(L"--delegate-agent", _ResolveEffectiveDelegateAgent(globals));
        pushFlagValue(L"--delegate-model", globals.DelegateModel());

        auto& shared = winrt::TerminalApp::implementation::SharedWta::Instance();
        if (!shared.AcquirePane(std::wstring_view{ wtaPath }, extraArgs))
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: SharedWta::AcquirePane failed");
            return false;
        }
        // From here on, any early-return that *isn't* a successful
        // pane attach MUST ReleasePane to undo the refcount bump.
        auto sharedAcquired = wil::scope_exit([&shared]() noexcept {
            shared.ReleasePane();
        });
        _agentPaneLog("_AutoCreateHiddenAgentPaneShared: wta-master pid=" + std::to_string(shared.ProcessId()));

        const auto masterPipeName = std::wstring{ shared.MasterPipeName() };
        if (masterPipeName.empty())
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: SharedWta::MasterPipeName empty post-AcquirePane");
            return false;
        }

        const auto stableId = tab->StableId();
        if (stableId.empty())
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: tab has no StableId");
            return false;
        }

        // Build the wta-helper cmdline. The helper is a normal conpty
        // child; it connects to the master pipe and speaks ACP JSON-RPC.
        // Per-process settings (--agent, --acp-model, ...) live on the
        // master cmdline; the helper only needs identity (--agent-id,
        // --owner-tab-id) plus a handful of view/behavior flags.
        std::wstring helperCmd;
        helperCmd.reserve(wtaPath.size() + masterPipeName.size() + 256);
        helperCmd.push_back(L'"');
        helperCmd.append(wtaPath);
        helperCmd.push_back(L'"');
        helperCmd.append(L" --connect-master \"").append(masterPipeName).append(L"\"");
        helperCmd.append(L" --owner-tab-id \"").append(std::wstring_view{ stableId }).append(L"\"");

        // The helper-side cmdline mirrors the per-pane subset of the
        // legacy spawn's cmdline. The master already owns --agent /
        // --acp-model / --delegate-* / --no-autofix / --language as
        // process-wide config (passed in `extraArgs` above); the helper
        // needs them re-stated only to drive its local UI (agent name in
        // the title bar, autofix toggle in the bar, language for its
        // own UI strings).
        const auto appendHelperFlagValue = [&helperCmd](const std::wstring_view flag, const std::wstring_view value) {
            if (value.empty())
            {
                return;
            }
            helperCmd.push_back(L' ');
            helperCmd.append(flag);
            helperCmd.push_back(L' ');
            QuoteAndEscapeCommandlineArg(value, helperCmd);
        };
        appendHelperFlagValue(L"--agent", agentCliPath);
        appendHelperFlagValue(L"--agent-id", globals.EffectiveAcpAgent());
        appendHelperFlagValue(L"--acp-model", globals.AcpModel());
        appendHelperFlagValue(L"--delegate-agent", _ResolveEffectiveDelegateAgent(globals));
        appendHelperFlagValue(L"--delegate-model", globals.DelegateModel());
        if (!globals.EffectiveAutoFixEnabled())
        {
            helperCmd.append(L" --no-autofix");
        }
        if (const auto lang = _ResolveEffectiveLanguage(globals); !lang.empty())
        {
            appendHelperFlagValue(L"--language", lang);
        }
        if (intoSessionsView)
        {
            helperCmd.append(L" --initial-view sessions");
        }
        if (autoStash)
        {
            // Pre-warm path: the helper is spawned for a pane that C++
            // will stash immediately. Tell the helper to seed its
            // `tab.pane_open = false` so the initial `agent_state_changed`
            // echo matches the stashed C++ state. Without this the
            // helper defaults to `pane_open = true` (the "user just
            // opened the pane" assumption), C++ receives the echo on a
            // stashed pane and restores it — defeating pre-warm.
            helperCmd.append(L" --start-stashed");
        }

        // Plan-C: bundle the resume request with helper spawn. Caller
        // (currently `OnResumeInNewAgentTabRequested` via the pending-
        // load-session map in `OnAgentStateChanged`) sets these when the
        // F2 Enter-on-Historical/Ended-row path needs the freshly-spawned
        // helper to immediately ACP `session/load` instead of creating a
        // fresh session. Helper-side flag handling lives in main.rs
        // (`--initial-load-session-id` + `--initial-load-cwd`).
        if (!initialLoadSessionId.empty())
        {
            const auto sidW = winrt::to_hstring(initialLoadSessionId);
            appendHelperFlagValue(L"--initial-load-session-id", std::wstring_view{ sidW });
            if (!initialLoadCwd.empty())
            {
                const auto cwdW = winrt::to_hstring(initialLoadCwd);
                appendHelperFlagValue(L"--initial-load-cwd", std::wstring_view{ cwdW });
            }
        }

        // Resolve cwd. Priority matches the legacy spawn:
        //   a) VirtualWorkingDirectory (CLI-remoted commands like `wt agent`)
        //   b) Active pane CWD of THIS tab (from shell integration / OSC 9;9)
        //   c) Profile's configured starting directory
        //   d) User's home directory
        //
        // Use `tab->GetActiveTerminalControl()` rather than the page's
        // `_GetActiveControl()`: for `openInBackground=true` new tabs the
        // tab being pre-warmed is NOT the focused one, so reading from the
        // page-focused control would pick up the foreground tab's cwd and
        // the helper would start in the wrong directory (autofix and
        // agent context would attribute to the wrong project). Reading
        // directly from `tab` resolves to whichever pane is active on
        // this specific tab. If shell integration hasn't reported a cwd
        // yet (common for a just-spawned background tab) we fall through
        // to (c)/(d) below.
        winrt::hstring startingDirectory = _WindowProperties.VirtualWorkingDirectory();
        if (startingDirectory.empty())
        {
            if (const auto activeControl = tab->GetActiveTerminalControl())
            {
                startingDirectory = activeControl.WorkingDirectory();
            }
        }
        if (startingDirectory.empty())
        {
            if (const auto profile = tab->GetFocusedProfile())
            {
                startingDirectory = profile.EvaluatedStartingDirectory();
            }
        }
        if (startingDirectory.empty())
        {
            wchar_t homePath[MAX_PATH];
            if (GetEnvironmentVariableW(L"USERPROFILE", homePath, MAX_PATH) > 0)
            {
                startingDirectory = winrt::hstring{ homePath };
            }
        }

        NewTerminalArgs args;
        args.Commandline(winrt::hstring{ helperCmd });
        args.Profile(globals.AiCoordinatorProfile());
        if (!startingDirectory.empty())
        {
            args.StartingDirectory(startingDirectory);
        }

        auto rawPane = _MakeTerminalPane(args, nullptr, nullptr);
        if (!rawPane)
        {
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: _MakeTerminalPane returned null");
            return false;
        }
        auto newPane = _WrapInAgentPaneContent(rawPane);
        newPane->IsAgentPane(true);

        // Wire the AgentPaneContent's bottom-bar click events to the page
        // so toolbar buttons drive the per-tab logic. We need the tab
        // bound here so the closure stays correct across reparents.
        if (const auto agentContent = newPane->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>())
        {
            _WireAgentPaneEvents(agentContent, tab);
            agentContent.SetAgentPanePosition(globals.AgentPanePosition());
        }

        {
            // The Pane::Closed handler only releases the SharedWta refcount.
            // All per-tab state lives on the AgentPaneContent / Tab and is
            // released naturally when the pane node is dropped from the
            // tab's pane tree.
            newPane->Closed([](auto&&, auto&&) {
                _agentPaneLog("agent pane closed");
                winrt::TerminalApp::implementation::SharedWta::Instance().ReleasePane();
            });
        }

        // The Closed handler now owns the SharedWta refcount; disarm the
        // scope_exit so a successful return doesn't double-release.
        sharedAcquired.release();

        const auto splitDirection = _AgentPanePositionToSplitDirection(globals.AgentPanePosition());
        tab->SplitPaneAtRoot(splitDirection, newPane);

        if (autoStash)
        {
            // Pre-warm path: spawn the helper conpty child NOW, then stash
            // the pane so the user doesn't see it.
            //
            // CRITICAL: `connection.Start()` (which spawns the wta-helper
            // process) is gated on the agent pane's SwapChainPanel firing
            // its first `LayoutUpdated` event — see
            // `TermControl.cpp:343-352`. `LayoutUpdated` is raised by XAML
            // during its layout pass, which runs *after* the UI thread
            // returns. If we stash synchronously after SplitPaneAtRoot,
            // `Pane::HidePane` strips the agent pane's border out of the
            // visual tree *before* layout runs, so `LayoutUpdated` never
            // fires, `_InitializeTerminal` never runs, `connection.Start()`
            // never runs, and the helper is never spawned. Pre-warm becomes
            // a no-op.
            //
            // Fix: force a synchronous layout pass via `UpdateLayout()` on
            // the agent pane's root grid before stashing. `UpdateLayout` is
            // XAML's official API for running measure+arrange synchronously;
            // `LayoutUpdated` is raised inside the call, the
            // TermControl handler runs, `connection.Start()` spawns the
            // helper. By the time `StashAgentPane` runs on the next line
            // the helper is already alive — `HidePane` just rewires the
            // grid while the conpty + helper keep running in the background.
            //
            // Everything stays in one UI-thread tick (no awaits), so XAML
            // never renders an intermediate frame. The user only sees the
            // post-stash state: terminal pane filling the tab.
            if (const auto root = newPane->GetRootElement())
            {
                root.UpdateLayout();
            }
            tab->StashAgentPane();
            _UpdateBottomBarState();
            _agentPaneLog("_AutoCreateHiddenAgentPaneShared: done — helper pre-warmed + stashed");
            return true;
        }

        // Focus the new agent pane so the helper receives keyboard input.
        // The bookkeeping path (FocusPane) is synchronous; the actual
        // XAML Focus(Programmatic) is deferred to a low-priority dispatcher
        // tick because synchronous Focus on a just-spawned TermControl
        // silently drops (the element is in the tree but layout hasn't
        // completed, and Programmatic focus — unlike Pointer focus from a
        // mouse click — does not survive that race). Without this defer,
        // the hotkey is unresponsive during "connecting" and only works
        // after the user clicks the pane manually once.
        if (const auto paneId = newPane->Id())
        {
            tab->FocusPane(paneId.value());
        }
        if (const auto ctrl = newPane->GetTerminalControl())
        {
            if (auto dispatcher = winrt::Windows::System::DispatcherQueue::GetForCurrentThread())
            {
                auto weakCtrl = winrt::make_weak(ctrl);
                dispatcher.TryEnqueue(
                    winrt::Windows::System::DispatcherQueuePriority::Low,
                    [weakCtrl]() {
                        if (const auto c = weakCtrl.get())
                        {
                            c.Focus(winrt::Windows::UI::Xaml::FocusState::Programmatic);
                        }
                    });
            }
        }

        // The pane was just rooted into the active tab; refresh the
        // window-level bottom bar so the toggle/sessions buttons reflect
        // the new "agent pane open" state.
        _UpdateBottomBarState();

        _agentPaneLog("_AutoCreateHiddenAgentPaneShared: done — helper conpty child spawned");
        return true;
    }

    // Subscribe to a freshly-created AgentPaneContent's StateChanged event
    // so the window-level bottom bar refreshes when the firing pane is on
    // the active tab. The handler carries a weak ref to the page so it
    // survives tab close gracefully.
    void TerminalPage::_WireAgentPaneEvents(const winrt::TerminalApp::AgentPaneContent& content,
                                            const winrt::com_ptr<Tab>& /*ownerTab*/)
    {
        if (!content)
        {
            return;
        }
        auto weakSelf = get_weak();
        content.StateChanged([weakSelf](const winrt::TerminalApp::AgentPaneContent& sender,
                                        const winrt::Windows::Foundation::IInspectable& /*args*/) {
            if (const auto self = weakSelf.get())
            {
                // Only refresh the bar if the firing pane belongs to the
                // currently active tab. Background-tab state changes are
                // not visible until the user switches tabs (the next
                // `_UpdatedSelectedTab` call will refresh from scratch).
                const auto activeTab = self->_GetFocusedTabImpl();
                if (activeTab && activeTab->FindAgentPaneContent() == sender)
                {
                    self->_UpdateBottomBarState();
                }
            }
        });
    }

    // Window-level bottom-bar "agent toggle" click. Targets the active tab:
    //   - pane visible in sessions view → switch the view to chat
    //     (the user clicked the *chat* toggle; they want chat, not a
    //     close — mirror of the `_HandleOpenAgentPane` hotkey behavior,
    //     ported from upstream PR #66 onto the per-tab routing model)
    //   - else if active tab already has an AgentPaneContent → close it
    //   - else → open one on the active tab (in chat view)
    void TerminalPage::_AgentToggleButtonOnClick(const winrt::Windows::Foundation::IInspectable& /*sender*/,
                                                 const winrt::Windows::UI::Xaml::RoutedEventArgs& /*eventArgs*/)
    {
        if (const auto activeTab = _GetFocusedTabImpl())
        {
            if (const auto agentContent = activeTab->FindAgentPaneContent())
            {
                const auto agentPane = activeTab->FindAgentPane();
                const bool isStashed = agentPane && agentPane->IsHidden();
                if (!isStashed && agentContent.IsSessionsView())
                {
                    _RequestAgentStateForTab(activeTab, "chat", std::nullopt);
                    _UpdateBottomBarState();
                    return;
                }
            }
        }

        _OpenOrReuseAgentPane(/*intoSessionsView*/ false, L"BottomBarToggle");
        _UpdateBottomBarState();
    }

    // Window-level bottom-bar "sessions toggle" click. Mirrors the
    // Ctrl+Shift+/ keybinding, scoped to the active tab:
    //   - active tab has no agent pane → open one in sessions view
    //   - already in sessions view → close the pane
    //   - in chat view → switch into sessions view
    void TerminalPage::_SessionToggleButtonOnClick(const winrt::Windows::Foundation::IInspectable& /*sender*/,
                                                   const winrt::Windows::UI::Xaml::RoutedEventArgs& /*eventArgs*/)
    {
        const auto activeTab = _GetFocusedTabImpl();
        if (!activeTab)
        {
            return;
        }
        if (const auto agentContent = activeTab->FindAgentPaneContent())
        {
            const auto agentPane = activeTab->FindAgentPane();
            const bool isStashed = agentPane && agentPane->IsHidden();
            if (isStashed)
            {
                // Stashed — unstash in sessions view.
                _RequestAgentStateForTab(activeTab, "sessions", /*pane_open*/ true);
            }
            else if (agentContent.IsSessionsView())
            {
                // Visible in sessions view — hide (stash).
                _RequestAgentStateForTab(activeTab, std::nullopt, /*pane_open*/ false);
            }
            else
            {
                // Visible in chat view — switch into sessions view.
                _RequestAgentStateForTab(activeTab, "sessions", /*pane_open*/ true);
            }
            _UpdateBottomBarState();
            return;
        }
        // No agent pane on this tab — spawn one in sessions view.
        _OpenOrReuseAgentPane(/*intoSessionsView*/ true, L"BottomBarSessions");
        _UpdateBottomBarState();
    }

    // Window-level bottom-bar "diagnostics" click. Targets the active tab's
    // AgentPaneContent — fires the cached autofix for that tab, or asks
    // wta to execute / dismiss / re-trigger the diagnosis depending on
    // the autofix state.
    void TerminalPage::_DiagnosticsButtonOnClick(const winrt::Windows::Foundation::IInspectable& /*sender*/,
                                                 const winrt::Windows::UI::Xaml::RoutedEventArgs& /*eventArgs*/)
    {
        const auto activeTab = _GetFocusedTabImpl();
        if (!activeTab)
        {
            return;
        }
        const auto agentContent = activeTab->FindAgentPaneContent();
        if (!agentContent)
        {
            return;
        }
        const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
        if (!impl)
        {
            return;
        }
        using AS = winrt::TerminalApp::implementation::AgentPaneContent::AutofixState;
        const auto state = impl->GetAutofixState();
        const auto paneId = impl->GetLastErrorPaneId();
        switch (state)
        {
        case AS::Armed:
            _TriggerAutofix(activeTab, L"DiagnosticsButton");
            break;
        case AS::Detected:
        {
            Json::Value evt;
            evt["type"] = "event";
            evt["method"] = "autofix_execute_from_detected";
            Json::Value params;
            params["pane_id"] = winrt::to_string(paneId);
            evt["params"] = params;
            Json::StreamWriterBuilder wb;
            wb["indentation"] = "";
            ProtocolVtSequenceReceived.raise(
                *this,
                winrt::to_hstring(Json::writeString(wb, evt)));
            break;
        }
        case AS::Suggested:
        {
            // Suggested has no executable action — the explanation lives in
            // the chat history. The user's click means "show me what's
            // wrong", so ensure the pane is visible in chat view. The pane
            // may be stashed or currently in sessions view, so handle both
            // before dismissing the bar indicator.
            const auto agentPane = activeTab->FindAgentPane();
            if (agentPane && !agentPane->IsHidden())
            {
                if (agentContent.IsSessionsView())
                {
                    _RequestAgentStateForTab(activeTab, "chat", std::nullopt);
                }
            }
            else
            {
                _OpenOrReuseAgentPane(/*intoSessionsView*/ false, L"AutofixSuggestion");
            }

            // Now that the user is reading the explanation in the pane,
            // drop the bar's Suggested indicator.
            Json::Value evt;
            evt["type"] = "event";
            evt["method"] = "autofix_dismiss_suggestion";
            Json::Value params;
            params["pane_id"] = winrt::to_string(paneId);
            evt["params"] = params;
            Json::StreamWriterBuilder wb;
            wb["indentation"] = "";
            ProtocolVtSequenceReceived.raise(
                *this,
                winrt::to_hstring(Json::writeString(wb, evt)));
            break;
        }
        case AS::Pending:
        case AS::Idle:
        default:
            // No action.
            break;
        }
    }

    // Recompute the window-level bottom bar's state from the active tab's
    // AgentPaneContent (or absence thereof). Called on:
    //   - tab switch (_UpdatedSelectedTab)
    //   - AgentPaneContent::StateChanged when the firing pane is the active tab
    //   - pane creation/teardown
    //
    // The bottom bar is hidden on non-terminal tabs (Settings, etc.); for a
    // terminal tab with no agent pane the bar shows the "open agent" affordance
    // with diagnostics disabled.
    // Visibility-only refresh — sets the bottom bar to Visible on
    // terminal/agent tabs and Collapsed on Settings/etc. Factored out
    // of `_UpdateBottomBarState` so `_UpdatedSelectedTab` can call it
    // synchronously on tab switch without also recomputing the
    // agent-state-dependent parts of the bar (toggle lit-state,
    // diagnostics) from the local AgentPaneContent mirror — those
    // remain owned by the wta-driven `OnAgentStateChanged` callback
    // path so the bar always reflects authoritative state.
    //
    // Hiding the bar collapses it (lets TabContent / Grid.Row=2 fill
    // the recovered space). The bar contains terminal-pane-oriented
    // controls (agent toggle, diagnostics, sessions) that have no
    // meaningful target on a non-terminal tab anyway.
    void TerminalPage::_UpdateBottomBarVisibility()
    {
        const auto focusedTabImpl = _GetFocusedTabImpl();
        bool isTerminalTab = true;
        if (focusedTabImpl)
        {
            if (const auto content = focusedTabImpl->GetActiveContent())
            {
                const bool isTerm = content.try_as<TerminalApp::TerminalPaneContent>() != nullptr;
                const bool isAgent = content.try_as<TerminalApp::AgentPaneContent>() != nullptr;
                isTerminalTab = isTerm || isAgent;
            }
        }
        if (auto barRoot = BottomBarRoot())
        {
            barRoot.Visibility(isTerminalTab ? Visibility::Visible : Visibility::Collapsed);
        }
    }

    void TerminalPage::_UpdateBottomBarState()
    {
        // Reuse the visibility helper so the show/hide decision lives
        // in exactly one place, then bail out for non-terminal tabs
        // (Settings, etc.) — the rest of this function only updates
        // agent-state-dependent UI on the bar.
        _UpdateBottomBarVisibility();

        const auto focusedTabImpl = _GetFocusedTabImpl();
        bool isTerminalTab = true;
        if (focusedTabImpl)
        {
            if (const auto content = focusedTabImpl->GetActiveContent())
            {
                const bool isTerm = content.try_as<TerminalApp::TerminalPaneContent>() != nullptr;
                const bool isAgent = content.try_as<TerminalApp::AgentPaneContent>() != nullptr;
                isTerminalTab = isTerm || isAgent;
            }
        }
        if (!isTerminalTab)
        {
            return;
        }

        // Look up the active tab's agent pane state. The pane is
        // window-level chrome, so when there's no AgentPaneContent on the
        // active tab the bar still shows but with the toggle dark and
        // diagnostics disabled.
        winrt::TerminalApp::AgentPaneContent activeAgent{ nullptr };
        if (focusedTabImpl)
        {
            activeAgent = focusedTabImpl->FindAgentPaneContent();
        }

        // The pane-visible highlight follows whichever toggle button "owns"
        // the current view:
        //   * chat view     → AgentToggleButton lit, SessionToggleButton dark
        //   * sessions view → SessionToggleButton lit, AgentToggleButton dark
        //   * no agent pane → both dark
        const auto kLitOverlay = winrt::Windows::UI::Xaml::Media::SolidColorBrush{
            winrt::Windows::UI::ColorHelper::FromArgb(30, 255, 255, 255)
        };
        const auto kTransparent = winrt::Windows::UI::Xaml::Media::SolidColorBrush{
            winrt::Windows::UI::Colors::Transparent()
        };
        // "Pane open" for the bar means visible-and-in-tree. A stashed
        // (hidden) pane still exists in the tab tree (so its helper +
        // ACP session stay alive) but the bar should show "closed" so
        // the next toggle press unstashes it.
        bool paneOpen = activeAgent != nullptr;
        if (paneOpen && focusedTabImpl)
        {
            if (const auto agentPane = focusedTabImpl->FindAgentPane())
            {
                if (agentPane->IsHidden())
                {
                    paneOpen = false;
                }
            }
        }
        const bool sessionsView = paneOpen && activeAgent.IsSessionsView();
        const bool sessionsLit = paneOpen && sessionsView;
        const bool chatLit = paneOpen && !sessionsView;
        if (auto toggleBtn = AgentToggleButton())
        {
            toggleBtn.Background(chatLit ? kLitOverlay : kTransparent);
        }
        if (auto sessionsBtn = SessionToggleButton())
        {
            sessionsBtn.Background(sessionsLit ? kLitOverlay : kTransparent);
        }

        // Swap the toggle icon to match the current pane position.
        {
            const auto position = _settings ? _settings.GlobalSettings().AgentPanePosition() : winrt::hstring{ L"bottom" };
            const bool isVertical = (position == L"right" || position == L"left");
            if (auto iconBottom = AgentToggleIconBottom())
                iconBottom.Visibility(isVertical ? Visibility::Collapsed : Visibility::Visible);
            if (auto iconRight = AgentToggleIconRight())
                iconRight.Visibility(isVertical ? Visibility::Visible : Visibility::Collapsed);
        }

        // Diagnostics button reflects the active tab's autofix state
        // (Idle when there's no agent pane).
        using AS = winrt::TerminalApp::implementation::AgentPaneContent::AutofixState;
        AS autofixState = AS::Idle;
        winrt::hstring fixPreview;
        winrt::hstring hotkeyHint;
        winrt::hstring suggestionTitle;
        winrt::hstring detectedSummary;
        // Autofix-state read must NOT be gated on pane visibility. The
        // helper keeps running and detecting command failures even when
        // the agent pane is stashed (pre-warm path: helper is spawned
        // and connected from the moment the tab opens, with the pane
        // hidden). `OnAutofixStateChanged` writes the latest state into
        // AgentPaneContent's cache regardless of stash, so the bar
        // should reflect it regardless of stash too. (The toggle-button
        // highlights above DO gate on `paneOpen` — that's correct, they
        // mean "which view is currently shown.") Without this, the bar
        // shows Idle forever until the user opens the pane, which
        // defeats autofix-without-toggle.
        if (activeAgent)
        {
            if (const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(activeAgent))
            {
                autofixState = impl->GetAutofixState();
                fixPreview = impl->GetFixPreview();
                hotkeyHint = impl->GetHotkeyHint();
                suggestionTitle = impl->GetSuggestionTitle();
                detectedSummary = impl->GetDetectedSummary();
            }
        }

        if (auto diagBtn = DiagnosticsButton())
        {
            auto label = DiagnosticsLabel();
            auto icon = DiagnosticsIcon();

            switch (autofixState)
            {
            case AS::Pending:
            {
                diagBtn.Opacity(1.0);
                diagBtn.IsEnabled(false);
                ToolTipService::SetToolTip(
                    diagBtn,
                    box_value(RS_(L"Diagnostics_AnalyzingTooltip")));
                if (icon)
                {
                    icon.Foreground(
                        winrt::Windows::UI::Xaml::Media::SolidColorBrush{
                            winrt::Windows::UI::ColorHelper::FromArgb(255, 0xD6, 0xB7, 0x00) });
                }
                if (label)
                {
                    label.Text(RS_(L"Diagnostics_ErrorPendingLabel"));
                    label.Foreground(
                        winrt::Windows::UI::Xaml::Media::SolidColorBrush{
                            winrt::Windows::UI::ColorHelper::FromArgb(255, 0xD6, 0xB7, 0x00) });
                    label.Visibility(Visibility::Visible);
                }
                break;
            }
            case AS::Armed:
            {
                diagBtn.Opacity(1.0);
                diagBtn.IsEnabled(true);

                const auto hotkey = hotkeyHint.empty()
                                        ? std::wstring{ L"Ctrl+Alt+." }
                                        : std::wstring{ hotkeyHint };
                label.Text(winrt::hstring{ RS_fmt(L"Diagnostics_ErrorArmedLabelFormat", hotkey) });

                const auto accent = winrt::Windows::UI::Xaml::Media::SolidColorBrush{
                    winrt::Windows::UI::ColorHelper::FromArgb(255, 0xFF, 0xD7, 0x00)
                };
                if (icon)
                {
                    icon.Foreground(accent);
                }
                if (label)
                {
                    label.Foreground(accent);
                    label.Visibility(Visibility::Visible);
                }

                std::wstring tooltip;
                if (!fixPreview.empty())
                {
                    std::wstring preview;
                    std::wstring fp{ fixPreview };
                    if (fp.size() > 120)
                    {
                        preview.append(fp, 0, 120);
                        preview += L"…";
                    }
                    else
                    {
                        preview = fp;
                    }
                    tooltip = RS_fmt(L"Diagnostics_FixReadyTooltipWithPreviewFormat", hotkey, preview);
                }
                else
                {
                    tooltip = RS_fmt(L"Diagnostics_FixReadyTooltipFormat", hotkey);
                }
                ToolTipService::SetToolTip(
                    diagBtn,
                    box_value(winrt::hstring{ tooltip }));
                break;
            }
            case AS::Suggested:
            {
                diagBtn.Opacity(1.0);
                diagBtn.IsEnabled(true);

                const auto accent = winrt::Windows::UI::Xaml::Media::SolidColorBrush{
                    winrt::Windows::UI::ColorHelper::FromArgb(255, 0xFF, 0xD7, 0x00)
                };
                if (icon)
                {
                    icon.Foreground(accent);
                }
                if (label)
                {
                    label.Text(RS_(L"Diagnostics_SuggestionReadyLabel"));
                    label.Foreground(accent);
                    label.Visibility(Visibility::Visible);
                }

                std::wstring tooltip{ RS_(L"Diagnostics_SuggestionTooltipIntro") };
                if (!suggestionTitle.empty())
                {
                    tooltip += L"\n";
                    tooltip += suggestionTitle;
                    tooltip += L"\n";
                }
                tooltip += RS_(L"Diagnostics_SuggestionTooltipInstruction");
                ToolTipService::SetToolTip(
                    diagBtn,
                    box_value(winrt::hstring{ tooltip }));
                break;
            }
            case AS::Detected:
            {
                diagBtn.Opacity(1.0);
                diagBtn.IsEnabled(true);

                const auto hotkey = hotkeyHint.empty()
                                        ? std::wstring{ L"Ctrl+Alt+." }
                                        : std::wstring{ hotkeyHint };
                std::wstring labelText = L"Click " + hotkey + L" to fix error";
                const auto accent = winrt::Windows::UI::Xaml::Media::SolidColorBrush{
                    winrt::Windows::UI::ColorHelper::FromArgb(255, 0xFF, 0xD7, 0x00)
                };
                if (icon)
                {
                    icon.Foreground(accent);
                }
                if (label)
                {
                    label.Text(winrt::hstring{ labelText });
                    label.Foreground(accent);
                    label.Visibility(Visibility::Visible);
                }
                std::wstring tooltip = L"Error detected. Click or press " + hotkey +
                                       L" to ask the AI agent to diagnose and suggest a fix.";
                if (!detectedSummary.empty())
                {
                    tooltip += L"\n\n";
                    tooltip += detectedSummary;
                }
                ToolTipService::SetToolTip(
                    diagBtn,
                    box_value(winrt::hstring{ tooltip }));
                break;
            }
            case AS::Idle:
            default:
            {
                diagBtn.Opacity(0.5);
                diagBtn.IsEnabled(false);
                ToolTipService::SetToolTip(
                    diagBtn,
                    box_value(RS_(L"Diagnostics_Tooltip")));
                if (icon)
                {
                    icon.Foreground(
                        winrt::Windows::UI::Xaml::Media::SolidColorBrush{
                            winrt::Windows::UI::ColorHelper::FromArgb(255, 0xB0, 0xB0, 0xB0) });
                }
                if (label)
                {
                    label.Visibility(Visibility::Collapsed);
                    label.Text(L"");
                }
                break;
            }
            }
        }
    }

    // Called whenever agent-identity settings may have changed. Diffs the
    // last known snapshot against the current one, tears down + rebuilds
    // the agent pane, and updates the snapshot.
    void TerminalPage::_RebuildAgentStack()
    {
        const auto current = _CaptureAgentSettingsSnapshot();

        {
            std::string diag = "_RebuildAgentStack: entered. current.acp=";
            diag += winrt::to_string(current.acpAgent);
            diag += " last.acp=";
            diag += winrt::to_string(_lastAgentSettings.acpAgent);
            diag += " initialized=";
            diag += (_agentSettingsSnapshotInitialized ? "true" : "false");
            diag += " rebuilding=";
            diag += (_agentRebuilding ? "true" : "false");
            _agentPaneLog(diag);
        }

        // First call just seeds the snapshot — there's nothing to rebuild.
        if (!_agentSettingsSnapshotInitialized)
        {
            _lastAgentSettings = current;
            _agentSettingsSnapshotInitialized = true;
            _agentPaneLog("_RebuildAgentStack: seeded snapshot, skip rebuild");
            return;
        }

        if (!_AgentSettingsChanged(_lastAgentSettings, current))
        {
            _agentPaneLog("_RebuildAgentStack: no change, skip rebuild");
            return;
        }

        // Reentrancy guard.
        if (_agentRebuilding)
        {
            _agentPaneLog("_RebuildAgentStack: already rebuilding, skipping nested trigger");
            return;
        }

        // Defer the rebuild when there's no terminal tab in focus:
        // TermControls on non-active tabs never raise
        // SwapChainPanel.LayoutUpdated, so `connection.Start()` never
        // runs and wta.exe never launches. _FlushPendingAgentRebuild
        // re-enters once a terminal tab becomes active.
        // `_lastAgentSettings` stays unchanged so the dirty diff fires
        // again on next entry.
        const auto focusedTab = _GetFocusedTabImpl();
        // Only `==` auto-generates for projected WinRT types — `!=` doesn't.
        const bool canHostPane = focusedTab && !(*focusedTab == _settingsTab);
        if (!canHostPane)
        {
            _pendingAgentRebuild = true;
            return;
        }

        _agentRebuilding = true;
        auto guard = wil::scope_exit([this]() noexcept { _agentRebuilding = false; });

        _agentPaneLog("_RebuildAgentStack: agent settings changed, rebuilding");

        // Tear down every tab's agent pane. The user must reopen each
        // (per-tab toggle) — there's no longer a "shared pane" to
        // reposition.
        bool hadAny = false;
        std::vector<winrt::com_ptr<Tab>> tabsThatHadAgentPane;
        for (const auto& t : _tabs)
        {
            if (auto tabImpl = _GetTabImpl(t))
            {
                if (tabImpl->FindAgentPane())
                {
                    hadAny = true;
                    tabsThatHadAgentPane.push_back(tabImpl);
                }
            }
        }

        if (!hadAny)
        {
            _lastAgentSettings = current;
            _agentPaneLog("_RebuildAgentStack: no existing agent pane, snapshot only");
            return;
        }

        for (const auto& tabImpl : tabsThatHadAgentPane)
        {
            _TeardownAgentPane(tabImpl);
        }
        _lastAgentSettings = current;

        // Recreate on the active terminal tab so the user sees something
        // immediately. Other tabs that had an agent pane will need to be
        // re-toggled by the user.
        _OpenOrReuseAgentPane(false, L"SettingsReload");
    }

    void TerminalPage::_FlushPendingAgentRebuild()
    {
        if (!_pendingAgentRebuild)
        {
            return;
        }
        const auto focusedTab = _GetFocusedTabImpl();
        if (!focusedTab || *focusedTab == _settingsTab)
        {
            return;
        }
        _pendingAgentRebuild = false;
        _RebuildAgentStack();
    }

    void TerminalPage::_OpenOrReuseAgentPane(bool intoSessionsView, const wchar_t* triggerSource)
    {
        _agentPaneLog(std::string{ "_OpenOrReuseAgentPane called, intoSessionsView=" } + (intoSessionsView ? "true" : "false"));

        const auto emitAgentPaneOpened = [&]() {
#if defined(WT_BRANDING_RELEASE)
            constexpr uint8_t branding = 3;
#elif defined(WT_BRANDING_PREVIEW)
            constexpr uint8_t branding = 2;
#elif defined(WT_BRANDING_CANARY)
            constexpr uint8_t branding = 1;
#else
            constexpr uint8_t branding = 0;
#endif
            TraceLoggingWrite(
                g_hTerminalAppProvider,
                "AgentPaneOpened",
                TraceLoggingDescription("Event emitted when the agent pane is opened"),
                TraceLoggingWideString(triggerSource, "TriggerSource", "How the agent pane was triggered"),
                TraceLoggingValue(branding, "Branding"),
                TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
        };

        const auto& globals = _settings.GlobalSettings();

        // Surface GPO policy / no-wta failures up-front so the user gets a
        // teaching tip instead of a silent no-op.
        const auto wtaPath = _DetectWtaPath();
        if (wtaPath.empty())
        {
            _agentPaneLog("EARLY RETURN: wta not found — no AI assistant configured");
            if (auto tip{ FindName(L"WindowIdToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _UpdateTeachingTipTheme(tip.try_as<winrt::Windows::UI::Xaml::FrameworkElement>());
                tip.Title(RS_(L"AgentNotConfiguredTitle"));
                tip.Subtitle(RS_(L"AgentNotConfiguredSubtitle"));
                tip.IsOpen(true);
            }
            return;
        }
        if (const auto agentCliPath = _ResolveEffectiveAgentCliPath(globals, [this]() { return _DetectAgentCli(); });
            agentCliPath.empty() && AgentPolicy::IsAllowedAgentsPolicyConfigured())
        {
            _agentPaneLog("EARLY RETURN: all agents blocked by GPO policy");
            if (auto tip{ FindName(L"WindowIdToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _UpdateTeachingTipTheme(tip.try_as<winrt::Windows::UI::Xaml::FrameworkElement>());
                tip.Title(RS_(L"AgentBlockedByPolicyTitle"));
                tip.Subtitle(RS_(L"AgentBlockedByPolicySubtitle"));
                tip.IsOpen(true);
            }
            return;
        }

        const auto focusedTab = _GetFocusedTabImpl();
        if (!focusedTab)
        {
            _agentPaneLog("_OpenOrReuseAgentPane: no focused tab");
            return;
        }

        // Per-tab model: check whether *this* tab already has an agent pane.
        if (const auto existingPane = focusedTab->FindAgentPane())
        {
            // Stashed (hidden) — user wants to show it again. Apply locally
            // FIRST so successive fast hotkey presses see the updated
            // `IsHidden()` immediately (the wta round-trip is too slow
            // otherwise — multiple rapid presses would all read the stale
            // pre-toggle state and "eat" each other). wta is still informed
            // so its tab.pane_open state stays in sync; the echo back into
            // `OnAgentStateChanged` is idempotent on the already-applied
            // local state.
            if (existingPane->IsHidden())
            {
                _agentPaneLog("found stashed agent pane on focused tab — unstashing locally + notifying wta");
                const auto splitDir = _AgentPanePositionToSplitDirection(_settings.GlobalSettings().AgentPanePosition());
                focusedTab->RestoreStashedAgentPane(splitDir);
                // ALWAYS specify view on unstash. If we left it `nullopt`,
                // wta would echo back its stored view (which is whatever
                // the pane was in when it got hidden) — so a Ctrl+Shift+.
                // (chat) unstash on a pane that was hidden in sessions view
                // would re-open in sessions view. Explicit view forces wta
                // to send the user-requested view in the echo.
                const std::string_view viewReq = intoSessionsView ? "sessions" : "chat";
                _RequestAgentStateForTab(focusedTab, viewReq, /*pane_open*/ true);
                // Eager local apply so the bottom bar + AgentPaneContent
                // header flip instantly without waiting for the wta echo.
                if (const auto agentContent = focusedTab->FindAgentPaneContent())
                {
                    agentContent.SetSessionsView(intoSessionsView);
                }
                _UpdateBottomBarState();
                emitAgentPaneOpened();
                return;
            }

            _agentPaneLog("found agent pane on focused tab");

            if (intoSessionsView)
            {
                // Open-into-sessions: never close, just flip the view +
                // ensure the pane is focused (it must be, by virtue of
                // existing in this tab).
                if (const auto& content{ existingPane->GetContent() })
                {
                    content.Focus(FocusState::Programmatic);
                }
                _RequestAgentStateForTab(focusedTab, "sessions", /*pane_open*/ true);
                emitAgentPaneOpened();
                return;
            }

            // Toggle close. Apply locally FIRST (see comment on the unstash
            // branch above for why we don't wait for wta's echo). wta echo
            // lands in OnAgentStateChanged and is idempotent against the
            // already-stashed pane.
            _agentPaneLog("toggle: stashing existing agent pane locally + notifying wta");
            focusedTab->StashAgentPane();
            _RequestAgentStateForTab(focusedTab, std::nullopt, /*pane_open*/ false);
            _UpdateBottomBarState();
            return;
        }

        _agentPaneLog("no agent pane on focused tab, creating new one");

        if (!_AutoCreateHiddenAgentPaneShared(focusedTab, intoSessionsView))
        {
            _agentPaneLog("_OpenOrReuseAgentPane: _AutoCreateHiddenAgentPaneShared failed");
            return;
        }
        emitAgentPaneOpened();
    }

    // Focus toggle: ensure the focused tab has an agent pane (creating one
    // if necessary), then cycle focus between it and the previously focused
    // pane. Never tears down — pressing while already focused on the agent
    // returns focus to the prior pane.
    void TerminalPage::_FocusAgentPane()
    {
        _agentPaneLog("_FocusAgentPane called");

        const auto activeTab = _GetFocusedTabImpl();
        if (!activeTab)
        {
            return;
        }

        const auto existingPane = activeTab->FindAgentPane();
        if (!existingPane)
        {
            // No agent pane on this tab — open one.
            _OpenOrReuseAgentPane(false, L"FocusAction");
            return;
        }

        const auto agentId = existingPane->Id();
        const auto activePane = activeTab->GetActivePane();
        const auto activeId = activePane ? activePane->Id() : std::nullopt;
        const bool agentIsFocused = agentId.has_value() &&
                                    activeId.has_value() &&
                                    agentId.value() == activeId.value();
        if (agentIsFocused)
        {
            const auto target = _FindSourceOfAgentPaneId(activeTab->GetRootPane());
            if (target.has_value())
            {
                activeTab->FocusPane(target.value());
            }
            return;
        }
        if (agentId.has_value())
        {
            activeTab->FocusPane(agentId.value());
        }
        if (const auto ctrl = existingPane->GetTerminalControl())
        {
            ctrl.Focus(winrt::Windows::UI::Xaml::FocusState::Programmatic);
        }
    }

    // Walk the pane tree and return the id of the pane currently flagged as
    // "source of agent pane" — i.e. the last non-agent pane the user was on
    // before focus moved to the agent. Tab::_UpdateActivePane keeps this in
    // sync for both hotkey and mouse focus changes, so it's the source of
    // truth for "where to go back to".
    std::optional<uint32_t> TerminalPage::_FindSourceOfAgentPaneId(const std::shared_ptr<Pane>& root)
    {
        std::optional<uint32_t> result;
        if (!root)
        {
            return result;
        }
        root->WalkTree([&](const std::shared_ptr<Pane>& p) {
            if (!result.has_value() && p->IsSourceOfAgentPane())
            {
                result = p->Id();
            }
        });
        return result;
    }

    // Method Description:
    // - This method is called once on startup, on the first LayoutUpdated event.
    //   We'll use this event to know that we have an ActualWidth and
    //   ActualHeight, so we can now attempt to process our list of startup
    //   actions.
    // - We'll remove this event handler when the event is first handled.
    // - If there are no startup actions, we'll open a single tab with the
    //   default profile.
    // Arguments:
    // - <unused>
    // Return Value:
    // - <none>
    void TerminalPage::_OnFirstLayout(const IInspectable& /*sender*/, const IInspectable& /*eventArgs*/)
    {
        // Only let this succeed once.
        _layoutUpdatedRevoker.revoke();

        // This event fires every time the layout changes, but it is always the
        // last one to fire in any layout change chain. That gives us great
        // flexibility in finding the right point at which to initialize our
        // renderer (and our terminal). Any earlier than the last layout update
        // and we may not know the terminal's starting size.
        if (_startupState == StartupState::NotInitialized)
        {
            _startupState = StartupState::InStartup;

            if (_startupConnection)
            {
                CreateTabFromConnection(std::move(_startupConnection));
            }
            else if (!_startupActions.empty())
            {
                ProcessStartupActions(std::move(_startupActions));
            }

            _CompleteInitialization();
        }
    }

    // Method Description:
    // - Process all the startup actions in the provided list of startup
    //   actions. We'll do this all at once here.
    // Arguments:
    // - actions: a winrt vector of actions to process. Note that this must NOT
    //   be an IVector&, because we need the collection to be accessible on the
    //   other side of the co_await.
    // - initial: if true, we're parsing these args during startup, and we
    //   should fire an Initialized event.
    // - cwd: If not empty, we should try switching to this provided directory
    //   while processing these actions. This will allow something like `wt -w 0
    //   nt -d .` from inside another directory to work as expected.
    // Return Value:
    // - <none>
    safe_void_coroutine TerminalPage::ProcessStartupActions(std::vector<ActionAndArgs> actions, const winrt::hstring cwd, const winrt::hstring env)
    {
        const auto strong = get_strong();

        // If the caller provided a CWD, "switch" to that directory, then switch
        // back once we're done.
        auto originalVirtualCwd{ _WindowProperties.VirtualWorkingDirectory() };
        auto originalVirtualEnv{ _WindowProperties.VirtualEnvVars() };
        auto restoreCwd = wil::scope_exit([&]() {
            if (!cwd.empty())
            {
                // ignore errors, we'll just power on through. We'd rather do
                // something rather than fail silently if the directory doesn't
                // actually exist.
                _WindowProperties.VirtualWorkingDirectory(originalVirtualCwd);
                _WindowProperties.VirtualEnvVars(originalVirtualEnv);
            }
        });
        if (!cwd.empty())
        {
            _WindowProperties.VirtualWorkingDirectory(cwd);
            _WindowProperties.VirtualEnvVars(env);
        }

        // The current TerminalWindow & TerminalPage architecture is rather instable
        // and fails to start up if the first tab isn't created synchronously.
        //
        // While that's a fair assumption in on itself, simultaneously WinUI will
        // not assign tab contents a size if they're not shown at least once,
        // which we need however in order to initialize ControlCore with a size.
        //
        // So, we do two things here:
        // * DO NOT suspend if this is the first tab.
        // * DO suspend between the creation of panes (or tabs) in order to allow
        //   WinUI to layout the new controls and for ControlCore to get a size.
        //
        // This same logic is also applied to CreateTabFromConnection.
        //
        // See GH#13136.
        auto suspend = _tabs.Size() > 0;

        for (size_t i = 0; i < actions.size(); ++i)
        {
            if (suspend)
            {
                co_await wil::resume_foreground(Dispatcher(), CoreDispatcherPriority::Low);
            }

            _actionDispatch->DoAction(actions[i]);
            suspend = true;
        }

        // GH#6586: now that we're done processing all startup commands,
        // focus the active control. This will work as expected for both
        // commandline invocations and for `wt` action invocations.
        if (const auto& tabImpl{ _GetFocusedTabImpl() })
        {
            if (const auto& content{ tabImpl->GetActiveContent() })
            {
                content.Focus(FocusState::Programmatic);
            }
        }
    }

    safe_void_coroutine TerminalPage::CreateTabFromConnection(ITerminalConnection connection)
    {
        const auto strong = get_strong();

        // This is the exact same logic as in ProcessStartupActions.
        if (_tabs.Size() > 0)
        {
            co_await wil::resume_foreground(Dispatcher(), CoreDispatcherPriority::Low);
        }

        NewTerminalArgs newTerminalArgs;

        if (const auto conpty = connection.try_as<ConptyConnection>())
        {
            newTerminalArgs.Commandline(conpty.Commandline());
            newTerminalArgs.TabTitle(conpty.StartingTitle());
        }

        // GH #12370: We absolutely cannot allow a defterm connection to
        // auto-elevate. Defterm doesn't work for elevated scenarios in the
        // first place. If we try accepting the connection, the spawning an
        // elevated version of the Terminal with that profile... that's a
        // recipe for disaster. We won't ever open up a tab in this window.
        newTerminalArgs.Elevate(false);

        const auto newPane = _MakePane(newTerminalArgs, nullptr, std::move(connection));
        newPane->WalkTree([](const auto& pane) {
            pane->FinalizeConfigurationGivenDefault();
        });
        _CreateNewTabFromPane(newPane);
    }

    // Method Description:
    // - Perform and steps that need to be done once our initial state is all
    //   set up. This includes entering fullscreen mode and firing our
    //   Initialized event.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    safe_void_coroutine TerminalPage::_CompleteInitialization()
    {
        _startupState = StartupState::Initialized;

        // No auto-create-on-first-tab pre-warm under the per-tab model.
        // Each tab independently spawns an agent pane on user request.

        // GH#632 - It's possible that the user tried to create the terminal
        // with only one tab, with only an elevated profile. If that happens,
        // we'll create _another_ process to host the elevated version of that
        // profile. This can happen from the jumplist, or if the default profile
        // is `elevate:true`, or from the commandline.
        //
        // However, we need to make sure to close this window in that scenario.
        // Since there aren't any _tabs_ in this window, we won't ever get a
        // closed event. So do it manually.
        //
        // GH#12267: Make sure that we don't instantly close ourselves when
        // we're readying to accept a defterm connection. In that case, we don't
        // have a tab yet, but will once we're initialized.
        if (_tabs.Size() == 0)
        {
            CloseWindowRequested.raise(*this, nullptr);
            co_return;
        }
        else
        {
            // GH#11561: When we start up, our window is initially just a frame
            // with a transparent content area. We're gonna do all this startup
            // init on the UI thread, so the UI won't actually paint till it's
            // all done. This results in a few frames where the frame is
            // visible, before the page paints for the first time, before any
            // tabs appears, etc.
            //
            // To mitigate this, we're gonna wait for the UI thread to finish
            // everything it's gotta do for the initial init, and _then_ fire
            // our Initialized event. By waiting for everything else to finish
            // (CoreDispatcherPriority::Low), we let all the tabs and panes
            // actually get created. In the window layer, we're gonna cloak the
            // window till this event is fired, so we don't actually see this
            // frame until we're actually all ready to go.
            //
            // This will result in the window seemingly not loading as fast, but
            // it will actually take exactly the same amount of time before it's
            // usable.
            //
            // We also experimented with drawing a solid BG color before the
            // initialization is finished. However, there are still a few frames
            // after the frame is displayed before the XAML content first draws,
            // so that didn't actually resolve any issues.
            // Show the first-run experience overlay on the very first launch.
            // This covers the terminal content until the user clicks Next.
            if (_IsFreRequired())
            {
                _ShowFreOverlay();
            }

            Dispatcher().RunAsync(CoreDispatcherPriority::Low, [weak = get_weak()]() {
                if (auto self{ weak.get() })
                {
                    self->Initialized.raise(*self, nullptr);
                }
            });
        }
    }

    // Method Description:
    // - Show a dialog with "About" information. Displays the app's Display
    //   Name, version, getting started link, source code link, documentation link, release
    //   Notes link, send feedback link and privacy policy link.
    void TerminalPage::_ShowAboutDialog()
    {
        _ShowDialogHelper(L"AboutDialog");
    }

    winrt::hstring TerminalPage::ApplicationDisplayName()
    {
        return CascadiaSettings::ApplicationDisplayName();
    }

    winrt::hstring TerminalPage::ApplicationVersion()
    {
        return CascadiaSettings::ApplicationVersion();
    }

    // Method Description:
    // - Helper to show a content dialog
    // - We only open a content dialog if there isn't one open already
    winrt::Windows::Foundation::IAsyncOperation<ContentDialogResult> TerminalPage::_ShowDialogHelper(const std::wstring_view& name)
    {
        if (auto presenter{ _dialogPresenter.get() })
        {
            co_return co_await presenter.ShowDialog(FindName(name).try_as<WUX::Controls::ContentDialog>());
        }
        co_return ContentDialogResult::None;
    }

    // Method Description:
    // - Displays the unified close confirmation dialog configured for the
    //   given scenario. Resets the "don't ask me again" checkbox before showing.
    //   If the user confirms and checked "don't ask me again", sets
    //   confirmOnClose to Never and writes settings to disk.
    // - Only one dialog can be visible at a time. If another dialog is visible
    //   when this is called, nothing happens. See _ShowDialog for details
    winrt::Windows::Foundation::IAsyncOperation<ContentDialogResult> TerminalPage::_ShowConfirmCloseDialog(ConfirmCloseDialogKind kind)
    {
        // Load the dialog (triggers x:Load) and configure its strings.
        const auto dialog = FindName(L"ConfirmCloseDialog").as<ContentDialog>();

        winrt::hstring title;
        winrt::hstring primary;
        switch (kind)
        {
        case ConfirmCloseDialogKind::CloseAll:
            title = RS_(L"ConfirmCloseDialog_CloseAllTitle");
            primary = RS_(L"ConfirmCloseDialog_CloseAllPrimary");
            break;
        case ConfirmCloseDialogKind::Window:
            title = RS_(L"ConfirmCloseDialog_WindowTitle");
            primary = RS_(L"ConfirmCloseDialog_WindowPrimary");
            break;
        case ConfirmCloseDialogKind::Tab:
            title = RS_(L"ConfirmCloseDialog_TabTitle");
            primary = RS_(L"ConfirmCloseDialog_TabPrimary");
            break;
        case ConfirmCloseDialogKind::MultiplePanes:
            title = RS_(L"ConfirmCloseDialog_MultiplePanesTitle");
            primary = RS_(L"ConfirmCloseDialog_MultiplePanesPrimary");
            break;
        case ConfirmCloseDialogKind::MultipleTabs:
            title = RS_(L"ConfirmCloseDialog_MultipleTabsTitle");
            primary = RS_(L"ConfirmCloseDialog_MultipleTabsPrimary");
            break;
        case ConfirmCloseDialogKind::Pane:
            title = RS_(L"ConfirmCloseDialog_PaneTitle");
            primary = RS_(L"ConfirmCloseDialog_PanePrimary");
            break;
        }
        dialog.Title(winrt::box_value(title));
        dialog.PrimaryButtonText(primary);
        dialog.CloseButtonText(RS_(L"ConfirmCloseDialog_Cancel"));

        // BODGY: After a ContentDialog is dismissed, FindName() can no longer
        // resolve children inside it. Use Content() to get the checkbox directly.
        const auto checkbox = dialog.Content().as<CheckBox>();
        checkbox.IsChecked(false);

        auto result = ContentDialogResult::None;
        if (auto presenter{ _dialogPresenter.get() })
        {
            const auto weak = get_weak();
            result = co_await presenter.ShowDialog(dialog);

            // ShowDialog blocks until the dialog is dismissed, so it is
            // possible for `this` to be torn down while we wait. Re-acquire
            // a strong reference before touching any of our state.
            const auto strong = weak.get();
            if (!strong)
            {
                co_return ContentDialogResult::None;
            }

            if (result == ContentDialogResult::Primary && checkbox.IsChecked().Value())
            {
                _settings.GlobalSettings().ConfirmOnClose(ConfirmOnClose::Never);
                _settings.WriteSettingsToDisk();
            }
        }

        co_return result;
    }

    // Method Description:
    // - Displays a dialog for warnings found while closing the terminal tab marked as read-only
    winrt::Windows::Foundation::IAsyncOperation<ContentDialogResult> TerminalPage::_ShowCloseReadOnlyDialog()
    {
        return _ShowDialogHelper(L"CloseReadOnlyDialog");
    }

    // Method Description:
    // - Displays a dialog to warn the user about the fact that the text that
    //   they are trying to paste contains the "new line" character which can
    //   have the effect of starting commands without the user's knowledge if
    //   it is pasted on a shell where the "new line" character marks the end
    //   of a command.
    // - Only one dialog can be visible at a time. If another dialog is visible
    //   when this is called, nothing happens. See _ShowDialog for details
    winrt::Windows::Foundation::IAsyncOperation<ContentDialogResult> TerminalPage::_ShowMultiLinePasteWarningDialog()
    {
        return _ShowDialogHelper(L"MultiLinePasteDialog");
    }

    // Method Description:
    // - Displays a dialog to warn the user about the fact that the text that
    //   they are trying to paste is very long, in case they did not mean to
    //   paste it but pressed the paste shortcut by accident.
    // - Only one dialog can be visible at a time. If another dialog is visible
    //   when this is called, nothing happens. See _ShowDialog for details
    winrt::Windows::Foundation::IAsyncOperation<ContentDialogResult> TerminalPage::_ShowLargePasteWarningDialog()
    {
        return _ShowDialogHelper(L"LargePasteDialog");
    }

    // Method Description:
    // - Builds the flyout (dropdown) attached to the new tab button, and
    //   attaches it to the button. Populates the flyout with one entry per
    //   Profile, displaying the profile's name. Clicking each flyout item will
    //   open a new tab with that profile.
    //   Below the profiles are the static menu items: settings, command palette
    void TerminalPage::_CreateNewTabFlyout()
    {
        auto newTabFlyout = WUX::Controls::MenuFlyout{};
        newTabFlyout.Placement(WUX::Controls::Primitives::FlyoutPlacementMode::BottomEdgeAlignedLeft);

        // Create profile entries from the NewTabMenu configuration using a
        // recursive helper function. This returns a std::vector of FlyoutItemBases,
        // that we then add to our Flyout.
        auto entries = _settings.GlobalSettings().NewTabMenu();
        auto items = _CreateNewTabFlyoutItems(entries);
        for (const auto& item : items)
        {
            newTabFlyout.Items().Append(item);
        }

        // add menu separator
        auto separatorItem = WUX::Controls::MenuFlyoutSeparator{};
        newTabFlyout.Items().Append(separatorItem);

        // add static items
        {
            // Create the settings button.
            auto settingsItem = WUX::Controls::MenuFlyoutItem{};
            settingsItem.Text(RS_(L"SettingsMenuItem"));
            const auto settingsToolTip = RS_(L"SettingsToolTip");

            WUX::Controls::ToolTipService::SetToolTip(settingsItem, box_value(settingsToolTip));
            Automation::AutomationProperties::SetHelpText(settingsItem, settingsToolTip);

            WUX::Controls::SymbolIcon ico{};
            ico.Symbol(WUX::Controls::Symbol::Setting);
            settingsItem.Icon(ico);

            settingsItem.Click({ this, &TerminalPage::_SettingsButtonOnClick });
            newTabFlyout.Items().Append(settingsItem);

            auto actionMap = _settings.ActionMap();
            const auto settingsKeyChord{ actionMap.GetKeyBindingForAction(L"Terminal.OpenSettingsUI") };
            if (settingsKeyChord)
            {
                _SetAcceleratorForMenuItem(settingsItem, settingsKeyChord);
            }

            // Create the command palette button.
            auto commandPaletteFlyout = WUX::Controls::MenuFlyoutItem{};
            commandPaletteFlyout.Text(RS_(L"CommandPaletteMenuItem"));
            const auto commandPaletteToolTip = RS_(L"CommandPaletteToolTip");

            WUX::Controls::ToolTipService::SetToolTip(commandPaletteFlyout, box_value(commandPaletteToolTip));
            Automation::AutomationProperties::SetHelpText(commandPaletteFlyout, commandPaletteToolTip);

            WUX::Controls::FontIcon commandPaletteIcon{};
            commandPaletteIcon.Glyph(L"\xE945");
            commandPaletteIcon.FontFamily(Media::FontFamily{ L"Segoe Fluent Icons, Segoe MDL2 Assets" });
            commandPaletteFlyout.Icon(commandPaletteIcon);

            commandPaletteFlyout.Click({ this, &TerminalPage::_CommandPaletteButtonOnClick });
            newTabFlyout.Items().Append(commandPaletteFlyout);

            const auto commandPaletteKeyChord{ actionMap.GetKeyBindingForAction(L"Terminal.ToggleCommandPalette") };
            if (commandPaletteKeyChord)
            {
                _SetAcceleratorForMenuItem(commandPaletteFlyout, commandPaletteKeyChord);
            }

            // Create the about button.
            auto aboutFlyout = WUX::Controls::MenuFlyoutItem{};
            aboutFlyout.Text(RS_(L"AboutMenuItem"));
            const auto aboutToolTip = RS_(L"AboutToolTip");

            WUX::Controls::ToolTipService::SetToolTip(aboutFlyout, box_value(aboutToolTip));
            Automation::AutomationProperties::SetHelpText(aboutFlyout, aboutToolTip);

            WUX::Controls::SymbolIcon aboutIcon{};
            aboutIcon.Symbol(WUX::Controls::Symbol::Help);
            aboutFlyout.Icon(aboutIcon);

            aboutFlyout.Click({ this, &TerminalPage::_AboutButtonOnClick });
            newTabFlyout.Items().Append(aboutFlyout);
        }

        // Before opening the fly-out set focus on the current tab
        // so no matter how fly-out is closed later on the focus will return to some tab.
        // We cannot do it on closing because if the window loses focus (alt+tab)
        // the closing event is not fired.
        // It is important to set the focus on the tab
        // Since the previous focus location might be discarded in the background,
        // e.g., the command palette will be dismissed by the menu,
        // and then closing the fly-out will move the focus to wrong location.
        newTabFlyout.Opening([weakThis{ get_weak() }](auto&&, auto&&) {
            if (auto page{ weakThis.get() })
            {
                page->_FocusCurrentTab(true);

                TraceLoggingWrite(
                    g_hTerminalAppProvider,
                    "NewTabMenuOpened",
                    TraceLoggingDescription("Event emitted when the new tab menu is opened"),
                    TraceLoggingValue(page->NumberOfTabs(), "TabCount", "The Count of tabs currently opened in this window"),
                    TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                    TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
            }
        });
        // Necessary for fly-out sub items to get focus on a tab before collapsing. Related to #15049
        newTabFlyout.Closing([weakThis{ get_weak() }](auto&&, auto&&) {
            if (auto page{ weakThis.get() })
            {
                if (!page->_commandPaletteIs(Visibility::Visible))
                {
                    page->_FocusCurrentTab(true);
                }

                TraceLoggingWrite(
                    g_hTerminalAppProvider,
                    "NewTabMenuClosed",
                    TraceLoggingDescription("Event emitted when the new tab menu is closed"),
                    TraceLoggingValue(page->NumberOfTabs(), "TabCount", "The Count of tabs currently opened in this window"),
                    TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                    TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
            }
        });
        _newTabButton.Flyout(newTabFlyout);
    }

    // Method Description:
    // - For a given list of tab menu entries, this method will create the corresponding
    //   list of flyout items. This is a recursive method that calls itself when it comes
    //   across a folder entry.
    std::vector<WUX::Controls::MenuFlyoutItemBase> TerminalPage::_CreateNewTabFlyoutItems(IVector<NewTabMenuEntry> entries)
    {
        std::vector<WUX::Controls::MenuFlyoutItemBase> items;

        if (entries == nullptr || entries.Size() == 0)
        {
            return items;
        }

        for (const auto& entry : entries)
        {
            if (entry == nullptr)
            {
                continue;
            }

            switch (entry.Type())
            {
            case NewTabMenuEntryType::Separator:
            {
                items.push_back(WUX::Controls::MenuFlyoutSeparator{});
                break;
            }
            // A folder has a custom name and icon, and has a number of entries that require
            // us to call this method recursively.
            case NewTabMenuEntryType::Folder:
            {
                const auto folderEntry = entry.as<FolderEntry>();
                const auto folderEntries = folderEntry.Entries();

                // If the folder is empty, we should skip the entry if AllowEmpty is false, or
                // when the folder should inline.
                // The IsEmpty check includes semantics for nested (empty) folders
                if (folderEntries.Size() == 0 && (!folderEntry.AllowEmpty() || folderEntry.Inlining() == FolderEntryInlining::Auto))
                {
                    break;
                }

                // Recursively generate flyout items
                auto folderEntryItems = _CreateNewTabFlyoutItems(folderEntries);

                // If the folder should auto-inline and there is only one item, do so.
                if (folderEntry.Inlining() == FolderEntryInlining::Auto && folderEntryItems.size() == 1)
                {
                    for (auto const& folderEntryItem : folderEntryItems)
                    {
                        items.push_back(folderEntryItem);
                    }

                    break;
                }

                // Otherwise, create a flyout
                auto folderItem = WUX::Controls::MenuFlyoutSubItem{};
                folderItem.Text(folderEntry.Name());

                auto icon = _CreateNewTabFlyoutIcon(folderEntry.Icon().Resolved());
                folderItem.Icon(icon);

                for (const auto& folderEntryItem : folderEntryItems)
                {
                    folderItem.Items().Append(folderEntryItem);
                }

                // If the folder is empty, and by now we know we set AllowEmpty to true,
                // create a placeholder item here
                if (folderEntries.Size() == 0)
                {
                    auto placeholder = WUX::Controls::MenuFlyoutItem{};
                    placeholder.Text(RS_(L"NewTabMenuFolderEmpty"));
                    placeholder.IsEnabled(false);

                    folderItem.Items().Append(placeholder);
                }

                items.push_back(folderItem);
                break;
            }
            // Any "collection entry" will simply make us add each profile in the collection
            // separately. This collection is stored as a map <int, Profile>, so the correct
            // profile index is already known.
            case NewTabMenuEntryType::RemainingProfiles:
            case NewTabMenuEntryType::MatchProfiles:
            {
                const auto remainingProfilesEntry = entry.as<ProfileCollectionEntry>();
                if (remainingProfilesEntry.Profiles() == nullptr)
                {
                    break;
                }

                for (auto&& [profileIndex, remainingProfile] : remainingProfilesEntry.Profiles())
                {
                    items.push_back(_CreateNewTabFlyoutProfile(remainingProfile, profileIndex, {}));
                }

                break;
            }
            // A single profile, the profile index is also given in the entry
            case NewTabMenuEntryType::Profile:
            {
                const auto profileEntry = entry.as<ProfileEntry>();
                if (profileEntry.Profile() == nullptr)
                {
                    break;
                }

                auto profileItem = _CreateNewTabFlyoutProfile(profileEntry.Profile(), profileEntry.ProfileIndex(), profileEntry.Icon().Resolved());
                items.push_back(profileItem);
                break;
            }
            case NewTabMenuEntryType::Action:
            {
                const auto actionEntry = entry.as<ActionEntry>();
                const auto actionId = actionEntry.ActionId();
                if (_settings.ActionMap().GetActionByID(actionId))
                {
                    auto actionItem = _CreateNewTabFlyoutAction(actionId, actionEntry.Icon().Resolved());
                    items.push_back(actionItem);
                }

                break;
            }
            }
        }

        return items;
    }

    // Method Description:
    // - This method creates a flyout menu item for a given profile with the given index.
    //   It makes sure to set the correct icon, keybinding, and click-action.
    WUX::Controls::MenuFlyoutItem TerminalPage::_CreateNewTabFlyoutProfile(const Profile profile, int profileIndex, const winrt::hstring& iconPathOverride)
    {
        auto profileMenuItem = WUX::Controls::MenuFlyoutItem{};

        // Add the keyboard shortcuts based on the number of profiles defined
        // Look for a keychord that is bound to the equivalent
        // NewTab(ProfileIndex=N) action
        NewTerminalArgs newTerminalArgs{ profileIndex };
        NewTabArgs newTabArgs{ newTerminalArgs };
        const auto id = fmt::format(FMT_COMPILE(L"Terminal.OpenNewTabProfile{}"), profileIndex);
        const auto profileKeyChord{ _settings.ActionMap().GetKeyBindingForAction(id) };

        // make sure we find one to display
        if (profileKeyChord)
        {
            _SetAcceleratorForMenuItem(profileMenuItem, profileKeyChord);
        }

        auto profileName = profile.Name();
        profileMenuItem.Text(profileName);

        // If a custom icon path has been specified, set it as the icon for
        // this flyout item. Otherwise, if an icon is set for this profile, set that icon
        // for this flyout item.
        const auto& iconPath = iconPathOverride.empty() ? profile.Icon().Resolved() : iconPathOverride;
        if (!iconPath.empty())
        {
            const auto icon = _CreateNewTabFlyoutIcon(iconPath);
            profileMenuItem.Icon(icon);
        }

        if (profile.Guid() == _settings.GlobalSettings().DefaultProfile())
        {
            // Contrast the default profile with others in font weight.
            profileMenuItem.FontWeight(FontWeights::Bold());
        }

        auto newTabRun = WUX::Documents::Run();
        newTabRun.Text(RS_(L"NewTabRun/Text"));
        auto newPaneRun = WUX::Documents::Run();
        newPaneRun.Text(RS_(L"NewPaneRun/Text"));
        newPaneRun.FontStyle(FontStyle::Italic);
        auto newWindowRun = WUX::Documents::Run();
        newWindowRun.Text(RS_(L"NewWindowRun/Text"));
        newWindowRun.FontStyle(FontStyle::Italic);
        auto elevatedRun = WUX::Documents::Run();
        elevatedRun.Text(RS_(L"ElevatedRun/Text"));
        elevatedRun.FontStyle(FontStyle::Italic);

        auto textBlock = WUX::Controls::TextBlock{};
        textBlock.Inlines().Append(newTabRun);
        textBlock.Inlines().Append(WUX::Documents::LineBreak{});
        textBlock.Inlines().Append(newPaneRun);
        textBlock.Inlines().Append(WUX::Documents::LineBreak{});
        textBlock.Inlines().Append(newWindowRun);
        textBlock.Inlines().Append(WUX::Documents::LineBreak{});
        textBlock.Inlines().Append(elevatedRun);

        auto toolTip = WUX::Controls::ToolTip{};
        toolTip.Content(textBlock);
        WUX::Controls::ToolTipService::SetToolTip(profileMenuItem, toolTip);

        profileMenuItem.Click([profileIndex, weakThis{ get_weak() }](auto&&, auto&&) {
            if (auto page{ weakThis.get() })
            {
                TraceLoggingWrite(
                    g_hTerminalAppProvider,
                    "NewTabMenuItemClicked",
                    TraceLoggingDescription("Event emitted when an item from the new tab menu is invoked"),
                    TraceLoggingValue(page->NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
                    TraceLoggingValue("Profile", "ItemType", "The type of item that was clicked in the new tab menu"),
                    TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                    TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

                NewTerminalArgs newTerminalArgs{ profileIndex };
                page->_OpenNewTerminalViaDropdown(newTerminalArgs);
            }
        });

        // Using the static method on the base class seems to do what we want in terms of placement.
        WUX::Controls::Primitives::FlyoutBase::SetAttachedFlyout(profileMenuItem, _CreateRunAsAdminFlyout(profileIndex));

        // Since we are not setting the ContextFlyout property of the item we have to handle the ContextRequested event
        // and rely on the base class to show our menu.
        profileMenuItem.ContextRequested([profileMenuItem](auto&&, auto&&) {
            WUX::Controls::Primitives::FlyoutBase::ShowAttachedFlyout(profileMenuItem);
        });

        return profileMenuItem;
    }

    // Method Description:
    // - This method creates a flyout menu item for a given action
    //   It makes sure to set the correct icon, keybinding, and click-action.
    WUX::Controls::MenuFlyoutItem TerminalPage::_CreateNewTabFlyoutAction(const winrt::hstring& actionId, const winrt::hstring& iconPathOverride)
    {
        auto actionMenuItem = WUX::Controls::MenuFlyoutItem{};
        const auto action{ _settings.ActionMap().GetActionByID(actionId) };
        const auto actionKeyChord{ _settings.ActionMap().GetKeyBindingForAction(actionId) };

        if (actionKeyChord)
        {
            _SetAcceleratorForMenuItem(actionMenuItem, actionKeyChord);
        }

        actionMenuItem.Text(action.Name());

        // If a custom icon path has been specified, set it as the icon for
        // this flyout item. Otherwise, if an icon is set for this action, set that icon
        // for this flyout item.
        const auto& iconPath = iconPathOverride.empty() ? action.Icon().Resolved() : iconPathOverride;
        if (!iconPath.empty())
        {
            const auto icon = _CreateNewTabFlyoutIcon(iconPath);
            actionMenuItem.Icon(icon);
        }

        actionMenuItem.Click([action, weakThis{ get_weak() }](auto&&, auto&&) {
            if (auto page{ weakThis.get() })
            {
                TraceLoggingWrite(
                    g_hTerminalAppProvider,
                    "NewTabMenuItemClicked",
                    TraceLoggingDescription("Event emitted when an item from the new tab menu is invoked"),
                    TraceLoggingValue(page->NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
                    TraceLoggingValue("Action", "ItemType", "The type of item that was clicked in the new tab menu"),
                    TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                    TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

                page->_actionDispatch->DoAction(action.ActionAndArgs());
            }
        });

        return actionMenuItem;
    }

    // Method Description:
    // - Helper method to create an IconElement that can be passed to MenuFlyoutItems and
    //   MenuFlyoutSubItems
    IconElement TerminalPage::_CreateNewTabFlyoutIcon(const winrt::hstring& iconSource)
    {
        if (iconSource.empty())
        {
            return nullptr;
        }

        auto icon = UI::IconPathConverter::IconWUX(iconSource);
        Automation::AutomationProperties::SetAccessibilityView(icon, Automation::Peers::AccessibilityView::Raw);

        return icon;
    }

    // Function Description:
    // Called when the openNewTabDropdown keybinding is used.
    // Shows the dropdown flyout.
    void TerminalPage::_OpenNewTabDropdown()
    {
        _newTabButton.Flyout().ShowAt(_newTabButton);
    }

    void TerminalPage::_OpenNewTerminalViaDropdown(const NewTerminalArgs newTerminalArgs)
    {
        // if alt is pressed, open a pane
        const auto window = CoreWindow::GetForCurrentThread();
        const auto rAltState = window.GetKeyState(VirtualKey::RightMenu);
        const auto lAltState = window.GetKeyState(VirtualKey::LeftMenu);
        const auto altPressed = WI_IsFlagSet(lAltState, CoreVirtualKeyStates::Down) ||
                                WI_IsFlagSet(rAltState, CoreVirtualKeyStates::Down);

        const auto shiftState{ window.GetKeyState(VirtualKey::Shift) };
        const auto rShiftState = window.GetKeyState(VirtualKey::RightShift);
        const auto lShiftState = window.GetKeyState(VirtualKey::LeftShift);
        const auto shiftPressed{ WI_IsFlagSet(shiftState, CoreVirtualKeyStates::Down) ||
                                 WI_IsFlagSet(lShiftState, CoreVirtualKeyStates::Down) ||
                                 WI_IsFlagSet(rShiftState, CoreVirtualKeyStates::Down) };

        const auto ctrlState{ window.GetKeyState(VirtualKey::Control) };
        const auto rCtrlState = window.GetKeyState(VirtualKey::RightControl);
        const auto lCtrlState = window.GetKeyState(VirtualKey::LeftControl);
        const auto ctrlPressed{ WI_IsFlagSet(ctrlState, CoreVirtualKeyStates::Down) ||
                                WI_IsFlagSet(rCtrlState, CoreVirtualKeyStates::Down) ||
                                WI_IsFlagSet(lCtrlState, CoreVirtualKeyStates::Down) };

        // Check for DebugTap
        auto debugTap = this->_settings.GlobalSettings().DebugFeaturesEnabled() &&
                        WI_IsFlagSet(lAltState, CoreVirtualKeyStates::Down) &&
                        WI_IsFlagSet(rAltState, CoreVirtualKeyStates::Down);

        const auto dispatchToElevatedWindow = ctrlPressed && !IsRunningElevated();

        auto sessionType = "";
        if ((shiftPressed || dispatchToElevatedWindow) && !debugTap)
        {
            // Manually fill in the evaluated profile.
            if (newTerminalArgs.ProfileIndex() != nullptr)
            {
                // We want to promote the index to a GUID because there is no "launch to profile index" command.
                const auto profile = _settings.GetProfileForArgs(newTerminalArgs);
                if (profile)
                {
                    newTerminalArgs.Profile(::Microsoft::Console::Utils::GuidToString(profile.Guid()));
                    newTerminalArgs.StartingDirectory(_evaluatePathForCwd(profile.EvaluatedStartingDirectory()));
                }
            }

            if (dispatchToElevatedWindow)
            {
                _OpenElevatedWT(newTerminalArgs);
                sessionType = "ElevatedWindow";
            }
            else
            {
                _OpenNewWindow(newTerminalArgs);
                sessionType = "Window";
            }
        }
        else
        {
            const auto newPane = _MakePane(newTerminalArgs);
            // If the newTerminalArgs caused us to open an elevated window
            // instead of creating a pane, it may have returned nullptr. Just do
            // nothing then.
            if (!newPane)
            {
                return;
            }
            if (altPressed && !debugTap)
            {
                this->_SplitPane(_GetFocusedTabImpl(),
                                 SplitDirection::Automatic,
                                 0.5f,
                                 newPane);
                sessionType = "Pane";
            }
            else
            {
                _CreateNewTabFromPane(newPane);
                sessionType = "Tab";
            }
        }

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "NewTabMenuCreatedNewTerminalSession",
            TraceLoggingDescription("Event emitted when a new terminal was created via the new tab menu"),
            TraceLoggingValue(NumberOfTabs(), "NewTabCount", "The count of tabs currently opened in this window"),
            TraceLoggingValue(sessionType, "SessionType", "The type of session that was created"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
    }

    std::wstring TerminalPage::_evaluatePathForCwd(const std::wstring_view path)
    {
        return Utils::EvaluateStartingDirectory(_WindowProperties.VirtualWorkingDirectory(), path);
    }

    // Method Description:
    // - Creates a new connection based on the profile settings
    // Arguments:
    // - the profile we want the settings from
    // - the terminal settings
    // Return value:
    // - the desired connection
    TerminalConnection::ITerminalConnection TerminalPage::_CreateConnectionFromSettings(Profile profile,
                                                                                        IControlSettings settings,
                                                                                        const bool inheritCursor)
    {
        static const auto textMeasurement = [&]() -> std::wstring_view {
            switch (_settings.GlobalSettings().TextMeasurement())
            {
            case TextMeasurement::Graphemes:
                return L"graphemes";
            case TextMeasurement::Wcswidth:
                return L"wcswidth";
            case TextMeasurement::Console:
                return L"console";
            default:
                return {};
            }
        }();
        static const auto ambiguousIsWide = [&]() -> bool {
            return _settings.GlobalSettings().AmbiguousWidth() == AmbiguousWidth::Wide;
        }();

        TerminalConnection::ITerminalConnection connection{ nullptr };

        auto connectionType = profile.ConnectionType();
        Windows::Foundation::Collections::ValueSet valueSet;

        if (connectionType == TerminalConnection::AzureConnection::ConnectionType() &&
            TerminalConnection::AzureConnection::IsAzureConnectionAvailable())
        {
            connection = TerminalConnection::AzureConnection{};
            valueSet = TerminalConnection::ConptyConnection::CreateSettings(winrt::hstring{},
                                                                            L".",
                                                                            L"Azure",
                                                                            false,
                                                                            L"",
                                                                            nullptr,
                                                                            settings.InitialRows(),
                                                                            settings.InitialCols(),
                                                                            winrt::guid(),
                                                                            profile.Guid());
        }

        else
        {
            auto settingsInternal{ winrt::get_self<Settings::TerminalSettings>(settings) };
            auto environment = settingsInternal->EnvironmentVariables();

            // Update the path to be relative to whatever our CWD is.
            //
            // Refer to the examples in
            // https://en.cppreference.com/w/cpp/filesystem/path/append
            //
            // We need to do this here, to ensure we tell the ConptyConnection
            // the correct starting path. If we're being invoked from another
            // terminal instance (e.g. `wt -w 0 -d .`), then we have switched our
            // CWD to the provided path. We should treat the StartingDirectory
            // as relative to the current CWD.
            //
            // The connection must be informed of the current CWD on
            // construction, because the connection might not spawn the child
            // process until later, on another thread, after we've already
            // restored the CWD to its original value.
            auto newWorkingDirectory{ _evaluatePathForCwd(settings.StartingDirectory()) };
            connection = TerminalConnection::ConptyConnection{};
            valueSet = TerminalConnection::ConptyConnection::CreateSettings(settings.Commandline(),
                                                                            newWorkingDirectory,
                                                                            settings.StartingTitle(),
                                                                            settingsInternal->ReloadEnvironmentVariables(),
                                                                            _WindowProperties.VirtualEnvVars(),
                                                                            environment,
                                                                            settings.InitialRows(),
                                                                            settings.InitialCols(),
                                                                            winrt::guid(),
                                                                            profile.Guid());

            if (inheritCursor)
            {
                valueSet.Insert(L"inheritCursor", Windows::Foundation::PropertyValue::CreateBoolean(true));
            }
        }

        if (!textMeasurement.empty())
        {
            valueSet.Insert(L"textMeasurement", Windows::Foundation::PropertyValue::CreateString(textMeasurement));
        }
        if (ambiguousIsWide)
        {
            valueSet.Insert(L"ambiguousIsWide", Windows::Foundation::PropertyValue::CreateBoolean(true));
        }

        if (const auto id = settings.SessionId(); id != winrt::guid{})
        {
            valueSet.Insert(L"sessionId", Windows::Foundation::PropertyValue::CreateGuid(id));
        }

        connection.Initialize(valueSet);

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "ConnectionCreated",
            TraceLoggingDescription("Event emitted upon the creation of a connection"),
            TraceLoggingGuid(connectionType, "ConnectionTypeGuid", "The type of the connection"),
            TraceLoggingGuid(profile.Guid(), "ProfileGuid", "The profile's GUID"),
            TraceLoggingGuid(connection.SessionId(), "SessionGuid", "The WT_SESSION's GUID"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

        return connection;
    }

    TerminalConnection::ITerminalConnection TerminalPage::_duplicateConnectionForRestart(const TerminalApp::TerminalPaneContent& paneContent)
    {
        if (paneContent == nullptr)
        {
            return nullptr;
        }

        const auto& control{ paneContent.GetTermControl() };
        if (control == nullptr)
        {
            return nullptr;
        }
        const auto& connection = control.Connection();
        auto profile{ paneContent.GetProfile() };

        Settings::TerminalSettingsCreateResult controlSettings{ nullptr };

        if (profile)
        {
            // TODO GH#5047 If we cache the NewTerminalArgs, we no longer need to do this.
            profile = GetClosestProfileForDuplicationOfProfile(profile);
            controlSettings = Settings::TerminalSettings::CreateWithProfile(_settings, profile);

            // Replace the Starting directory with the CWD, if given
            const auto workingDirectory = control.WorkingDirectory();
            if (Utils::IsValidDirectory(workingDirectory.c_str()))
            {
                controlSettings.DefaultSettings()->StartingDirectory(workingDirectory);
            }

            // To facilitate restarting defterm connections: grab the original
            // commandline out of the connection and shove that back into the
            // settings.
            if (const auto& conpty{ connection.try_as<TerminalConnection::ConptyConnection>() })
            {
                controlSettings.DefaultSettings()->Commandline(conpty.Commandline());
            }
        }

        return _CreateConnectionFromSettings(profile, *controlSettings.DefaultSettings(), true);
    }

    // Method Description:
    // - Called when the settings button is clicked. Launches a background
    //   thread to open the settings file in the default JSON editor.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::_SettingsButtonOnClick(const IInspectable&,
                                              const RoutedEventArgs&)
    {
        const auto window = CoreWindow::GetForCurrentThread();

        // check alt state
        const auto rAltState{ window.GetKeyState(VirtualKey::RightMenu) };
        const auto lAltState{ window.GetKeyState(VirtualKey::LeftMenu) };
        const auto altPressed{ WI_IsFlagSet(lAltState, CoreVirtualKeyStates::Down) ||
                               WI_IsFlagSet(rAltState, CoreVirtualKeyStates::Down) };

        // check shift state
        const auto shiftState{ window.GetKeyState(VirtualKey::Shift) };
        const auto lShiftState{ window.GetKeyState(VirtualKey::LeftShift) };
        const auto rShiftState{ window.GetKeyState(VirtualKey::RightShift) };
        const auto shiftPressed{ WI_IsFlagSet(shiftState, CoreVirtualKeyStates::Down) ||
                                 WI_IsFlagSet(lShiftState, CoreVirtualKeyStates::Down) ||
                                 WI_IsFlagSet(rShiftState, CoreVirtualKeyStates::Down) };

        auto target{ SettingsTarget::SettingsUI };
        if (shiftPressed)
        {
            target = SettingsTarget::SettingsFile;
        }
        else if (altPressed)
        {
            target = SettingsTarget::DefaultsFile;
        }

        const auto targetAsString = [&target]() {
            switch (target)
            {
            case SettingsTarget::SettingsFile:
                return "SettingsFile";
            case SettingsTarget::DefaultsFile:
                return "DefaultsFile";
            case SettingsTarget::SettingsUI:
            default:
                return "UI";
            }
        }();

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "NewTabMenuItemClicked",
            TraceLoggingDescription("Event emitted when an item from the new tab menu is invoked"),
            TraceLoggingValue(NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
            TraceLoggingValue("Settings", "ItemType", "The type of item that was clicked in the new tab menu"),
            TraceLoggingValue(targetAsString, "SettingsTarget", "The target settings file or UI"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

        _LaunchSettings(target);
    }

    // Method Description:
    // - Called when the command palette button is clicked. Opens the command palette.
    void TerminalPage::_CommandPaletteButtonOnClick(const IInspectable&,
                                                    const RoutedEventArgs&)
    {
        auto p = LoadCommandPalette();
        p.EnableCommandPaletteMode(CommandPaletteLaunchMode::Action);
        p.Visibility(Visibility::Visible);

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "NewTabMenuItemClicked",
            TraceLoggingDescription("Event emitted when an item from the new tab menu is invoked"),
            TraceLoggingValue(NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
            TraceLoggingValue("CommandPalette", "ItemType", "The type of item that was clicked in the new tab menu"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
    }

    // Method Description:
    // - Called when the about button is clicked. See _ShowAboutDialog for more info.
    // Arguments:
    // - <unused>
    // Return Value:
    // - <none>
    void TerminalPage::_AboutButtonOnClick(const IInspectable&,
                                           const RoutedEventArgs&)
    {
        _ShowAboutDialog();

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "NewTabMenuItemClicked",
            TraceLoggingDescription("Event emitted when an item from the new tab menu is invoked"),
            TraceLoggingValue(NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
            TraceLoggingValue("About", "ItemType", "The type of item that was clicked in the new tab menu"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
    }

    // (Window-level bottom bar — click handlers above target the active
    // tab's AgentPaneContent. AgentPaneContent::StateChanged fires when
    // a tab's autofix/sessions/position state mutates; the page subscribes
    // via _WireAgentPaneEvents and refreshes the bar when the firing pane
    // is on the active tab.)

    // Inbound event from WTA carrying an autofix_state update. Routed
    // by `tab_id` from the JSON to that tab's AgentPaneContent. Pages
    // (or tabs) without a matching AgentPaneContent no-op.
    void TerminalPage::OnAutofixStateChanged(hstring eventJson)
    {
        Json::Value evt;
        Json::CharReaderBuilder rb;
        std::istringstream ss(winrt::to_string(eventJson));
        std::string errs;
        if (!Json::parseFromStream(rb, ss, &evt, &errs))
        {
            return;
        }
        const auto& params = evt["params"];
        if (!params.isObject() || !params.isMember("state"))
        {
            return;
        }
        const auto stateStr = params["state"].asString();

        using AS = winrt::TerminalApp::implementation::AgentPaneContent::AutofixState;
        AS state = AS::Idle;
        if (stateStr == "pending")
        {
            state = AS::Pending;
#if defined(WT_BRANDING_RELEASE)
            constexpr uint8_t branding = 3;
#elif defined(WT_BRANDING_PREVIEW)
            constexpr uint8_t branding = 2;
#elif defined(WT_BRANDING_CANARY)
            constexpr uint8_t branding = 1;
#else
            constexpr uint8_t branding = 0;
#endif
            TraceLoggingWrite(
                g_hTerminalAppProvider,
                "ErrorDetected",
                TraceLoggingDescription("Event emitted when an error is auto-detected in a terminal pane"),
                TraceLoggingValue(branding, "Branding"),
                TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
        }
        else if (stateStr == "armed") state = AS::Armed;
        else if (stateStr == "suggested") state = AS::Suggested;
        else if (stateStr == "detected") state = AS::Detected;
        else if (stateStr == "cleared") state = AS::Idle;

        const auto pickStr = [&](const char* key) -> winrt::hstring {
            if (params.isMember(key) && params[key].isString())
            {
                return winrt::to_hstring(params[key].asString());
            }
            return {};
        };
        const auto paneId = pickStr("pane_id");
        const auto summary = pickStr("summary");
        const auto fixPreview = pickStr("fix_preview");
        const auto hotkeyHint = pickStr("hotkey_hint");
        const auto suggestionTitle = pickStr("suggestion_title");

        // Route by tab_id if present. Falls back to all tabs (no
        // routing) when wta omits it; in that case the event is
        // broadcast — last-write-wins.
        const auto tabId = pickStr("tab_id");
        if (!tabId.empty())
        {
            if (const auto tab = _FindTabByStableId(tabId))
            {
                if (const auto content = tab->FindAgentPaneContent())
                {
                    if (const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content))
                    {
                        impl->ApplyAutofixState(state, paneId, summary, fixPreview, hotkeyHint, suggestionTitle);
                    }
                }
            }
            return;
        }
        // No tab_id — fan out to every agent pane in this window.
        for (const auto& t : _tabs)
        {
            if (auto tabImpl = _GetTabImpl(t))
            {
                if (const auto content = tabImpl->FindAgentPaneContent())
                {
                    if (const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(content))
                    {
                        impl->ApplyAutofixState(state, paneId, summary, fixPreview, hotkeyHint, suggestionTitle);
                    }
                }
            }
        }
    }

    // Inbound event from WTA: {method:"agent_status", params:{name,version,model,state}}.
    // Find the agent leaf and forward the values to AgentPaneContent so the
    // XAML bar can refresh its label / future status indicator.
    void TerminalPage::OnAgentStatusChanged(hstring eventJson)
    {
        Json::Value evt;
        Json::CharReaderBuilder rb;
        std::istringstream ss(winrt::to_string(eventJson));
        std::string errs;
        if (!Json::parseFromStream(rb, ss, &evt, &errs))
        {
            return;
        }
        const auto& params = evt["params"];
        if (!params.isObject())
        {
            return;
        }

        const auto pickStr = [&](const char* key) -> winrt::hstring {
            if (!params.isMember(key))
            {
                return {};
            }
            const auto& v = params[key];
            if (v.isString())
            {
                return winrt::to_hstring(v.asString());
            }
            return {};
        };
        const auto name = pickStr("name");
        const auto version = pickStr("version");
        const auto model = pickStr("model");
        const auto state = pickStr("state");

        _agentPaneLog("OnAgentStatusChanged: payload=" + winrt::to_string(eventJson).substr(0, 600));

        // If WTA signals a new agent selection (e.g. from FRE or preflight),
        // persist it to settings so the next launch uses the same agent.
        const auto selectedAgent = pickStr("selected_agent");
        if (!selectedAgent.empty())
        {
            const auto& globals = _settings.GlobalSettings();
            if (globals.AcpAgent() != selectedAgent)
            {
                globals.AcpAgent(selectedAgent);
                // Update the snapshot so _RebuildAgentStack (triggered by
                // the file-watcher after WriteSettingsToDisk) sees no diff
                // and skips the teardown. The current WTA pane is already
                // connected to the right agent.
                _lastAgentSettings.acpAgent = std::wstring{ selectedAgent };
                try
                {
                    _settings.WriteSettingsToDisk();
                }
                catch (...)
                {
                    LOG_CAUGHT_EXCEPTION();
                }
                _agentPaneLog("OnAgentStatusChanged: persisted acpAgent=" + winrt::to_string(selectedAgent));
            }
        }

        // Sync the process-wide model-list cache. The Settings UI's
        // AIAgentsViewModel reads from this on construction, so any new
        // dropdown opened after this point sees the freshest list.
        if (params.isMember("available_models") && params["available_models"].isArray())
        {
            _agentPaneLog("OnAgentStatusChanged: available_models has " +
                          std::to_string(params["available_models"].size()) + " entries");
            std::vector<winrt::Microsoft::Terminal::Settings::Model::AcpModelInfo> entries;
            for (const auto& m : params["available_models"])
            {
                if (!m.isObject())
                {
                    continue;
                }
                winrt::hstring id;
                winrt::hstring name;
                winrt::hstring description;
                if (m.isMember("id") && m["id"].isString())
                {
                    id = winrt::to_hstring(m["id"].asString());
                }
                if (m.isMember("name") && m["name"].isString())
                {
                    name = winrt::to_hstring(m["name"].asString());
                }
                if (m.isMember("description") && m["description"].isString())
                {
                    description = winrt::to_hstring(m["description"].asString());
                }
                if (!id.empty())
                {
                    entries.push_back(winrt::Microsoft::Terminal::Settings::Model::AcpModelInfo{
                        id,
                        name.empty() ? id : name,
                        description });
                }
            }
            winrt::hstring currentId;
            if (params.isMember("current_model_id") && params["current_model_id"].isString())
            {
                currentId = winrt::to_hstring(params["current_model_id"].asString());
            }
            winrt::Microsoft::Terminal::Settings::Model::AcpRuntimeState::Current()
                .SetAvailableModels(
                    winrt::single_threaded_vector(std::move(entries)).GetView(),
                    currentId);
        }

        // Route by tab_id when present; otherwise fan out to every
        // agent pane in this window (e.g. settings broadcasts).
        const auto pickTabId = [&]() -> winrt::hstring {
            if (params.isMember("tab_id") && params["tab_id"].isString())
            {
                return winrt::to_hstring(params["tab_id"].asString());
            }
            return {};
        };
        const auto tabId = pickTabId();
        const auto update = [&](const winrt::com_ptr<Tab>& tabImpl) {
            if (const auto content = tabImpl->FindAgentPaneContent())
            {
                content.UpdateAgentStatus(name, version, model, state);
            }
        };
        if (!tabId.empty())
        {
            if (const auto tab = _FindTabByStableId(tabId))
            {
                update(tab);
            }
            return;
        }
        for (const auto& t : _tabs)
        {
            if (auto tabImpl = _GetTabImpl(t))
            {
                update(tabImpl);
            }
        }
    }

    // Inbound event from WTA:
    //   {method:"agent_state_changed", params:{view, pane_open}}
    //
    // **Single-writer handler** for all per-tab agent-pane UI state that
    // C++ caches as global per-pane state. wta is the sole owner of every
    // field carried here; we never write the mirrors from any other code
    // path (the lone exception is the pane-Closed handler, where wta is
    // dead and there is no one left to send projections). This is what
    // makes view-state / pane-visibility desync architecturally
    // impossible — a single writer means the mirrors can only ever
    // reflect wta's last reported snapshot.
    //
    // wta pushes this event whenever active-tab state changes:
    //   - `tab_changed` (active tab swap; via `project_active_tab_state`
    //     in `switch_tab_session`).
    //   - `set_agent_state` (C++-originated request; wta echoes back).
    //   - Esc out of Agents view, `/sessions` slash command,
    //     `load_session`, Ctrl+C×2 reset, and once at startup after
    //     `--initial-view`.
    //
    // Don't send anything back here. The mirror updates below are the
    // terminal state of the round-trip; emitting `set_agent_state` would
    // just bounce the same snapshot back.
    //
    // Future per-tab UI state plugs in as another field on `params` —
    // parse it here, update its mirror, no new IDL route or handler needed.
    void TerminalPage::OnAgentStateChanged(hstring eventJson)
    {
        Json::Value evt;
        Json::CharReaderBuilder rb;
        std::istringstream ss(winrt::to_string(eventJson));
        std::string errs;
        if (!Json::parseFromStream(rb, ss, &evt, &errs))
        {
            return;
        }
        const auto& params = evt["params"];
        if (!params.isObject())
        {
            return;
        }

        // Route by `tab_id`. wta always sends one with these snapshots.
        winrt::hstring tabId;
        if (params.isMember("tab_id") && params["tab_id"].isString())
        {
            tabId = winrt::to_hstring(params["tab_id"].asString());
        }
        if (tabId.empty())
        {
            // No routing info — drop silently. Each pane is tab-local; we
            // don't know which to update.
            _agentPaneLog("OnAgentStateChanged: missing tab_id, dropping");
            return;
        }
        const auto targetTab = _FindTabByStableId(tabId);
        if (!targetTab)
        {
            // Tab is unknown in this window — likely belongs to another window.
            return;
        }

        std::string logSuffix = " tab_id=" + winrt::to_string(tabId);

        std::optional<bool> wantOpen;
        if (params.isMember("pane_open") && params["pane_open"].isBool())
        {
            wantOpen = params["pane_open"].asBool();
            logSuffix += std::string{ " pane_open=" } + (*wantOpen ? "true" : "false");
        }
        std::optional<std::string> view;
        if (params.isMember("view") && params["view"].isString())
        {
            view = params["view"].asString();
            logSuffix += " view=" + *view;
        }
        _agentPaneLog(std::string{ "OnAgentStateChanged:" } + logSuffix);

        // Apply view to the existing AgentPaneContent if any.
        if (view.has_value())
        {
            if (const auto agentContent = targetTab->FindAgentPaneContent())
            {
                agentContent.SetSessionsView(*view == "sessions");
            }
        }

        // Apply pane_open as detach/reattach (NOT destroy/recreate). On
        // pane_open=false we detach the agent pane out of the tab's tree
        // and stash it on the Tab — the sibling terminal pane expands to
        // fill the recovered space, but the agent pane's TermControl +
        // conpty + wta-helper process stay alive in the stash. On
        // pane_open=true we re-attach the stash via SplitPane so the helper
        // keeps its ACP session and TUI history across toggles. The pane
        // is only truly destroyed when the tab itself closes (the Tab
        // destructor releases the stash) or by an explicit
        // `OnCloseAgentPaneRequested` (Ctrl+C×2 in TUI).
        if (wantOpen.has_value())
        {
            if (*wantOpen)
            {
                if (targetTab->HasStashedAgentPane())
                {
                    const auto splitDir = _AgentPanePositionToSplitDirection(_settings.GlobalSettings().AgentPanePosition());
                    targetTab->RestoreStashedAgentPane(splitDir);
                }
                else if (!targetTab->FindAgentPane())
                {
                    // No pane on this tab yet — first toggle-open is the
                    // spawn path. View defaults to chat unless `view=sessions`.
                    const bool intoSessions = view.has_value() && *view == "sessions";

                    // Plan-C: consume any pending load-session hint for
                    // this tab. Set by `OnResumeInNewAgentTabRequested`
                    // when the user pressed Enter on a Historical/Ended
                    // row in F2 — the new helper boots straight into a
                    // `session/load` of the requested session id instead
                    // of creating a fresh session. One-shot: the entry
                    // is moved out and erased here so a later
                    // `agent_state_changed` for the same tab (e.g. a
                    // tab_changed echo) doesn't accidentally re-spawn.
                    std::string pendingSid;
                    std::string pendingCwd;
                    if (const auto it = _pendingLoadSessions.find(tabId); it != _pendingLoadSessions.end())
                    {
                        pendingSid = std::move(it->second.sessionId);
                        pendingCwd = std::move(it->second.cwd);
                        _pendingLoadSessions.erase(it);
                        _agentPaneLog("OnAgentStateChanged: consuming pending load_session for tab " + winrt::to_string(tabId));
                    }
                    _AutoCreateHiddenAgentPaneShared(targetTab, intoSessions, /*autoStash*/ false, pendingSid, pendingCwd);
                }
            }
            else
            {
                if (targetTab->FindAgentPane())
                {
                    // The agent pane is being hidden — drop any chip
                    // override so the chip doesn't stay pinned on a
                    // background pane while the agent is out of sight.
                    targetTab->SetAgentChipOverride(std::nullopt);
                    targetTab->StashAgentPane();
                }
            }
        }

        // Bottom-bar catch-all. AgentPaneContent::SetSessionsView is idempotent
        // and skips `StateChanged` when the view didn't change — so a pure
        // `tab_changed` echo (same state, just routed to the now-focused tab)
        // would never re-render the bar without this. Cheap and idempotent.
        if (const auto activeTab = _GetFocusedTabImpl())
        {
            if (activeTab->StableId() == tabId)
            {
                _UpdateBottomBarState();
            }
        }
    }

    // Inbound event from WTA: {method:"close_agent_pane", params:{tab_id}}.
    // User pressed Ctrl+C twice in the agent pane TUI on a specific tab.
    // Tear down that tab's agent pane.
    void TerminalPage::OnCloseAgentPaneRequested(hstring eventJson)
    {
        _agentPaneLog("OnCloseAgentPaneRequested: user requested close from wta");
        Json::Value evt;
        Json::CharReaderBuilder rb;
        std::istringstream ss(winrt::to_string(eventJson));
        std::string errs;
        if (!Json::parseFromStream(rb, ss, &evt, &errs))
        {
            return;
        }
        winrt::hstring tabId;
        if (evt.isMember("params") && evt["params"].isObject() &&
            evt["params"].isMember("tab_id") && evt["params"]["tab_id"].isString())
        {
            tabId = winrt::to_hstring(evt["params"]["tab_id"].asString());
        }
        if (tabId.empty())
        {
            _agentPaneLog("OnCloseAgentPaneRequested: missing tab_id, dropping");
            return;
        }
        const auto ownerTab = _FindTabByStableId(tabId);
        if (!ownerTab)
        {
            // Tab is unknown in this window — belongs to another window.
            return;
        }
        // Tell wta to drop this tab's ACP session.
        _NotifyAgentTabReset(ownerTab->StableId());
        // The agent pane (and its helper) is going away, so any chip
        // override the helper had set is no longer authoritative. Drop it
        // here so the chip can't get pinned by a dead helper.
        ownerTab->SetAgentChipOverride(std::nullopt);
        _TeardownAgentPane(ownerTab);
    }

    // Send {method:"autofix_execute",params:{pane_id}} over the outbound
    // protocol bus. The agent pane on `ownerTab` must be in the Armed state.
    void TerminalPage::_TriggerAutofix(const winrt::com_ptr<Tab>& ownerTab, const wchar_t* triggerSource)
    {
        if (!ownerTab)
        {
            return;
        }
        const auto agentContent = ownerTab->FindAgentPaneContent();
        if (!agentContent)
        {
            return;
        }
        const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent);
        if (!impl)
        {
            return;
        }
        using AS = winrt::TerminalApp::implementation::AgentPaneContent::AutofixState;
        if (impl->GetAutofixState() != AS::Armed)
        {
            return;
        }
        const auto paneId = impl->GetLastErrorPaneId();

#if defined(WT_BRANDING_RELEASE)
        constexpr uint8_t branding = 3;
#elif defined(WT_BRANDING_PREVIEW)
        constexpr uint8_t branding = 2;
#elif defined(WT_BRANDING_CANARY)
        constexpr uint8_t branding = 1;
#else
        constexpr uint8_t branding = 0;
#endif
        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "ErrorFixAttempted",
            TraceLoggingDescription("Event emitted when the user attempts an agent-suggested fix"),
            TraceLoggingWideString(triggerSource, "TriggerSource", "How the fix was triggered"),
            TraceLoggingValue(branding, "Branding"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

        Json::Value evt;
        evt["type"] = "event";
        evt["method"] = "autofix_execute";
        Json::Value params;
        params["pane_id"] = winrt::to_string(paneId);
        params["tab_id"] = winrt::to_string(ownerTab->StableId());
        evt["params"] = params;
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        ProtocolVtSequenceReceived.raise(
            *this,
            winrt::to_hstring(Json::writeString(wb, evt)));

        // WTA will emit autofix_state:cleared — OnAutofixStateChanged handles the transition.
    }

    // Inbound event from WTA: {method:"set_agent_chip_target",
    //                          params:{tab_id, pane_session_id?}}.
    // Selects which pane in the tab shows the blue "Agent" chip. When
    // pane_session_id is missing or null the tab reverts to the default
    // chip behavior (driven by IsSourceOfAgentPane on each pane).
    void TerminalPage::OnAgentChipTargetChanged(hstring eventJson)
    {
        Json::Value evt;
        Json::CharReaderBuilder rb;
        std::istringstream ss(winrt::to_string(eventJson));
        std::string errs;
        if (!Json::parseFromStream(rb, ss, &evt, &errs))
        {
            return;
        }
        const auto& params = evt["params"];
        if (!params.isObject())
        {
            return;
        }

        winrt::hstring tabId;
        if (params.isMember("tab_id") && params["tab_id"].isString())
        {
            tabId = winrt::to_hstring(params["tab_id"].asString());
        }
        if (tabId.empty())
        {
            return;
        }
        const auto targetTab = _FindTabByStableId(tabId);
        if (!targetTab)
        {
            // Tab not in this window — fan-out will hit the right one.
            return;
        }

        std::optional<winrt::guid> sessionId;
        if (params.isMember("pane_session_id") &&
            params["pane_session_id"].isString())
        {
            const auto raw = params["pane_session_id"].asString();
            if (!raw.empty())
            {
                const auto wide = winrt::to_hstring(raw);
                try
                {
                    // Accept both braced ({…}) and plain GUID encodings.
                    sessionId = (raw.size() >= 2 && raw.front() == '{')
                                    ? winrt::guid{ ::Microsoft::Console::Utils::GuidFromString(wide.c_str()) }
                                    : winrt::guid{ ::Microsoft::Console::Utils::GuidFromPlainString(wide.c_str()) };
                }
                catch (...)
                {
                    sessionId = std::nullopt;
                }
            }
        }

        targetTab->SetAgentChipOverride(sessionId);
    }

    // Inbound event from WTA: {method:"resume_in_new_agent_tab",
    //                          params:{session_id, cwd}}.
    // Sent by the session view's Enter handler on a Historical/Ended row
    // (Plan-C ResumeInAgentPane path). We:
    //   1. Create a new tab with the default profile (using the historical
    //      session's cwd as the starting directory when provided).
    //   2. Stash the (session_id, cwd) in `_pendingLoadSessions` keyed by
    //      the new tab's StableId.
    //   3. Ask wta to mark the new tab's agent pane as open. wta echoes
    //      `agent_state_changed{pane_open:true, tab_id:<new>}` which
    //      lands in `OnAgentStateChanged`; the pending entry is consumed
    //      there and passed to `_AutoCreateHiddenAgentPaneShared` so the
    //      newly-spawned helper boots with `--initial-load-session-id`
    //      (atomic spawn + `session/load` via main.rs Plan-C glue).
    //
    // No separate `load_session` VT broadcast — the prior design had a
    // race where the broadcast often arrived at the WRONG helper because
    // every helper subscribes to the same shared COM event stream and
    // the new helper's pipe attach hadn't completed yet when the
    // broadcast fired.
    //
    // The shared-agent-pane model means we can't actually have two
    // independent ACP connections on one window. If the running WTA was
    // launched with a CLI that doesn't match the historical session's
    // origin, `session/load` will return an error that surfaces as an
    // AgentError in the new tab's chat view (best-effort by design — see
    // plan.md "Constraints established with user").
    void TerminalPage::OnResumeInNewAgentTabRequested(hstring eventJson)
    {
        _agentPaneLog("OnResumeInNewAgentTabRequested: received from wta");

        Json::Value evt;
        Json::CharReaderBuilder rb;
        std::string errs;
        const auto jsonStr = winrt::to_string(eventJson);
        std::istringstream is{ jsonStr };
        if (!Json::parseFromStream(rb, is, &evt, &errs))
        {
            _agentPaneLog("OnResumeInNewAgentTabRequested: failed to parse JSON: " + errs);
            return;
        }
        if (!evt.isMember("params") || !evt["params"].isObject())
        {
            _agentPaneLog("OnResumeInNewAgentTabRequested: missing params object");
            return;
        }
        const auto& params = evt["params"];
        const std::string sessionIdStr = params.get("session_id", "").asString();
        const std::string cwdStr = params.get("cwd", "").asString();
        if (sessionIdStr.empty())
        {
            _agentPaneLog("OnResumeInNewAgentTabRequested: empty session_id — ignoring");
            return;
        }

        // Step 1: create a new tab.
        Settings::Model::NewTerminalArgs newTerminalArgs{};
        if (!cwdStr.empty())
        {
            newTerminalArgs.StartingDirectory(winrt::to_hstring(cwdStr));
        }
        const auto hr = _OpenNewTab(newTerminalArgs, /*openInBackground*/ false);
        if (FAILED(hr))
        {
            _agentPaneLog("OnResumeInNewAgentTabRequested: _OpenNewTab failed");
            return;
        }

        // Step 2: register the pending load-session for the new tab and
        // ask wta to mark it as having an open agent pane. The resulting
        // `agent_state_changed{pane_open:true}` lands in
        // `OnAgentStateChanged`, which consumes the pending entry and
        // spawns the helper with the bundled resume request.
        const auto newTab = _GetFocusedTabImpl();
        if (!newTab)
        {
            _agentPaneLog("OnResumeInNewAgentTabRequested: no focused tab after _OpenNewTab");
            return;
        }
        const auto newStableId = newTab->StableId();
        if (newStableId.empty())
        {
            _agentPaneLog("OnResumeInNewAgentTabRequested: new tab has empty StableId");
            return;
        }
        _pendingLoadSessions[newStableId] = _PendingLoadSession{ sessionIdStr, cwdStr };
        _agentPaneLog("OnResumeInNewAgentTabRequested: stashed pending load_session for tab " +
                      winrt::to_string(newStableId) + " session_id=" + sessionIdStr);
        _RequestAgentStateForTab(newTab, std::nullopt, /*pane_open*/ true);
    }

    // Method Description:
    // - Called when the users pressed keyBindings while CommandPaletteElement is open.
    // - As of GH#8480, this is also bound to the TabRowControl's KeyUp event.
    //   That should only fire when focus is in the tab row, which is hard to
    //   do. Notably, that's possible:
    //   - When you have enough tabs to make the little scroll arrows appear,
    //     click one, then hit tab
    //   - When Narrator is in Scan mode (which is the a11y bug we're fixing here)
    // - This method is effectively an extract of TermControl::_KeyHandler and TermControl::_TryHandleKeyBinding.
    // Arguments:
    // - e: the KeyRoutedEventArgs containing info about the keystroke.
    // Return Value:
    // - <none>
    void TerminalPage::_KeyDownHandler(const Windows::Foundation::IInspectable& /*sender*/, const Windows::UI::Xaml::Input::KeyRoutedEventArgs& e)
    {
        const auto keyStatus = e.KeyStatus();
        const auto vkey = gsl::narrow_cast<WORD>(e.OriginalKey());
        const auto scanCode = gsl::narrow_cast<WORD>(keyStatus.ScanCode);
        const auto modifiers = _GetPressedModifierKeys();

        // GH#11076:
        // For some weird reason we sometimes receive a WM_KEYDOWN
        // message without vkey or scanCode if a user drags a tab.
        // The KeyChord constructor has a debug assertion ensuring that all KeyChord
        // either have a valid vkey/scanCode. This is important, because this prevents
        // accidental insertion of invalid KeyChords into classes like ActionMap.
        if (!vkey && !scanCode)
        {
            return;
        }

        // Alt-Numpad# input will send us a character once the user releases
        // Alt, so we should be ignoring the individual keydowns. The character
        // will be sent through the TSFInputControl. See GH#1401 for more
        // details
        if (modifiers.IsAltPressed() && (vkey >= VK_NUMPAD0 && vkey <= VK_NUMPAD9))
        {
            return;
        }

        // GH#2235: Terminal::Settings hasn't been modified to differentiate
        // between AltGr and Ctrl+Alt yet.
        // -> Don't check for key bindings if this is an AltGr key combination.
        if (modifiers.IsAltGrPressed())
        {
            return;
        }

        const auto actionMap = _settings.ActionMap();
        if (!actionMap)
        {
            return;
        }

        const auto cmd = actionMap.GetActionByKeyChord({
            modifiers.IsCtrlPressed(),
            modifiers.IsAltPressed(),
            modifiers.IsShiftPressed(),
            modifiers.IsWinPressed(),
            vkey,
            scanCode,
        });
        if (!cmd)
        {
            return;
        }

        if (!_actionDispatch->DoAction(cmd.ActionAndArgs()))
        {
            return;
        }

        if (_commandPaletteIs(Visibility::Visible) &&
            cmd.ActionAndArgs().Action() != ShortcutAction::ToggleCommandPalette)
        {
            CommandPaletteElement().Visibility(Visibility::Collapsed);
        }
        if (_suggestionsControlIs(Visibility::Visible) &&
            cmd.ActionAndArgs().Action() != ShortcutAction::ToggleCommandPalette)
        {
            SuggestionsElement().Visibility(Visibility::Collapsed);
        }

        // Let's assume the user has bound the dead key "^" to a sendInput command that sends "b".
        // If the user presses the two keys "^a" it'll produce "bâ", despite us marking the key event as handled.
        // The following is used to manually "consume" such dead keys and clear them from the keyboard state.
        _ClearKeyboardState(vkey, scanCode);
        e.Handled(true);
    }

    bool TerminalPage::OnDirectKeyEvent(const uint32_t vkey, const uint8_t scanCode, const bool down)
    {
        const auto modifiers = _GetPressedModifierKeys();
        if (vkey == VK_SPACE && modifiers.IsAltPressed() && down)
        {
            if (const auto actionMap = _settings.ActionMap())
            {
                if (const auto cmd = actionMap.GetActionByKeyChord({
                        modifiers.IsCtrlPressed(),
                        modifiers.IsAltPressed(),
                        modifiers.IsShiftPressed(),
                        modifiers.IsWinPressed(),
                        gsl::narrow_cast<int32_t>(vkey),
                        scanCode,
                    }))
                {
                    return _actionDispatch->DoAction(cmd.ActionAndArgs());
                }
            }
        }
        return false;
    }

    // Method Description:
    // - Get the modifier keys that are currently pressed. This can be used to
    //   find out which modifiers (ctrl, alt, shift) are pressed in events that
    //   don't necessarily include that state.
    // - This is a copy of TermControl::_GetPressedModifierKeys.
    // Return Value:
    // - The Microsoft::Terminal::Core::ControlKeyStates representing the modifier key states.
    ControlKeyStates TerminalPage::_GetPressedModifierKeys() noexcept
    {
        const auto window = CoreWindow::GetForCurrentThread();
        // DONT USE
        //      != CoreVirtualKeyStates::None
        // OR
        //      == CoreVirtualKeyStates::Down
        // Sometimes with the key down, the state is Down | Locked.
        // Sometimes with the key up, the state is Locked.
        // IsFlagSet(Down) is the only correct solution.

        struct KeyModifier
        {
            VirtualKey vkey;
            ControlKeyStates flags;
        };

        constexpr std::array<KeyModifier, 7> modifiers{ {
            { VirtualKey::RightMenu, ControlKeyStates::RightAltPressed },
            { VirtualKey::LeftMenu, ControlKeyStates::LeftAltPressed },
            { VirtualKey::RightControl, ControlKeyStates::RightCtrlPressed },
            { VirtualKey::LeftControl, ControlKeyStates::LeftCtrlPressed },
            { VirtualKey::Shift, ControlKeyStates::ShiftPressed },
            { VirtualKey::RightWindows, ControlKeyStates::RightWinPressed },
            { VirtualKey::LeftWindows, ControlKeyStates::LeftWinPressed },
        } };

        ControlKeyStates flags;

        for (const auto& mod : modifiers)
        {
            const auto state = window.GetKeyState(mod.vkey);
            const auto isDown = WI_IsFlagSet(state, CoreVirtualKeyStates::Down);

            if (isDown)
            {
                flags |= mod.flags;
            }
        }

        return flags;
    }

    // Method Description:
    // - Discards currently pressed dead keys.
    // - This is a copy of TermControl::_ClearKeyboardState.
    // Arguments:
    // - vkey: The vkey of the key pressed.
    // - scanCode: The scan code of the key pressed.
    void TerminalPage::_ClearKeyboardState(const WORD vkey, const WORD scanCode) noexcept
    {
        std::array<BYTE, 256> keyState;
        if (!GetKeyboardState(keyState.data()))
        {
            return;
        }

        // As described in "Sometimes you *want* to interfere with the keyboard's state buffer":
        //   http://archives.miloush.net/michkap/archive/2006/09/10/748775.html
        // > "The key here is to keep trying to pass stuff to ToUnicode until -1 is not returned."
        std::array<wchar_t, 16> buffer;
        while (ToUnicodeEx(vkey, scanCode, keyState.data(), buffer.data(), gsl::narrow_cast<int>(buffer.size()), 0b1, nullptr) < 0)
        {
        }
    }

    // Method Description:
    // - Configure the AppKeyBindings to use our ShortcutActionDispatch and the updated ActionMap
    //    as the object to handle dispatching ShortcutAction events.
    // Arguments:
    // - bindings: An IActionMapView object to wire up with our event handlers
    void TerminalPage::_HookupKeyBindings(const IActionMapView& actionMap) noexcept
    {
        _bindings->SetDispatch(*_actionDispatch);
        _bindings->SetActionMap(actionMap);
    }

    // Method Description:
    // - Register our event handlers with our ShortcutActionDispatch. The
    //   ShortcutActionDispatch is responsible for raising the appropriate
    //   events for an ActionAndArgs. WE'll handle each possible event in our
    //   own way.
    // Arguments:
    // - <none>
    void TerminalPage::_RegisterActionCallbacks()
    {
        // Hook up the ShortcutActionDispatch object's events to our handlers.
        // They should all be hooked up here, regardless of whether or not
        // there's an actual keychord for them.
#define ON_ALL_ACTIONS(action) HOOKUP_ACTION(action);
        ALL_SHORTCUT_ACTIONS
        INTERNAL_SHORTCUT_ACTIONS
#undef ON_ALL_ACTIONS
    }

    // Method Description:
    // - Get the title of the currently focused terminal control. If this tab is
    //   the focused tab, then also bubble this title to any listeners of our
    //   TitleChanged event.
    // Arguments:
    // - tab: the Tab to update the title for.
    void TerminalPage::_UpdateTitle(const Tab& tab)
    {
        if (tab == _GetFocusedTab())
        {
            TitleChanged.raise(*this, nullptr);
        }
    }

    // Method Description:
    // - Connects event handlers to the TermControl for events that we want to
    //   handle. This includes:
    //    * the Copy and Paste events, for setting and retrieving clipboard data
    //      on the right thread
    // Arguments:
    // - term: The newly created TermControl to connect the events for
    std::string TerminalPage::_FindSessionIdForControl(const TermControl& control)
    {
        if (const auto conn = control.Connection())
        {
            const auto sid = conn.SessionId();
            if (sid != winrt::guid{})
            {
                // Format as plain GUID string (no braces), matching WT_SESSION.
                wchar_t buf[40]{};
                StringFromGUID2(sid, buf, ARRAYSIZE(buf));
                // StringFromGUID2 produces {XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}
                // Strip braces for plain format.
                std::wstring ws(buf);
                if (ws.size() > 2 && ws.front() == L'{' && ws.back() == L'}')
                    ws = ws.substr(1, ws.size() - 2);
                return winrt::to_string(winrt::hstring{ ws });
            }
        }
        return {};
    }

    // Walk every tab's pane tree and return the StableId of the tab that
    // owns the given control. Used to tag protocol events with both pane
    // GUID and tab id so wta can route per-tab regardless of which tab is
    // currently active.
    std::string TerminalPage::_FindTabIdForControl(const TermControl& control)
    {
        if (!control)
        {
            return {};
        }
        for (const auto& tab : _tabs)
        {
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
            const auto match = rootPane->WalkTree([&](const auto& p) -> std::shared_ptr<Pane> {
                const auto ctl = p->GetTerminalControl();
                return (ctl && ctl == control) ? p : nullptr;
            });
            if (match)
            {
                return winrt::to_string(tabImpl->StableId());
            }
        }
        return {};
    }

    void TerminalPage::_RegisterTerminalEvents(TermControl term)
    {
        term.RaiseNotice({ this, &TerminalPage::_ControlNoticeRaisedHandler });

        term.WriteToClipboard({ get_weak(), &TerminalPage::_copyToClipboard });
        term.PasteFromClipboard({ this, &TerminalPage::_PasteFromClipboardHandler });

        term.OpenHyperlink({ this, &TerminalPage::_OpenHyperlinkHandler });

        // Add an event handler for when the terminal or tab wants to set a
        // progress indicator on the taskbar
        term.SetTaskbarProgress({ get_weak(), &TerminalPage::_SetTaskbarProgressHandler });

        term.ConnectionStateChanged({ get_weak(), &TerminalPage::_ConnectionStateChangedHandler });

        term.PropertyChanged([weakThis = get_weak()](auto& /*sender*/, auto& e) {
            if (auto page{ weakThis.get() })
            {
                if (e.PropertyName() == L"BackgroundBrush")
                {
                    page->_updateThemeColors();
                }
            }
        });

        term.ShowWindowChanged({ get_weak(), &TerminalPage::_ShowWindowChangedHandler });
        term.SearchMissingCommand({ get_weak(), &TerminalPage::_SearchMissingCommandHandler });
        term.WindowSizeChanged({ get_weak(), &TerminalPage::_WindowSizeChanged });

        // Forward VT sequences and connection state changes to protocol clients.
        // This is unconditional — if no pipe client is listening, the event raise is a noop.
        //
        // We capture a weak ref to the TermControl and resolve the connection SessionId
        // at event-fire time, because at _RegisterTerminalEvents time the Pane hasn't
        // been created yet (TermControl is set up before the Pane wraps it).
        //
        // VtSequenceReceived fires on the connection reader thread (background).
        // The dispatched continuation calls `_FindTabIdForControl`, which walks
        // `_tabs` and has UI thread affinity, so the event raise has to run on
        // the UI thread. `_FindSessionIdForControl` itself is thread-safe
        // (only reads `Connection().SessionId()`) and could be called inline,
        // but the rest of the work in this handler is gated on `_FindTabIdForControl`
        // and the protocol event raise, so we just defer the whole body.
        {
            winrt::weak_ref<TermControl> weakTerm{ term };

            term.VtSequenceReceived(
                [weakThis = get_weak(), weakTerm](auto&&, const winrt::hstring& seq) {
                    auto strongThis = weakThis.get();
                    if (!strongThis)
                        return;

                    // Dispatch to UI thread for the `_FindTabIdForControl` walk
                    // of `_tabs` and the protocol event raise. Fire-and-forget —
                    // don't block the connection reader thread.
                    strongThis->Dispatcher().RunAsync(
                        winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                        [weakThis, weakTerm, seq]() {
                            auto page = weakThis.get();
                            auto term2 = weakTerm.get();
                            if (!page || !term2)
                                return;

                            // GPO-blocked gate: when administrator policy
                            // explicitly disables auto-fix, the feature
                            // is off across the board — no Pending, no
                            // Detected pill, no background analysis.
                            // `IsAutoFixPolicyLocked()` returns true only
                            // for the Blocked policy state; Forced-on
                            // states the user can change fall through.
                            if (page->_settings.GlobalSettings().IsAutoFixPolicyLocked())
                                return;

                            // Early filter: WTA only acts on osc:133;*
                            // and AgentEvent payloads. Every other VT
                            // sequence (cursor moves, OSC 0/1 titles,
                            // color resets, …) gets classified as
                            // Informational and dropped on the other
                            // side, so skip the pane/tab lookup, JSON
                            // encode, and IPC for those entirely. This
                            // matters because VtSequenceReceived can
                            // fire hundreds of times a second on a
                            // busy terminal.
                            //
                            // (The `autoFixEnabled` user setting only
                            // controls whether WTA *automatically*
                            // invokes the LLM; the Detected pill needs
                            // these events too. Toggle changes
                            // hot-reload into WTA via
                            // `autofix_enabled_changed`, so no
                            // user-pref gate is needed here.)
                            auto seqStr = winrt::to_string(seq);
                            static constexpr std::string_view agentPrefix = "AgentEvent;";
                            static constexpr std::string_view osc133Prefix = "osc:133;";
                            const bool isAgentEvent = seqStr.starts_with(agentPrefix);
                            const bool isOsc133 = seqStr.starts_with(osc133Prefix);
                            if (!isAgentEvent && !isOsc133)
                            {
                                return;
                            }

                            const auto paneIdStr = page->_FindSessionIdForControl(term2);
                            if (paneIdStr.empty())
                                return;
                            const auto tabIdStr = page->_FindTabIdForControl(term2);

                            if (isAgentEvent)
                            {
                                auto jsonPayload = seqStr.substr(agentPrefix.size());
                                Json::Value agentParams;
                                Json::CharReaderBuilder rb;
                                std::string errs;
                                std::istringstream ss(jsonPayload);
                                if (Json::parseFromStream(rb, ss, &agentParams, &errs) &&
                                    agentParams.isObject() &&
                                    agentParams.isMember("event") &&
                                    agentParams["event"].isString())
                                {
                                    agentParams["pane_id"] = paneIdStr;
                                    if (!tabIdStr.empty())
                                    {
                                        agentParams["tab_id"] = tabIdStr;
                                    }

                                    Json::Value evt;
                                    evt["type"] = "event";
                                    evt["method"] = "agent_event";
                                    evt["params"] = agentParams;

                                    Json::StreamWriterBuilder wb;
                                    wb["indentation"] = "";
                                    page->ProtocolVtSequenceReceived.raise(
                                        *page,
                                        winrt::to_hstring(Json::writeString(wb, evt)));
                                }
                                return; // AgentEvent never falls through to vt_sequence
                            }

                            // isOsc133 path — forward as vt_sequence.
                            Json::Value evt;
                            evt["type"] = "event";
                            evt["method"] = "vt_sequence";
                            Json::Value params;
                            params["pane_id"] = paneIdStr;
                            if (!tabIdStr.empty())
                            {
                                params["tab_id"] = tabIdStr;
                            }
                            params["sequence"] = seqStr;
                            evt["params"] = params;
                            Json::StreamWriterBuilder wb;
                            wb["indentation"] = "";
                            page->ProtocolVtSequenceReceived.raise(
                                *page,
                                winrt::to_hstring(Json::writeString(wb, evt)));
                        });
                });

            term.ConnectionStateChanged(
                [weakThis = get_weak(), weakTerm](const auto& /*sender*/, auto&&) {
                    auto strongThis = weakThis.get();
                    if (!strongThis)
                        return;

                    // NOTE: `sender` here is NOT the TermControl. TermControl
                    // bubble-forwards this event from its inner ControlCore via
                    // BUBBLED_FORWARDED_TYPED_EVENT, which passes the original
                    // sender through unchanged. So `sender` is the ControlCore
                    // and `try_as<TermControl>()` always returns null, which
                    // would leave stateStr permanently "unknown" and the switch
                    // dead code. Read state from the captured weakTerm instead.
                    auto control = weakTerm.get();
                    if (!control)
                        return;

                    std::string stateStr;
                    switch (control.ConnectionState())
                    {
                    case ConnectionState::Connected:
                        stateStr = "connected";
                        break;
                    case ConnectionState::Closed:
                        stateStr = "closed";
                        break;
                    case ConnectionState::Failed:
                        stateStr = "failed";
                        break;
                    default:
                        return;
                    }

                    // Resolve the pane id SYNCHRONOUSLY here, while the
                    // control is still alive. If we deferred it into the
                    // dispatched lambda below, a `Closed` event raised as
                    // part of tab teardown would race with the
                    // TermControl/Connection destructors: by the time the
                    // UI thread runs the dispatched continuation,
                    // `weakTerm.get()` returns null and the event is
                    // dropped silently — leaving wta's session-list row
                    // stuck at Idle after the user closed the tab.
                    // `_FindSessionIdForControl` only reads
                    // `control.Connection().SessionId()`, no `_tabs`
                    // access, so it is safe off the UI thread.
                    const auto paneIdStr = strongThis->_FindSessionIdForControl(control);
                    if (paneIdStr.empty())
                        return;

                    // Dispatch only the actual event raise (and the
                    // `tab_id` lookup, which DOES touch `_tabs`) to the
                    // UI thread. The captured `paneIdStr` is a plain
                    // std::string and survives the term's destruction;
                    // `tab_id` falls back to empty when the term is
                    // already gone, but the event still fires with the
                    // pane_id so wta's PaneClosed prune can run.
                    strongThis->Dispatcher().RunAsync(
                        winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
                        [weakThis, weakTerm, paneIdStr, stateStr]() {
                            auto page = weakThis.get();
                            if (!page)
                                return;

                            // connection_state is pane-lifecycle plumbing that
                            // wta needs regardless of AutoFix being enabled —
                            // it drives session-list demotion (PaneClosed)
                            // when an agent CLI exits and the pane is closed.
                            // Volume is low (a handful of events per pane
                            // lifecycle), so always forward.
                            // `_FindTabIdForControl` walks `_tabs`, so it
                            // must run on the UI thread AND have a live
                            // term to compare panes against. If the term
                            // already died (close-time race), fall back
                            // to no tab_id — autofix routing may be
                            // imperfect for this one event, but the
                            // pane_id alone is sufficient for the
                            // session-list / PaneClosed prune path.
                            auto term2 = weakTerm.get();
                            const auto tabIdStr = term2
                                ? page->_FindTabIdForControl(term2)
                                : std::string{};

                            Json::Value evt;
                            evt["type"] = "event";
                            evt["method"] = "connection_state";
                            Json::Value params;
                            params["pane_id"] = paneIdStr;
                            if (!tabIdStr.empty())
                            {
                                params["tab_id"] = tabIdStr;
                            }
                            params["state"] = stateStr;
                            evt["params"] = params;
                            Json::StreamWriterBuilder wb;
                            wb["indentation"] = "";
                            page->ProtocolVtSequenceReceived.raise(
                                *page,
                                winrt::to_hstring(Json::writeString(wb, evt)));
                        });
                });
        }

        // Don't even register for the event if the feature is compiled off.
        if constexpr (Feature_ShellCompletions::IsEnabled())
        {
            term.CompletionsChanged({ get_weak(), &TerminalPage::_ControlCompletionsChangedHandler });
        }
        winrt::weak_ref<TermControl> weakTerm{ term };
        term.ContextMenu().Opening([weak = get_weak(), weakTerm](auto&& sender, auto&& /*args*/) {
            if (const auto& page{ weak.get() })
            {
                page->_PopulateContextMenu(weakTerm.get(), sender.try_as<MUX::Controls::CommandBarFlyout>(), false);
            }
        });
        term.SelectionContextMenu().Opening([weak = get_weak(), weakTerm](auto&& sender, auto&& /*args*/) {
            if (const auto& page{ weak.get() })
            {
                page->_PopulateContextMenu(weakTerm.get(), sender.try_as<MUX::Controls::CommandBarFlyout>(), true);
            }
        });
        if constexpr (Feature_QuickFix::IsEnabled())
        {
            term.QuickFixMenu().Opening([weak = get_weak(), weakTerm](auto&& sender, auto&& /*args*/) {
                if (const auto& page{ weak.get() })
                {
                    page->_PopulateQuickFixMenu(weakTerm.get(), sender.try_as<Controls::MenuFlyout>());
                }
            });
        }
    }

    // Method Description:
    // - Connects event handlers to the Tab for events that we want to
    //   handle. This includes:
    //    * the TitleChanged event, for changing the text of the tab
    //    * the Color{Selected,Cleared} events to change the color of a tab.
    // Arguments:
    // - hostingTab: The Tab that's hosting this TermControl instance
    void TerminalPage::_RegisterTabEvents(Tab& hostingTab)
    {
        auto weakTab{ hostingTab.get_weak() };
        auto weakThis{ get_weak() };
        // PropertyChanged is the generic mechanism by which the Tab
        // communicates changes to any of its observable properties, including
        // the Title
        hostingTab.PropertyChanged([weakTab, weakThis](auto&&, const WUX::Data::PropertyChangedEventArgs& args) {
            auto page{ weakThis.get() };
            auto tab{ weakTab.get() };
            if (page && tab)
            {
                const auto propertyName = args.PropertyName();
                if (propertyName == L"Title")
                {
                    page->_UpdateTitle(*tab);
                }
                else if (propertyName == L"Content")
                {
                    if (*tab == page->_GetFocusedTab())
                    {
                        const auto children = page->_tabContent.Children();

                        children.Clear();
                        if (auto content = tab->Content())
                        {
                            page->_tabContent.Children().Append(std::move(content));
                        }

                        tab->Focus(FocusState::Programmatic);
                    }
                }
            }
        });

        // Add an event handler for when the terminal or tab wants to set a
        // progress indicator on the taskbar
        hostingTab.TaskbarProgressChanged({ get_weak(), &TerminalPage::_SetTaskbarProgressHandler });

        hostingTab.RestartTerminalRequested({ get_weak(), &TerminalPage::_restartPaneConnection });
    }

    // Method Description:
    // - Helper to manually exit "zoom" when certain actions take place.
    //   Anything that modifies the state of the pane tree should probably
    //   un-zoom the focused pane first, so that the user can see the full pane
    //   tree again. These actions include:
    //   * Splitting a new pane
    //   * Closing a pane
    //   * Moving focus between panes
    //   * Resizing a pane
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::_UnZoomIfNeeded()
    {
        if (const auto activeTab{ _GetFocusedTabImpl() })
        {
            if (activeTab->IsZoomed())
            {
                // Remove the content from the tab first, so Pane::UnZoom can
                // re-attach the content to the tree w/in the pane
                _tabContent.Children().Clear();
                // In ExitZoom, we'll change the Tab's Content(), triggering the
                // content changed event, which will re-attach the tab's new content
                // root to the tree.
                activeTab->ExitZoom();
            }
        }
    }

    // Method Description:
    // - Attempt to move focus between panes, as to focus the child on
    //   the other side of the separator. See Pane::NavigateFocus for details.
    // - Moves the focus of the currently focused tab.
    // Arguments:
    // - direction: The direction to move the focus in.
    // Return Value:
    // - Whether changing the focus succeeded. This allows a keychord to propagate
    //   to the terminal when no other panes are present (GH#6219)
    bool TerminalPage::_MoveFocus(const FocusDirection& direction)
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            return tabImpl->NavigateFocus(direction);
        }
        return false;
    }

    // Method Description:
    // - Attempt to swap the positions of the focused pane with another pane.
    //   See Pane::SwapPane for details.
    // Arguments:
    // - direction: The direction to move the focused pane in.
    // Return Value:
    // - true if panes were swapped.
    bool TerminalPage::_SwapPane(const FocusDirection& direction)
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            _UnZoomIfNeeded();
            return tabImpl->SwapPane(direction);
        }
        return false;
    }

    TermControl TerminalPage::_GetActiveControl() const
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            return tabImpl->GetActiveTerminalControl();
        }
        return nullptr;
    }

    CommandPalette TerminalPage::LoadCommandPalette()
    {
        if (const auto p = CommandPaletteElement())
        {
            return p;
        }

        return _loadCommandPaletteSlowPath();
    }
    bool TerminalPage::_commandPaletteIs(WUX::Visibility visibility)
    {
        const auto p = CommandPaletteElement();
        return p && p.Visibility() == visibility;
    }

    CommandPalette TerminalPage::_loadCommandPaletteSlowPath()
    {
        const auto p = FindName(L"CommandPaletteElement").as<CommandPalette>();

        p.SetActionMap(_settings.ActionMap());

        // When the visibility of the command palette changes to "collapsed",
        // the palette has been closed. Toss focus back to the currently active control.
        p.RegisterPropertyChangedCallback(UIElement::VisibilityProperty(), [this](auto&&, auto&&) {
            if (_commandPaletteIs(Visibility::Collapsed))
            {
                _FocusActiveControl(nullptr, nullptr);
            }
        });
        p.DispatchCommandRequested({ this, &TerminalPage::_OnDispatchCommandRequested });
        p.CommandLineExecutionRequested({ this, &TerminalPage::_OnCommandLineExecutionRequested });
        p.SwitchToTabRequested({ this, &TerminalPage::_OnSwitchToTabRequested });
        p.PreviewAction({ this, &TerminalPage::_PreviewActionHandler });
        p.AgentForegroundPromptRequested({ this, &TerminalPage::_OnAgentForegroundPromptRequested });
        p.AgentBackgroundTaskRequested({ this, &TerminalPage::_OnAgentBackgroundTaskRequested });

        return p;
    }

    SuggestionsControl TerminalPage::LoadSuggestionsUI()
    {
        if (const auto p = SuggestionsElement())
        {
            return p;
        }

        return _loadSuggestionsElementSlowPath();
    }
    bool TerminalPage::_suggestionsControlIs(WUX::Visibility visibility)
    {
        const auto p = SuggestionsElement();
        return p && p.Visibility() == visibility;
    }

    SuggestionsControl TerminalPage::_loadSuggestionsElementSlowPath()
    {
        const auto p = FindName(L"SuggestionsElement").as<SuggestionsControl>();

        p.RegisterPropertyChangedCallback(UIElement::VisibilityProperty(), [this](auto&&, auto&&) {
            if (SuggestionsElement().Visibility() == Visibility::Collapsed)
            {
                _FocusActiveControl(nullptr, nullptr);
            }
        });
        p.DispatchCommandRequested({ this, &TerminalPage::_OnDispatchCommandRequested });
        p.PreviewAction({ this, &TerminalPage::_PreviewActionHandler });

        return p;
    }

    // Method Description:
    // - Warn the user that they are about to close all open windows, then
    //   signal that we want to close everything.
    safe_void_coroutine TerminalPage::RequestQuit()
    {
        const auto setting = _settings.GlobalSettings().ConfirmOnClose();
        if (setting != ConfirmOnClose::Never && !_displayingCloseDialog)
        {
            _displayingCloseDialog = true;

            const auto weak = get_weak();
            auto warningResult = co_await _ShowConfirmCloseDialog(ConfirmCloseDialogKind::CloseAll);
            const auto strong = weak.get();
            if (!strong)
            {
                co_return;
            }

            _displayingCloseDialog = false;

            if (warningResult != ContentDialogResult::Primary)
            {
                co_return;
            }
        }

        QuitRequested.raise(nullptr, nullptr);
    }

    void TerminalPage::PersistState()
    {
        // This method may be called for a window even if it hasn't had a tab yet or lost all of them.
        // We shouldn't persist such windows.
        const auto tabCount = _tabs.Size();
        if (_startupState != StartupState::Initialized || tabCount == 0)
        {
            return;
        }

        std::vector<ActionAndArgs> actions;

        for (auto tab : _tabs)
        {
            auto t = winrt::get_self<implementation::Tab>(tab);
            auto tabActions = t->BuildStartupActions(BuildStartupKind::Persist);
            actions.insert(actions.end(), std::make_move_iterator(tabActions.begin()), std::make_move_iterator(tabActions.end()));
        }

        // Avoid persisting a window with zero tabs, because `BuildStartupActions` happened to return an empty vector.
        if (actions.empty())
        {
            return;
        }

        // if the focused tab was not the last tab, restore that
        auto idx = _GetFocusedTabIndex();
        if (idx && idx != tabCount - 1)
        {
            ActionAndArgs action;
            action.Action(ShortcutAction::SwitchToTab);
            SwitchToTabArgs switchToTabArgs{ idx.value() };
            action.Args(switchToTabArgs);

            actions.emplace_back(std::move(action));
        }

        // If the user set a custom name, save it
        if (const auto& windowName{ _WindowProperties.WindowName() }; !windowName.empty())
        {
            ActionAndArgs action;
            action.Action(ShortcutAction::RenameWindow);
            RenameWindowArgs args{ windowName };
            action.Args(args);

            actions.emplace_back(std::move(action));
        }

        WindowLayout layout;
        layout.TabLayout(winrt::single_threaded_vector<ActionAndArgs>(std::move(actions)));

        auto mode = LaunchMode::DefaultMode;
        WI_SetFlagIf(mode, LaunchMode::FullscreenMode, _isFullscreen);
        WI_SetFlagIf(mode, LaunchMode::FocusMode, _isInFocusMode);
        WI_SetFlagIf(mode, LaunchMode::MaximizedMode, _isMaximized);

        layout.LaunchMode({ mode });

        // Only save the content size because the tab size will be added on load.
        const auto contentWidth = static_cast<float>(_tabContent.ActualWidth());
        const auto contentHeight = static_cast<float>(_tabContent.ActualHeight());
        const winrt::Windows::Foundation::Size windowSize{ contentWidth, contentHeight };

        layout.InitialSize(windowSize);

        // We don't actually know our own position. So we have to ask the window
        // layer for that.
        const auto launchPosRequest{ winrt::make<LaunchPositionRequest>() };
        RequestLaunchPosition.raise(*this, launchPosRequest);
        layout.InitialPosition(launchPosRequest.Position());

        ApplicationState::SharedInstance().AppendPersistedWindowLayout(layout);
    }

    // Method Description:
    // - Determines whether a close-window action should show a confirmation
    //   dialog, based on the confirmOnClose setting and the current window state.
    // Arguments:
    // - <none>
    // Return Value:
    // - true, if a warning dialog should be shown before closing the window
    bool TerminalPage::_ShouldWarnOnClose() const
    {
        const auto setting = _settings.GlobalSettings().ConfirmOnClose();
        switch (setting)
        {
        case ConfirmOnClose::Always:
            return true;
        case ConfirmOnClose::Automatic:
        {
            // Warn if there's more than one tab, or the one tab has more than one pane.
            return _HasMultipleTabs() || _GetTabImpl(_tabs.GetAt(0))->GetLeafPaneCount() > 1;
        }
        case ConfirmOnClose::Never:
        default:
            return false;
        }
    }

    // Method Description:
    // - Determines whether closing a specific tab should show a confirmation
    //   dialog, based on the confirmOnClose setting and the tab's state.
    // Arguments:
    // - tab: The tab being closed
    // Return Value:
    // - true, if a warning dialog should be shown before closing the tab
    bool TerminalPage::_ShouldWarnOnCloseTab(const winrt::com_ptr<Tab>& tab) const
    {
        const auto setting = _settings.GlobalSettings().ConfirmOnClose();
        switch (setting)
        {
        case ConfirmOnClose::Always:
            return true;
        case ConfirmOnClose::Automatic:
            // Warn if this tab has more than one pane.
            return tab->GetLeafPaneCount() > 1;
        case ConfirmOnClose::Never:
        default:
            return false;
        }
    }

    // Method Description:
    // - Close the terminal app. If the confirmOnClose setting indicates we should
    //   warn for the current window state, show a warning dialog.
    safe_void_coroutine TerminalPage::CloseWindow()
    {
        if (_ShouldWarnOnClose() &&
            !_displayingCloseDialog)
        {
            if (_newTabButton && _newTabButton.Flyout())
            {
                _newTabButton.Flyout().Hide();
            }
            _DismissTabContextMenus();
            _displayingCloseDialog = true;

            const auto weak = get_weak();
            auto warningResult = co_await _ShowConfirmCloseDialog(ConfirmCloseDialogKind::Window);
            // Hold a strong reference to `this` after the co_await; we may
            // be the last holder if the window was already being torn down.
            auto strong = weak.get();
            if (!strong)
            {
                co_return;
            }

            _displayingCloseDialog = false;

            if (warningResult != ContentDialogResult::Primary)
            {
                co_return;
            }
        }

        CloseWindowRequested.raise(*this, nullptr);
    }

    std::vector<IPaneContent> TerminalPage::Panes() const
    {
        std::vector<IPaneContent> panes;

        for (const auto tab : _tabs)
        {
            const auto impl = _GetTabImpl(tab);
            if (!impl)
            {
                continue;
            }

            impl->GetRootPane()->WalkTree([&](auto&& pane) {
                if (auto content = pane->GetContent())
                {
                    panes.push_back(std::move(content));
                }
            });
        }

        return panes;
    }

    // Method Description:
    // - Move the viewport of the terminal of the currently focused tab up or
    //      down a number of lines.
    // Arguments:
    // - scrollDirection: ScrollUp will move the viewport up, ScrollDown will move the viewport down
    // - rowsToScroll: a number of lines to move the viewport. If not provided we will use a system default.
    void TerminalPage::_Scroll(ScrollDirection scrollDirection, const Windows::Foundation::IReference<uint32_t>& rowsToScroll)
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            uint32_t realRowsToScroll;
            if (rowsToScroll == nullptr)
            {
                // The magic value of WHEEL_PAGESCROLL indicates that we need to scroll the entire page
                realRowsToScroll = _systemRowsToScroll == WHEEL_PAGESCROLL ?
                                       tabImpl->GetActiveTerminalControl().ViewHeight() :
                                       _systemRowsToScroll;
            }
            else
            {
                // use the custom value specified in the command
                realRowsToScroll = rowsToScroll.Value();
            }
            auto scrollDelta = _ComputeScrollDelta(scrollDirection, realRowsToScroll);
            tabImpl->Scroll(scrollDelta);
        }
    }

    // Method Description:
    // - Moves the currently active pane on the currently active tab to the
    //   specified tab. If the tab index is greater than the number of
    //   tabs, then a new tab will be created for the pane. Similarly, if a pane
    //   is the last remaining pane on a tab, that tab will be closed upon moving.
    // - No move will occur if the tabIdx is the same as the current tab, or if
    //   the specified tab is not a host of terminals (such as the settings tab).
    // - If the Window is specified, the pane will instead be detached and moved
    //   to the window with the given name/id.
    // Return Value:
    // - true if the pane was successfully moved to the new tab.
    bool TerminalPage::_MovePane(MovePaneArgs args)
    {
        const auto tabIdx{ args.TabIndex() };
        const auto windowId{ args.Window() };

        auto focusedTab{ _GetFocusedTabImpl() };

        if (!focusedTab)
        {
            return false;
        }

        // If there was a windowId in the action, try to move it to the
        // specified window instead of moving it in our tab row.
        if (!windowId.empty())
        {
            if (const auto tabImpl{ _GetFocusedTabImpl() })
            {
                if (const auto pane{ tabImpl->GetActivePane() })
                {
                    auto startupActions = pane->BuildStartupActions(0, 1, BuildStartupKind::MovePane);
                    _DetachPaneFromWindow(pane);
                    _MoveContent(std::move(startupActions.args), windowId, tabIdx);
                    focusedTab->DetachPane();

                    if (auto autoPeer = Automation::Peers::FrameworkElementAutomationPeer::FromElement(*this))
                    {
                        if (windowId == L"new")
                        {
                            autoPeer.RaiseNotificationEvent(Automation::Peers::AutomationNotificationKind::ActionCompleted,
                                                            Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                                            RS_(L"TerminalPage_PaneMovedAnnouncement_NewWindow"),
                                                            L"TerminalPageMovePaneToNewWindow" /* unique name for this notification category */);
                        }
                        else
                        {
                            autoPeer.RaiseNotificationEvent(Automation::Peers::AutomationNotificationKind::ActionCompleted,
                                                            Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                                            RS_fmt(L"TerminalPage_PaneMovedAnnouncement_ExistingWindow2", windowId),
                                                            L"TerminalPageMovePaneToExistingWindow" /* unique name for this notification category */);
                        }
                    }
                    return true;
                }
            }
        }

        // If we are trying to move from the current tab to the current tab do nothing.
        if (_GetFocusedTabIndex() == tabIdx)
        {
            return false;
        }

        // Moving the pane from the current tab might close it, so get the next
        // tab before its index changes.
        if (tabIdx < _tabs.Size())
        {
            auto targetTab = _GetTabImpl(_tabs.GetAt(tabIdx));
            // if the selected tab is not a host of terminals (e.g. settings)
            // don't attempt to add a pane to it.
            if (!targetTab)
            {
                return false;
            }
            auto pane = focusedTab->DetachPane();
            targetTab->AttachPane(pane);
            _SetFocusedTab(*targetTab);

            if (auto autoPeer = Automation::Peers::FrameworkElementAutomationPeer::FromElement(*this))
            {
                const auto tabTitle = targetTab->Title();
                autoPeer.RaiseNotificationEvent(Automation::Peers::AutomationNotificationKind::ActionCompleted,
                                                Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                                RS_fmt(L"TerminalPage_PaneMovedAnnouncement_ExistingTab", tabTitle),
                                                L"TerminalPageMovePaneToExistingTab" /* unique name for this notification category */);
            }
        }
        else
        {
            auto pane = focusedTab->DetachPane();
            _CreateNewTabFromPane(pane);
            if (auto autoPeer = Automation::Peers::FrameworkElementAutomationPeer::FromElement(*this))
            {
                autoPeer.RaiseNotificationEvent(Automation::Peers::AutomationNotificationKind::ActionCompleted,
                                                Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                                RS_(L"TerminalPage_PaneMovedAnnouncement_NewTab"),
                                                L"TerminalPageMovePaneToNewTab" /* unique name for this notification category */);
            }
        }

        return true;
    }

    // Detach a tree of panes from this terminal. Helper used for moving panes
    // and tabs to other windows.
    void TerminalPage::_DetachPaneFromWindow(std::shared_ptr<Pane> pane)
    {
        pane->WalkTree([&](auto p) {
            if (const auto& control{ p->GetTerminalControl() })
            {
                _manager.Detach(control);
            }
        });
    }

    void TerminalPage::_DetachTabFromWindow(const winrt::com_ptr<Tab>& tab)
    {
        // Detach the root pane, which will act like the whole tab got detached.
        if (const auto rootPane = tab->GetRootPane())
        {
            _DetachPaneFromWindow(rootPane);
        }
    }

    // Method Description:
    // - Serialize these actions to json, and raise them as a RequestMoveContent
    //   event. Our Window will raise that to the window manager / monarch, who
    //   will dispatch this blob of json back to the window that should handle
    //   this.
    // - `actions` will be emptied into a winrt IVector as a part of this method
    //   and should be expected to be empty after this call.
    void TerminalPage::_MoveContent(std::vector<Settings::Model::ActionAndArgs>&& actions,
                                    const winrt::hstring& windowName,
                                    const uint32_t tabIndex,
                                    const std::optional<winrt::Windows::Foundation::Point>& dragPoint)
    {
        const auto winRtActions{ winrt::single_threaded_vector<ActionAndArgs>(std::move(actions)) };
        const auto str{ ActionAndArgs::Serialize(winRtActions) };
        const auto request = winrt::make_self<RequestMoveContentArgs>(windowName,
                                                                      str,
                                                                      tabIndex);
        if (dragPoint.has_value())
        {
            request->WindowPosition(*dragPoint);
        }
        RequestMoveContent.raise(*this, *request);
    }

    bool TerminalPage::_MoveTab(winrt::com_ptr<Tab> tab, MoveTabArgs args)
    {
        if (!tab)
        {
            return false;
        }

        // If there was a windowId in the action, try to move it to the
        // specified window instead of moving it in our tab row.
        const auto windowId{ args.Window() };
        if (!windowId.empty())
        {
            // if the windowId is the same as our name, do nothing
            if (windowId == WindowProperties().WindowName() ||
                windowId == winrt::to_hstring(WindowProperties().WindowId()))
            {
                return true;
            }

            if (tab)
            {
                auto startupActions = tab->BuildStartupActions(BuildStartupKind::Content);
                _DetachTabFromWindow(tab);
                _MoveContent(std::move(startupActions), windowId, 0);
                _RemoveTab(*tab, /*movingAway*/ true);
                if (auto autoPeer = Automation::Peers::FrameworkElementAutomationPeer::FromElement(*this))
                {
                    const auto tabTitle = tab->Title();
                    if (windowId == L"new")
                    {
                        autoPeer.RaiseNotificationEvent(Automation::Peers::AutomationNotificationKind::ActionCompleted,
                                                        Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                                        RS_fmt(L"TerminalPage_TabMovedAnnouncement_NewWindow", tabTitle),
                                                        L"TerminalPageMoveTabToNewWindow" /* unique name for this notification category */);
                    }
                    else
                    {
                        autoPeer.RaiseNotificationEvent(Automation::Peers::AutomationNotificationKind::ActionCompleted,
                                                        Automation::Peers::AutomationNotificationProcessing::ImportantMostRecent,
                                                        RS_fmt(L"TerminalPage_TabMovedAnnouncement_Default", tabTitle, windowId),
                                                        L"TerminalPageMoveTabToExistingWindow" /* unique name for this notification category */);
                    }
                }
                return true;
            }
        }

        const auto direction = args.Direction();
        if (direction != MoveTabDirection::None)
        {
            // Use the requested tab, if provided. Otherwise, use the currently
            // focused tab.
            const auto tabIndex = til::coalesce(_GetTabIndex(*tab),
                                                _GetFocusedTabIndex());
            if (tabIndex)
            {
                const auto currentTabIndex = tabIndex.value();
                const auto delta = direction == MoveTabDirection::Forward ? 1 : -1;
                _TryMoveTab(currentTabIndex, currentTabIndex + delta);
            }
        }

        return true;
    }

    // When the tab's active pane changes, we'll want to lookup a new icon
    // for it. The Title change will be propagated upwards through the tab's
    // PropertyChanged event handler.
    void TerminalPage::_activePaneChanged(winrt::TerminalApp::Tab sender,
                                          Windows::Foundation::IInspectable /*args*/)
    {
        if (const auto tab{ _GetTabImpl(sender) })
        {
            // Possibly update the icon of the tab.
            _UpdateTabIcon(*tab);

            _updateThemeColors();

            // Update the taskbar progress as well. We'll raise our own
            // SetTaskbarProgress event here, to get tell the hosting
            // application to re-query this value from us.
            SetTaskbarProgress.raise(*this, nullptr);

            auto profile = tab->GetFocusedProfile();
            _UpdateBackground(profile);
        }

        _adjustProcessPriorityThrottled->Run();
    }

    uint32_t TerminalPage::NumberOfTabs() const
    {
        return _tabs.Size();
    }

    // Method Description:
    // - Called when it is determined that an existing tab or pane should be
    //   attached to our window. content represents a blob of JSON describing
    //   some startup actions for rebuilding the specified panes. They will
    //   include `__content` properties with the GUID of the existing
    //   ControlInteractivity's we should use, rather than starting new ones.
    // - _MakePane is already enlightened to use the ContentId property to
    //   reattach instead of create new content, so this method simply needs to
    //   parse the JSON and pump it into our action handler. Almost the same as
    //   doing something like `wt -w 0 nt`.
    void TerminalPage::AttachContent(IVector<Settings::Model::ActionAndArgs> args, uint32_t tabIndex)
    {
        if (args == nullptr ||
            args.Size() == 0)
        {
            return;
        }

        const auto& firstAction = args.GetAt(0);
        const bool firstIsSplitPane{ firstAction.Action() == ShortcutAction::SplitPane };

        // `splitPane` allows the user to specify which tab to split. In that
        // case, split specifically the requested pane.
        //
        // If there's not enough tabs, then just turn this pane into a new tab.
        //
        // If the first action is `newTab`, the index is always going to be 0,
        // so don't do anything in that case.
        if (firstIsSplitPane && tabIndex < _tabs.Size())
        {
            _SelectTab(tabIndex);
        }

        for (const auto& action : args)
        {
            _actionDispatch->DoAction(action);
        }

        // After handling all the actions, then re-check the tabIndex. We might
        // have been called as a part of a tab drag/drop. In that case, the
        // tabIndex is actually relevant, and we need to move the tab we just
        // made into position.
        if (!firstIsSplitPane && tabIndex != -1)
        {
            // Move the currently active tab to the requested index Use the
            // currently focused tab index, because we don't know if the new tab
            // opened at the end of the list, or adjacent to the previously
            // active tab. This is affected by the user's "newTabPosition"
            // setting.
            if (const auto focusedTabIndex = _GetFocusedTabIndex())
            {
                const auto source = *focusedTabIndex;
                _TryMoveTab(source, tabIndex);
            }
            // else: This shouldn't really be possible, because the tab we _just_ opened should be active.
        }
    }

    // Method Description:
    // - Split the focused pane of the given tab, either horizontally or vertically, and place the
    //   given pane accordingly
    // Arguments:
    // - tab: The tab that is going to be split.
    // - newPane: the pane to add to our tree of panes
    // - splitDirection: one value from the TerminalApp::SplitDirection enum, indicating how the
    //   new pane should be split from its parent.
    // - splitSize: the size of the split
    void TerminalPage::_SplitPane(const winrt::com_ptr<Tab>& tab,
                                  const SplitDirection splitDirection,
                                  const float splitSize,
                                  std::shared_ptr<Pane> newPane,
                                  bool focusNewPane)
    {
        auto activeTab = tab;
        // Clever hack for a crash in startup, with multiple sub-commands. Say
        // you have the following commandline:
        //
        //   wtd nt -p "elevated cmd" ; sp -p "elevated cmd" ; sp -p "Command Prompt"
        //
        // Where "elevated cmd" is an elevated profile.
        //
        // In that scenario, we won't dump off the commandline immediately to an
        // elevated window, because it's got the final unelevated split in it.
        // However, when we get to that command, there won't be a tab yet. So
        // we'd crash right about here.
        //
        // Instead, let's just promote this first split to be a tab instead.
        // Crash avoided, and we don't need to worry about inserting a new-tab
        // command in at the start.
        if (!tab)
        {
            if (_tabs.Size() == 0)
            {
                _CreateNewTabFromPane(newPane);
                return;
            }
            else
            {
                activeTab = _GetFocusedTabImpl();
            }
        }

        // For now, prevent splitting the _settingsTab. We can always revisit this later.
        if (*activeTab == _settingsTab)
        {
            return;
        }

        // Agent panes are fixed panels and cannot be split.
        if (const auto activePane = activeTab->GetActivePane())
        {
            if (activePane->IsAgentPane())
            {
                return;
            }
        }

        // If the caller is calling us with the return value of _MakePane
        // directly, it's possible that nullptr was returned, if the connections
        // was supposed to be launched in an elevated window. In that case, do
        // nothing here. We don't have a pane with which to create the split.
        if (!newPane)
        {
            return;
        }
        const auto contentWidth = static_cast<float>(_tabContent.ActualWidth());
        const auto contentHeight = static_cast<float>(_tabContent.ActualHeight());
        const winrt::Windows::Foundation::Size availableSpace{ contentWidth, contentHeight };

        const auto realSplitType = activeTab->PreCalculateCanSplit(splitDirection, splitSize, availableSpace);
        if (!realSplitType)
        {
            return;
        }

        _UnZoomIfNeeded();
        auto [original, newGuy] = activeTab->SplitPane(*realSplitType, splitSize, newPane);

        // After GH#6586, the control will no longer focus itself
        // automatically when it's finished being laid out. Manually focus
        // the control here instead.
        if (focusNewPane && _startupState == StartupState::Initialized)
        {
            if (const auto& content{ newGuy->GetContent() })
            {
                content.Focus(FocusState::Programmatic);
            }
        }
    }

    // Method Description:
    // - Switches the split orientation of the currently focused pane.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::_ToggleSplitOrientation()
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            _UnZoomIfNeeded();
            tabImpl->ToggleSplitOrientation();
        }
    }

    // Method Description:
    // - Attempt to move a separator between panes, as to resize each child on
    //   either size of the separator. See Pane::ResizePane for details.
    // - Moves a separator on the currently focused tab.
    // Arguments:
    // - direction: The direction to move the separator in.
    // Return Value:
    // - whether a pane was resized
    bool TerminalPage::_ResizePane(const ResizeDirection& direction)
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            _UnZoomIfNeeded();
            return tabImpl->ResizePane(direction);
        }
        return false;
    }

    // Method Description:
    // - Move the viewport of the terminal of the currently focused tab up or
    //      down a page. The page length will be dependent on the terminal view height.
    // Arguments:
    // - scrollDirection: ScrollUp will move the viewport up, ScrollDown will move the viewport down
    void TerminalPage::_ScrollPage(ScrollDirection scrollDirection)
    {
        // Do nothing if for some reason, there's no terminal tab in focus. We don't want to crash.
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            if (const auto& control{ _GetActiveControl() })
            {
                const auto termHeight = control.ViewHeight();
                auto scrollDelta = _ComputeScrollDelta(scrollDirection, termHeight);
                tabImpl->Scroll(scrollDelta);
            }
        }
    }

    void TerminalPage::_ScrollToBufferEdge(ScrollDirection scrollDirection)
    {
        if (const auto tabImpl{ _GetFocusedTabImpl() })
        {
            auto scrollDelta = _ComputeScrollDelta(scrollDirection, INT_MAX);
            tabImpl->Scroll(scrollDelta);
        }
    }

    // Method Description:
    // - Gets the title of the currently focused terminal control. If there
    //   isn't a control selected for any reason, returns "Terminal"
    // Arguments:
    // - <none>
    // Return Value:
    // - the title of the focused control if there is one, else "Terminal"
    hstring TerminalPage::Title()
    {
        if (_settings.GlobalSettings().ShowTitleInTitlebar())
        {
            if (const auto tab{ _GetFocusedTab() })
            {
                return tab.Title();
            }
        }
        return { L"Terminal" };
    }

    // Method Description:
    // - Handles the special case of providing a text override for the UI shortcut due to VK_OEM issue.
    //      Looks at the flags from the KeyChord modifiers and provides a concatenated string value of all
    //      in the same order that XAML would put them as well.
    // Return Value:
    // - a string representation of the key modifiers for the shortcut
    //NOTE: This needs to be localized with https://github.com/microsoft/terminal/issues/794 if XAML framework issue not resolved before then
    static std::wstring _FormatOverrideShortcutText(VirtualKeyModifiers modifiers)
    {
        std::wstring buffer{ L"" };

        if (WI_IsFlagSet(modifiers, VirtualKeyModifiers::Control))
        {
            buffer += L"Ctrl+";
        }

        if (WI_IsFlagSet(modifiers, VirtualKeyModifiers::Shift))
        {
            buffer += L"Shift+";
        }

        if (WI_IsFlagSet(modifiers, VirtualKeyModifiers::Menu))
        {
            buffer += L"Alt+";
        }

        if (WI_IsFlagSet(modifiers, VirtualKeyModifiers::Windows))
        {
            buffer += L"Win+";
        }

        return buffer;
    }

    // Method Description:
    // - Takes a MenuFlyoutItem and a corresponding KeyChord value and creates the accelerator for UI display.
    //   Takes into account a special case for an error condition for a comma
    // Arguments:
    // - MenuFlyoutItem that will be displayed, and a KeyChord to map an accelerator
    void TerminalPage::_SetAcceleratorForMenuItem(WUX::Controls::MenuFlyoutItem& menuItem,
                                                  const KeyChord& keyChord)
    {
#ifdef DEP_MICROSOFT_UI_XAML_708_FIXED
        // work around https://github.com/microsoft/microsoft-ui-xaml/issues/708 in case of VK_OEM_COMMA
        if (keyChord.Vkey() != VK_OEM_COMMA)
        {
            // use the XAML shortcut to give us the automatic capabilities
            auto menuShortcut = Windows::UI::Xaml::Input::KeyboardAccelerator{};

            // TODO: Modify this when https://github.com/microsoft/terminal/issues/877 is resolved
            menuShortcut.Key(static_cast<Windows::System::VirtualKey>(keyChord.Vkey()));

            // add the modifiers to the shortcut
            menuShortcut.Modifiers(keyChord.Modifiers());

            // add to the menu
            menuItem.KeyboardAccelerators().Append(menuShortcut);
        }
        else // we've got a comma, so need to just use the alternate method
#endif
        {
            // extract the modifier and key to a nice format
            auto overrideString = _FormatOverrideShortcutText(keyChord.Modifiers());
            auto mappedCh = MapVirtualKeyW(keyChord.Vkey(), MAPVK_VK_TO_CHAR);
            if (mappedCh != 0)
            {
                menuItem.KeyboardAcceleratorTextOverride(overrideString + gsl::narrow_cast<wchar_t>(mappedCh));
            }
        }
    }

    // Method Description:
    // - Calculates the appropriate size to snap to in the given direction, for
    //   the given dimension. If the global setting `snapToGridOnResize` is set
    //   to `false`, this will just immediately return the provided dimension,
    //   effectively disabling snapping.
    // - See Pane::CalcSnappedDimension
    float TerminalPage::CalcSnappedDimension(const bool widthOrHeight, const float dimension) const
    {
        if (_settings && _settings.GlobalSettings().SnapToGridOnResize())
        {
            if (const auto tabImpl{ _GetFocusedTabImpl() })
            {
                return tabImpl->CalcSnappedDimension(widthOrHeight, dimension);
            }
        }
        return dimension;
    }

    // Function Description:
    // - This function is called when the `TermControl` requests that we send
    //   it the clipboard's content.
    // - Retrieves the data from the Windows Clipboard and converts it to text.
    // - Shows warnings if the clipboard is too big or contains multiple lines
    //   of text.
    // - Sends the text back to the TermControl through the event's
    //   `HandleClipboardData` member function.
    // - Does some of this in a background thread, as to not hang/crash the UI thread.
    // Arguments:
    // - eventArgs: the PasteFromClipboard event sent from the TermControl
    safe_void_coroutine TerminalPage::_PasteFromClipboardHandler(const IInspectable sender, const PasteFromClipboardEventArgs eventArgs)
    try
    {
        // The old Win32 clipboard API as used below is somewhere in the order of 300-1000x faster than
        // the WinRT one on average, depending on CPU load. Don't use the WinRT clipboard API if you can.
        const auto weakThis = get_weak();
        const auto dispatcher = Dispatcher();
        const auto globalSettings = _settings.GlobalSettings();
        const auto bracketedPaste = eventArgs.BracketedPasteEnabled();
        const auto sourceId = sender.try_as<ControlInteractivity>().Id();

        // GetClipboardData might block for up to 30s for delay-rendered contents.
        co_await winrt::resume_background();

        winrt::hstring text;
        if (const auto clipboard = clipboard::open(nullptr))
        {
            text = clipboard::read();
        }

        if (!bracketedPaste && globalSettings.TrimPaste())
        {
            text = winrt::hstring{ Utils::TrimPaste(text) };
        }

        // LOAD BEARING: Send an empty bracketed paste even if the clipboard was empty.
        // Bracketed Paste provides an application a way to know whether the
        // user pasted, even if there was no applicable content on it. This
        // behavior is observed in GNOME Terminal, among others.
        if (!bracketedPaste && text.empty())
        {
            co_return;
        }

        bool warnMultiLine = false;
        switch (globalSettings.WarnAboutMultiLinePaste())
        {
        case WarnAboutMultiLinePaste::Automatic:
            // NOTE that this is unsafe, because a shell that doesn't support bracketed paste
            // will allow an attacker to enable the mode, not realize that, and then accept
            // the paste as if it was a series of legitimate commands. See GH#13014.
            warnMultiLine = !bracketedPaste;
            break;
        case WarnAboutMultiLinePaste::Always:
            warnMultiLine = true;
            break;
        default:
            warnMultiLine = false;
            break;
        }

        if (warnMultiLine)
        {
            const std::wstring_view view{ text };
            warnMultiLine = view.find_first_of(L"\r\n") != std::wstring_view::npos;
        }

        constexpr std::size_t minimumSizeForWarning = 1024 * 5; // 5 KiB
        const auto warnLargeText = text.size() > minimumSizeForWarning && globalSettings.WarnAboutLargePaste();

        if (warnMultiLine || warnLargeText)
        {
            co_await wil::resume_foreground(dispatcher);

            if (const auto strongThis = weakThis.get())
            {
                // We have to initialize the dialog here to be able to change the text of the text block within it
                std::ignore = FindName(L"MultiLinePasteDialog");

                // WinUI absolutely cannot deal with large amounts of text (at least O(n), possibly O(n^2),
                // so we limit the string length here and add an ellipsis if necessary.
                auto clipboardText = text;
                if (clipboardText.size() > 1024)
                {
                    const std::wstring_view view{ text };
                    // Make sure we don't cut in the middle of a surrogate pair
                    const auto len = til::utf16_iterate_prev(view, 512);
                    clipboardText = til::hstring_format(FMT_COMPILE(L"{}\n…"), view.substr(0, len));
                }

                ClipboardText().Text(std::move(clipboardText));

                // The vertical offset on the scrollbar does not reset automatically, so reset it manually
                ClipboardContentScrollViewer().ScrollToVerticalOffset(0);

                auto warningResult = ContentDialogResult::Primary;
                if (warnMultiLine)
                {
                    warningResult = co_await _ShowMultiLinePasteWarningDialog();
                }
                else if (warnLargeText)
                {
                    warningResult = co_await _ShowLargePasteWarningDialog();
                }

                // Clear the clipboard text so it doesn't lie around in memory
                ClipboardText().Text({});

                if (warningResult != ContentDialogResult::Primary)
                {
                    // user rejected the paste
                    co_return;
                }
            }

            co_await winrt::resume_background();
        }

        // This will end up calling ConptyConnection::WriteInput which calls WriteFile which may block for
        // an indefinite amount of time. Avoid freezes and deadlocks by running this on a background thread.
        assert(!dispatcher.HasThreadAccess());
        eventArgs.HandleClipboardData(text);

        // GH#18821: If broadcast input is active, paste the same text into all other
        // panes on the tab. We do this here (rather than re-reading the
        // clipboard per-pane) so that only one paste warning is shown.
        co_await wil::resume_foreground(dispatcher);
        if (const auto strongThis = weakThis.get())
        {
            if (const auto& tab{ strongThis->_GetFocusedTabImpl() })
            {
                if (tab->TabStatus().IsInputBroadcastActive())
                {
                    tab->GetRootPane()->WalkTree([&](auto&& pane) {
                        if (const auto control = pane->GetTerminalControl())
                        {
                            if (control.ContentId() != sourceId && !control.ReadOnly())
                            {
                                control.RawWriteString(text);
                            }
                        }
                    });
                }
            }
        }
    }
    CATCH_LOG();

    safe_void_coroutine TerminalPage::_OpenHyperlinkHandler(const IInspectable /*sender*/, const Microsoft::Terminal::Control::OpenHyperlinkEventArgs eventArgs)
    {
        try
        {
            auto uriString{ eventArgs.Uri() };
            auto parsed = winrt::Windows::Foundation::Uri(uriString);
            if (_IsUriSupported(parsed))
            {
                bool shouldLaunch{ _IsUriConsideredSomewhatSafe(parsed) };

                if (!shouldLaunch)
                {
                    if (auto presenter{ _dialogPresenter.get() })
                    {
                        // FindName needs to be called first to actually load the xaml object
                        auto unopenedUriDialog = FindName(L"UriErrorDialog").try_as<WUX::Controls::ContentDialog>();

                        // Insert the reason and the URI
                        unopenedUriDialog.SecondaryButtonText(RS_(L"UnsafeUrlConfirmAllowAction"));
                        CouldNotOpenUriReason().Text(RS_(L"UnsafeUrlConfirmText"));
                        UnopenedUri().Text(uriString);

                        // Show the dialog
                        auto result = co_await presenter.ShowDialog(unopenedUriDialog);
                        shouldLaunch = result == ContentDialogResult::Secondary;
                    }
                }

                if (shouldLaunch)
                {
                    ShellExecuteW(nullptr, L"open", uriString.c_str(), nullptr, nullptr, SW_SHOWNORMAL);
                }
            }
            else
            {
                _ShowCouldNotOpenDialog(RS_(L"UnsupportedSchemeText"), uriString);
            }
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            _ShowCouldNotOpenDialog(RS_(L"InvalidUriText"), eventArgs.Uri());
        }
    }

    // Method Description:
    // - Opens up a dialog box explaining why we could not open a URI
    // Arguments:
    // - The reason (unsupported scheme, invalid uri, potentially more in the future)
    // - The uri
    void TerminalPage::_ShowCouldNotOpenDialog(winrt::hstring reason, winrt::hstring uri)
    {
        if (auto presenter{ _dialogPresenter.get() })
        {
            // FindName needs to be called first to actually load the xaml object
            auto unopenedUriDialog = FindName(L"UriErrorDialog").try_as<WUX::Controls::ContentDialog>();

            // Insert the reason and the URI
            unopenedUriDialog.SecondaryButtonText({});
            CouldNotOpenUriReason().Text(reason);
            UnopenedUri().Text(uri);

            // Show the dialog
            presenter.ShowDialog(unopenedUriDialog);
        }
    }

    // Method Description:
    // - Determines if the given URI is currently supported
    // Arguments:
    // - The parsed URI
    // Return value:
    // - True if we support it, false otherwise
    bool TerminalPage::_IsUriSupported(const winrt::Windows::Foundation::Uri& parsedUri)
    {
        if (parsedUri.SchemeName() == L"http" || parsedUri.SchemeName() == L"https")
        {
            return true;
        }
        if (parsedUri.SchemeName() == L"file")
        {
            const auto host = parsedUri.Host();
            // If no hostname was provided or if the hostname was "localhost", Host() will return an empty string
            // and we allow it
            if (host == L"")
            {
                return true;
            }

            // GH#10188: WSL paths are okay. We'll let those through.
            if (host == L"wsl$" || host == L"wsl.localhost")
            {
                return true;
            }

            // TODO: by the OSC 8 spec, if a hostname (other than localhost) is provided, we _should_ be
            // comparing that value against what is returned by GetComputerNameExW and making sure they match.
            // However, ShellExecute does not seem to be happy with file URIs of the form
            //          file://{hostname}/path/to/file.ext
            // and so while we could do the hostname matching, we do not know how to actually open the URI
            // if its given in that form. So for now we ignore all hostnames other than localhost
            return false;
        }

        // In this case, the app manually output a URI other than file:// or
        // http(s)://. We'll trust the user knows what they're doing when
        // clicking on those sorts of links.
        // See discussion in GH#7562 for more details.
        return true;
    }

    bool TerminalPage::_IsUriConsideredSomewhatSafe(const winrt::Windows::Foundation::Uri& parsedUri)
    {
        if (parsedUri.SchemeName() == L"http" || parsedUri.SchemeName() == L"https")
        {
            return true;
        }
        if (parsedUri.SchemeName() == L"file")
        {
            static const auto pathext{ wil::TryGetEnvironmentVariableW<std::wstring>(L"PATHEXT") };
            const auto filename = parsedUri.Path();
            for (const auto& e : til::split_iterator{ std::wstring_view{ pathext }, L';' })
            {
                if (til::ends_with_insensitive_ascii(filename, e))
                {
                    return false;
                }
            }

            return true;
        }

        return false;
    }

    // Important! Don't take this eventArgs by reference, we need to extend the
    // lifetime of it to the other side of the co_await!
    safe_void_coroutine TerminalPage::_ControlNoticeRaisedHandler(const IInspectable /*sender*/,
                                                                  const Microsoft::Terminal::Control::NoticeEventArgs eventArgs)
    {
        auto weakThis = get_weak();
        co_await wil::resume_foreground(Dispatcher());
        if (auto page = weakThis.get())
        {
            auto message = eventArgs.Message();

            winrt::hstring title;

            switch (eventArgs.Level())
            {
            case NoticeLevel::Debug:
                title = RS_(L"NoticeDebug"); //\xebe8
                break;
            case NoticeLevel::Info:
                title = RS_(L"NoticeInfo"); // \xe946
                break;
            case NoticeLevel::Warning:
                title = RS_(L"NoticeWarning"); //\xe7ba
                break;
            case NoticeLevel::Error:
                title = RS_(L"NoticeError"); //\xe783
                break;
            }

            page->_ShowControlNoticeDialog(title, message);
        }
    }

    void TerminalPage::_ShowControlNoticeDialog(const winrt::hstring& title, const winrt::hstring& message)
    {
        if (auto presenter{ _dialogPresenter.get() })
        {
            // FindName needs to be called first to actually load the xaml object
            auto controlNoticeDialog = FindName(L"ControlNoticeDialog").try_as<WUX::Controls::ContentDialog>();

            ControlNoticeDialog().Title(winrt::box_value(title));

            // Insert the message
            NoticeMessage().Text(message);

            // Show the dialog
            presenter.ShowDialog(controlNoticeDialog);
        }
    }

    // Method Description:
    // - Copy text from the focused terminal to the Windows Clipboard
    // Arguments:
    // - dismissSelection: if not enabled, copying text doesn't dismiss the selection
    // - singleLine: if enabled, copy contents as a single line of text
    // - withControlSequences: if enabled, the copied plain text contains color/style ANSI escape codes from the selection
    // - formats: dictate which formats need to be copied
    // Return Value:
    // - true iff we we able to copy text (if a selection was active)
    bool TerminalPage::_CopyText(const bool dismissSelection, const bool singleLine, const bool withControlSequences, const CopyFormat formats)
    {
        if (const auto& control{ _GetActiveControl() })
        {
            return control.CopySelectionToClipboard(dismissSelection, singleLine, withControlSequences, formats);
        }
        return false;
    }

    // Method Description:
    // - Send an event (which will be caught by AppHost) to set the progress indicator on the taskbar
    // Arguments:
    // - sender (not used)
    // - eventArgs: the arguments specifying how to set the progress indicator
    safe_void_coroutine TerminalPage::_SetTaskbarProgressHandler(const IInspectable /*sender*/, const IInspectable /*eventArgs*/)
    {
        const auto weak = get_weak();
        co_await wil::resume_foreground(Dispatcher());
        if (const auto strong = weak.get())
        {
            SetTaskbarProgress.raise(*this, nullptr);
        }
    }

    // Method Description:
    // - Send an event (which will be caught by AppHost) to change the show window state of the entire hosting window
    // Arguments:
    // - sender (not used)
    // - args: the arguments specifying how to set the display status to ShowWindow for our window handle
    void TerminalPage::_ShowWindowChangedHandler(const IInspectable /*sender*/, const Microsoft::Terminal::Control::ShowWindowArgs args)
    {
        ShowWindowChanged.raise(*this, args);
    }

    Windows::Foundation::IAsyncOperation<IVectorView<MatchResult>> TerminalPage::_FindPackageAsync(hstring query)
    {
        const PackageManager packageManager = WindowsPackageManagerFactory::CreatePackageManager();
        PackageCatalogReference catalogRef{
            packageManager.GetPredefinedPackageCatalog(PredefinedPackageCatalog::OpenWindowsCatalog)
        };
        catalogRef.PackageCatalogBackgroundUpdateInterval(std::chrono::hours(24));

        ConnectResult connectResult{ nullptr };
        for (int retries = 0;;)
        {
            connectResult = catalogRef.Connect();
            if (connectResult.Status() == ConnectResultStatus::Ok)
            {
                break;
            }

            if (++retries == 3)
            {
                co_return nullptr;
            }
        }

        PackageCatalog catalog = connectResult.PackageCatalog();
        PackageMatchFilter filter = WindowsPackageManagerFactory::CreatePackageMatchFilter();
        filter.Value(query);
        filter.Field(PackageMatchField::Command);
        filter.Option(PackageFieldMatchOption::Equals);

        FindPackagesOptions options = WindowsPackageManagerFactory::CreateFindPackagesOptions();
        options.Filters().Append(filter);
        options.ResultLimit(20);

        const auto result = co_await catalog.FindPackagesAsync(options);
        const IVectorView<MatchResult> pkgList = result.Matches();
        co_return pkgList;
    }

    Windows::Foundation::IAsyncAction TerminalPage::_SearchMissingCommandHandler(const IInspectable /*sender*/, const Microsoft::Terminal::Control::SearchMissingCommandEventArgs args)
    {
        if (!Feature_QuickFix::IsEnabled())
        {
            co_return;
        }

        const auto weak = get_weak();
        const auto dispatcher = Dispatcher();

        // All of the code until resume_foreground is static and
        // doesn't touch `this`, so we don't need weak/strong_ref.
        co_await winrt::resume_background();

        // no packages were found, nothing to suggest
        const auto pkgList = co_await _FindPackageAsync(args.MissingCommand());
        if (!pkgList || pkgList.Size() == 0)
        {
            co_return;
        }

        std::vector<hstring> suggestions;
        suggestions.reserve(pkgList.Size());
        for (const auto& pkg : pkgList)
        {
            // --id and --source ensure we don't collide with another package catalog
            suggestions.emplace_back(fmt::format(FMT_COMPILE(L"winget install --id {} -s winget"), pkg.CatalogPackage().Id()));
        }

        co_await wil::resume_foreground(dispatcher);
        const auto strong = weak.get();
        if (!strong)
        {
            co_return;
        }

        auto term = _GetActiveControl();
        if (!term)
        {
            co_return;
        }
        term.UpdateWinGetSuggestions(single_threaded_vector<hstring>(std::move(suggestions)));
        term.RefreshQuickFixMenu();
    }

    void TerminalPage::_WindowSizeChanged(const IInspectable sender, const Microsoft::Terminal::Control::WindowSizeChangedEventArgs args)
    {
        // Raise if:
        // - Not in quake mode
        // - Not in fullscreen
        // - Only one tab exists
        // - Only one pane exists
        // else:
        // - Reset conpty to its original size back
        if (!WindowProperties().IsQuakeWindow() && !Fullscreen() &&
            NumberOfTabs() == 1 && _GetFocusedTabImpl()->GetLeafPaneCount() == 1)
        {
            WindowSizeChanged.raise(*this, args);
        }
        else if (const auto& control{ sender.try_as<TermControl>() })
        {
            const auto& connection = control.Connection();

            if (const auto& conpty{ connection.try_as<TerminalConnection::ConptyConnection>() })
            {
                conpty.ResetSize();
            }
        }
    }

    void TerminalPage::_copyToClipboard(const IInspectable, const WriteToClipboardEventArgs args) const
    {
        if (const auto clipboard = clipboard::open(_hostingHwnd.value_or(nullptr)))
        {
            const auto plain = args.Plain();
            const auto html = args.Html();
            const auto rtf = args.Rtf();

            clipboard::write(
                { plain.data(), plain.size() },
                { reinterpret_cast<const char*>(html.data()), html.size() },
                { reinterpret_cast<const char*>(rtf.data()), rtf.size() });
        }
    }

    // Method Description:
    // - Paste text from the Windows Clipboard to the focused terminal
    void TerminalPage::_PasteText()
    {
        if (const auto& control{ _GetActiveControl() })
        {
            control.PasteTextFromClipboard();
        }
    }

    // Function Description:
    // - Called when the settings button is clicked. ShellExecutes the settings
    //   file, as to open it in the default editor for .json files. Does this in
    //   a background thread, as to not hang/crash the UI thread.
    safe_void_coroutine TerminalPage::_LaunchSettings(const SettingsTarget target)
    {
        if (target == SettingsTarget::SettingsUI)
        {
            OpenSettingsUI();
        }
        else
        {
            // This will switch the execution of the function to a background (not
            // UI) thread. This is IMPORTANT, because the Windows.Storage API's
            // (used for retrieving the path to the file) will crash on the UI
            // thread, because the main thread is a STA.
            //
            // NOTE: All remaining code of this function doesn't touch `this`, so we don't need weak/strong_ref.
            // NOTE NOTE: Don't touch `this` when you make changes here.
            co_await winrt::resume_background();

            auto openFile = [](const auto& filePath) {
                HINSTANCE res = ShellExecute(nullptr, nullptr, filePath.c_str(), nullptr, nullptr, SW_SHOW);
                if (static_cast<int>(reinterpret_cast<uintptr_t>(res)) <= 32)
                {
                    ShellExecute(nullptr, nullptr, L"notepad", filePath.c_str(), nullptr, SW_SHOW);
                }
            };

            auto openFolder = [](const auto& filePath) {
                HINSTANCE res = ShellExecute(nullptr, nullptr, filePath.c_str(), nullptr, nullptr, SW_SHOW);
                if (static_cast<int>(reinterpret_cast<uintptr_t>(res)) <= 32)
                {
                    ShellExecute(nullptr, nullptr, L"open", filePath.c_str(), nullptr, SW_SHOW);
                }
            };

            switch (target)
            {
            case SettingsTarget::DefaultsFile:
                openFile(CascadiaSettings::DefaultSettingsPath());
                break;
            case SettingsTarget::SettingsFile:
                openFile(CascadiaSettings::SettingsPath());
                break;
            case SettingsTarget::Directory:
                openFolder(CascadiaSettings::SettingsDirectory());
                break;
            case SettingsTarget::AllFiles:
                openFile(CascadiaSettings::DefaultSettingsPath());
                openFile(CascadiaSettings::SettingsPath());
                break;
            }
        }
    }

    // Method Description:
    // - Responds to the TabView control's Tab Closing event by removing
    //      the indicated tab from the set and focusing another one.
    //      The event is cancelled so App maintains control over the
    //      items in the tabview.
    // Arguments:
    // - sender: the control that originated this event
    // - eventArgs: the event's constituent arguments
    void TerminalPage::_OnTabCloseRequested(const IInspectable& /*sender*/, const MUX::Controls::TabViewTabCloseRequestedEventArgs& eventArgs)
    {
        const auto tabViewItem = eventArgs.Tab();
        if (auto tab{ _GetTabByTabViewItem(tabViewItem) })
        {
            _HandleCloseTabRequested(tab);
        }
    }

    TermControl TerminalPage::_CreateNewControlAndContent(const Settings::TerminalSettingsCreateResult& settings, const ITerminalConnection& connection)
    {
        // Do any initialization that needs to apply to _every_ TermControl we
        // create here.
        const auto content = _manager.CreateCore(*settings.DefaultSettings(), settings.UnfocusedSettings().try_as<IControlAppearance>(), connection);
        const TermControl control{ content };
        return _SetupControl(control);
    }

    TermControl TerminalPage::_AttachControlToContent(const uint64_t& contentId)
    {
        if (const auto& content{ _manager.TryLookupCore(contentId) })
        {
            // We have to pass in our current keybindings, because that's an
            // object that belongs to this TerminalPage, on this thread. If we
            // don't, then when we move the content to another thread, and it
            // tries to handle a key, it'll callback on the original page's
            // stack, inevitably resulting in a wrong_thread
            return _SetupControl(TermControl::NewControlByAttachingContent(content));
        }
        return nullptr;
    }

    TermControl TerminalPage::_SetupControl(const TermControl& term)
    {
        // GH#12515: ConPTY assumes it's hidden at the start. If we're not, let it know now.
        if (_visible)
        {
            term.WindowVisibilityChanged(_visible);
        }

        // Even in the case of re-attaching content from another window, this
        // will correctly update the control's owning HWND
        if (_hostingHwnd.has_value())
        {
            term.OwningHwnd(reinterpret_cast<uint64_t>(*_hostingHwnd));
        }

        term.KeyBindings(*_bindings);

        _RegisterTerminalEvents(term);
        return term;
    }

    // Method Description:
    // - Creates a pane and returns a shared_ptr to it
    // - The caller should handle where the pane goes after creation,
    //   either to split an already existing pane or to create a new tab with it
    // Arguments:
    // - newTerminalArgs: an object that may contain a blob of parameters to
    //   control which profile is created and with possible other
    //   configurations. See CascadiaSettings::BuildSettings for more details.
    // - sourceTab: an optional tab reference that indicates that the created
    //   pane should be a duplicate of the tab's focused pane
    // - existingConnection: optionally receives a connection from the outside
    //   world instead of attempting to create one
    // Return Value:
    // - If the newTerminalArgs required us to open the pane as a new elevated
    //   connection, then we'll return nullptr. Otherwise, we'll return a new
    //   Pane for this connection.
    std::shared_ptr<Pane> TerminalPage::_MakeTerminalPane(const NewTerminalArgs& newTerminalArgs,
                                                          const winrt::TerminalApp::Tab& sourceTab,
                                                          TerminalConnection::ITerminalConnection existingConnection)
    {
        // First things first - Check for making a pane from content ID.
        if (newTerminalArgs &&
            newTerminalArgs.ContentId() != 0)
        {
            // Don't need to worry about duplicating or anything - we'll
            // serialize the actual profile's GUID along with the content guid.
            const auto& profile = _settings.GetProfileForArgs(newTerminalArgs);
            const auto control = _AttachControlToContent(newTerminalArgs.ContentId());
            auto paneContent{ winrt::make<TerminalPaneContent>(profile, _terminalSettingsCache, control) };
            auto resultPane = std::make_shared<Pane>(paneContent);

            // Cross-window agent-pane drag: if the source tab stashed an
            // original StableId for this ContentId, the migrating pane was
            // an agent pane. Re-wrap into AgentPaneContent here so the
            // target window restores the chrome (bottom bar, status, click
            // handlers).
            const uint64_t contentId = newTerminalArgs.ContentId();
            winrt::hstring oldTabId;
            if (winrt::TerminalApp::implementation::AgentPaneDragStash::Take(contentId, oldTabId))
            {
                // Drag-in is targeting the focused (destination) tab. If
                // pre-warm already created an agent pane on this tab (race:
                // NewTab's deferred dispatcher tick fires ~300ms BEFORE this
                // SplitPane re-wrap, sees no agent pane yet, and spawns a
                // pre-warm one), close that pane first so the drag-in pane
                // is the only AgentPaneContent on the tab. The preexisting
                // pre-warm pane's `Pane::Closed` handler releases its
                // SharedWta refcount and its helper conpty exits via EOF;
                // the brief wasted helper spawn is the cost of letting
                // pre-warm fire unconditionally on every new tab (vs.
                // gating it on an unreliable / over-broad "is any drag in
                // flight" signal).
                //
                // Per-tab dedup: we know the drag-in is targeting THIS
                // focused tab specifically (NewTab focused it just before
                // this SplitPane fires), so this only tears down panes on
                // the right tab — no false-positives in unrelated windows
                // / tabs.
                if (const auto focusedTab = _GetFocusedTabImpl())
                {
                    if (const auto existingAgentPane = focusedTab->FindAgentPane())
                    {
                        _agentPaneLog(
                            std::string{ "_MakeTerminalPane: drag-in tearing down pre-warm leftover on tab " } +
                            winrt::to_string(focusedTab->StableId()));
                        existingAgentPane->Close();
                    }
                }

                if (auto wrapped = _WrapInAgentPaneContent(resultPane))
                {
                    wrapped->IsAgentPane(true);

                    // Mirror the `Pane::Closed` → `SharedWta::ReleasePane`
                    // wiring that `_AutoCreateHiddenAgentPaneShared`
                    // installs on the source side. The drag-in pane is a
                    // freshly-constructed `Pane` object; without this
                    // handler, any path that calls `pane->Close()` on it
                    // (Ctrl+C×2 → `OnCloseAgentPaneRequested` →
                    // `_TeardownAgentPane`, or settings-rebuild via
                    // `_RebuildAgentStack` → `_TeardownAgentPane`) would
                    // raise `Closed` without anyone decrementing the
                    // SharedWta refcount that the source side's
                    // `AcquirePane()` contributed. The tab-close walk
                    // in `_RemoveTab` wouldn't catch it either, because
                    // the pane is already gone from the tree by the time
                    // the tab finally closes. Net: the master process
                    // would be kept alive past its last live pane.
                    //
                    // No new `AcquirePane()` here — the source side's
                    // existing refcount carries across the drag (source's
                    // `_RemoveTab(movingAway=true)` deliberately skips
                    // `ReleasePane` precisely so the dragged helper has a
                    // refcount to live on). This `Closed` handler is the
                    // matching `Release` for that.
                    wrapped->Closed([](auto&&, auto&&) {
                        _agentPaneLog("drag-in agent pane closed");
                        winrt::TerminalApp::implementation::SharedWta::Instance().ReleasePane();
                    });

                    if (const auto agentContent = wrapped->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>())
                    {
                        agentContent.SetAgentPanePosition(_settings.GlobalSettings().AgentPanePosition());

                        // Wire `StateChanged` BEFORE emitting `tab_renamed`.
                        // The deferred walk in `_InitializeTab` would normally
                        // handle this, but cross-window drag-in has a timing
                        // problem: the SplitPane that calls this re-wrap
                        // fires ~300ms AFTER NewTab's deferred dispatcher tick
                        // has already run; at walk time the agent pane wasn't
                        // in the tree yet, so `_WireAgentPaneEvents` was
                        // never invoked. We do it here instead.
                        //
                        // Ordering matters: `tab_renamed` (emitted a few lines
                        // below) drives the helper to re-project state, which
                        // ends up calling `ApplyAutofixState` → `StateChanged`
                        // on this very `AgentPaneContent`. If the wire happens
                        // after `tab_renamed`, that `StateChanged` raise has
                        // nobody listening and the bottom bar stays stale —
                        // exactly the bug this drag-in path is meant to fix.
                        // The helper round-trip through wtcli + COM is many
                        // ms so in practice we always win the race, but the
                        // synchronous-correct ordering is to wire first.
                        // (`ownerTab` arg is unused.)
                        _WireAgentPaneEvents(agentContent, winrt::com_ptr<Tab>{ nullptr });
                    }
                    // Emit `tab_renamed` IMMEDIATELY here. The cross-window
                    // drag flow runs NewTab → SplitPane as serialized actions:
                    // NewTab already created the target tab (and focused it)
                    // by the time SplitPane (the call site for us) runs, so
                    // the focused tab's StableId IS the new tab id. wta
                    // helpers receive `tab_renamed { old, new }` right away
                    // and rekey their TabSession HashMap key — the
                    // helper-owned per-tab state (view, pane_open, messages
                    // history) survives the drag instead of getting
                    // replaced by a default chat / pane_open=false session
                    // on the next tab_changed. Without this immediate emit
                    // the agent pane visually arrives but wta clobbers it
                    // with a fresh-default state echo (pane_open=false →
                    // C++ stashes the just-arrived pane → user sees
                    // "agent pane gone after drag").
                    if (!oldTabId.empty())
                    {
                        if (const auto focusedTab = _GetFocusedTabImpl())
                        {
                            const auto newTabId = focusedTab->StableId();
                            if (!newTabId.empty() && newTabId != oldTabId)
                            {
                                Json::Value evt;
                                evt["type"] = "event";
                                evt["method"] = "tab_renamed";
                                Json::Value params;
                                params["old_tab_id"] = winrt::to_string(oldTabId);
                                params["new_tab_id"] = winrt::to_string(newTabId);
                                // Dest window id. The helper for the dragged
                                // tab reads this and updates its stale
                                // `self.window_id` during rekey, so post-drag
                                // set_agent_state events from the new window
                                // pass the per-tab window filter.
                                params["window_id"] = std::to_string(_WindowProperties.WindowId());
                                evt["params"] = params;
                                Json::StreamWriterBuilder wb;
                                wb["indentation"] = "";
                                const auto payload = winrt::to_hstring(Json::writeString(wb, evt));
                                _agentPaneLog(
                                    std::string{ "_MakeTerminalPane: emitting tab_renamed old=" } +
                                    winrt::to_string(oldTabId) + " new=" + winrt::to_string(newTabId));
                                ProtocolVtSequenceReceived.raise(*this, payload);
                            }
                            else
                            {
                                _agentPaneLog(
                                    std::string{ "_MakeTerminalPane: skipping tab_renamed (newTabIdEmpty=" } +
                                    (newTabId.empty() ? "true" : "false") + " sameAsOld=" +
                                    (newTabId == oldTabId ? "true" : "false") + ")");
                            }
                        }
                        else
                        {
                            _agentPaneLog("_MakeTerminalPane: no focused tab — tab_renamed deferred to _InitializeTab");
                            if (const auto agentContent = wrapped->GetContent().try_as<winrt::TerminalApp::AgentPaneContent>())
                            {
                                if (const auto impl = winrt::get_self<winrt::TerminalApp::implementation::AgentPaneContent>(agentContent))
                                {
                                    impl->SetPendingRenameFromTabId(oldTabId);
                                }
                            }
                        }
                    }
                    _agentPaneLog("_MakeTerminalPane: re-wrapped drag-in pane as AgentPaneContent");
                    return wrapped;
                }
                _agentPaneLog("_MakeTerminalPane: drag-in agent pane wrap failed — falling back to plain pane");
            }

            return resultPane;
        }

        Settings::TerminalSettingsCreateResult controlSettings{ nullptr };
        Profile profile{ nullptr };

        if (const auto& tabImpl{ _GetTabImpl(sourceTab) })
        {
            profile = tabImpl->GetFocusedProfile();
            if (profile)
            {
                // TODO GH#5047 If we cache the NewTerminalArgs, we no longer need to do this.
                profile = GetClosestProfileForDuplicationOfProfile(profile);
                controlSettings = Settings::TerminalSettings::CreateWithProfile(_settings, profile);
                const auto workingDirectory = tabImpl->GetActiveTerminalControl().WorkingDirectory();
                if (Utils::IsValidDirectory(workingDirectory.c_str()))
                {
                    controlSettings.DefaultSettings()->StartingDirectory(workingDirectory);
                }
            }
        }
        if (!profile)
        {
            profile = _settings.GetProfileForArgs(newTerminalArgs);
            controlSettings = Settings::TerminalSettings::CreateWithNewTerminalArgs(_settings, newTerminalArgs);
        }

        // Try to handle auto-elevation
        if (_maybeElevate(newTerminalArgs, controlSettings, profile))
        {
            return nullptr;
        }

        const auto sessionId = controlSettings.DefaultSettings()->SessionId();
        const auto hasSessionId = sessionId != winrt::guid{};

        TerminalConnection::ITerminalConnection connection{ nullptr };
        if (existingConnection)
        {
            connection = existingConnection;
            connection.Resize(controlSettings.DefaultSettings()->InitialRows(), controlSettings.DefaultSettings()->InitialCols());
        }
        else
        {
            connection = _CreateConnectionFromSettings(profile, *controlSettings.DefaultSettings(), hasSessionId);
        }

        TerminalConnection::ITerminalConnection debugConnection{ nullptr };
        if (_settings.GlobalSettings().DebugFeaturesEnabled())
        {
            const auto window = CoreWindow::GetForCurrentThread();
            const auto rAltState = window.GetKeyState(VirtualKey::RightMenu);
            const auto lAltState = window.GetKeyState(VirtualKey::LeftMenu);
            const auto bothAltsPressed = WI_IsFlagSet(lAltState, CoreVirtualKeyStates::Down) &&
                                         WI_IsFlagSet(rAltState, CoreVirtualKeyStates::Down);
            if (bothAltsPressed)
            {
                std::tie(connection, debugConnection) = OpenDebugTapConnection(connection);
            }
        }

        const auto control = _CreateNewControlAndContent(controlSettings, connection);

        if (hasSessionId)
        {
            using namespace std::string_view_literals;

            const auto settingsDir = CascadiaSettings::SettingsDirectory();
            const auto admin = IsRunningElevated();
            const auto filenamePrefix = admin ? L"elevated_"sv : L"buffer_"sv;
            const auto path = fmt::format(FMT_COMPILE(L"{}\\{}{}.txt"), settingsDir, filenamePrefix, sessionId);
            control.RestoreFromPath(path);
        }

        auto paneContent{ winrt::make<TerminalPaneContent>(profile, _terminalSettingsCache, control) };

        auto resultPane = std::make_shared<Pane>(paneContent);

        if (debugConnection) // this will only be set if global debugging is on and tap is active
        {
            auto newControl = _CreateNewControlAndContent(controlSettings, debugConnection);
            // Split (auto) with the debug tap.
            auto debugContent{ winrt::make<TerminalPaneContent>(profile, _terminalSettingsCache, newControl) };
            auto debugPane = std::make_shared<Pane>(debugContent);

            // Since we're doing this split directly on the pane (instead of going through Tab,
            // we need to handle the panes 'active' states

            // Set the pane we're splitting to active (otherwise Split will not do anything)
            resultPane->SetActive();
            auto [original, _] = resultPane->Split(SplitDirection::Automatic, 0.5f, debugPane);

            // Set the non-debug pane as active
            resultPane->ClearActive();
            original->SetActive();
        }

        return resultPane;
    }

    // NOTE: callers of _MakePane should be able to accept nullptr as a return
    // value gracefully.
    std::shared_ptr<Pane> TerminalPage::_MakePane(const INewContentArgs& contentArgs,
                                                  const winrt::TerminalApp::Tab& sourceTab,
                                                  TerminalConnection::ITerminalConnection existingConnection)

    {
        const auto& newTerminalArgs{ contentArgs.try_as<NewTerminalArgs>() };
        if (contentArgs == nullptr || newTerminalArgs != nullptr || contentArgs.Type().empty())
        {
            // Terminals are of course special, and have to deal with debug taps, duplicating the tab, etc.
            return _MakeTerminalPane(newTerminalArgs, sourceTab, existingConnection);
        }

        IPaneContent content{ nullptr };

        const auto& paneType{ contentArgs.Type() };
        if (paneType == L"scratchpad")
        {
            const auto& scratchPane{ winrt::make_self<ScratchpadContent>() };

            // This is maybe a little wacky - add our key event handler to the pane
            // we made. So that we can get actions for keys that the content didn't
            // handle.
            scratchPane->GetRoot().KeyDown({ get_weak(), &TerminalPage::_KeyDownHandler });

            content = *scratchPane;
        }
        else if (paneType == L"settings")
        {
            content = _makeSettingsContent();
        }
        else if (paneType == L"snippets")
        {
            // Prevent the user from opening a bunch of snippets panes.
            //
            // Look at the focused tab, and if it already has one, then just focus it.
            if (const auto& focusedTab{ _GetFocusedTabImpl() })
            {
                const auto rootPane{ focusedTab->GetRootPane() };
                const bool found = rootPane == nullptr ? false : rootPane->WalkTree([](const auto& p) -> bool {
                    if (const auto& snippets{ p->GetContent().try_as<SnippetsPaneContent>() })
                    {
                        snippets->Focus(FocusState::Programmatic);
                        return true;
                    }
                    return false;
                });
                // Bail out if we already found one.
                if (found)
                {
                    return nullptr;
                }
            }

            const auto& tasksContent{ winrt::make_self<SnippetsPaneContent>() };
            tasksContent->UpdateSettings(_settings);
            tasksContent->GetRoot().KeyDown({ this, &TerminalPage::_KeyDownHandler });
            tasksContent->DispatchCommandRequested({ this, &TerminalPage::_OnDispatchCommandRequested });
            if (const auto& termControl{ _GetActiveControl() })
            {
                tasksContent->SetLastActiveControl(termControl);
            }

            content = *tasksContent;
        }
        else if (paneType == L"x-markdown")
        {
            if (Feature_MarkdownPane::IsEnabled())
            {
                const auto& markdownContent{ winrt::make_self<MarkdownPaneContent>(L"") };
                markdownContent->UpdateSettings(_settings);
                markdownContent->GetRoot().KeyDown({ this, &TerminalPage::_KeyDownHandler });

                // This one doesn't use DispatchCommand, because we don't create
                // Command's freely at runtime like we do with just plain old actions.
                markdownContent->DispatchActionRequested([weak = get_weak()](const auto& sender, const auto& actionAndArgs) {
                    if (const auto& page{ weak.get() })
                    {
                        page->_actionDispatch->DoAction(sender, actionAndArgs);
                    }
                });
                if (const auto& termControl{ _GetActiveControl() })
                {
                    markdownContent->SetLastActiveControl(termControl);
                }

                content = *markdownContent;
            }
        }

        assert(content);

        return std::make_shared<Pane>(content);
    }

    void TerminalPage::_restartPaneConnection(
        const TerminalApp::TerminalPaneContent& paneContent,
        const winrt::Windows::Foundation::IInspectable&)
    {
        // Note: callers are likely passing in `nullptr` as the args here, as
        // the TermControl.RestartTerminalRequested event doesn't actually pass
        // any args upwards itself. If we ever change this, make sure you check
        // for nulls
        if (const auto& connection{ _duplicateConnectionForRestart(paneContent) })
        {
            // Reset the terminal's VT state before attaching the new connection.
            // The previous client may have left dirty modes (e.g., bracketed
            // paste, mouse tracking, alternate buffer, kitty keyboard) that
            // would corrupt input/output for the new shell process.
            const auto& termControl = paneContent.GetTermControl();
            termControl.HardResetWithoutErase();
            termControl.Connection(connection);
            connection.Start();
        }
    }

    // Method Description:
    // - Sets background image and applies its settings (stretch, opacity and alignment)
    // - Checks path validity
    // Arguments:
    // - newAppearance
    // Return Value:
    // - <none>
    void TerminalPage::_SetBackgroundImage(const winrt::Microsoft::Terminal::Settings::Model::IAppearanceConfig& newAppearance)
    {
        if (!_settings.GlobalSettings().UseBackgroundImageForWindow())
        {
            _tabContent.Background(nullptr);
            return;
        }

        const auto path = newAppearance.BackgroundImagePath().Resolved();
        if (path.empty())
        {
            _tabContent.Background(nullptr);
            return;
        }

        Windows::Foundation::Uri imageUri{ nullptr };
        try
        {
            imageUri = Windows::Foundation::Uri{ path };
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            _tabContent.Background(nullptr);
            return;
        }
        // Check if the image brush is already pointing to the image
        // in the modified settings; if it isn't (or isn't there),
        // set a new image source for the brush

        auto brush = _tabContent.Background().try_as<Media::ImageBrush>();
        Media::Imaging::BitmapImage imageSource = brush == nullptr ? nullptr : brush.ImageSource().try_as<Media::Imaging::BitmapImage>();

        if (imageSource == nullptr ||
            imageSource.UriSource() == nullptr ||
            !imageSource.UriSource().Equals(imageUri))
        {
            Media::ImageBrush b{};
            // Note that BitmapImage handles the image load asynchronously,
            // which is especially important since the image
            // may well be both large and somewhere out on the
            // internet.
            Media::Imaging::BitmapImage image(imageUri);
            b.ImageSource(image);
            _tabContent.Background(b);
        }

        // Pull this into a separate block. If the image didn't change, but the
        // properties of the image did, we should still update them.
        if (const auto newBrush{ _tabContent.Background().try_as<Media::ImageBrush>() })
        {
            newBrush.Stretch(newAppearance.BackgroundImageStretchMode());
            newBrush.Opacity(newAppearance.BackgroundImageOpacity());
        }
    }

    // Method Description:
    // - Hook up keybindings, and refresh the UI of the terminal.
    //   This includes update the settings of all the tabs according
    //   to their profiles, update the title and icon of each tab, and
    //   finally create the tab flyout
    void TerminalPage::_RefreshUIForSettingsReload()
    {
        // Re-wire the keybindings to their handlers, as we'll have created a
        // new AppKeyBindings object.
        _HookupKeyBindings(_settings.ActionMap());

        // Refresh UI elements

        // Recreate the TerminalSettings cache here. We'll use that as we're
        // updating terminal panes, so that we don't have to build a _new_
        // TerminalSettings for every profile we update - we can just look them
        // up the previous ones we built.
        _terminalSettingsCache->Reset(_settings);

        for (const auto& tab : _tabs)
        {
            if (auto tabImpl{ _GetTabImpl(tab) })
            {
                // Let the tab know that there are new settings. It's up to each content to decide what to do with them.
                tabImpl->UpdateSettings(_settings);

                // Update the icon of the tab for the currently focused profile in that tab.
                // Only do this for TerminalTabs. Other types of tabs won't have multiple panes
                // and profiles so the Title and Icon will be set once and only once on init.
                _UpdateTabIcon(*tabImpl);

                // Force the TerminalTab to re-grab its currently active control's title.
                tabImpl->UpdateTitle();
            }

            auto tabImpl{ winrt::get_self<Tab>(tab) };
            tabImpl->SetActionMap(_settings.ActionMap());
        }

        if (const auto focusedTab{ _GetFocusedTabImpl() })
        {
            if (const auto profile{ focusedTab->GetFocusedProfile() })
            {
                _SetBackgroundImage(profile.DefaultAppearance());
            }
        }

        // repopulate the new tab button's flyout with entries for each
        // profile, which might have changed
        _UpdateTabWidthMode();
        _CreateNewTabFlyout();

        // Reload the current value of alwaysOnTop from the settings file. This
        // will let the user hot-reload this setting, but any runtime changes to
        // the alwaysOnTop setting will be lost.
        _isAlwaysOnTop = _settings.GlobalSettings().AlwaysOnTop();
        AlwaysOnTopChanged.raise(*this, nullptr);

        _showTabsFullscreen = _settings.GlobalSettings().ShowTabsFullscreen();

        // Settings AllowDependentAnimations will affect whether animations are
        // enabled application-wide, so we don't need to check it each time we
        // want to create an animation.
        WUX::Media::Animation::Timeline::AllowDependentAnimations(!_settings.GlobalSettings().DisableAnimations());

        _tabRow.ShowElevationShield(IsRunningElevated() && _settings.GlobalSettings().ShowAdminShield());

        Media::SolidColorBrush transparent{ Windows::UI::Colors::Transparent() };
        _tabView.Background(transparent);

        ////////////////////////////////////////////////////////////////////////
        // Begin Theme handling
        _updateThemeColors();

        _updateAllTabCloseButtons();

        // The user may have changed the "show title in titlebar" setting.
        TitleChanged.raise(*this, nullptr);

        // Reposition existing agent panes if the position setting changed.
        _RepositionAgentPanes();

        // If any of the agent-identity settings (agent / model / custom
        // command for either ACP or delegate) changed, tear down and
        // recreate the affected layers so the new values take effect
        // without a terminal restart.
        _RebuildAgentStack();
    }

    void TerminalPage::_updateAllTabCloseButtons()
    {
        // Update the state of the CloseButtonOverlayMode property of
        // our TabView, to match the tab.showCloseButton property in the theme.
        //
        // Also update every tab's individual IsClosable to match the same property.
        const auto theme = _settings.GlobalSettings().CurrentTheme();
        const auto visibility = (theme && theme.Tab()) ?
                                    theme.Tab().ShowCloseButton() :
                                    Settings::Model::TabCloseButtonVisibility::Always;

        _tabItemMiddleClickHookEnabled = visibility == Settings::Model::TabCloseButtonVisibility::Never;

        for (const auto& tab : _tabs)
        {
            tab.CloseButtonVisibility(visibility);
        }

        switch (visibility)
        {
        case Settings::Model::TabCloseButtonVisibility::Never:
            _tabView.CloseButtonOverlayMode(MUX::Controls::TabViewCloseButtonOverlayMode::Auto);
            break;
        case Settings::Model::TabCloseButtonVisibility::Hover:
            _tabView.CloseButtonOverlayMode(MUX::Controls::TabViewCloseButtonOverlayMode::OnPointerOver);
            break;
        case Settings::Model::TabCloseButtonVisibility::ActiveOnly:
        default:
            _tabView.CloseButtonOverlayMode(MUX::Controls::TabViewCloseButtonOverlayMode::Always);
            break;
        }
    }

    // Method Description:
    // - Sets the initial actions to process on startup. We'll make a copy of
    //   this list, and process these actions when we're loaded.
    // - This function will have no effective result after Create() is called.
    // Arguments:
    // - actions: a list of Actions to process on startup.
    // Return Value:
    // - <none>
    void TerminalPage::SetStartupActions(std::vector<ActionAndArgs> actions)
    {
        _startupActions = std::move(actions);
    }

    void TerminalPage::SetStartupConnection(ITerminalConnection connection)
    {
        _startupConnection = std::move(connection);
    }

    winrt::TerminalApp::IDialogPresenter TerminalPage::DialogPresenter() const
    {
        return _dialogPresenter.get();
    }

    void TerminalPage::DialogPresenter(winrt::TerminalApp::IDialogPresenter dialogPresenter)
    {
        _dialogPresenter = dialogPresenter;
    }

    // Method Description:
    // - Get the combined taskbar state for the page. This is the combination of
    //   all the states of all the tabs, which are themselves a combination of
    //   all their panes. Taskbar states are given a priority based on the rules
    //   in:
    //   https://docs.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-itaskbarlist3-setprogressstate
    //   under "How the Taskbar Button Chooses the Progress Indicator for a Group"
    // Arguments:
    // - <none>
    // Return Value:
    // - A TaskbarState object representing the combined taskbar state and
    //   progress percentage of all our tabs.
    winrt::TerminalApp::TaskbarState TerminalPage::TaskbarState() const
    {
        auto state{ winrt::make<winrt::TerminalApp::implementation::TaskbarState>() };

        for (const auto& tab : _tabs)
        {
            if (auto tabImpl{ _GetTabImpl(tab) })
            {
                auto tabState{ tabImpl->GetCombinedTaskbarState() };
                // lowest priority wins
                if (tabState.Priority() < state.Priority())
                {
                    state = tabState;
                }
            }
        }

        return state;
    }

    // Method Description:
    // - This is the method that App will call when the titlebar
    //   has been clicked. It dismisses any open flyouts.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::TitlebarClicked()
    {
        if (_newTabButton && _newTabButton.Flyout())
        {
            _newTabButton.Flyout().Hide();
        }
        _DismissTabContextMenus();
    }

    // Method Description:
    // - Notifies all attached console controls that the visibility of the
    //   hosting window has changed. The underlying PTYs may need to know this
    //   for the proper response to `::GetConsoleWindow()` from a Win32 console app.
    // Arguments:
    // - showOrHide: Show is true; hide is false.
    // Return Value:
    // - <none>
    void TerminalPage::WindowVisibilityChanged(const bool showOrHide)
    {
        _visible = showOrHide;
        for (const auto& tab : _tabs)
        {
            if (auto tabImpl{ _GetTabImpl(tab) })
            {
                // Manually enumerate the panes in each tab; this will let us recycle TerminalSettings
                // objects but only have to iterate one time.
                tabImpl->GetRootPane()->WalkTree([&](auto&& pane) {
                    if (auto control = pane->GetTerminalControl())
                    {
                        control.WindowVisibilityChanged(showOrHide);
                    }
                });
            }
        }
    }

    // Method Description:
    // - Called when the user tries to do a search using keybindings.
    //   This will tell the active terminal control of the passed tab
    //   to create a search box and enable find process.
    // Arguments:
    // - tab: the tab where the search box should be created
    // Return Value:
    // - <none>
    void TerminalPage::_Find(const Tab& tab)
    {
        if (const auto& control{ tab.GetActiveTerminalControl() })
        {
            control.CreateSearchBoxControl();
        }
    }

    // Method Description:
    // - Toggles borderless mode. Hides the tab row, and raises our
    //   FocusModeChanged event.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::ToggleFocusMode()
    {
        SetFocusMode(!_isInFocusMode);
    }

    void TerminalPage::SetFocusMode(const bool inFocusMode)
    {
        const auto newInFocusMode = inFocusMode;
        if (newInFocusMode != FocusMode())
        {
            _isInFocusMode = newInFocusMode;
            _UpdateTabView();
            FocusModeChanged.raise(*this, nullptr);
        }
    }

    // Method Description:
    // - Toggles fullscreen mode. Hides the tab row, and raises our
    //   FullscreenChanged event.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::ToggleFullscreen()
    {
        SetFullscreen(!_isFullscreen);
    }

    // Method Description:
    // - Toggles always on top mode. Raises our AlwaysOnTopChanged event.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::ToggleAlwaysOnTop()
    {
        _isAlwaysOnTop = !_isAlwaysOnTop;
        AlwaysOnTopChanged.raise(*this, nullptr);
    }

    // Method Description:
    // - Sets the tab split button color when a new tab color is selected
    // Arguments:
    // - color: The color of the newly selected tab, used to properly calculate
    //          the foreground color of the split button (to match the font
    //          color of the tab)
    // - accentColor: the actual color we are going to use to paint the tab row and
    //                split button, so that there is some contrast between the tab
    //                and the non-client are behind it
    // Return Value:
    // - <none>
    void TerminalPage::_SetNewTabButtonColor(const til::color color, const til::color accentColor)
    {
        constexpr auto lightnessThreshold = 0.6f;
        // TODO GH#3327: Look at what to do with the tab button when we have XAML theming
        const auto isBrightColor = ColorFix::GetLightness(color) >= lightnessThreshold;
        const auto isLightAccentColor = ColorFix::GetLightness(accentColor) >= lightnessThreshold;
        const auto hoverColorAdjustment = isLightAccentColor ? -0.05f : 0.05f;
        const auto pressedColorAdjustment = isLightAccentColor ? -0.1f : 0.1f;

        const auto foregroundColor = isBrightColor ? Colors::Black() : Colors::White();
        const auto hoverColor = til::color{ ColorFix::AdjustLightness(accentColor, hoverColorAdjustment) };
        const auto pressedColor = til::color{ ColorFix::AdjustLightness(accentColor, pressedColorAdjustment) };

        Media::SolidColorBrush backgroundBrush{ accentColor };
        Media::SolidColorBrush backgroundHoverBrush{ hoverColor };
        Media::SolidColorBrush backgroundPressedBrush{ pressedColor };
        Media::SolidColorBrush foregroundBrush{ foregroundColor };

        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonBackground"), backgroundBrush);
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonBackgroundPointerOver"), backgroundHoverBrush);
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonBackgroundPressed"), backgroundPressedBrush);

        // Load bearing: The SplitButton uses SplitButtonForegroundSecondary for
        // the secondary button, but {TemplateBinding Foreground} for the
        // primary button.
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonForeground"), foregroundBrush);
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonForegroundPointerOver"), foregroundBrush);
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonForegroundPressed"), foregroundBrush);
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonForegroundSecondary"), foregroundBrush);
        _newTabButton.Resources().Insert(winrt::box_value(L"SplitButtonForegroundSecondaryPressed"), foregroundBrush);

        _newTabButton.Background(backgroundBrush);
        _newTabButton.Foreground(foregroundBrush);

        // This is just like what we do in Tab::_RefreshVisualState. We need
        // to manually toggle the visual state, so the setters in the visual
        // state group will re-apply, and set our currently selected colors in
        // the resources.
        VisualStateManager::GoToState(_newTabButton, L"FlyoutOpen", true);
        VisualStateManager::GoToState(_newTabButton, L"Normal", true);
    }

    // Method Description:
    // - Clears the tab split button color to a system color
    //   (or white if none is found) when the tab's color is cleared
    // - Clears the tab row color to a system color
    //   (or white if none is found) when the tab's color is cleared
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::_ClearNewTabButtonColor()
    {
        // TODO GH#3327: Look at what to do with the tab button when we have XAML theming
        winrt::hstring keys[] = {
            L"SplitButtonBackground",
            L"SplitButtonBackgroundPointerOver",
            L"SplitButtonBackgroundPressed",
            L"SplitButtonForeground",
            L"SplitButtonForegroundSecondary",
            L"SplitButtonForegroundPointerOver",
            L"SplitButtonForegroundPressed",
            L"SplitButtonForegroundSecondaryPressed"
        };

        // simply clear any of the colors in the split button's dict
        for (auto keyString : keys)
        {
            auto key = winrt::box_value(keyString);
            if (_newTabButton.Resources().HasKey(key))
            {
                _newTabButton.Resources().Remove(key);
            }
        }

        const auto res = Application::Current().Resources();

        const auto defaultBackgroundKey = winrt::box_value(L"TabViewItemHeaderBackground");
        const auto defaultForegroundKey = winrt::box_value(L"SystemControlForegroundBaseHighBrush");
        winrt::Windows::UI::Xaml::Media::SolidColorBrush backgroundBrush;
        winrt::Windows::UI::Xaml::Media::SolidColorBrush foregroundBrush;

        // TODO: Related to GH#3917 - I think if the system is set to "Dark"
        // theme, but the app is set to light theme, then this lookup still
        // returns to us the dark theme brushes. There's gotta be a way to get
        // the right brushes...
        // See also GH#5741
        if (res.HasKey(defaultBackgroundKey))
        {
            auto obj = res.Lookup(defaultBackgroundKey);
            backgroundBrush = obj.try_as<winrt::Windows::UI::Xaml::Media::SolidColorBrush>();
        }
        else
        {
            backgroundBrush = winrt::Windows::UI::Xaml::Media::SolidColorBrush{ winrt::Windows::UI::Colors::Black() };
        }

        if (res.HasKey(defaultForegroundKey))
        {
            auto obj = res.Lookup(defaultForegroundKey);
            foregroundBrush = obj.try_as<winrt::Windows::UI::Xaml::Media::SolidColorBrush>();
        }
        else
        {
            foregroundBrush = winrt::Windows::UI::Xaml::Media::SolidColorBrush{ winrt::Windows::UI::Colors::White() };
        }

        _newTabButton.Background(backgroundBrush);
        _newTabButton.Foreground(foregroundBrush);
    }

    // Function Description:
    // - This is a helper method to get the commandline out of a
    //   ExecuteCommandline action, break it into subcommands, and attempt to
    //   parse it into actions. This is used by _HandleExecuteCommandline for
    //   processing commandlines in the current WT window.
    // Arguments:
    // - args: the ExecuteCommandlineArgs to synthesize a list of startup actions for.
    // Return Value:
    // - an empty list if we failed to parse; otherwise, a list of actions to execute.
    std::vector<ActionAndArgs> TerminalPage::ConvertExecuteCommandlineToActions(const ExecuteCommandlineArgs& args)
    {
        ::TerminalApp::AppCommandlineArgs appArgs;
        if (appArgs.ParseArgs(args) == 0)
        {
            return appArgs.GetStartupActions();
        }

        return {};
    }

    void TerminalPage::_FocusActiveControl(IInspectable /*sender*/,
                                           IInspectable /*eventArgs*/)
    {
        _FocusCurrentTab(false);
    }

    bool TerminalPage::FocusMode() const
    {
        return _isInFocusMode;
    }

    bool TerminalPage::Fullscreen() const
    {
        return _isFullscreen;
    }

    // Method Description:
    // - Returns true if we're currently in "Always on top" mode. When we're in
    //   always on top mode, the window should be on top of all other windows.
    //   If multiple windows are all "always on top", they'll maintain their own
    //   z-order, with all the windows on top of all other non-topmost windows.
    // Arguments:
    // - <none>
    // Return Value:
    // - true if we should be in "always on top" mode
    bool TerminalPage::AlwaysOnTop() const
    {
        return _isAlwaysOnTop;
    }

    // Method Description:
    // - Returns true if the tab row should be visible when we're in full screen
    //   state.
    // Arguments:
    // - <none>
    // Return Value:
    // - true if the tab row should be visible in full screen state
    bool TerminalPage::ShowTabsFullscreen() const
    {
        return _showTabsFullscreen;
    }

    // Method Description:
    // - Updates the visibility of the tab row when in fullscreen state.
    void TerminalPage::SetShowTabsFullscreen(bool newShowTabsFullscreen)
    {
        if (_showTabsFullscreen == newShowTabsFullscreen)
        {
            return;
        }

        _showTabsFullscreen = newShowTabsFullscreen;

        // if we're currently in fullscreen, update tab view to make
        // sure tabs are given the correct visibility
        if (_isFullscreen)
        {
            _UpdateTabView();
        }
    }

    void TerminalPage::SetFullscreen(bool newFullscreen)
    {
        if (_isFullscreen == newFullscreen)
        {
            return;
        }
        _isFullscreen = newFullscreen;
        _UpdateTabView();
        FullscreenChanged.raise(*this, nullptr);
    }

    // Method Description:
    // - Updates the page's state for isMaximized when the window changes externally.
    void TerminalPage::Maximized(bool newMaximized)
    {
        _isMaximized = newMaximized;
    }

    // Method Description:
    // - Asks the window to change its maximized state.
    void TerminalPage::RequestSetMaximized(bool newMaximized)
    {
        if (_isMaximized == newMaximized)
        {
            return;
        }
        _isMaximized = newMaximized;
        ChangeMaximizeRequested.raise(*this, nullptr);
    }

    TerminalApp::IPaneContent TerminalPage::_makeSettingsContent()
    {
        if (auto app{ winrt::Windows::UI::Xaml::Application::Current().try_as<winrt::TerminalApp::App>() })
        {
            if (auto appPrivate{ winrt::get_self<implementation::App>(app) })
            {
                // Lazily load the Settings UI components so that we don't do it on startup.
                appPrivate->PrepareForSettingsUI();
            }
        }

        // Create the SUI pane content
        auto settingsContent{ winrt::make_self<SettingsPaneContent>(_settings) };
        auto sui = settingsContent->SettingsUI();
        _settingsMainPage = sui;

        sui.InitShellIntegrationRequested({ get_weak(), &TerminalPage::_OnSettingsInitShellIntegration });

        if (_hostingHwnd)
        {
            sui.SetHostingWindow(reinterpret_cast<uint64_t>(*_hostingHwnd));
        }

        // GH#8767 - let unhandled keys in the SUI try to run commands too.
        sui.KeyDown({ get_weak(), &TerminalPage::_KeyDownHandler });

        sui.OpenJson([weakThis{ get_weak() }](auto&& /*s*/, winrt::Microsoft::Terminal::Settings::Model::SettingsTarget e) {
            if (auto page{ weakThis.get() })
            {
                page->_LaunchSettings(e);
            }
        });

        sui.ShowLoadWarningsDialog([weakThis{ get_weak() }](auto&& /*s*/, const Windows::Foundation::Collections::IVectorView<winrt::Microsoft::Terminal::Settings::Model::SettingsLoadWarnings>& warnings) {
            if (auto page{ weakThis.get() })
            {
                page->ShowLoadWarningsDialog.raise(*page, warnings);
            }
        });

        return *settingsContent;
    }

    // Method Description:
    // - Creates a settings UI tab and focuses it. If there's already a settings UI tab open,
    //   just focus the existing one.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::OpenSettingsUI()
    {
        // If we're holding the settings tab's switch command, don't create a new one, switch to the existing one.
        if (!_settingsTab)
        {
            // Create the tab
            auto resultPane = std::make_shared<Pane>(_makeSettingsContent());
            _settingsTab = _CreateNewTabFromPane(resultPane);
        }
        else
        {
            _tabView.SelectedItem(_settingsTab.TabViewItem());
        }

    }

    // Method Description:
    // - Returns a com_ptr to the implementation type of the given tab if it's a Tab.
    //   If the tab is not a TerminalTab, returns nullptr.
    // Arguments:
    // - tab: the projected type of a Tab
    // Return Value:
    // - If the tab is a TerminalTab, a com_ptr to the implementation type.
    //   If the tab is not a TerminalTab, nullptr
    winrt::com_ptr<Tab> TerminalPage::_GetTabImpl(const TerminalApp::Tab& tab)
    {
        winrt::com_ptr<Tab> tabImpl;
        tabImpl.copy_from(winrt::get_self<Tab>(tab));
        return tabImpl;
    }

    // Method Description:
    // - Computes the delta for scrolling the tab's viewport.
    // Arguments:
    // - scrollDirection - direction (up / down) to scroll
    // - rowsToScroll - the number of rows to scroll
    // Return Value:
    // - delta - Signed delta, where a negative value means scrolling up.
    int TerminalPage::_ComputeScrollDelta(ScrollDirection scrollDirection, const uint32_t rowsToScroll)
    {
        return scrollDirection == ScrollUp ? -1 * rowsToScroll : rowsToScroll;
    }

    // Method Description:
    // - Reads system settings for scrolling (based on the step of the mouse scroll).
    // Upon failure fallbacks to default.
    // Return Value:
    // - The number of rows to scroll or a magic value of WHEEL_PAGESCROLL
    // indicating that we need to scroll an entire view height
    uint32_t TerminalPage::_ReadSystemRowsToScroll()
    {
        uint32_t systemRowsToScroll;
        if (!SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, &systemRowsToScroll, 0))
        {
            LOG_LAST_ERROR();

            // If SystemParametersInfoW fails, which it shouldn't, fall back to
            // Windows' default value.
            return DefaultRowsToScroll;
        }

        return systemRowsToScroll;
    }

    // Method Description:
    // - Displays a dialog stating the "Touch Keyboard and Handwriting Panel
    //   Service" is disabled.
    void TerminalPage::ShowKeyboardServiceWarning() const
    {
        if (!_IsMessageDismissed(InfoBarMessage::KeyboardServiceWarning))
        {
            if (const auto keyboardServiceWarningInfoBar = FindName(L"KeyboardServiceWarningInfoBar").try_as<MUX::Controls::InfoBar>())
            {
                keyboardServiceWarningInfoBar.IsOpen(true);
            }
        }
    }

    // Function Description:
    // - Helper function to get the OS-localized name for the "Touch Keyboard
    //   and Handwriting Panel Service". If we can't open up the service for any
    //   reason, then we'll just return the service's key, "TabletInputService".
    // Return Value:
    // - The OS-localized name for the TabletInputService
    winrt::hstring _getTabletServiceName()
    {
        wil::unique_schandle hManager{ OpenSCManagerW(nullptr, nullptr, 0) };

        if (LOG_LAST_ERROR_IF(!hManager.is_valid()))
        {
            return winrt::hstring{ TabletInputServiceKey };
        }

        DWORD cchBuffer = 0;
        const auto ok = GetServiceDisplayNameW(hManager.get(), TabletInputServiceKey.data(), nullptr, &cchBuffer);

        // Windows 11 doesn't have a TabletInputService.
        // (It was renamed to TextInputManagementService, because people kept thinking that a
        // service called "tablet-something" is system-irrelevant on PCs and can be disabled.)
        if (ok || GetLastError() != ERROR_INSUFFICIENT_BUFFER)
        {
            return winrt::hstring{ TabletInputServiceKey };
        }

        std::wstring buffer;
        cchBuffer += 1; // Add space for a null
        buffer.resize(cchBuffer);

        if (LOG_LAST_ERROR_IF(!GetServiceDisplayNameW(hManager.get(),
                                                      TabletInputServiceKey.data(),
                                                      buffer.data(),
                                                      &cchBuffer)))
        {
            return winrt::hstring{ TabletInputServiceKey };
        }
        return winrt::hstring{ buffer };
    }

    // Method Description:
    // - Return the fully-formed warning message for the
    //   "KeyboardServiceDisabled" InfoBar. This InfoBar is used to warn the user
    //   if the keyboard service is disabled, and uses the OS localization for
    //   the service's actual name. It's bound to the bar in XAML.
    // Return Value:
    // - The warning message, including the OS-localized service name.
    winrt::hstring TerminalPage::KeyboardServiceDisabledText()
    {
        const auto serviceName{ _getTabletServiceName() };
        const auto text{ RS_fmt(L"KeyboardServiceWarningText", serviceName) };
        return winrt::hstring{ text };
    }

    // Method Description:
    // - Update the RequestedTheme of the specified FrameworkElement and all its
    //   Parent elements. We need to do this so that we can actually theme all
    //   of the elements of the TeachingTip. See GH#9717
    // Arguments:
    // - element: The TeachingTip to set the theme on.
    // Return Value:
    // - <none>
    void TerminalPage::_UpdateTeachingTipTheme(winrt::Windows::UI::Xaml::FrameworkElement element)
    {
        auto theme{ _settings.GlobalSettings().CurrentTheme() };
        auto requestedTheme{ theme.RequestedTheme() };
        while (element)
        {
            element.RequestedTheme(requestedTheme);
            element = element.Parent().try_as<winrt::Windows::UI::Xaml::FrameworkElement>();
        }
    }

    // Method Description:
    // - Display the name and ID of this window in a TeachingTip. If the window
    //   has no name, the name will be presented as "<unnamed-window>".
    // - This can be invoked by either:
    //   * An identifyWindow action, that displays the info only for the current
    //     window
    //   * An identifyWindows action, that displays the info for all windows.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::IdentifyWindow()
    {
        // If we haven't ever loaded the TeachingTip, then do so now and
        // create the toast for it.
        if (_windowIdToast == nullptr)
        {
            if (auto tip{ FindName(L"WindowIdToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _windowIdToast = std::make_shared<Toast>(tip);
                // IsLightDismissEnabled == true is bugged and poorly interacts with multi-windowing.
                // It causes the tip to be immediately dismissed when another tip is opened in another window.
                tip.IsLightDismissEnabled(false);
                // Make sure to use the weak ref when setting up this callback.
                tip.Closed({ get_weak(), &TerminalPage::_FocusActiveControl });
            }
        }
        _UpdateTeachingTipTheme(WindowIdToast().try_as<winrt::Windows::UI::Xaml::FrameworkElement>());

        if (_windowIdToast != nullptr)
        {
            _windowIdToast->Open();
        }
    }

    void TerminalPage::ShowTerminalWorkingDirectory()
    {
        // If we haven't ever loaded the TeachingTip, then do so now and
        // create the toast for it.
        if (_windowCwdToast == nullptr)
        {
            if (auto tip{ FindName(L"WindowCwdToast").try_as<MUX::Controls::TeachingTip>() })
            {
                _windowCwdToast = std::make_shared<Toast>(tip);
                // Make sure to use the weak ref when setting up this
                // callback.
                tip.Closed({ get_weak(), &TerminalPage::_FocusActiveControl });
            }
        }
        _UpdateTeachingTipTheme(WindowCwdToast().try_as<winrt::Windows::UI::Xaml::FrameworkElement>());

        if (_windowCwdToast != nullptr)
        {
            _windowCwdToast->Open();
        }
    }

    // Method Description:
    // - Called when the user hits the "Ok" button on the WindowRenamer TeachingTip.
    // - Will raise an event that will bubble up to the monarch, asking if this
    //   name is acceptable.
    //   - we'll eventually get called back in TerminalPage::WindowName(hstring).
    // Arguments:
    // - <unused>
    // Return Value:
    // - <none>
    void TerminalPage::_WindowRenamerActionClick(const IInspectable& /*sender*/,
                                                 const IInspectable& /*eventArgs*/)
    {
        auto newName = WindowRenamerTextBox().Text();
        _RequestWindowRename(newName);
    }

    void TerminalPage::_RequestWindowRename(const winrt::hstring& newName)
    {
        auto request = winrt::make<implementation::RenameWindowRequestedArgs>(newName);
        // The WindowRenamer is _not_ a Toast - we want it to stay open until
        // the user dismisses it.
        if (WindowRenamer())
        {
            WindowRenamer().IsOpen(false);
        }
        RenameWindowRequested.raise(*this, request);
        // We can't just use request.Successful here, because the handler might
        // (will) be handling this asynchronously, so when control returns to
        // us, this hasn't actually been handled yet. We'll get called back in
        // RenameFailed if this fails.
        //
        // Theoretically we could do a IAsyncOperation<RenameWindowResult> kind
        // of thing with co_return winrt::make<RenameWindowResult>(false).
    }

    // Method Description:
    // - Used to track if the user pressed enter with the renamer open. If we
    //   immediately focus it after hitting Enter on the command palette, then
    //   the Enter keydown will dismiss the command palette and open the
    //   renamer, and then the enter keyup will go to the renamer. So we need to
    //   make sure both a down and up go to the renamer.
    // Arguments:
    // - e: the KeyRoutedEventArgs describing the key that was released
    // Return Value:
    // - <none>
    void TerminalPage::_WindowRenamerKeyDown(const IInspectable& /*sender*/,
                                             const winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs& e)
    {
        const auto key = e.OriginalKey();
        if (key == Windows::System::VirtualKey::Enter)
        {
            _renamerPressedEnter = true;
        }
    }

    // Method Description:
    // - Manually handle Enter and Escape for committing and dismissing a window
    //   rename. This is highly similar to the TabHeaderControl's KeyUp handler.
    // Arguments:
    // - e: the KeyRoutedEventArgs describing the key that was released
    // Return Value:
    // - <none>
    void TerminalPage::_WindowRenamerKeyUp(const IInspectable& sender,
                                           const winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs& e)
    {
        const auto key = e.OriginalKey();
        if (key == Windows::System::VirtualKey::Enter && _renamerPressedEnter)
        {
            // User is done making changes, close the rename box
            _WindowRenamerActionClick(sender, nullptr);
        }
        else if (key == Windows::System::VirtualKey::Escape)
        {
            // User wants to discard the changes they made
            WindowRenamerTextBox().Text(_WindowProperties.WindowName());
            WindowRenamer().IsOpen(false);
            _renamerPressedEnter = false;
        }
    }

    // Method Description:
    // - This function stops people from duplicating the base profile, because
    //   it gets ~ ~ weird ~ ~ when they do. Remove when TODO GH#5047 is done.
    Profile TerminalPage::GetClosestProfileForDuplicationOfProfile(const Profile& profile) const noexcept
    {
        if (profile == _settings.ProfileDefaults())
        {
            return _settings.FindProfile(_settings.GlobalSettings().DefaultProfile());
        }
        return profile;
    }

    // Function Description:
    // - Helper to launch a new WT instance elevated. It'll do this by spawning
    //   a helper process, that will ask the shell to elevate the process for
    //   us. This might cause a UAC prompt. The elevation is performed on a
    //   background thread, as to not block the UI thread.
    // Arguments:
    // - newTerminalArgs: A NewTerminalArgs describing the terminal instance
    //   that should be spawned. The Profile should be filled in with the GUID
    //   of the profile we want to launch.
    // Return Value:
    // - <none>
    // Important: Don't take the param by reference, since we'll be doing work
    // on another thread.
    void TerminalPage::_OpenElevatedWT(NewTerminalArgs newTerminalArgs)
    {
        // BODGY
        //
        // We're going to construct the commandline we want, then toss it to a
        // helper process called `elevate-shim.exe` that happens to live next to
        // us. elevate-shim.exe will be the one to call ShellExecute with the
        // args that we want (to elevate the given profile).
        //
        // We can't be the one to call ShellExecute ourselves. ShellExecute
        // requires that the calling process stays alive until the child is
        // spawned. However, in the case of something like `wt -p
        // AlwaysElevateMe`, then the original WT will try to ShellExecute a new
        // wt.exe (elevated) and immediately exit, preventing ShellExecute from
        // successfully spawning the elevated WT.

        std::filesystem::path exePath = wil::GetModuleFileNameW<std::wstring>(nullptr);
        exePath.replace_filename(L"elevate-shim.exe");

        // Build the commandline to pass to wt for this set of NewTerminalArgs
        auto cmdline{
            fmt::format(FMT_COMPILE(L"new-tab {}"), newTerminalArgs.ToCommandline())
        };

        wil::unique_process_information pi;
        STARTUPINFOW si{};
        si.cb = sizeof(si);

        LOG_IF_WIN32_BOOL_FALSE(CreateProcessW(exePath.c_str(),
                                               cmdline.data(),
                                               nullptr,
                                               nullptr,
                                               FALSE,
                                               0,
                                               nullptr,
                                               nullptr,
                                               &si,
                                               &pi));

        // TODO: GH#8592 - It may be useful to pop a Toast here in the original
        // Terminal window informing the user that the tab was opened in a new
        // window.
    }

    // Method Description:
    // - If the requested settings want us to elevate this new terminal
    //   instance, and we're not currently elevated, then open the new terminal
    //   as an elevated instance (using _OpenElevatedWT). Does nothing if we're
    //   already elevated, or if the control settings don't want to be elevated.
    // Arguments:
    // - newTerminalArgs: The NewTerminalArgs for this terminal instance
    // - controlSettings: The constructed TerminalSettingsCreateResult for this Terminal instance
    // - profile: The Profile we're using to launch this Terminal instance
    // Return Value:
    // - true iff we tossed this request to an elevated window. Callers can use
    //   this result to early-return if needed.
    bool TerminalPage::_maybeElevate(const NewTerminalArgs& newTerminalArgs,
                                     const Settings::TerminalSettingsCreateResult& controlSettings,
                                     const Profile& profile)
    {
        // When duplicating a tab there aren't any newTerminalArgs.
        if (!newTerminalArgs)
        {
            return false;
        }

        const auto defaultSettings = controlSettings.DefaultSettings();

        // If we don't even want to elevate we can return early.
        // If we're already elevated we can also return, because it doesn't get any more elevated than that.
        if (!defaultSettings->Elevate() || IsRunningElevated())
        {
            return false;
        }

        // Manually set the Profile of the NewTerminalArgs to the guid we've
        // resolved to. If there was a profile in the NewTerminalArgs, this
        // will be that profile's GUID. If there wasn't, then we'll use
        // whatever the default profile's GUID is.
        newTerminalArgs.Profile(::Microsoft::Console::Utils::GuidToString(profile.Guid()));
        newTerminalArgs.StartingDirectory(_evaluatePathForCwd(defaultSettings->StartingDirectory()));
        _OpenElevatedWT(newTerminalArgs);
        return true;
    }

    // Method Description:
    // - Handles the change of connection state.
    // If the connection state is failure show information bar suggesting to configure termination behavior
    // (unless user asked not to show this message again)
    // Arguments:
    // - sender: the ICoreState instance containing the connection state
    // Return Value:
    // - <none>
    safe_void_coroutine TerminalPage::_ConnectionStateChangedHandler(const IInspectable& sender, const IInspectable& /*args*/)
    {
        if (const auto coreState{ sender.try_as<winrt::Microsoft::Terminal::Control::ICoreState>() })
        {
            const auto newConnectionState = coreState.ConnectionState();
            const auto weak = get_weak();
            co_await wil::resume_foreground(Dispatcher());
            const auto strong = weak.get();
            if (!strong)
            {
                co_return;
            }

            _adjustProcessPriorityThrottled->Run();

            if (newConnectionState == ConnectionState::Failed && !_IsMessageDismissed(InfoBarMessage::CloseOnExitInfo))
            {
                if (const auto infoBar = FindName(L"CloseOnExitInfoBar").try_as<MUX::Controls::InfoBar>())
                {
                    infoBar.IsOpen(true);
                }
            }
        }
    }

    // Method Description:
    // - Persists the user's choice not to show information bar guiding to configure termination behavior.
    // Then hides this information buffer.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::_CloseOnExitInfoDismissHandler(const IInspectable& /*sender*/, const IInspectable& /*args*/) const
    {
        _DismissMessage(InfoBarMessage::CloseOnExitInfo);
        if (const auto infoBar = FindName(L"CloseOnExitInfoBar").try_as<MUX::Controls::InfoBar>())
        {
            infoBar.IsOpen(false);
        }
    }

    // Method Description:
    // - Persists the user's choice not to show information bar warning about "Touch keyboard and Handwriting Panel Service" disabled
    // Then hides this information buffer.
    // Arguments:
    // - <none>
    // Return Value:
    // - <none>
    void TerminalPage::_KeyboardServiceWarningInfoDismissHandler(const IInspectable& /*sender*/, const IInspectable& /*args*/) const
    {
        _DismissMessage(InfoBarMessage::KeyboardServiceWarning);
        if (const auto infoBar = FindName(L"KeyboardServiceWarningInfoBar").try_as<MUX::Controls::InfoBar>())
        {
            infoBar.IsOpen(false);
        }
    }

    // Method Description:
    // - Checks whether information bar message was dismissed earlier (in the application state)
    // Arguments:
    // - message: message to look for in the state
    // Return Value:
    // - true, if the message was dismissed
    bool TerminalPage::_IsMessageDismissed(const InfoBarMessage& message)
    {
        if (const auto dismissedMessages{ ApplicationState::SharedInstance().DismissedMessages() })
        {
            for (const auto& dismissedMessage : dismissedMessages)
            {
                if (dismissedMessage == message)
                {
                    return true;
                }
            }
        }
        return false;
    }

    // Method Description:
    // - Persists the user's choice to dismiss information bar message (in application state)
    // Arguments:
    // - message: message to dismiss
    // Return Value:
    // - <none>
    void TerminalPage::_DismissMessage(const InfoBarMessage& message)
    {
        const auto applicationState = ApplicationState::SharedInstance();
        std::vector<InfoBarMessage> messages;

        if (const auto values = applicationState.DismissedMessages())
        {
            messages.resize(values.Size());
            values.GetMany(0, messages);
        }

        if (std::none_of(messages.begin(), messages.end(), [&](const auto& m) { return m == message; }))
        {
            messages.emplace_back(message);
        }

        applicationState.DismissedMessages(std::move(messages));
    }

    void TerminalPage::_updateThemeColors()
    {
        if (_settings == nullptr)
        {
            return;
        }

        const auto theme = _settings.GlobalSettings().CurrentTheme();
        auto requestedTheme{ theme.RequestedTheme() };

        {
            _updatePaneResources(requestedTheme);

            for (const auto& tab : _tabs)
            {
                if (auto tabImpl{ _GetTabImpl(tab) })
                {
                    // The root pane will propagate the theme change to all its children.
                    if (const auto& rootPane{ tabImpl->GetRootPane() })
                    {
                        rootPane->UpdateResources(_paneResources);
                    }
                }
            }
        }

        const auto res = Application::Current().Resources();

        // Use our helper to lookup the theme-aware version of the resource.
        const auto tabViewBackgroundKey = winrt::box_value(L"TabViewBackground");
        const auto backgroundSolidBrush = ThemeLookup(res, requestedTheme, tabViewBackgroundKey).as<Media::SolidColorBrush>();

        til::color bgColor = backgroundSolidBrush.Color();

        Media::Brush terminalBrush{ nullptr };
        if (const auto tab{ _GetFocusedTabImpl() })
        {
            if (const auto& pane{ tab->GetActivePane() })
            {
                if (const auto& lastContent{ pane->GetLastFocusedContent() })
                {
                    terminalBrush = lastContent.BackgroundBrush();
                }
            }
        }

        // GH#19604: Get the theme's tabRow color to use as the acrylic tint.
        const auto tabRowBg{ theme.TabRow() ? (_activated ? theme.TabRow().Background() :
                                                            theme.TabRow().UnfocusedBackground()) :
                                              ThemeColor{ nullptr } };

        if (_settings.GlobalSettings().UseAcrylicInTabRow() && (_activated || _settings.GlobalSettings().EnableUnfocusedAcrylic()))
        {
            if (tabRowBg)
            {
                bgColor = ThemeColor::ColorFromBrush(tabRowBg.Evaluate(res, terminalBrush, true));
            }

            const auto acrylicBrush = Media::AcrylicBrush();
            acrylicBrush.BackgroundSource(Media::AcrylicBackgroundSource::HostBackdrop);
            acrylicBrush.FallbackColor(bgColor);
            acrylicBrush.TintColor(bgColor);
            acrylicBrush.TintOpacity(0.5);

            TitlebarBrush(acrylicBrush);
        }
        else if (tabRowBg)
        {
            const auto themeBrush{ tabRowBg.Evaluate(res, terminalBrush, true) };
            bgColor = ThemeColor::ColorFromBrush(themeBrush);
            // If the tab content returned nullptr for the terminalBrush, we
            // _don't_ want to use it as the tab row background. We want to just
            // use the default tab row background.
            TitlebarBrush(themeBrush ? themeBrush : backgroundSolidBrush);
        }
        else
        {
            // Nothing was set in the theme - fall back to our original `TabViewBackground` color.
            TitlebarBrush(backgroundSolidBrush);
        }

        if (!_settings.GlobalSettings().ShowTabsInTitlebar())
        {
            _tabRow.Background(TitlebarBrush());
        }

        // Second: Update the colors of our individual TabViewItems. This
        // applies tab.background to the tabs via Tab::ThemeColor.
        //
        // Do this second, so that we already know the bgColor of the titlebar.
        {
            const auto tabBackground = theme.Tab() ? theme.Tab().Background() : nullptr;
            const auto tabUnfocusedBackground = theme.Tab() ? theme.Tab().UnfocusedBackground() : nullptr;
            for (const auto& tab : _tabs)
            {
                winrt::com_ptr<Tab> tabImpl;
                tabImpl.copy_from(winrt::get_self<Tab>(tab));
                tabImpl->ThemeColor(tabBackground, tabUnfocusedBackground, bgColor);
            }
        }
        // Update the new tab button to have better contrast with the new color.
        // In theory, it would be convenient to also change these for the
        // inactive tabs as well, but we're leaving that as a follow up.
        _SetNewTabButtonColor(bgColor, bgColor);

        // Third: the window frame. This is basically the same logic as the tab row background.
        // We'll set our `FrameBrush` property, for the window to later use.
        const auto windowTheme{ theme.Window() };
        if (auto windowFrame{ windowTheme ? (_activated ? windowTheme.Frame() :
                                                          windowTheme.UnfocusedFrame()) :
                                            ThemeColor{ nullptr } })
        {
            const auto themeBrush{ windowFrame.Evaluate(res, terminalBrush, true) };
            FrameBrush(themeBrush);
        }
        else
        {
            // Nothing was set in the theme - fall back to null. The window will
            // use that as an indication to use the default window frame.
            FrameBrush(nullptr);
        }
    }

    // Function Description:
    // - Attempts to load some XAML resources that Panes will need. This includes:
    //   * The Color they'll use for active Panes's borders - SystemAccentColor
    //   * The Brush they'll use for inactive Panes - TabViewBackground (to match the
    //     color of the titlebar)
    // Arguments:
    // - requestedTheme: this should be the currently active Theme for the app
    // Return Value:
    // - <none>
    void TerminalPage::_updatePaneResources(const winrt::Windows::UI::Xaml::ElementTheme& requestedTheme)
    {
        const auto res = Application::Current().Resources();
        const auto accentColorKey = winrt::box_value(L"SystemAccentColor");
        if (res.HasKey(accentColorKey))
        {
            const auto colorFromResources = ThemeLookup(res, requestedTheme, accentColorKey);
            // If SystemAccentColor is _not_ a Color for some reason, use
            // Transparent as the color, so we don't do this process again on
            // the next pane (by leaving s_focusedBorderBrush nullptr)
            auto actualColor = winrt::unbox_value_or<Color>(colorFromResources, Colors::Black());
            _paneResources.focusedBorderBrush = SolidColorBrush(actualColor);
        }
        else
        {
            // DON'T use Transparent here - if it's "Transparent", then it won't
            // be able to hittest for clicks, and then clicking on the border
            // will eat focus.
            _paneResources.focusedBorderBrush = SolidColorBrush{ Colors::Black() };
        }

        const auto unfocusedBorderBrushKey = winrt::box_value(L"UnfocusedBorderBrush");
        if (res.HasKey(unfocusedBorderBrushKey))
        {
            // MAKE SURE TO USE ThemeLookup, so that we get the correct resource for
            // the requestedTheme, not just the value from the resources (which
            // might not respect the settings' requested theme)
            auto obj = ThemeLookup(res, requestedTheme, unfocusedBorderBrushKey);
            _paneResources.unfocusedBorderBrush = obj.try_as<winrt::Windows::UI::Xaml::Media::SolidColorBrush>();
        }
        else
        {
            // DON'T use Transparent here - if it's "Transparent", then it won't
            // be able to hittest for clicks, and then clicking on the border
            // will eat focus.
            _paneResources.unfocusedBorderBrush = SolidColorBrush{ Colors::Black() };
        }

        const auto broadcastColorKey = winrt::box_value(L"BroadcastPaneBorderColor");
        if (res.HasKey(broadcastColorKey))
        {
            // MAKE SURE TO USE ThemeLookup
            auto obj = ThemeLookup(res, requestedTheme, broadcastColorKey);
            _paneResources.broadcastBorderBrush = obj.try_as<winrt::Windows::UI::Xaml::Media::SolidColorBrush>();
        }
        else
        {
            // DON'T use Transparent here - if it's "Transparent", then it won't
            // be able to hittest for clicks, and then clicking on the border
            // will eat focus.
            _paneResources.broadcastBorderBrush = SolidColorBrush{ Colors::Black() };
        }

        const auto agentColorKey = winrt::box_value(L"AgentPaneBorderColor");
        if (res.HasKey(agentColorKey))
        {
            auto obj = ThemeLookup(res, requestedTheme, agentColorKey);
            _paneResources.agentFocusedBorderBrush = obj.try_as<winrt::Windows::UI::Xaml::Media::SolidColorBrush>();
        }
        else
        {
            _paneResources.agentFocusedBorderBrush = SolidColorBrush{ Colors::Black() };
        }
    }

    void TerminalPage::_adjustProcessPriority() const
    {
        // Windowing is single-threaded, so this will not cause a race condition.
        static uint64_t s_lastUpdateHash{ 0 };
        static bool s_supported{ true };

        if (!s_supported || !_hostingHwnd.has_value())
        {
            return;
        }

        std::array<HANDLE, 32> processes;
        auto it = processes.begin();
        const auto end = processes.end();

        auto&& appendFromControl = [&](auto&& control) {
            if (it == end)
            {
                return;
            }
            if (control)
            {
                if (const auto conn{ control.Connection() })
                {
                    if (const auto pty{ conn.try_as<winrt::Microsoft::Terminal::TerminalConnection::ConptyConnection>() })
                    {
                        if (const uint64_t process{ pty.RootProcessHandle() }; process != 0)
                        {
                            *it++ = reinterpret_cast<HANDLE>(process);
                        }
                    }
                }
            }
        };

        auto&& appendFromTab = [&](auto&& tabImpl) {
            if (const auto pane{ tabImpl->GetRootPane() })
            {
                pane->WalkTree([&](auto&& child) {
                    if (const auto& control{ child->GetTerminalControl() })
                    {
                        appendFromControl(control);
                    }
                });
            }
        };

        if (!_activated)
        {
            // When a window is out of focus, we want to attach all of the processes
            // under it to the window so they all go into the background at the same time.
            for (auto&& tab : _tabs)
            {
                if (auto tabImpl{ _GetTabImpl(tab) })
                {
                    appendFromTab(tabImpl);
                }
            }
        }
        else
        {
            // When a window is in focus, propagate our foreground boost (if we have one)
            // to current all panes in the current tab.
            if (auto tabImpl{ _GetFocusedTabImpl() })
            {
                appendFromTab(tabImpl);
            }
        }

        const auto count{ gsl::narrow_cast<DWORD>(it - processes.begin()) };
        const auto hash = til::hash((void*)processes.data(), count * sizeof(HANDLE));

        if (hash == s_lastUpdateHash)
        {
            return;
        }

        s_lastUpdateHash = hash;
        const auto hr = TerminalTrySetWindowAssociatedProcesses(_hostingHwnd.value(), count, count ? processes.data() : nullptr);

        if (S_FALSE == hr)
        {
            // Don't bother trying again or logging. The wrapper tells us it's unsupported.
            s_supported = false;
            return;
        }

        TraceLoggingWrite(
            g_hTerminalAppProvider,
            "CalledNewQoSAPI",
            TraceLoggingValue(reinterpret_cast<uintptr_t>(_hostingHwnd.value()), "hwnd"),
            TraceLoggingValue(count),
            TraceLoggingHResult(hr));
#ifdef _DEBUG
        OutputDebugStringW(fmt::format(FMT_COMPILE(L"Submitted {} processes to TerminalTrySetWindowAssociatedProcesses; return=0x{:08x}\n"), count, hr).c_str());
#endif
    }

    void TerminalPage::WindowActivated(const bool activated)
    {
        // Stash if we're activated. Use that when we reload
        // the settings, change active panes, etc.
        _activated = activated;
        _updateThemeColors();

        _adjustProcessPriorityThrottled->Run();

        if (const auto& tab{ _GetFocusedTabImpl() })
        {
            if (tab->TabStatus().IsInputBroadcastActive())
            {
                tab->GetRootPane()->WalkTree([activated](const auto& p) {
                    if (const auto& control{ p->GetTerminalControl() })
                    {
                        control.CursorVisibility(activated ?
                                                     Microsoft::Terminal::Control::CursorDisplayState::Shown :
                                                     Microsoft::Terminal::Control::CursorDisplayState::Default);
                    }
                });
            }
        }
    }

    safe_void_coroutine TerminalPage::_ControlCompletionsChangedHandler(const IInspectable sender,
                                                                        const CompletionsChangedEventArgs args)
    {
        // This won't even get hit if the velocity flag is disabled - we gate
        // registering for the event based off of
        // Feature_ShellCompletions::IsEnabled back in _RegisterTerminalEvents

        // User must explicitly opt-in on Preview builds
        if (!_settings.GlobalSettings().EnableShellCompletionMenu())
        {
            co_return;
        }

        // Parse the json string into a collection of actions
        try
        {
            auto commandsCollection = Command::ParsePowerShellMenuComplete(args.MenuJson(),
                                                                           args.ReplacementLength());

            auto weakThis{ get_weak() };
            Dispatcher().RunAsync(CoreDispatcherPriority::Normal, [weakThis, commandsCollection, sender]() {
                // On the UI thread...
                if (const auto& page{ weakThis.get() })
                {
                    // Open the Suggestions UI with the commands from the control
                    page->_OpenSuggestions(sender.try_as<TermControl>(), commandsCollection, SuggestionsMode::Menu, L"");
                }
            });
        }
        CATCH_LOG();
    }

    void TerminalPage::_OpenSuggestions(
        const TermControl& sender,
        IVector<Command> commandsCollection,
        winrt::TerminalApp::SuggestionsMode mode,
        winrt::hstring filterText)

    {
        // ON THE UI THREAD
        assert(Dispatcher().HasThreadAccess());

        if (commandsCollection == nullptr)
        {
            return;
        }
        if (commandsCollection.Size() == 0)
        {
            if (const auto p = SuggestionsElement())
            {
                p.Visibility(Visibility::Collapsed);
            }
            return;
        }

        const auto& control{ sender ? sender : _GetActiveControl() };
        if (!control)
        {
            return;
        }

        const auto& sxnUi{ LoadSuggestionsUI() };

        const auto characterSize{ control.CharacterDimensions() };
        // This is in control-relative space. We'll need to convert it to page-relative space.
        const auto cursorPos{ control.CursorPositionInDips() };
        const auto controlTransform = control.TransformToVisual(this->Root());
        const auto realCursorPos{ controlTransform.TransformPoint({ cursorPos.X, cursorPos.Y }) }; // == controlTransform + cursorPos
        const Windows::Foundation::Size windowDimensions{ gsl::narrow_cast<float>(ActualWidth()), gsl::narrow_cast<float>(ActualHeight()) };

        sxnUi.Open(mode,
                   commandsCollection,
                   filterText,
                   realCursorPos,
                   windowDimensions,
                   characterSize.Height);
    }

    void TerminalPage::_PopulateContextMenu(const TermControl& control,
                                            const MUX::Controls::CommandBarFlyout& menu,
                                            const bool withSelection)
    {
        // withSelection can be used to add actions that only appear if there's
        // selected text, like "search the web"

        if (!control || !menu)
        {
            return;
        }

        // Helper lambda for dispatching an ActionAndArgs onto the
        // ShortcutActionDispatch. Used below to wire up each menu entry to the
        // respective action.

        auto weak = get_weak();
        auto makeCallback = [weak](const ActionAndArgs& actionAndArgs) {
            return [weak, actionAndArgs](auto&&, auto&&) {
                if (auto page{ weak.get() })
                {
                    page->_actionDispatch->DoAction(actionAndArgs);
                }
            };
        };

        auto makeItem = [&makeCallback](const winrt::hstring& label,
                                        const winrt::hstring& icon,
                                        const auto& action,
                                        auto& targetMenu) {
            AppBarButton button{};

            if (!icon.empty())
            {
                auto iconElement = UI::IconPathConverter::IconWUX(icon);
                Automation::AutomationProperties::SetAccessibilityView(iconElement, Automation::Peers::AccessibilityView::Raw);
                button.Icon(iconElement);
            }

            button.Label(label);
            button.Click(makeCallback(action));
            targetMenu.SecondaryCommands().Append(button);
        };

        auto makeMenuItem = [](const winrt::hstring& label,
                               const winrt::hstring& icon,
                               const auto& subMenu,
                               auto& targetMenu) {
            AppBarButton button{};

            if (!icon.empty())
            {
                auto iconElement = UI::IconPathConverter::IconWUX(icon);
                Automation::AutomationProperties::SetAccessibilityView(iconElement, Automation::Peers::AccessibilityView::Raw);
                button.Icon(iconElement);
            }

            button.Label(label);
            button.Flyout(subMenu);
            targetMenu.SecondaryCommands().Append(button);
        };

        auto makeContextItem = [&makeCallback](const winrt::hstring& label,
                                               const winrt::hstring& icon,
                                               const winrt::hstring& tooltip,
                                               const auto& action,
                                               const auto& subMenu,
                                               auto& targetMenu) {
            AppBarButton button{};

            if (!icon.empty())
            {
                auto iconElement = UI::IconPathConverter::IconWUX(icon);
                Automation::AutomationProperties::SetAccessibilityView(iconElement, Automation::Peers::AccessibilityView::Raw);
                button.Icon(iconElement);
            }

            button.Label(label);
            button.Click(makeCallback(action));
            WUX::Controls::ToolTipService::SetToolTip(button, box_value(tooltip));
            button.ContextFlyout(subMenu);
            targetMenu.SecondaryCommands().Append(button);
        };

        const auto focusedProfile = _GetFocusedTabImpl()->GetFocusedProfile();
        auto separatorItem = AppBarSeparator{};
        auto activeProfiles = _settings.ActiveProfiles();
        auto activeProfileCount = gsl::narrow_cast<int>(activeProfiles.Size());
        MUX::Controls::CommandBarFlyout splitPaneMenu{};

        // Wire up each item to the action that should be performed. By actually
        // connecting these to actions, we ensure the implementation is
        // consistent. This also leaves room for customizing this menu with
        // actions in the future.

        makeItem(RS_(L"DuplicateTabText"), L"\xF5ED", ActionAndArgs{ ShortcutAction::DuplicateTab, nullptr }, menu);

        const auto splitPaneRightText = RS_(L"SplitPaneRightText");
        const auto splitPaneDownText = RS_(L"SplitPaneDownText");
        const auto splitPaneUpText = RS_(L"SplitPaneUpText");
        const auto splitPaneLeftText = RS_(L"SplitPaneLeftText");
        const auto splitPaneToolTipText = RS_(L"SplitPaneToolTipText");

        // GetFocusedProfile can return null if no child of the focused tab
        // was the last control to be focused (e.g. transient focus states).
        // Skip the "duplicate current pane" entries when that happens —
        // calling .Name()/.Icon() on a null IProfile crashes with AV in
        // consume_*<IProfile>::Icon().
        if (focusedProfile)
        {
            const auto focusedProfileName = focusedProfile.Name();
            const auto focusedProfileIcon = focusedProfile.Icon().Resolved();
            const auto splitPaneDuplicateText = RS_(L"SplitPaneDuplicateText") + L" " + focusedProfileName; // SplitPaneDuplicateText

            MUX::Controls::CommandBarFlyout splitPaneContextMenu{};
            makeItem(splitPaneRightText, focusedProfileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Duplicate, SplitDirection::Right, .5, nullptr } }, splitPaneContextMenu);
            makeItem(splitPaneDownText, focusedProfileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Duplicate, SplitDirection::Down, .5, nullptr } }, splitPaneContextMenu);
            makeItem(splitPaneUpText, focusedProfileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Duplicate, SplitDirection::Up, .5, nullptr } }, splitPaneContextMenu);
            makeItem(splitPaneLeftText, focusedProfileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Duplicate, SplitDirection::Left, .5, nullptr } }, splitPaneContextMenu);

            makeContextItem(splitPaneDuplicateText, focusedProfileIcon, splitPaneToolTipText, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Duplicate, SplitDirection::Automatic, .5, nullptr } }, splitPaneContextMenu, splitPaneMenu);

            // Separator between the "duplicate current" group and the per-profile list.
            splitPaneMenu.SecondaryCommands().Append(AppBarSeparator{});
        }

        for (auto profileIndex = 0; profileIndex < activeProfileCount; profileIndex++)
        {
            const auto profile = activeProfiles.GetAt(profileIndex);
            const auto profileName = profile.Name();
            const auto profileIcon = profile.Icon().Resolved();

            NewTerminalArgs args{};
            args.Profile(profileName);

            MUX::Controls::CommandBarFlyout splitPaneContextMenu{};
            makeItem(splitPaneRightText, profileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Right, .5, args } }, splitPaneContextMenu);
            makeItem(splitPaneDownText, profileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Down, .5, args } }, splitPaneContextMenu);
            makeItem(splitPaneUpText, profileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Up, .5, args } }, splitPaneContextMenu);
            makeItem(splitPaneLeftText, profileIcon, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Left, .5, args } }, splitPaneContextMenu);

            makeContextItem(profileName, profileIcon, splitPaneToolTipText, ActionAndArgs{ ShortcutAction::SplitPane, SplitPaneArgs{ SplitType::Manual, SplitDirection::Automatic, .5, args } }, splitPaneContextMenu, splitPaneMenu);
        }

        makeMenuItem(RS_(L"SplitPaneText"), L"\xF246", splitPaneMenu, menu);

        // Only wire up "Close Pane" if there's multiple panes.
        if (_GetFocusedTabImpl()->GetLeafPaneCount() > 1)
        {
            MUX::Controls::CommandBarFlyout swapPaneMenu{};
            const auto rootPane = _GetFocusedTabImpl()->GetRootPane();
            const auto mruPanes = _GetFocusedTabImpl()->GetMruPanes();
            auto activePane = _GetFocusedTabImpl()->GetActivePane();
            rootPane->WalkTree([&](auto p) {
                if (const auto& c{ p->GetTerminalControl() })
                {
                    if (c == control)
                    {
                        activePane = p;
                    }
                }
            });

            if (auto neighbor = rootPane->NavigateDirection(activePane, FocusDirection::Down, mruPanes))
            {
                makeItem(RS_(L"SwapPaneDownText"), neighbor->GetProfile().Icon().Resolved(), ActionAndArgs{ ShortcutAction::SwapPane, SwapPaneArgs{ FocusDirection::Down } }, swapPaneMenu);
            }

            if (auto neighbor = rootPane->NavigateDirection(activePane, FocusDirection::Right, mruPanes))
            {
                makeItem(RS_(L"SwapPaneRightText"), neighbor->GetProfile().Icon().Resolved(), ActionAndArgs{ ShortcutAction::SwapPane, SwapPaneArgs{ FocusDirection::Right } }, swapPaneMenu);
            }

            if (auto neighbor = rootPane->NavigateDirection(activePane, FocusDirection::Up, mruPanes))
            {
                makeItem(RS_(L"SwapPaneUpText"), neighbor->GetProfile().Icon().Resolved(), ActionAndArgs{ ShortcutAction::SwapPane, SwapPaneArgs{ FocusDirection::Up } }, swapPaneMenu);
            }

            if (auto neighbor = rootPane->NavigateDirection(activePane, FocusDirection::Left, mruPanes))
            {
                makeItem(RS_(L"SwapPaneLeftText"), neighbor->GetProfile().Icon().Resolved(), ActionAndArgs{ ShortcutAction::SwapPane, SwapPaneArgs{ FocusDirection::Left } }, swapPaneMenu);
            }

            makeMenuItem(RS_(L"SwapPaneText"), L"\xF1CB", swapPaneMenu, menu);

            makeItem(RS_(L"TogglePaneZoomText"), L"\xE8A3", ActionAndArgs{ ShortcutAction::TogglePaneZoom, nullptr }, menu);
            makeItem(RS_(L"CloseOtherPanesText"), L"\xE89F", ActionAndArgs{ ShortcutAction::CloseOtherPanes, nullptr }, menu);
            makeItem(RS_(L"PaneClose"), L"\xE89F", ActionAndArgs{ ShortcutAction::ClosePane, nullptr }, menu);
        }

        if (control.ConnectionState() >= ConnectionState::Closed)
        {
            makeItem(RS_(L"RestartConnectionText"), L"\xE72C", ActionAndArgs{ ShortcutAction::RestartConnection, nullptr }, menu);
        }

        if (withSelection)
        {
            makeItem(RS_(L"SearchWebText"), L"\xF6FA", ActionAndArgs{ ShortcutAction::SearchForText, nullptr }, menu);
        }

        makeItem(RS_(L"TabClose"), L"\xE711", ActionAndArgs{ ShortcutAction::CloseTab, CloseTabArgs{ _GetFocusedTabIndex().value() } }, menu);
    }

    void TerminalPage::_PopulateQuickFixMenu(const TermControl& control,
                                             const Controls::MenuFlyout& menu)
    {
        if (!control || !menu)
        {
            return;
        }

        // Helper lambda for dispatching a SendInput ActionAndArgs onto the
        // ShortcutActionDispatch. Used below to wire up each menu entry to the
        // respective action. Then clear the quick fix menu.
        auto weak = get_weak();
        auto makeCallback = [weak](const hstring& suggestion) {
            return [weak, suggestion](auto&&, auto&&) {
                if (auto page{ weak.get() })
                {
                    const auto actionAndArgs = ActionAndArgs{ ShortcutAction::SendInput, SendInputArgs{ hstring{ L"\u0003" } + suggestion } };
                    page->_actionDispatch->DoAction(actionAndArgs);
                    if (auto ctrl = page->_GetActiveControl())
                    {
                        ctrl.ClearQuickFix();
                    }

                    TraceLoggingWrite(
                        g_hTerminalAppProvider,
                        "QuickFixSuggestionUsed",
                        TraceLoggingDescription("Event emitted when a winget suggestion from is used"),
                        TraceLoggingValue("QuickFixMenu", "Source"),
                        TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                        TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
                }
            };
        };

        // Wire up each item to the action that should be performed. By actually
        // connecting these to actions, we ensure the implementation is
        // consistent. This also leaves room for customizing this menu with
        // actions in the future.

        menu.Items().Clear();
        const auto quickFixes = control.CommandHistory().QuickFixes();
        for (const auto& qf : quickFixes)
        {
            MenuFlyoutItem item{};

            auto iconElement = UI::IconPathConverter::IconWUX(L"\ue74c");
            Automation::AutomationProperties::SetAccessibilityView(iconElement, Automation::Peers::AccessibilityView::Raw);
            item.Icon(iconElement);

            item.Text(qf);
            item.Click(makeCallback(qf));
            ToolTipService::SetToolTip(item, box_value(qf));
            menu.Items().Append(item);
        }
    }

    // Handler for our WindowProperties's PropertyChanged event. We'll use this
    // to pop the "Identify Window" toast when the user renames our window.
    void TerminalPage::_windowPropertyChanged(const IInspectable& /*sender*/, const WUX::Data::PropertyChangedEventArgs& args)
    {
        if (args.PropertyName() != L"WindowName")
        {
            return;
        }

        // DON'T display the confirmation if this is the name we were
        // given on startup!
        if (_startupState == StartupState::Initialized)
        {
            IdentifyWindow();
        }
    }

    void TerminalPage::_onTabDragStarting(const winrt::Microsoft::UI::Xaml::Controls::TabView&,
                                          const winrt::Microsoft::UI::Xaml::Controls::TabViewTabDragStartingEventArgs& e)
    {
        // Get the tab impl from this event.
        const auto eventTab = e.Tab();
        const auto tabBase = _GetTabByTabViewItem(eventTab);
        winrt::com_ptr<Tab> tabImpl;
        tabImpl.copy_from(winrt::get_self<Tab>(tabBase));
        if (tabImpl)
        {
            // First: stash the tab we started dragging.
            // We're going to be asked for this.
            _stashed.draggedTab = tabImpl;

            // Stash the offset from where we started the drag to the
            // tab's origin. We'll use that offset in the future to help
            // position the dropped window.
            const auto inverseScale = 1.0f / static_cast<float>(eventTab.XamlRoot().RasterizationScale());
            POINT cursorPos;
            GetCursorPos(&cursorPos);
            ScreenToClient(*_hostingHwnd, &cursorPos);
            _stashed.dragOffset.X = cursorPos.x * inverseScale;
            _stashed.dragOffset.Y = cursorPos.y * inverseScale;

            // Into the DataPackage, let's stash our own window ID.
            const auto id{ _WindowProperties.WindowId() };

            // Get our PID
            const auto pid{ GetCurrentProcessId() };

            e.Data().Properties().Insert(L"windowId", winrt::box_value(id));
            e.Data().Properties().Insert(L"pid", winrt::box_value<uint32_t>(pid));
            e.Data().RequestedOperation(DataPackageOperation::Move);

            // The next thing that will happen:
            //  * Another TerminalPage will get a TabStripDragOver, then get a
            //    TabStripDrop
            //    * This will be handled by the _other_ page asking the monarch
            //      to ask us to send our content to them.
            //  * We'll get a TabDroppedOutside to indicate that this tab was
            //    dropped _not_ on a TabView.
            //    * This will be handled by _onTabDroppedOutside, which will
            //      raise a MoveContent (to a new window) event.
        }
    }

    void TerminalPage::_onTabStripDragOver(const winrt::Windows::Foundation::IInspectable& /*sender*/,
                                           const winrt::Windows::UI::Xaml::DragEventArgs& e)
    {
        // We must mark that we can accept the drag/drop. The system will never
        // call TabStripDrop on us if we don't indicate that we're willing.
        const auto& props{ e.DataView().Properties() };
        if (props.HasKey(L"windowId") &&
            props.HasKey(L"pid") &&
            (winrt::unbox_value_or<uint32_t>(props.TryLookup(L"pid"), 0u) == GetCurrentProcessId()))
        {
            e.AcceptedOperation(DataPackageOperation::Move);
        }

        // You may think to yourself, this is a great place to increase the
        // width of the TabView artificially, to make room for the new tab item.
        // However, we'll never get a message that the tab left the tab view
        // (without being dropped). So there's no good way to resize back down.
    }

    // Method Description:
    // - Called on the TARGET of a tab drag/drop. We'll unpack the DataPackage
    //   to find who the tab came from. We'll then ask the Monarch to ask the
    //   sender to move that tab to us.
    void TerminalPage::_onTabStripDrop(winrt::Windows::Foundation::IInspectable /*sender*/,
                                       winrt::Windows::UI::Xaml::DragEventArgs e)
    {
        // Get the PID and make sure it is the same as ours.
        if (const auto& pidObj{ e.DataView().Properties().TryLookup(L"pid") })
        {
            const auto pid{ winrt::unbox_value_or<uint32_t>(pidObj, 0u) };
            if (pid != GetCurrentProcessId())
            {
                // The PID doesn't match ours. We can't handle this drop.
                return;
            }
        }
        else
        {
            // No PID? We can't handle this drop. Bail.
            return;
        }

        const auto& windowIdObj{ e.DataView().Properties().TryLookup(L"windowId") };
        if (windowIdObj == nullptr)
        {
            // No windowId? Bail.
            return;
        }
        const uint64_t src{ winrt::unbox_value<uint64_t>(windowIdObj) };

        // Figure out where in the tab strip we're dropping this tab. Add that
        // index to the request. This is largely taken from the WinUI sample
        // app.

        // First we need to get the position in the List to drop to
        auto index = -1;

        // Determine which items in the list our pointer is between.
        for (auto i = 0u; i < _tabView.TabItems().Size(); i++)
        {
            if (const auto& item{ _tabView.ContainerFromIndex(i).try_as<winrt::MUX::Controls::TabViewItem>() })
            {
                const auto posX{ e.GetPosition(item).X }; // The point of the drop, relative to the tab
                const auto itemWidth{ item.ActualWidth() }; // The right of the tab
                // If the drag point is on the left half of the tab, then insert here.
                if (posX < itemWidth / 2)
                {
                    index = i;
                    break;
                }
            }
        }

        // `this` is safe to use
        const auto request = winrt::make_self<RequestReceiveContentArgs>(src, _WindowProperties.WindowId(), index);

        // This will go up to the monarch, who will then dispatch the request
        // back down to the source TerminalPage, who will then perform a
        // RequestMoveContent to move their tab to us.
        RequestReceiveContent.raise(*this, *request);
    }

    // Method Description:
    // - This is called on the drag/drop SOURCE TerminalPage, when the monarch has
    //   requested that we send our tab to another window. We'll need to
    //   serialize the tab, and send it to the monarch, who will then send it to
    //   the destination window.
    // - Fortunately, sending the tab is basically just a MoveTab action, so we
    //   can largely reuse that.
    void TerminalPage::SendContentToOther(winrt::TerminalApp::RequestReceiveContentArgs args)
    {
        // validate that we're the source window of the tab in this request
        if (args.SourceWindow() != _WindowProperties.WindowId())
        {
            return;
        }
        if (!_stashed.draggedTab)
        {
            return;
        }

        _sendDraggedTabToWindow(winrt::to_hstring(args.TargetWindow()), args.TabIndex(), std::nullopt);
    }

    void TerminalPage::_onTabDroppedOutside(winrt::IInspectable /*sender*/,
                                            winrt::MUX::Controls::TabViewTabDroppedOutsideEventArgs /*e*/)
    {
        // Get the current pointer point from the CoreWindow
        const auto& pointerPoint{ CoreWindow::GetForCurrentThread().PointerPosition() };

        // This is called when a tab FROM OUR WINDOW was dropped outside the
        // tabview. We already know which tab was being dragged. We'll just
        // invoke a moveTab action with the target window being -1. That will
        // force the creation of a new window.

        if (!_stashed.draggedTab)
        {
            return;
        }

        // We need to convert the pointer point to a point that we can use
        // to position the new window. We'll use the drag offset from before
        // so that the tab in the new window is positioned so that it's
        // basically still directly under the cursor.

        // -1 is the magic number for "new window"
        // 0 as the tab index, because we don't care. It's making a new window. It'll be the only tab.
        const winrt::Windows::Foundation::Point adjusted = {
            pointerPoint.X - _stashed.dragOffset.X,
            pointerPoint.Y - _stashed.dragOffset.Y,
        };
        _sendDraggedTabToWindow(winrt::hstring{ L"-1" }, 0, adjusted);
    }

    void TerminalPage::_sendDraggedTabToWindow(const winrt::hstring& windowId,
                                               const uint32_t tabIndex,
                                               std::optional<winrt::Windows::Foundation::Point> dragPoint)
    {
        auto startupActions = _stashed.draggedTab->BuildStartupActions(BuildStartupKind::Content);
        _DetachTabFromWindow(_stashed.draggedTab);

        _MoveContent(std::move(startupActions), windowId, tabIndex, dragPoint);
        // _RemoveTab will make sure to null out the _stashed.draggedTab.
        // movingAway=true so an agent pane in this tab survives the drag —
        // the target window will rebind it via `tab_renamed`.
        _RemoveTab(*_stashed.draggedTab, /*movingAway*/ true);
    }

    /// <summary>
    /// Creates a sub flyout menu for profile items in the split button menu that when clicked will show a menu item for
    /// Run as Administrator
    /// </summary>
    /// <param name="profileIndex">The index for the profileMenuItem</param>
    /// <returns>MenuFlyout that will show when the context is request on a profileMenuItem</returns>
    WUX::Controls::MenuFlyout TerminalPage::_CreateRunAsAdminFlyout(int profileIndex)
    {
        // Create the MenuFlyout and set its placement
        WUX::Controls::MenuFlyout profileMenuItemFlyout{};
        profileMenuItemFlyout.Placement(WUX::Controls::Primitives::FlyoutPlacementMode::BottomEdgeAlignedRight);

        // Create the menu item and an icon to use in the menu
        WUX::Controls::MenuFlyoutItem runAsAdminItem{};
        WUX::Controls::FontIcon adminShieldIcon{};

        adminShieldIcon.Glyph(L"\xEA18");
        adminShieldIcon.FontFamily(Media::FontFamily{ L"Segoe Fluent Icons, Segoe MDL2 Assets" });

        runAsAdminItem.Icon(adminShieldIcon);
        runAsAdminItem.Text(RS_(L"RunAsAdminFlyout/Text"));

        // Click handler for the flyout item
        runAsAdminItem.Click([profileIndex, weakThis{ get_weak() }](auto&&, auto&&) {
            if (auto page{ weakThis.get() })
            {
                TraceLoggingWrite(
                    g_hTerminalAppProvider,
                    "NewTabMenuItemElevateSubmenuItemClicked",
                    TraceLoggingDescription("Event emitted when the elevate submenu item from the new tab menu is invoked"),
                    TraceLoggingValue(page->NumberOfTabs(), "TabCount", "The count of tabs currently opened in this window"),
                    TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                    TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

                NewTerminalArgs args{ profileIndex };
                args.Elevate(true);
                page->_OpenNewTerminalViaDropdown(args);
            }
        });

        profileMenuItemFlyout.Items().Append(runAsAdminItem);

        return profileMenuItemFlyout;
    }
}
