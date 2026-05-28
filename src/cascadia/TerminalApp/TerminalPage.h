// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <ThrottledFunc.h>

#include "TerminalPage.g.h"
#include "Tab.h"
#include "AppKeyBindings.h"
#include "AppCommandlineArgs.h"
#include "RenameWindowRequestedArgs.g.h"
#include "RequestMoveContentArgs.g.h"
#include "LaunchPositionRequest.g.h"
#include "Toast.h"

#include "WindowsPackageManagerFactory.h"

#define DECLARE_ACTION_HANDLER(action) void _Handle##action(const IInspectable& sender, const Microsoft::Terminal::Settings::Model::ActionEventArgs& args);

namespace TerminalAppLocalTests
{
    class TabTests;
    class SettingsTests;
}

namespace Microsoft::Terminal::Core
{
    class ControlKeyStates;
}

namespace winrt::Microsoft::Terminal::Settings
{
    struct TerminalSettingsCreateResult;
}

namespace winrt::TerminalApp::implementation
{
    struct TerminalSettingsCache;

    inline constexpr uint32_t DefaultRowsToScroll{ 3 };
    inline constexpr std::wstring_view TabletInputServiceKey{ L"TabletInputService" };

    enum StartupState : int
    {
        NotInitialized = 0,
        InStartup = 1,
        Initialized = 2
    };

    enum ScrollDirection : int
    {
        ScrollUp = 0,
        ScrollDown = 1
    };

    enum class ConfirmCloseDialogKind
    {
        Pane,
        Tab,
        MultiplePanes,
        MultipleTabs,
        Window,
        CloseAll
    };

    struct RenameWindowRequestedArgs : RenameWindowRequestedArgsT<RenameWindowRequestedArgs>
    {
        WINRT_PROPERTY(winrt::hstring, ProposedName);

    public:
        RenameWindowRequestedArgs(const winrt::hstring& name) :
            _ProposedName{ name } {};
    };

    struct RequestMoveContentArgs : RequestMoveContentArgsT<RequestMoveContentArgs>
    {
        WINRT_PROPERTY(winrt::hstring, Window);
        WINRT_PROPERTY(winrt::hstring, Content);
        WINRT_PROPERTY(uint32_t, TabIndex);
        WINRT_PROPERTY(Windows::Foundation::IReference<Windows::Foundation::Point>, WindowPosition);

    public:
        RequestMoveContentArgs(const winrt::hstring window, const winrt::hstring content, uint32_t tabIndex) :
            _Window{ window },
            _Content{ content },
            _TabIndex{ tabIndex } {};
    };

    struct LaunchPositionRequest : LaunchPositionRequestT<LaunchPositionRequest>
    {
        LaunchPositionRequest() = default;

        til::property<winrt::Microsoft::Terminal::Settings::Model::LaunchPosition> Position;
    };

    struct WinGetSearchParams
    {
        winrt::Microsoft::Management::Deployment::PackageMatchField Field;
        winrt::Microsoft::Management::Deployment::PackageFieldMatchOption MatchOption;
    };

    struct TerminalPage : TerminalPageT<TerminalPage>
    {
    public:
        TerminalPage(TerminalApp::WindowProperties properties, const TerminalApp::ContentManager& manager);
        ~TerminalPage();

        // This implements shobjidl's IInitializeWithWindow, but due to a XAML Compiler bug we cannot
        // put it in our inheritance graph. https://github.com/microsoft/microsoft-ui-xaml/issues/3331
        STDMETHODIMP Initialize(HWND hwnd);

        void SetSettings(Microsoft::Terminal::Settings::Model::CascadiaSettings settings, bool needRefreshUI);

        void Create();
        Windows::UI::Xaml::Automation::Peers::AutomationPeer OnCreateAutomationPeer();

        bool ShouldImmediatelyHandoffToElevated(const Microsoft::Terminal::Settings::Model::CascadiaSettings& settings) const;
        void HandoffToElevated(const Microsoft::Terminal::Settings::Model::CascadiaSettings& settings);

        hstring Title();

        void TitlebarClicked();
        void WindowVisibilityChanged(const bool showOrHide);

        float CalcSnappedDimension(const bool widthOrHeight, const float dimension) const;

        winrt::hstring ApplicationDisplayName();
        winrt::hstring ApplicationVersion();

        CommandPalette LoadCommandPalette();
        SuggestionsControl LoadSuggestionsUI();

        safe_void_coroutine RequestQuit();
        safe_void_coroutine CloseWindow();
        void PersistState();
        std::vector<IPaneContent> Panes() const;

        void ToggleFocusMode();
        void ToggleFullscreen();
        void ToggleAlwaysOnTop();
        bool FocusMode() const;
        bool Fullscreen() const;
        bool AlwaysOnTop() const;
        bool ShowTabsFullscreen() const;
        void SetShowTabsFullscreen(bool newShowTabsFullscreen);
        void SetFullscreen(bool);
        void SetFocusMode(const bool inFocusMode);
        void Maximized(bool newMaximized);
        void RequestSetMaximized(bool newMaximized);

        void SetStartupActions(std::vector<Microsoft::Terminal::Settings::Model::ActionAndArgs> actions);
        void SetStartupConnection(winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection);

        static std::vector<Microsoft::Terminal::Settings::Model::ActionAndArgs> ConvertExecuteCommandlineToActions(const Microsoft::Terminal::Settings::Model::ExecuteCommandlineArgs& args);

        winrt::TerminalApp::IDialogPresenter DialogPresenter() const;
        void DialogPresenter(winrt::TerminalApp::IDialogPresenter dialogPresenter);

        winrt::TerminalApp::TaskbarState TaskbarState() const;

        void ShowKeyboardServiceWarning() const;
        winrt::hstring KeyboardServiceDisabledText();

        void IdentifyWindow();
        void ActionSaved(winrt::hstring input, winrt::hstring name, winrt::hstring keyChord);
        void ActionSaveFailed(winrt::hstring message);
        void ShowTerminalWorkingDirectory();

