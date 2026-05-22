// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "TerminalProtocolComServer.h"
#include "WindowEmperor.h"
#include "AppHost.h"

#include <json/json.h>
#include <til/io.h>
#include "../TerminalProtocol/ProtocolParsing.h"

#include <thread>

namespace ProtocolParsing = Microsoft::Terminal::Protocol::Parsing;

namespace Protocol = winrt::Microsoft::Terminal::Protocol;

// Static state — set once before registration, never mutated.
WindowEmperor* TerminalProtocolComServer::s_emperor = nullptr;

static DWORD g_comRegistration = 0;
static std::shared_mutex g_mtx;
static std::thread g_comMtaThread;
static wil::unique_event g_comMtaStop;

// Static instance tracking for event delivery to COM clients
std::mutex TerminalProtocolComServer::s_instancesMutex;
std::vector<TerminalProtocolComServer*> TerminalProtocolComServer::s_instances;

void TerminalProtocolComServer::s_setEmperor(WindowEmperor* emperor) noexcept
{
    s_emperor = emperor;
}

HRESULT TerminalProtocolComServer::s_StartListening()
try
{
    std::unique_lock lock{ g_mtx };

    // Register the COM class factory on a dedicated MTA thread so that
    // incoming COM calls are dispatched to MTA worker threads rather than
    // the STA/UI thread. This keeps long-running calls off the UI thread.
    g_comMtaStop.create(wil::EventOptions::ManualReset);

    wil::unique_event ready(wil::EventOptions::ManualReset);
    HRESULT regHr = S_OK;

    g_comMtaThread = std::thread([&ready, &regHr]() {
        auto coInit = wil::CoInitializeEx(COINIT_MULTITHREADED);

        auto factory = winrt::make_self<Factory<TerminalProtocolComServer>>();

        regHr = CoRegisterClassObject(
            __uuidof(TerminalProtocolComServer),
            factory.as<::IUnknown>().get(),
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE,
            &g_comRegistration);

        ready.SetEvent();

        // Keep this MTA thread alive so the COM registration stays active.
        WaitForSingleObject(g_comMtaStop.get(), INFINITE);
    });

    ready.wait();
    RETURN_IF_FAILED(regHr);
    return S_OK;
}
CATCH_RETURN()

HRESULT TerminalProtocolComServer::s_StopListening()
{
    std::unique_lock lock{ g_mtx };

    if (g_comRegistration)
    {
        RETURN_IF_FAILED(CoRevokeClassObject(g_comRegistration));
        g_comRegistration = 0;
    }

    // Signal the MTA thread to exit
    if (g_comMtaStop)
    {
        g_comMtaStop.SetEvent();
    }
    if (g_comMtaThread.joinable())
    {
        g_comMtaThread.join();
    }

    return S_OK;
}

TerminalProtocolComServer::~TerminalProtocolComServer()
{
    _removeInstance();
}

void TerminalProtocolComServer::_addInstance()
{
    std::lock_guard lock{ s_instancesMutex };
    if (_instanceRegistered)
        return;
    _instanceRegistered = true;
    s_instances.push_back(this);
}

void TerminalProtocolComServer::_removeInstance()
{
    std::lock_guard lock{ s_instancesMutex };
    std::erase(s_instances, this);
}

// ============================================================================
// Helper: get TerminalPage from AppHost
// ============================================================================

static winrt::TerminalApp::TerminalPage _getPage(AppHost* host)
{
    if (!host)
        return nullptr;
    const auto logic = host->Logic();
    if (!logic)
        return nullptr;
    const auto root = logic.GetRoot();
    if (!root)
        return nullptr;
    return root.try_as<winrt::TerminalApp::TerminalPage>();
}

// Helper: parse a JSON string into Json::Value
static bool _parseJson(const std::string& str, Json::Value& out)
{
    Json::CharReaderBuilder rb;
    std::string errs;
    std::istringstream ss(str);
    return Json::parseFromStream(rb, ss, &out, &errs);
}

void TerminalProtocolComServer::_ensurePageEventsRegistered()
{
    if (!s_emperor)
        return;

    // Use a retryable pattern instead of call_once: if no page is found on
    // the first Subscribe() call (e.g. during early startup), we allow retry
    // on subsequent calls rather than permanently giving up.
    static std::atomic<bool> s_registered{ false };
    static std::mutex s_regMutex;

    if (s_registered.load(std::memory_order_acquire))
        return;

    std::lock_guard lock{ s_regMutex };
    if (s_registered.load(std::memory_order_relaxed))
        return;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        page.ProtocolVtSequenceReceived(
            [](auto&&, const winrt::hstring& eventJson) {
                s_NotifyEventToComClients(winrt::to_string(eventJson));
            });
        s_registered.store(true, std::memory_order_release);
        return;
    }
    // No page found — don't mark registered, allow retry on next Subscribe().
}

