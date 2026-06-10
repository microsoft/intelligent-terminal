/*++
Copyright (c) Microsoft Corporation Licensed under the MIT license.

Class Name:
- WindowEmperor.h

Abstract:
- The WindowEmperor is our class for managing the single Terminal process
  with all our windows. It will be responsible for handling the commandline
  arguments. It will initially try to find another terminal process to
  communicate with. If it does, it'll hand off to the existing process.
- If it determines that it should create a window, it will set up a new thread
  for that window, and a message loop on the main thread for handling global
  state, such as hotkeys and the notification icon.

--*/

#pragma once

class AppHost;
struct TerminalProtocolComServer;

class WindowEmperor
{

public:
    enum UserMessages : UINT
    {
        WM_CLOSE_TERMINAL_WINDOW = WM_USER,
        WM_MESSAGE_BOX_CLOSED,
        WM_IDENTIFY_ALL_WINDOWS,
        WM_NOTIFY_FROM_NOTIFICATION_AREA,
        WM_COM_IDLE_CHECK, // Posted by COM MTA thread when live object count changes
    };

    // Grace period before exiting when no windows and no COM clients remain.
    static constexpr UINT_PTR IDT_COM_IDLE = 42;
    // Grace period after the process becomes fully idle (no windows, no COM).
    static constexpr DWORD COM_IDLE_TIMEOUT_MS = 5000;
    // Maximum time a headless process waits for stale COM objects to
    // disconnect before force-exiting. Covers crashed clients whose
    // stub references haven't been reclaimed by the COM garbage collector.
    static constexpr DWORD COM_STALE_TIMEOUT_MS = 30000;

    WindowEmperor();
    ~WindowEmperor();

    HWND GetMainWindow() const noexcept;
    AppHost* GetWindowById(uint64_t id) const noexcept;
    AppHost* GetWindowByName(std::wstring_view name) const noexcept;
    void CreateNewWindow(winrt::TerminalApp::WindowRequestedArgs args);
    void HandleCommandlineArgs(int nCmdShow);
    void FocusTabInAnyWindow(const winrt::TerminalApp::Tab& tab) const;

    // Protocol server access
    const std::wstring& GetComClsid() const noexcept { return _comClsid; }
    const std::vector<std::shared_ptr<::AppHost>>& GetWindows() const noexcept { return _windows; }
    AppHost* GetMostRecentWindow() const noexcept { return _mostRecentWindow(); }

private:
    struct SummonWindowSelectionArgs
    {
        uint64_t WindowID = 0;
        std::wstring_view WindowName;
        bool OnCurrentDesktop = false;
        winrt::TerminalApp::SummonWindowBehavior SummonBehavior;
    };

    [[nodiscard]] static LRESULT __stdcall _wndProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) noexcept;

    AppHost* _mostRecentWindow() const noexcept;
    bool _summonWindow(const SummonWindowSelectionArgs& args) const;
    void _summonAllWindows() const;
    void _dispatchSpecialKey(const MSG& msg) const;
    void _dispatchCommandline(winrt::TerminalApp::CommandlineArgs args);
    void _dispatchCommandlineCommon(winrt::array_view<const winrt::hstring> args, wil::zwstring_view currentDirectory, wil::zwstring_view envString, uint32_t showWindowCommand);
    safe_void_coroutine _dispatchCommandlineCurrentDesktop(winrt::TerminalApp::CommandlineArgs args);
    LRESULT _messageHandler(HWND window, UINT message, WPARAM wParam, LPARAM lParam) noexcept;
    void _createMessageWindow(const wchar_t* className);
    void _postQuitMessageIfNeeded() const;
    void _updateComIdleTimer();
    DWORD _activeComIdleTimeoutMs{ 0 }; // tracks the currently running timer value to avoid resets
    safe_void_coroutine _showMessageBox(winrt::hstring message, bool error);
    void _notificationAreaMenuRequested(WPARAM wParam);
    void _notificationAreaMenuClicked(WPARAM wParam, LPARAM lParam) const;
    void _hotkeyPressed(long hotkeyIndex);
    void _registerHotKey(int index, const winrt::Microsoft::Terminal::Control::KeyChord& hotkey) noexcept;
    void _unregisterHotKey(int index) noexcept;
    void _setupGlobalHotkeys();
    void _setupSessionPersistence(bool enabled);
    void _persistState(const winrt::Microsoft::Terminal::Settings::Model::ApplicationState& state) const;
    void _finalizeSessionPersistence() const;
    void _checkWindowsForNotificationIcon();
    void _setupAumid(const std::wstring& aumid);

    wil::unique_hwnd _window;
    winrt::TerminalApp::App _app{ nullptr };
    std::vector<std::shared_ptr<::AppHost>> _windows;

    // Protocol server for AI CLI integration
    std::wstring _comClsid; // Stringified CLSID for WT_COM_CLSID env var
    void _initializeProtocolServer();
    std::vector<winrt::Microsoft::Terminal::Settings::Model::GlobalSummonArgs> _hotkeys;
    NOTIFYICONDATA _notificationIcon{};
    UINT WM_TASKBARCREATED = 0;
    HMENU _currentWindowMenu = nullptr;
    bool _notificationIconShown = false;
    bool _skipPersistence = false;
    bool _needsPersistenceCleanup = false;
    SafeDispatcherTimer _persistStateTimer;
    std::optional<bool> _currentSystemThemeIsDark;
    int32_t _windowCount = 0;
    int32_t _messageBoxCount = 0;
    std::wstring _pendingAumidLnkPath;
    std::wstring _pendingAumid;

#if 0 // #ifdef NDEBUG
    static constexpr void _assertIsMainThread() noexcept
    {
    }
#else
    void _assertIsMainThread() const noexcept
    {
        WI_ASSERT_MSG(_mainThreadId == GetCurrentThreadId(), "This part of WindowEmperor must be accessed from the UI thread");
    }
    DWORD _mainThreadId = GetCurrentThreadId();
#endif
};