        safe_void_coroutine ProcessStartupActions(std::vector<Microsoft::Terminal::Settings::Model::ActionAndArgs> actions,
                                                  const winrt::hstring cwd = winrt::hstring{},
                                                  const winrt::hstring env = winrt::hstring{});
        safe_void_coroutine CreateTabFromConnection(winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection connection);

        TerminalApp::WindowProperties WindowProperties() const noexcept { return _WindowProperties; };

        bool CanDragDrop() const noexcept;
        bool IsRunningElevated() const noexcept;

        void OpenSettingsUI();
        void WindowActivated(const bool activated);
        bool FocusTab(const winrt::TerminalApp::Tab& tab);

        bool OnDirectKeyEvent(const uint32_t vkey, const uint8_t scanCode, const bool down);

        void AttachContent(Windows::Foundation::Collections::IVector<Microsoft::Terminal::Settings::Model::ActionAndArgs> args, uint32_t tabIndex);
        void SendContentToOther(winrt::TerminalApp::RequestReceiveContentArgs args);

        uint32_t NumberOfTabs() const;

        // Terminal Protocol Bridge Methods
        uint32_t TabCount() const;
        Windows::Foundation::IReference<uint32_t> FocusedTabIndex() const;
        Windows::Foundation::IAsyncOperation<Microsoft::Terminal::Protocol::PaneInfo> GetProtocolActivePane();
        Windows::Foundation::IAsyncOperation<Windows::Foundation::Collections::IVector<Microsoft::Terminal::Protocol::TabInfo>> GetProtocolTabs();
        Windows::Foundation::IAsyncOperation<Windows::Foundation::Collections::IVector<Microsoft::Terminal::Protocol::PaneInfo>> GetProtocolPanes(uint32_t tabIdFilter);
        Windows::Foundation::IAsyncOperation<Microsoft::Terminal::Protocol::PaneOutput> ReadProtocolPaneOutput(winrt::guid sessionId, hstring source, int32_t maxLines);
        Windows::Foundation::IAsyncOperation<Microsoft::Terminal::Protocol::ProcessStatus> GetProtocolProcessStatus(winrt::guid sessionId);
        Windows::Foundation::IAsyncOperation<Microsoft::Terminal::Protocol::SessionVariable> GetProtocolSessionVariable(winrt::guid sessionId, hstring name);
        Windows::Foundation::IAsyncOperation<bool> SetProtocolSessionVariable(winrt::guid sessionId, hstring name, hstring value);
        Windows::Foundation::IAsyncOperation<Microsoft::Terminal::Protocol::TabCreationResult> CreateProtocolTab(Microsoft::Terminal::Settings::Model::NewTerminalArgs args, bool background);
        Windows::Foundation::IAsyncOperation<Microsoft::Terminal::Protocol::TabCreationResult> SplitProtocolPane(winrt::guid sessionId, Microsoft::Terminal::Settings::Model::SplitDirection direction, float size, Microsoft::Terminal::Settings::Model::NewTerminalArgs args, bool background);
        Windows::Foundation::IAsyncOperation<bool> CloseProtocolPane(winrt::guid sessionId);
        Windows::Foundation::IAsyncOperation<bool> SendProtocolInput(winrt::guid sessionId, hstring text);
        Windows::Foundation::IAsyncOperation<bool> FocusProtocolPane(winrt::guid sessionId);
        void OnAutofixStateChanged(hstring eventJson);
        void OnAgentStatusChanged(hstring eventJson);
        void OnCloseAgentPaneRequested(hstring eventJson);
        void OnAgentStateChanged(hstring eventJson);
        void OnResumeInNewAgentTabRequested(hstring eventJson);
        void OnAgentChipTargetChanged(hstring eventJson);

        til::property_changed_event PropertyChanged;

        // -------------------------------- WinRT Events ---------------------------------
        til::typed_event<IInspectable, winrt::hstring> ProtocolVtSequenceReceived;
        til::typed_event<IInspectable, IInspectable> TitleChanged;
        til::typed_event<IInspectable, IInspectable> CloseWindowRequested;
        til::typed_event<IInspectable, winrt::Windows::UI::Xaml::UIElement> SetTitleBarContent;
        til::typed_event<IInspectable, IInspectable> FocusModeChanged;
        til::typed_event<IInspectable, IInspectable> FullscreenChanged;
        til::typed_event<IInspectable, IInspectable> ChangeMaximizeRequested;
        til::typed_event<IInspectable, IInspectable> AlwaysOnTopChanged;
        til::typed_event<IInspectable, IInspectable> RaiseVisualBell;
        til::typed_event<IInspectable, IInspectable> SetTaskbarProgress;
        til::typed_event<IInspectable, IInspectable> Initialized;
        til::typed_event<IInspectable, IInspectable> IdentifyWindowsRequested;
        til::typed_event<IInspectable, winrt::TerminalApp::RenameWindowRequestedArgs> RenameWindowRequested;
        til::typed_event<IInspectable, IInspectable> SummonWindowRequested;
        til::typed_event<IInspectable, winrt::TerminalApp::Tab> FocusTabRequested;
        til::typed_event<IInspectable, winrt::Microsoft::Terminal::Control::WindowSizeChangedEventArgs> WindowSizeChanged;

        til::typed_event<IInspectable, IInspectable> OpenSystemMenu;
        til::typed_event<IInspectable, IInspectable> QuitRequested;
        til::typed_event<IInspectable, winrt::Microsoft::Terminal::Control::ShowWindowArgs> ShowWindowChanged;
        til::typed_event<Windows::Foundation::IInspectable, Windows::Foundation::Collections::IVectorView<winrt::Microsoft::Terminal::Settings::Model::SettingsLoadWarnings>> ShowLoadWarningsDialog;

        til::typed_event<Windows::Foundation::IInspectable, winrt::TerminalApp::RequestMoveContentArgs> RequestMoveContent;
        til::typed_event<Windows::Foundation::IInspectable, winrt::TerminalApp::RequestReceiveContentArgs> RequestReceiveContent;