void TerminalProtocolComServer::s_NotifyEventToComClients(const std::string& eventJson)
{
    const auto eventHstr = winrt::to_hstring(eventJson);

    // Snapshot callbacks under lock, then invoke outside the lock to avoid
    // deadlocks if a callback reenters the server (e.g. via SendEvent).
    std::vector<Protocol::IProtocolEventCallback> callbacks;
    {
        std::lock_guard lock{ s_instancesMutex };
        for (auto* instance : s_instances)
        {
            std::lock_guard cbLock{ instance->_callbackMutex };
            if (instance->_callback)
                callbacks.push_back(instance->_callback);
        }
    }

    for (auto& callback : callbacks)
    {
        try
        {
            callback.OnEvent(eventHstr);
        }
        catch (...)
        {
            // Client disconnected — find and clear the callback.
            std::lock_guard lock{ s_instancesMutex };
            for (auto* instance : s_instances)
            {
                std::lock_guard cbLock{ instance->_callbackMutex };
                if (instance->_callback == callback)
                {
                    instance->_callback = nullptr;
                    break;
                }
            }
        }
    }
}

// ============================================================================
// IProtocolServer
// ============================================================================

Protocol::PaneInfo TerminalProtocolComServer::GetActivePane()
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto host = s_emperor->GetMostRecentWindow();
    THROW_HR_IF(E_FAIL, !host);

    const auto page = _getPage(host);
    THROW_HR_IF(E_FAIL, !page);

    auto info = page.GetProtocolActivePane().get();
    THROW_HR_IF(E_FAIL, info.SessionId == winrt::guid{});

    // TerminalPage doesn't know the window ID — fill it in here.
    const auto& props = host->Logic().WindowProperties();
    info.WindowId = props.WindowId();

    return info;
}

winrt::com_array<Protocol::WindowInfo> TerminalProtocolComServer::ListWindows()
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto mostRecent = s_emperor->GetMostRecentWindow();
    std::vector<Protocol::WindowInfo> items;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto logic = host->Logic();
        if (!logic)
            continue;

        const auto& props = logic.WindowProperties();

        Protocol::WindowInfo info{};
        info.WindowId = props.WindowId();
        info.Title = props.WindowNameForDisplay();
        info.IsFocused = (host.get() == mostRecent);
        info.TabCount = logic.TabCount();
        items.push_back(std::move(info));
    }

    return { items.begin(), items.end() };
}

// ============================================================================
// Queries
// ============================================================================

Protocol::AuthResult TerminalProtocolComServer::Authenticate(winrt::hstring const& /*token*/)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    // DEV BYPASS: always authenticate — credential plumbing not yet implemented.
    _authenticated = true;

    // Register for event delivery on successful authentication
    if (_authenticated)
    {
        _addInstance();
    }

    Protocol::AuthResult result{};
    result.Authenticated = _authenticated;
    // 2.2 — SendInput restored on the COM surface; pane identifiers remain GUIDs.
    result.ProtocolVersion = L"2.2";
    return result;
}

winrt::hstring TerminalProtocolComServer::GetCapabilities()
{
    static const std::vector<std::string> supportedMethods = {
        "authenticate",
        "get_capabilities",
        "get_active_pane",
        "list_windows",
        "list_tabs",
        "list_panes",
        "read_pane_output",
        "get_process_status",
        "get_session_variable",
        "get_settings",
        "create_tab",
        "split_pane",
        "close_pane",
        "send_input",
        "set_session_variable",
        "subscribe",
        "unsubscribe",
        "send_event",
    };

    Json::Value methods(Json::arrayValue);
    for (const auto& m : supportedMethods)
        methods.append(m);

    Json::StreamWriterBuilder wb;
    wb["indentation"] = "";
    return winrt::to_hstring(Json::writeString(wb, methods));
}

// ============================================================================
// Queries
// ============================================================================

winrt::com_array<Protocol::TabInfo> TerminalProtocolComServer::ListTabs(
    uint64_t windowIdFilter)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    std::vector<Protocol::TabInfo> items;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto logic = host->Logic();
        if (!logic)
            continue;

        const auto& props = logic.WindowProperties();
        if (windowIdFilter != 0 && props.WindowId() != windowIdFilter)
            continue;

        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto windowId = props.WindowId();
        const auto tabs = page.GetProtocolTabs().get();
        for (uint32_t i = 0; i < tabs.Size(); ++i)
        {
            auto t = tabs.GetAt(i);
            t.WindowId = windowId;
            items.push_back(std::move(t));
        }
    }

    return { items.begin(), items.end() };
}

winrt::com_array<Protocol::PaneInfo> TerminalProtocolComServer::ListPanes(
    uint64_t windowIdFilter,
    uint32_t tabIdFilter)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    std::vector<Protocol::PaneInfo> items;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto logic = host->Logic();
        if (!logic)
            continue;

        const auto& props = logic.WindowProperties();
        if (windowIdFilter != 0 && props.WindowId() != windowIdFilter)
            continue;

        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto windowId = props.WindowId();
        const auto panes = page.GetProtocolPanes(tabIdFilter).get();
        for (uint32_t i = 0; i < panes.Size(); ++i)
        {
            auto p = panes.GetAt(i);
            p.WindowId = windowId;
            items.push_back(std::move(p));
        }
    }

    return { items.begin(), items.end() };
}

Protocol::PaneOutput TerminalProtocolComServer::ReadPaneOutput(
    winrt::guid sessionId,
    winrt::hstring const& source,
    int32_t maxLines)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto effectiveSource = source.empty() ? winrt::hstring{ L"scrollback" } : source;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        auto info = page.ReadProtocolPaneOutput(sessionId, effectiveSource, maxLines).get();
        if (info.SessionId != winrt::guid{})
            return info;
    }

    winrt::throw_hresult(E_FAIL); // Pane not found
}

Protocol::ProcessStatus TerminalProtocolComServer::GetProcessStatus(
    winrt::guid sessionId)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        auto info = page.GetProtocolProcessStatus(sessionId).get();
        if (info.SessionId != winrt::guid{})
            return info;
    }

    winrt::throw_hresult(E_FAIL);
}

Protocol::SessionVariable TerminalProtocolComServer::GetSessionVariable(
    winrt::guid sessionId,
    winrt::hstring const& name)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        auto info = page.GetProtocolSessionVariable(sessionId, name).get();
        if (info.SessionId != winrt::guid{})
            return info;
    }

    winrt::throw_hresult(E_FAIL);
}

winrt::hstring TerminalProtocolComServer::GetSettings()
{
    const std::filesystem::path settingsPath{
        std::wstring_view{ winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings::SettingsPath() }
    };
    return winrt::to_hstring(til::io::read_file_as_utf8_string_if_exists(settingsPath));
}

// ============================================================================
// Mutations
// ============================================================================

Protocol::TabCreationResult TerminalProtocolComServer::CreateTab(
    uint64_t windowId,
    winrt::hstring const& profile,
    winrt::hstring const& commandline,
    winrt::hstring const& title,
    winrt::hstring const& startingDirectory,
    bool suppressAppTitle,
    bool background)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    // Find target window.
    AppHost* targetHost = nullptr;
    if (windowId != 0)
    {
        targetHost = s_emperor->GetWindowById(windowId);
    }
    else
    {
        targetHost = s_emperor->GetMostRecentWindow();
    }
    THROW_HR_IF(E_FAIL, !targetHost);

    const auto page = _getPage(targetHost);
    THROW_HR_IF(E_FAIL, !page);

    // Build NewTerminalArgs.
    winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs newTermArgs;
    if (!profile.empty())
        newTermArgs.Profile(profile);
    if (!commandline.empty())
        newTermArgs.Commandline(commandline);
    if (!startingDirectory.empty())
        newTermArgs.StartingDirectory(startingDirectory);
    if (!title.empty())
    {
        newTermArgs.TabTitle(title);
        if (suppressAppTitle)
            newTermArgs.SuppressApplicationTitle(true);
    }

    auto cr = page.CreateProtocolTab(newTermArgs, background).get();
    THROW_HR_IF(E_FAIL, cr.SessionId == winrt::guid{});

    const auto& props = targetHost->Logic().WindowProperties();
    cr.WindowId = props.WindowId();
    return cr;
}