        til::typed_event<IInspectable, winrt::TerminalApp::LaunchPositionRequest> RequestLaunchPosition;

        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, TitlebarBrush, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, FrameBrush, PropertyChanged.raise, nullptr);

        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SavedActionName, PropertyChanged.raise, L"");
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SavedActionKeyChord, PropertyChanged.raise, L"");
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SavedActionCommandLine, PropertyChanged.raise, L"");

    private:
        friend struct TerminalPageT<TerminalPage>; // for Xaml to bind events
        std::optional<HWND> _hostingHwnd;

        // If you add controls here, but forget to null them either here or in
        // the ctor, you're going to have a bad time. It'll mysteriously fail to
        // activate the app.
        // ALSO: If you add any UIElements as roots here, make sure they're
        // updated in App::_ApplyTheme. The roots currently is _tabRow
        // (which is a root when the tabs are in the titlebar.)
        Microsoft::UI::Xaml::Controls::TabView _tabView{ nullptr };
        TerminalApp::TabRowControl _tabRow{ nullptr };
        Windows::UI::Xaml::Controls::Grid _tabContent{ nullptr };
        Microsoft::UI::Xaml::Controls::SplitButton _newTabButton{ nullptr };
        winrt::TerminalApp::ColorPickupFlyout _tabColorPicker{ nullptr };

        Microsoft::Terminal::Settings::Model::CascadiaSettings _settings{ nullptr };

        Windows::Foundation::Collections::IObservableVector<TerminalApp::Tab> _tabs;
        Windows::Foundation::Collections::IObservableVector<TerminalApp::Tab> _mruTabs;
        static winrt::com_ptr<Tab> _GetTabImpl(const TerminalApp::Tab& tab);

        void _UpdateTabIndices();

        TerminalApp::Tab _settingsTab{ nullptr };
        winrt::Microsoft::Terminal::Settings::Editor::MainPage _settingsMainPage{ nullptr };

        bool _isInFocusMode{ false };
        bool _isFullscreen{ false };
        bool _isMaximized{ false };
        bool _isAlwaysOnTop{ false };

        bool _showTabsFullscreen{ false };

        std::optional<uint32_t> _loadFromPersistedLayoutIdx{};

        bool _rearranging{ false };
        std::optional<int> _rearrangeFrom{};
        std::optional<int> _rearrangeTo{};
        bool _removing{ false };

        bool _activated{ false };
        bool _visible{ true };

        std::vector<std::vector<Microsoft::Terminal::Settings::Model::ActionAndArgs>> _previouslyClosedPanesAndTabs{};

        uint32_t _systemRowsToScroll{ DefaultRowsToScroll };

        // use a weak reference to prevent circular dependency with AppLogic
        winrt::weak_ref<winrt::TerminalApp::IDialogPresenter> _dialogPresenter;

        winrt::com_ptr<AppKeyBindings> _bindings{ winrt::make_self<implementation::AppKeyBindings>() };
        winrt::com_ptr<ShortcutActionDispatch> _actionDispatch{ winrt::make_self<implementation::ShortcutActionDispatch>() };

        winrt::Windows::UI::Xaml::Controls::Grid::LayoutUpdated_revoker _layoutUpdatedRevoker;
        StartupState _startupState{ StartupState::NotInitialized };

        std::vector<Microsoft::Terminal::Settings::Model::ActionAndArgs> _startupActions;
        winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection _startupConnection{ nullptr };

        std::shared_ptr<Toast> _windowIdToast{ nullptr };
        std::shared_ptr<Toast> _actionSavedToast{ nullptr };
        std::shared_ptr<Toast> _actionSaveFailedToast{ nullptr };
        std::shared_ptr<Toast> _windowCwdToast{ nullptr };

        // ── Per-tab agent pane + window-level bottom bar ────────────────
        // Each tab independently owns (or doesn't own) one agent pane,
        // which lives as an AgentPaneContent leaf in that tab's pane tree.
        // The bottom bar lives at the window level (TerminalPage.xaml)
        // but reflects the active tab's agent-pane state, so its display
        // toggles each time the user switches tabs.
        //
        // The bottom-bar click handlers (`_AgentToggleButtonOnClick`,
        // `_SessionToggleButtonOnClick`, `_DiagnosticsButtonOnClick`)
        // target the *active* tab's AgentPaneContent (or open one if
        // it doesn't exist yet).
        void _AgentToggleButtonOnClick(const winrt::Windows::Foundation::IInspectable& sender,
                                       const winrt::Windows::UI::Xaml::RoutedEventArgs& eventArgs);
        void _SessionToggleButtonOnClick(const winrt::Windows::Foundation::IInspectable& sender,
                                         const winrt::Windows::UI::Xaml::RoutedEventArgs& eventArgs);
        void _DiagnosticsButtonOnClick(const winrt::Windows::Foundation::IInspectable& sender,
                                       const winrt::Windows::UI::Xaml::RoutedEventArgs& eventArgs);
        // Recomputes the bottom bar's visibility / toggle-lit / diagnostics
        // affordance from the active tab's AgentPaneContent (or absence
        // thereof). Called on tab switch and whenever an AgentPaneContent
        // raises `StateChanged` for the active tab.
        void _UpdateBottomBarState();
        // Refresh ONLY the bottom-bar visibility (show on terminal/agent
        // tabs, collapse on Settings/etc.). Safe to call synchronously
        // on tab switch because the decision depends solely on the
        // focused tab's content type and not on any wta-projected
        // agent state. `_UpdateBottomBarState` calls this first and
        // then refreshes the agent-state-dependent UI (toggle
        // highlights, diagnostics).
        void _UpdateBottomBarVisibility();
        // Subscribe to an AgentPaneContent's StateChanged event so the
        // window-level bottom bar refreshes when its state mutates.
        // Wired once per AgentPaneContent creation.
        void _WireAgentPaneEvents(const winrt::TerminalApp::AgentPaneContent& content,
                                  const winrt::com_ptr<Tab>& ownerTab);

        void _TriggerAutofix(const winrt::com_ptr<Tab>& ownerTab, const wchar_t* triggerSource);

        // Hot-reload of agent/model settings. Snapshot is captured on first
        // SetSettings and after every rebuild; a diff drives teardown/rebuild
        // of the agent pane.
        struct AgentSettingsSnapshot
        {
            std::wstring acpAgent;
            std::wstring acpModel;
            std::wstring acpCustomCommand;
            std::wstring delegateAgent;
            std::wstring delegateModel;
            std::wstring delegateCustomCommand;
        };
        AgentSettingsSnapshot _lastAgentSettings{};
        bool _agentSettingsSnapshotInitialized{ false };
        // Snapshot of AutoFixEnabled at last SetSettings call. When the
        // user toggles "Auto-suggest fixes" we send the new value to WTA
        // over the protocol so it can update its in-memory gate without
        // requiring the agent pane to be torn down and restarted.
        bool _lastAutoFixEnabled{ false };
        bool _autoFixEnabledSnapshotInitialized{ false };
        bool _agentRebuilding{ false };
        // Set when a settings change wants a rebuild but the active
        // tab can't host an agent pane (e.g. the Settings tab itself).
        // _FlushPendingAgentRebuild runs the deferred rebuild from
        // _OnTabSelectionChanged once a terminal tab is active.
        bool _pendingAgentRebuild{ false };

        // Plan-C resume-into-new-tab bookkeeping. When the F2 session
        // manager's Enter handler on a Historical/Ended row creates a
        // new tab, it stashes the requested session id + cwd here keyed
        // by the new tab's StableId. `OnAgentStateChanged` consumes the
        // entry the moment it spawns the new helper for that tab —
        // passing the values down as `--initial-load-session-id` +
        // `--initial-load-cwd` so the boot-time ACP `session/load` is
        // atomic with helper spawn. Replaces the prior race-prone
        // "spawn helper, then broadcast `load_session` VT event" path
        // (the VT broadcast often landed in the wrong helper because
        // every helper subscribed to the same shared COM event stream).
        //
        // Entries are one-shot; an unconsumed entry leaks until the
        // page is torn down (only happens if the user closes the new
        // tab before its `agent_state_changed{pane_open:true}` round-
        // trips back from wta). Tiny worst-case memory cost.
        struct _PendingLoadSession
        {
            std::string sessionId;
            std::string cwd;
        };
        std::unordered_map<winrt::hstring, _PendingLoadSession> _pendingLoadSessions;
        AgentSettingsSnapshot _CaptureAgentSettingsSnapshot() const;
        static bool _AgentSettingsChanged(const AgentSettingsSnapshot& a, const AgentSettingsSnapshot& b);
        void _TeardownAgentPane(const winrt::com_ptr<Tab>& tab);
        void _RebuildAgentStack();
        void _FlushPendingAgentRebuild();
        // Helper+master agent-pane creation (Z-M3, default since Z-M6):
        // spawns a wta-helper as a normal conpty child for this pane and
        // connects it to the SharedWta-managed wta-master process over a
        // named pipe (helper ↔ master speaks ACP JSON-RPC). Returns true
        // when the helper was spawned successfully.
        //
        // `intoSessionsView` is passed through to the helper as
        // `--initial-view sessions`. Called from `_OpenOrReuseAgentPane`
        // user-initiated paths.
        //
        // `initialLoadSessionId` + `initialLoadCwd` plumb a Plan-C boot-time
        // resume hint down to the helper: when non-empty, the spawned wta
        // process gets `--initial-load-session-id` (+ `--initial-load-cwd`)
        // on its cmdline and immediately calls `session/load` instead of
        // creating a fresh session. Used by the F2 "Enter on Historical /
        // Ended row" path to bundle spawn + resume atomically (replacing
        // the prior race-prone "spawn, then broadcast `load_session` VT"
        // design).
        //
        // `autoStash=true` is the pre-warm path called from `_InitializeTab`:
        // the helper conpty is spawned but the pane is immediately stashed
        // (`Tab::StashAgentPane`) so the user sees only the terminal pane.
        // Focus stays on the original terminal; no telemetry fires (the
        // pane wasn't *opened*, just pre-warmed). This is what makes
        // autofix work without the user ever opening the agent pane.
        bool _AutoCreateHiddenAgentPaneShared(winrt::com_ptr<Tab> tab,
                                              bool intoSessionsView = false,
                                              bool autoStash = false,
                                              std::string_view initialLoadSessionId = {},
                                              std::string_view initialLoadCwd = {});
        // Wraps the raw terminal pane's TerminalPaneContent in an
        // AgentPaneContent so the leaf renders the 36px XAML agent bar
        // above the wta TermControl + the bottom-bar below.
        std::shared_ptr<Pane> _WrapInAgentPaneContent(std::shared_ptr<Pane> rawPane);

        // Per-tab: ask wta to flip view/pane_open state on a specific tab.
        // Sole writer of `set_agent_state` for that tab. Routes through
        // `ProtocolVtSequenceReceived` like the rest of the C++ → wta events.
        void _RequestAgentStateForTab(const winrt::com_ptr<Tab>& tab,
                                      std::optional<std::string_view> view,
                                      std::optional<bool> paneOpen);
        // Tells wta that a tab is being destroyed so it can drop the matching
        // TabSession and any session_to_tab entries pointing at it.
        void _NotifyAgentTabClosed(const winrt::hstring& tabId);
        void _NotifyAgentTabReset(const winrt::hstring& tabId);
        void _NotifyAgentTabChanged(const winrt::hstring& tabId);
        // Look up a tab by its StableId; returns nullptr if unknown.
        winrt::com_ptr<Tab> _FindTabByStableId(const winrt::hstring& stableId) const;
        // Look up a tab by the AgentPaneContent instance hosted in it.
        winrt::com_ptr<Tab> _FindTabHostingAgentPaneContent(const winrt::TerminalApp::AgentPaneContent& content) const;

        winrt::Windows::UI::Xaml::Controls::TextBox::LayoutUpdated_revoker _renamerLayoutUpdatedRevoker;
        int _renamerLayoutCount{ 0 };
        bool _renamerPressedEnter{ false };

        TerminalApp::WindowProperties _WindowProperties{ nullptr };
        PaneResources _paneResources;

        TerminalApp::ContentManager _manager{ nullptr };

        std::shared_ptr<TerminalSettingsCache> _terminalSettingsCache{};

        struct StashedDragData
        {
            winrt::com_ptr<winrt::TerminalApp::implementation::Tab> draggedTab{ nullptr };
            winrt::Windows::Foundation::Point dragOffset{ 0, 0 };
        } _stashed;

        safe_void_coroutine _NewTerminalByDrop(const Windows::Foundation::IInspectable&, winrt::Windows::UI::Xaml::DragEventArgs e);

        __declspec(noinline) CommandPalette _loadCommandPaletteSlowPath();
        bool _commandPaletteIs(winrt::Windows::UI::Xaml::Visibility visibility);
        __declspec(noinline) SuggestionsControl _loadSuggestionsElementSlowPath();
        bool _suggestionsControlIs(winrt::Windows::UI::Xaml::Visibility visibility);

        winrt::Windows::Foundation::IAsyncOperation<winrt::Windows::UI::Xaml::Controls::ContentDialogResult> _ShowDialogHelper(const std::wstring_view& name);

        void _ShowAboutDialog();
        winrt::Windows::Foundation::IAsyncOperation<winrt::Windows::UI::Xaml::Controls::ContentDialogResult> _ShowConfirmCloseDialog(ConfirmCloseDialogKind kind);
        winrt::Windows::Foundation::IAsyncOperation<winrt::Windows::UI::Xaml::Controls::ContentDialogResult> _ShowCloseReadOnlyDialog();
        winrt::Windows::Foundation::IAsyncOperation<winrt::Windows::UI::Xaml::Controls::ContentDialogResult> _ShowMultiLinePasteWarningDialog();
        winrt::Windows::Foundation::IAsyncOperation<winrt::Windows::UI::Xaml::Controls::ContentDialogResult> _ShowLargePasteWarningDialog();

        safe_void_coroutine _InitShellIntegration(const Microsoft::Terminal::Settings::Model::ShellIntegrationTarget target);
        void _ShowShellIntegrationDialog(const winrt::hstring& title, const winrt::hstring& message);
        void _OnSettingsInitShellIntegration(const winrt::Windows::Foundation::IInspectable& sender, const Microsoft::Terminal::Settings::Model::ShellIntegrationTarget target);

        void _CreateNewTabFlyout();
        std::vector<winrt::Windows::UI::Xaml::Controls::MenuFlyoutItemBase> _CreateNewTabFlyoutItems(winrt::Windows::Foundation::Collections::IVector<Microsoft::Terminal::Settings::Model::NewTabMenuEntry> entries);
        winrt::Windows::UI::Xaml::Controls::IconElement _CreateNewTabFlyoutIcon(const winrt::hstring& icon);
        winrt::Windows::UI::Xaml::Controls::MenuFlyoutItem _CreateNewTabFlyoutProfile(const Microsoft::Terminal::Settings::Model::Profile profile, int profileIndex, const winrt::hstring& iconPathOverride);
        winrt::Windows::UI::Xaml::Controls::MenuFlyoutItem _CreateNewTabFlyoutAction(const winrt::hstring& actionId, const winrt::hstring& iconPathOverride);

        void _OpenNewTabDropdown();
        HRESULT _OpenNewTab(const Microsoft::Terminal::Settings::Model::INewContentArgs& newContentArgs, bool openInBackground = false);
        TerminalApp::Tab _CreateNewTabFromPane(std::shared_ptr<Pane> pane, uint32_t insertPosition = -1, bool openInBackground = false);

        std::wstring _evaluatePathForCwd(std::wstring_view path);

        winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection _CreateConnectionFromSettings(Microsoft::Terminal::Settings::Model::Profile profile, Microsoft::Terminal::Control::IControlSettings settings, const bool inheritCursor);
        winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection _duplicateConnectionForRestart(const TerminalApp::TerminalPaneContent& paneContent);
        void _restartPaneConnection(const TerminalApp::TerminalPaneContent&, const winrt::Windows::Foundation::IInspectable&);

        safe_void_coroutine _OpenNewWindow(const Microsoft::Terminal::Settings::Model::INewContentArgs newContentArgs);

        void _OpenNewTerminalViaDropdown(const Microsoft::Terminal::Settings::Model::NewTerminalArgs newTerminalArgs);

        bool _displayingCloseDialog{ false };
        void _SettingsButtonOnClick(const IInspectable& sender, const Windows::UI::Xaml::RoutedEventArgs& eventArgs);
        void _CommandPaletteButtonOnClick(const IInspectable& sender, const Windows::UI::Xaml::RoutedEventArgs& eventArgs);
        void _AboutButtonOnClick(const IInspectable& sender, const Windows::UI::Xaml::RoutedEventArgs& eventArgs);

        void _KeyDownHandler(const Windows::Foundation::IInspectable& sender, const Windows::UI::Xaml::Input::KeyRoutedEventArgs& e);
        static ::Microsoft::Terminal::Core::ControlKeyStates _GetPressedModifierKeys() noexcept;
        static void _ClearKeyboardState(const WORD vkey, const WORD scanCode) noexcept;
        void _HookupKeyBindings(const Microsoft::Terminal::Settings::Model::IActionMapView& actionMap) noexcept;
        void _RegisterActionCallbacks();

        void _UpdateTitle(const Tab& tab);
        void _UpdateTabIcon(Tab& tab);
        void _UpdateTabView();
        void _UpdateTabWidthMode();
        void _SetBackgroundImage(const winrt::Microsoft::Terminal::Settings::Model::IAppearanceConfig& newAppearance);

        void _DuplicateFocusedTab();
        void _DuplicateTab(const Tab& tab);

        safe_void_coroutine _ExportTab(const Tab& tab, winrt::hstring filepath);

        winrt::Windows::Foundation::IAsyncAction _HandleCloseTabRequested(winrt::TerminalApp::Tab tab, bool skipConfirmClose = false);
        void _CloseTabAtIndex(uint32_t index);
        void _RemoveTab(const winrt::TerminalApp::Tab& tab, bool movingAway = false);
        safe_void_coroutine _RemoveTabs(const std::vector<winrt::TerminalApp::Tab> tabs);

        void _InitializeTab(winrt::com_ptr<Tab> newTabImpl, uint32_t insertPosition = -1, bool openInBackground = false);
        void _RegisterTerminalEvents(Microsoft::Terminal::Control::TermControl term);
        std::string _FindSessionIdForControl(const Microsoft::Terminal::Control::TermControl& control);
        std::string _FindTabIdForControl(const Microsoft::Terminal::Control::TermControl& control);
        void _RegisterTabEvents(Tab& hostingTab);

        void _DismissTabContextMenus();
        void _FocusCurrentTab(const bool focusAlways);
        bool _HasMultipleTabs() const;

        void _SelectNextTab(const bool bMoveRight, const Windows::Foundation::IReference<Microsoft::Terminal::Settings::Model::TabSwitcherMode>& customTabSwitcherMode);
        bool _SelectTab(uint32_t tabIndex);
        bool _MoveFocus(const Microsoft::Terminal::Settings::Model::FocusDirection& direction);
        bool _SwapPane(const Microsoft::Terminal::Settings::Model::FocusDirection& direction);
        bool _MovePane(const Microsoft::Terminal::Settings::Model::MovePaneArgs args);
        bool _MoveTab(winrt::com_ptr<Tab> tab, const Microsoft::Terminal::Settings::Model::MoveTabArgs args);

        std::shared_ptr<ThrottledFunc<>> _adjustProcessPriorityThrottled;
        void _adjustProcessPriority() const;

        template<typename F>
        bool _ApplyToActiveControls(F f) const
        {
            if (const auto tab{ _GetFocusedTabImpl() })
            {
                if (const auto activePane = tab->GetActivePane())
                {
                    activePane->WalkTree([&](auto p) {
                        if (const auto& control{ p->GetTerminalControl() })
                        {
                            f(control);
                        }
                    });

                    return true;
                }
            }
            return false;
        }

        winrt::Microsoft::Terminal::Control::TermControl _GetActiveControl() const;
        std::optional<uint32_t> _GetFocusedTabIndex() const noexcept;
        std::optional<uint32_t> _GetTabIndex(const TerminalApp::Tab& tab) const noexcept;
        TerminalApp::Tab _GetFocusedTab() const noexcept;
        winrt::com_ptr<Tab> _GetFocusedTabImpl() const noexcept;
        TerminalApp::Tab _GetTabByTabViewItem(const IInspectable& tabViewItem) const noexcept;

        void _HandleClosePaneRequested(std::shared_ptr<Pane> pane);
        bool _ShouldWarnOnClose() const;
        bool _ShouldWarnOnCloseTab(const winrt::com_ptr<Tab>& tab) const;
        safe_void_coroutine _SetFocusedTab(const winrt::TerminalApp::Tab tab);
        safe_void_coroutine _CloseFocusedPane();
        safe_void_coroutine _ClosePanes(weak_ref<Tab> weakTab, std::vector<uint32_t> paneIds);
        void _CloseRemainingPanes(weak_ref<Tab> weakTab, std::vector<uint32_t> paneIds);
        winrt::Windows::Foundation::IAsyncOperation<bool> _PaneConfirmCloseReadOnly(std::shared_ptr<Pane> pane);
        void _AddPreviouslyClosedPaneOrTab(std::vector<Microsoft::Terminal::Settings::Model::ActionAndArgs>&& args);

        void _Scroll(ScrollDirection scrollDirection, const Windows::Foundation::IReference<uint32_t>& rowsToScroll);

        void _SplitPane(const winrt::com_ptr<Tab>& tab,
                        const Microsoft::Terminal::Settings::Model::SplitDirection splitType,
                        const float splitSize,
                        std::shared_ptr<Pane> newPane,
                        bool focusNewPane = true);
        bool _ResizePane(const Microsoft::Terminal::Settings::Model::ResizeDirection& direction);
        void _ToggleSplitOrientation();

        void _ScrollPage(ScrollDirection scrollDirection);
        void _ScrollToBufferEdge(ScrollDirection scrollDirection);
        void _SetAcceleratorForMenuItem(Windows::UI::Xaml::Controls::MenuFlyoutItem& menuItem, const winrt::Microsoft::Terminal::Control::KeyChord& keyChord);

        safe_void_coroutine _PasteFromClipboardHandler(const IInspectable sender,
                                                       const Microsoft::Terminal::Control::PasteFromClipboardEventArgs eventArgs);

        safe_void_coroutine _OpenHyperlinkHandler(const IInspectable sender, const Microsoft::Terminal::Control::OpenHyperlinkEventArgs eventArgs);
        static bool _IsUriSupported(const winrt::Windows::Foundation::Uri& parsedUri);
        static bool _IsUriConsideredSomewhatSafe(const winrt::Windows::Foundation::Uri& parsedUri);

        void _ShowCouldNotOpenDialog(winrt::hstring reason, winrt::hstring uri);
        bool _CopyText(bool dismissSelection, bool singleLine, bool withControlSequences, Microsoft::Terminal::Control::CopyFormat formats);

        safe_void_coroutine _SetTaskbarProgressHandler(const IInspectable sender, const IInspectable eventArgs);

        void _copyToClipboard(IInspectable, Microsoft::Terminal::Control::WriteToClipboardEventArgs args) const;
        void _PasteText();

        safe_void_coroutine _ControlNoticeRaisedHandler(const IInspectable sender, const Microsoft::Terminal::Control::NoticeEventArgs eventArgs);
        void _ShowControlNoticeDialog(const winrt::hstring& title, const winrt::hstring& message);

        safe_void_coroutine _LaunchSettings(const Microsoft::Terminal::Settings::Model::SettingsTarget target);

        void _TabDragStarted(const IInspectable& sender, const IInspectable& eventArgs);
        void _TabDragCompleted(const IInspectable& sender, const IInspectable& eventArgs);

        // BODGY: WinUI's TabView has a broken close event handler:
        // If the close button is disabled, middle-clicking the tab raises no close
        // event. Because that's dumb, we implement our own middle-click handling.
        // `_tabItemMiddleClickHookEnabled` is true whenever the close button is hidden,
        // and that enables all of the rest of this machinery (and this workaround).
        bool _tabItemMiddleClickHookEnabled = false;
        bool _tabItemMiddleClickExited = false;
        PointerEntered_revoker _tabItemMiddleClickPointerEntered;
        PointerExited_revoker _tabItemMiddleClickPointerExited;
        PointerCaptureLost_revoker _tabItemMiddleClickPointerCaptureLost;
        void _OnTabPointerPressed(const IInspectable& sender, const Windows::UI::Xaml::Input::PointerRoutedEventArgs& eventArgs);
        safe_void_coroutine _OnTabPointerReleasedCloseTab(IInspectable sender);

        void _OnTabSelectionChanged(const IInspectable& sender, const Windows::UI::Xaml::Controls::SelectionChangedEventArgs& eventArgs);
        void _OnTabItemsChanged(const IInspectable& sender, const Windows::Foundation::Collections::IVectorChangedEventArgs& eventArgs);
        void _OnTabCloseRequested(const IInspectable& sender, const Microsoft::UI::Xaml::Controls::TabViewTabCloseRequestedEventArgs& eventArgs);
        void _OnFirstLayout(const IInspectable& sender, const IInspectable& eventArgs);
        void _UpdatedSelectedTab(const winrt::TerminalApp::Tab& tab);
        void _UpdateBackground(const winrt::Microsoft::Terminal::Settings::Model::Profile& profile);

        void _OnDispatchCommandRequested(const IInspectable& sender, const Microsoft::Terminal::Settings::Model::Command& command);
        void _OnCommandLineExecutionRequested(const IInspectable& sender, const winrt::hstring& commandLine);
        void _OnSwitchToTabRequested(const IInspectable& sender, const winrt::TerminalApp::Tab& tab);
        void _OnAgentForegroundPromptRequested(const IInspectable& sender, const winrt::hstring& prompt);
        void _OnAgentBackgroundTaskRequested(const IInspectable& sender, const winrt::hstring& prompt);

        // Agent pane helpers
        winrt::hstring _DetectAgentCli() const;
        winrt::hstring _DetectWtaPath() const;
        std::optional<uint32_t> _FindSourceOfAgentPaneId(const std::shared_ptr<Pane>& root);
        void _DelegatePromptToAgent(const winrt::hstring& prompt);

        // Note (Phase 5): the per-pane wta-process watch + Job Object members
        // and their setup/teardown methods were removed when the legacy
        // per-pane-wta architecture was deleted. Helper processes are now
        // ordinary conpty children of TermControl — TermControl /
        // ConptyConnection owns their lifetime.
        void _OpenOrReuseAgentPane(bool intoSessionsView, const wchar_t* triggerSource);
        void _FocusAgentPane();
        void _RepositionAgentPanes();
        static winrt::Microsoft::Terminal::Settings::Model::SplitDirection _AgentPanePositionToSplitDirection(const winrt::hstring& position);

        // First-run experience
        bool _IsFreRequired() const;
        void _ShowFreOverlay();
        void _OnFreCompleted(const winrt::TerminalApp::FreOverlay& sender, const winrt::Windows::Foundation::IInspectable& args);

        void _Find(const Tab& tab);

        winrt::Microsoft::Terminal::Control::TermControl _CreateNewControlAndContent(const winrt::Microsoft::Terminal::Settings::TerminalSettingsCreateResult& settings,
                                                                                     const winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection& connection);
        winrt::Microsoft::Terminal::Control::TermControl _SetupControl(const winrt::Microsoft::Terminal::Control::TermControl& term);
        winrt::Microsoft::Terminal::Control::TermControl _AttachControlToContent(const uint64_t& contentGuid);

        TerminalApp::IPaneContent _makeSettingsContent();
        std::shared_ptr<Pane> _MakeTerminalPane(const Microsoft::Terminal::Settings::Model::NewTerminalArgs& newTerminalArgs = nullptr,
                                                const winrt::TerminalApp::Tab& sourceTab = nullptr,
                                                winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection existingConnection = nullptr);
        std::shared_ptr<Pane> _MakePane(const Microsoft::Terminal::Settings::Model::INewContentArgs& newContentArgs = nullptr,
                                        const winrt::TerminalApp::Tab& sourceTab = nullptr,
                                        winrt::Microsoft::Terminal::TerminalConnection::ITerminalConnection existingConnection = nullptr);

        void _RefreshUIForSettingsReload();

        void _SetNewTabButtonColor(til::color color, til::color accentColor);
        void _ClearNewTabButtonColor();

        safe_void_coroutine _CompleteInitialization();

        void _FocusActiveControl(IInspectable sender, IInspectable eventArgs);

        void _UnZoomIfNeeded();

        static int _ComputeScrollDelta(ScrollDirection scrollDirection, const uint32_t rowsToScroll);
        static uint32_t _ReadSystemRowsToScroll();

        void _UpdateMRUTab(const winrt::TerminalApp::Tab& tab);

        void _TryMoveTab(const uint32_t currentTabIndex, const int32_t suggestedNewTabIndex);

        void _PreviewAction(const Microsoft::Terminal::Settings::Model::ActionAndArgs& args);
        void _PreviewActionHandler(const IInspectable& sender, const Microsoft::Terminal::Settings::Model::Command& args);
        void _EndPreview();
        void _RunRestorePreviews();
        void _PreviewColorScheme(const Microsoft::Terminal::Settings::Model::SetColorSchemeArgs& args);
        void _PreviewAdjustOpacity(const Microsoft::Terminal::Settings::Model::AdjustOpacityArgs& args);
        void _PreviewSendInput(const Microsoft::Terminal::Settings::Model::SendInputArgs& args);

        winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs _lastPreviewedAction{ nullptr };
        std::vector<std::function<void()>> _restorePreviewFuncs{};

        HRESULT _OnNewConnection(const winrt::Microsoft::Terminal::TerminalConnection::ConptyConnection& connection);
        void _HandleToggleInboundPty(const IInspectable& sender, const Microsoft::Terminal::Settings::Model::ActionEventArgs& args);

        void _WindowRenamerActionClick(const IInspectable& sender, const IInspectable& eventArgs);
        void _RequestWindowRename(const winrt::hstring& newName);
        void _WindowRenamerKeyDown(const IInspectable& sender, const winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs& e);
        void _WindowRenamerKeyUp(const IInspectable& sender, const winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs& e);

        void _UpdateTeachingTipTheme(winrt::Windows::UI::Xaml::FrameworkElement element);

        winrt::Microsoft::Terminal::Settings::Model::Profile GetClosestProfileForDuplicationOfProfile(const winrt::Microsoft::Terminal::Settings::Model::Profile& profile) const noexcept;

        bool _maybeElevate(const winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs& newTerminalArgs,
                           const winrt::Microsoft::Terminal::Settings::TerminalSettingsCreateResult& controlSettings,
                           const winrt::Microsoft::Terminal::Settings::Model::Profile& profile);
        void _OpenElevatedWT(winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs newTerminalArgs);

        safe_void_coroutine _ConnectionStateChangedHandler(const winrt::Windows::Foundation::IInspectable& sender, const winrt::Windows::Foundation::IInspectable& args);
        void _CloseOnExitInfoDismissHandler(const winrt::Windows::Foundation::IInspectable& sender, const winrt::Windows::Foundation::IInspectable& args) const;
        void _KeyboardServiceWarningInfoDismissHandler(const winrt::Windows::Foundation::IInspectable& sender, const winrt::Windows::Foundation::IInspectable& args) const;
        static bool _IsMessageDismissed(const winrt::Microsoft::Terminal::Settings::Model::InfoBarMessage& message);
        static void _DismissMessage(const winrt::Microsoft::Terminal::Settings::Model::InfoBarMessage& message);

        void _updateThemeColors();
        void _updateAllTabCloseButtons();
        void _updatePaneResources(const winrt::Windows::UI::Xaml::ElementTheme& requestedTheme);

        safe_void_coroutine _ControlCompletionsChangedHandler(const winrt::Windows::Foundation::IInspectable sender, const winrt::Microsoft::Terminal::Control::CompletionsChangedEventArgs args);

        void _OpenSuggestions(const Microsoft::Terminal::Control::TermControl& sender, Windows::Foundation::Collections::IVector<winrt::Microsoft::Terminal::Settings::Model::Command> commandsCollection, winrt::TerminalApp::SuggestionsMode mode, winrt::hstring filterText);

        void _ShowWindowChangedHandler(const IInspectable sender, const winrt::Microsoft::Terminal::Control::ShowWindowArgs args);
        Windows::Foundation::IAsyncAction _SearchMissingCommandHandler(const IInspectable sender, const winrt::Microsoft::Terminal::Control::SearchMissingCommandEventArgs args);
        static Windows::Foundation::IAsyncOperation<Windows::Foundation::Collections::IVectorView<winrt::Microsoft::Management::Deployment::MatchResult>> _FindPackageAsync(hstring query);

        void _WindowSizeChanged(const IInspectable sender, const winrt::Microsoft::Terminal::Control::WindowSizeChangedEventArgs args);
        void _windowPropertyChanged(const IInspectable& sender, const winrt::Windows::UI::Xaml::Data::PropertyChangedEventArgs& args);

        void _onTabDragStarting(const winrt::Microsoft::UI::Xaml::Controls::TabView& sender, const winrt::Microsoft::UI::Xaml::Controls::TabViewTabDragStartingEventArgs& e);
        void _onTabStripDragOver(const winrt::Windows::Foundation::IInspectable& sender, const winrt::Windows::UI::Xaml::DragEventArgs& e);
        void _onTabStripDrop(winrt::Windows::Foundation::IInspectable sender, winrt::Windows::UI::Xaml::DragEventArgs e);
        void _onTabDroppedOutside(winrt::Windows::Foundation::IInspectable sender, winrt::Microsoft::UI::Xaml::Controls::TabViewTabDroppedOutsideEventArgs e);

        void _DetachPaneFromWindow(std::shared_ptr<Pane> pane);
        void _DetachTabFromWindow(const winrt::com_ptr<Tab>& tabImpl);
        void _MoveContent(std::vector<winrt::Microsoft::Terminal::Settings::Model::ActionAndArgs>&& actions,
                          const winrt::hstring& windowName,
                          const uint32_t tabIndex,
                          const std::optional<winrt::Windows::Foundation::Point>& dragPoint = std::nullopt);
        void _sendDraggedTabToWindow(const winrt::hstring& windowId, const uint32_t tabIndex, std::optional<winrt::Windows::Foundation::Point> dragPoint);

        void _PopulateContextMenu(const Microsoft::Terminal::Control::TermControl& control, const Microsoft::UI::Xaml::Controls::CommandBarFlyout& sender, const bool withSelection);
        void _PopulateQuickFixMenu(const Microsoft::Terminal::Control::TermControl& control, const Windows::UI::Xaml::Controls::MenuFlyout& sender);
        winrt::Windows::UI::Xaml::Controls::MenuFlyout _CreateRunAsAdminFlyout(int profileIndex);

        winrt::Microsoft::Terminal::Control::TermControl _senderOrActiveControl(const winrt::Windows::Foundation::IInspectable& sender);
        winrt::com_ptr<Tab> _senderOrFocusedTab(const IInspectable& sender);

        void _activePaneChanged(winrt::TerminalApp::Tab tab, Windows::Foundation::IInspectable args);
        safe_void_coroutine _doHandleSuggestions(Microsoft::Terminal::Settings::Model::SuggestionsArgs realArgs);

        void _SendDesktopNotification(const winrt::hstring& tabTitle, const winrt::hstring& body, const winrt::com_ptr<Tab>& tab, const winrt::TerminalApp::IPaneContent& content);

#pragma region ActionHandlers
        // These are all defined in AppActionHandlers.cpp
#define ON_ALL_ACTIONS(action) DECLARE_ACTION_HANDLER(action);
        ALL_SHORTCUT_ACTIONS
        INTERNAL_SHORTCUT_ACTIONS
#undef ON_ALL_ACTIONS
#pragma endregion

        friend class TerminalAppLocalTests::TabTests;
        friend class TerminalAppLocalTests::SettingsTests;
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TerminalPage);
}