Protocol::TabCreationResult TerminalProtocolComServer::SplitPane(
    winrt::guid sessionId,
    winrt::hstring const& direction,
    float size,
    winrt::hstring const& profile,
    winrt::hstring const& commandline,
    bool background)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, sessionId == winrt::guid{});

    // Map direction string to SplitDirection enum via shared parsing logic.
    const auto parsedDir = ProtocolParsing::ParseSplitDirection(winrt::to_string(direction));
    auto splitDir = static_cast<winrt::Microsoft::Terminal::Settings::Model::SplitDirection>(
        static_cast<int>(parsedDir));

    // Build NewTerminalArgs.
    winrt::Microsoft::Terminal::Settings::Model::NewTerminalArgs newTermArgs;
    if (!profile.empty())
        newTermArgs.Profile(profile);
    if (!commandline.empty())
        newTermArgs.Commandline(commandline);

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        auto cr = page.SplitProtocolPane(sessionId, splitDir, size, newTermArgs, background).get();
        if (cr.SessionId == winrt::guid{})
            continue; // pane not in this window

        const auto& props = host->Logic().WindowProperties();
        cr.WindowId = props.WindowId();
        return cr;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::ClosePane(winrt::guid sessionId)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, sessionId == winrt::guid{});

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.CloseProtocolPane(sessionId).get())
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::SendInput(winrt::guid sessionId, winrt::hstring const& text)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, sessionId == winrt::guid{});

    // Empty input is a no-op, matching ControlCore::SendInput semantics so
    // COM clients that send "" don't see surprising E_INVALIDARG failures.
    if (text.empty())
    {
        return;
    }

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.SendProtocolInput(sessionId, text).get())
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::FocusPane(winrt::guid sessionId)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, sessionId == winrt::guid{});

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.FocusProtocolPane(sessionId).get())
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::SetSessionVariable(
    winrt::guid sessionId,
    winrt::hstring const& name,
    winrt::hstring const& value)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, sessionId == winrt::guid{});
    THROW_HR_IF(E_INVALIDARG, name.empty());

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.SetProtocolSessionVariable(sessionId, name, value).get())
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

// ============================================================================
// Events — push-based via callback
// ============================================================================

void TerminalProtocolComServer::Subscribe(Protocol::IProtocolEventCallback const& callback)
{
    THROW_HR_IF(E_INVALIDARG, !callback);
    THROW_HR_IF(E_ACCESSDENIED, !_authenticated);

    {
        std::lock_guard lock{ _callbackMutex };
        _callback = callback;
    }

    // Ensure page events are wired up (one-time global init).
    _ensurePageEventsRegistered();
}

void TerminalProtocolComServer::Unsubscribe()
{
    std::lock_guard lock{ _callbackMutex };
    _callback = nullptr;
}

void TerminalProtocolComServer::SendEvent(winrt::hstring const& eventJson)
{
    THROW_HR_IF(E_ACCESSDENIED, !_authenticated);

    auto jsonStr = winrt::to_string(eventJson);
    Json::Value evt;
    const auto route = ProtocolParsing::ClassifySendEvent(jsonStr, evt);
    THROW_HR_IF(E_INVALIDARG, route == ProtocolParsing::SendEventRoute::Invalid);

    switch (route)
    {
    case ProtocolParsing::SendEventRoute::AutofixState:
        _dispatchAutofixStateToPage(eventJson);
        return;
    case ProtocolParsing::SendEventRoute::AgentStatus:
        _dispatchAgentStatusToPage(eventJson);
        return;
    case ProtocolParsing::SendEventRoute::CloseAgentPane:
        // User pressed Ctrl+C twice in the wta TUI. Marshal to the UI
        // thread and tell TerminalPage to tear down the shared agent pane.
        _dispatchCloseAgentPaneToPage(eventJson);
        return;
    case ProtocolParsing::SendEventRoute::ViewChanged:
        // wta TUI flipped its internal view (Esc out of session view,
        // `/sessions` slash command). C++ mirrors the new view onto the
        // agent bar title + the bottom bar's sessions/chat highlight.
        _dispatchViewChangedToPage(eventJson);
        return;
    case ProtocolParsing::SendEventRoute::ResumeInNewAgentTab:
        // Session view's Shift+Enter handler in the wta TUI. Carries
        // {session_id, cwd} for a historical session. WT creates a new
        // tab, reconciles the shared agent pane onto it, then publishes
        // a `load_session` event back to wta with the new tab's StableId
        // so the existing ACP connection calls `session/load` for that
        // tab. See TerminalPage::OnResumeInNewAgentTabRequested.
        _dispatchResumeInNewAgentTabToPage(eventJson);
        return;
    case ProtocolParsing::SendEventRoute::Broadcast:
    {
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        s_NotifyEventToComClients(Json::writeString(wb, evt));
        return;
    }
    default:
        return;
    }
}

void TerminalProtocolComServer::_dispatchAutofixStateToPage(const winrt::hstring& eventJson)
{
    if (!s_emperor)
    {
        return;
    }
    // Find any window's TerminalPage and dispatch to its UI thread. The
    // bottom bar state is per-window; for v1 we fan out to every window so
    // whichever is focused shows the update.
    for (const auto& host : s_emperor->GetWindows())
    {
        auto page = _getPage(host.get());
        if (!page)
        {
            continue;
        }
        const auto dispatcher = page.Dispatcher();
        if (!dispatcher)
        {
            continue;
        }
        // SendEvent runs on an arbitrary COM MTA thread; XAML requires the
        // UI thread. Capture by value so the lambda owns the hstring/page.
        dispatcher.RunAsync(
            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
            [page, eventJson]() {
                try
                {
                    page.OnAutofixStateChanged(eventJson);
                }
                catch (...)
                {
                    // Swallow: page may have been torn down during dispatch.
                }
            });
    }
}

void TerminalProtocolComServer::_dispatchAgentStatusToPage(const winrt::hstring& eventJson)
{
    if (!s_emperor)
    {
        return;
    }
    // Same fan-out shape as autofix: every window gets the event so its
    // AgentPaneContent (if any) can update. Per-window owns its own agent leaf.
    for (const auto& host : s_emperor->GetWindows())
    {
        auto page = _getPage(host.get());
        if (!page)
        {
            continue;
        }
        const auto dispatcher = page.Dispatcher();
        if (!dispatcher)
        {
            continue;
        }
        dispatcher.RunAsync(
            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
            [page, eventJson]() {
                try
                {
                    page.OnAgentStatusChanged(eventJson);
                }
                catch (...)
                {
                    // Swallow: page may have been torn down during dispatch.
                }
            });
    }
}

void TerminalProtocolComServer::_dispatchCloseAgentPaneToPage(const winrt::hstring& eventJson)
{
    if (!s_emperor)
    {
        return;
    }
    // Fan out to every window; the wta that emitted this event lives in one of
    // them and only that window's TerminalPage has the matching _agentPane.
    // Pages with no agent pane no-op the call (see OnCloseAgentPaneRequested).
    for (const auto& host : s_emperor->GetWindows())
    {
        auto page = _getPage(host.get());
        if (!page)
        {
            continue;
        }
        const auto dispatcher = page.Dispatcher();
        if (!dispatcher)
        {
            continue;
        }
        dispatcher.RunAsync(
            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
            [page, eventJson]() {
                try
                {
                    page.OnCloseAgentPaneRequested(eventJson);
                }
                catch (...)
                {
                    // Swallow: page may have been torn down during dispatch.
                }
            });
    }
}

void TerminalProtocolComServer::_dispatchViewChangedToPage(const winrt::hstring& eventJson)
{
    if (!s_emperor)
    {
        return;
    }
    // Same fan-out shape as the other dispatchers: the agent pane lives in
    // exactly one window, but we don't know which from here, and pages with
    // no agent pane no-op the call (see OnAgentViewChanged).
    for (const auto& host : s_emperor->GetWindows())
    {
        auto page = _getPage(host.get());
        if (!page)
        {
            continue;
        }
        const auto dispatcher = page.Dispatcher();
        if (!dispatcher)
        {
            continue;
        }
        dispatcher.RunAsync(
            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
            [page, eventJson]() {
                try
                {
                    page.OnAgentViewChanged(eventJson);
                }
                catch (...)
                {
                    // Swallow: page may have been torn down during dispatch.
                }
            });
    }
}

void TerminalProtocolComServer::_dispatchResumeInNewAgentTabToPage(const winrt::hstring& eventJson)
{
    if (!s_emperor)
    {
        return;
    }
    // Same fan-out shape as the other dispatchers. The shared agent pane
    // lives in exactly one window; pages with no agent pane no-op the call
    // (see OnResumeInNewAgentTabRequested).
    for (const auto& host : s_emperor->GetWindows())
    {
        auto page = _getPage(host.get());
        if (!page)
        {
            continue;
        }
        const auto dispatcher = page.Dispatcher();
        if (!dispatcher)
        {
            continue;
        }
        dispatcher.RunAsync(
            winrt::Windows::UI::Core::CoreDispatcherPriority::Normal,
            [page, eventJson]() {
                try
                {
                    page.OnResumeInNewAgentTabRequested(eventJson);
                }
                catch (...)
                {
                    // Swallow: page may have been torn down during dispatch.
                }
            });
    }
}
