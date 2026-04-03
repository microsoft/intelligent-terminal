// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "TerminalProtocolComServer.h"
#include "WindowEmperor.h"
#include "AppHost.h"

#include <json/json.h>
#include <til/io.h>

#include <thread>

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
bool TerminalProtocolComServer::s_pageEventsRegistered = false;

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
    // the STA/UI thread.  This is critical for methods that block
    // (QuickPick waits for user input) — dispatching those on the UI
    // thread would deadlock or freeze the app.
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
    if (s_pageEventsRegistered || !s_emperor)
        return;
    s_pageEventsRegistered = true;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        page.ProtocolVtSequenceReceived(
            [](auto&&, const winrt::hstring& eventJson) {
                s_NotifyEventToComClients(winrt::to_string(eventJson));
            });
        break; // Single-window for now
    }
}

void TerminalProtocolComServer::s_NotifyEventToComClients(const std::string& eventJson)
{
    const auto eventHstr = winrt::to_hstring(eventJson);

    std::lock_guard lock{ s_instancesMutex };
    for (auto* instance : s_instances)
    {
        // Fire the WinRT event on each instance.
        // TODO: Once EventReceived is in the IDL, cross-process clients
        //       will receive these via MBM automatically.
        instance->_eventReceived(nullptr, eventHstr);
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

    auto info = page.GetProtocolActivePane();
    THROW_HR_IF(E_FAIL, info.PaneId.empty());

    // TerminalPage doesn't know the window ID — fill it in here.
    const auto& props = host->Logic().WindowProperties();
    info.WindowId = winrt::to_hstring(std::to_string(props.WindowId()));

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
        info.WindowId = winrt::to_hstring(std::to_string(props.WindowId()));
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

Protocol::AuthResult TerminalProtocolComServer::Authenticate(winrt::hstring const& token)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto tokenStr = winrt::to_string(token);
    const auto& expectedToken = s_emperor->GetMcpToken();

    // DEV BYPASS: allow empty token to authenticate without credentials.
    // TODO: Remove this bypass before shipping.
    if (tokenStr.empty())
    {
        _authenticated = true;
    }
    else
    {
        // Constant-time comparison to prevent timing attacks.
        bool match = (tokenStr.size() == expectedToken.size());
        volatile bool dummy = false;
        for (size_t i = 0; i < std::min(tokenStr.size(), expectedToken.size()); ++i)
        {
            if (tokenStr[i] != expectedToken[i])
                dummy = true;
        }
        _authenticated = match && !dummy;
    }

    // Register for event delivery on successful authentication
    if (_authenticated)
    {
        _addInstance();
    }

    Protocol::AuthResult result{};
    result.Authenticated = _authenticated;
    result.ProtocolVersion = L"1.0";
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
        "set_settings",
        "quick_pick",
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
    winrt::hstring const& windowIdFilter)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto filter = winrt::to_string(windowIdFilter);
    std::vector<Protocol::TabInfo> items;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto logic = host->Logic();
        if (!logic)
            continue;

        const auto& props = logic.WindowProperties();
        const auto windowIdStr = std::to_string(props.WindowId());
        if (!filter.empty() && windowIdStr != filter)
            continue;

        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto windowIdHstr = winrt::to_hstring(windowIdStr);
        const auto tabs = page.GetProtocolTabs();
        for (uint32_t i = 0; i < tabs.Size(); ++i)
        {
            auto t = tabs.GetAt(i);
            t.WindowId = windowIdHstr;
            items.push_back(std::move(t));
        }
    }

    return { items.begin(), items.end() };
}

winrt::com_array<Protocol::PaneInfo> TerminalProtocolComServer::ListPanes(
    winrt::hstring const& windowIdFilter,
    winrt::hstring const& tabIdFilter)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    const auto winFilter = winrt::to_string(windowIdFilter);
    std::vector<Protocol::PaneInfo> items;

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto logic = host->Logic();
        if (!logic)
            continue;

        const auto& props = logic.WindowProperties();
        const auto windowIdStr = std::to_string(props.WindowId());
        if (!winFilter.empty() && windowIdStr != winFilter)
            continue;

        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto windowIdHstr = winrt::to_hstring(windowIdStr);
        const auto panes = page.GetProtocolPanes(tabIdFilter);
        for (uint32_t i = 0; i < panes.Size(); ++i)
        {
            auto p = panes.GetAt(i);
            p.WindowId = windowIdHstr;
            items.push_back(std::move(p));
        }
    }

    return { items.begin(), items.end() };
}

Protocol::PaneOutput TerminalProtocolComServer::ReadPaneOutput(
    winrt::hstring const& paneId,
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

        auto info = page.ReadProtocolPaneOutput(paneId, effectiveSource, maxLines);
        if (!info.PaneId.empty())
            return info;
    }

    winrt::throw_hresult(E_FAIL); // Pane not found
}

Protocol::ProcessStatus TerminalProtocolComServer::GetProcessStatus(
    winrt::hstring const& paneId)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        auto info = page.GetProtocolProcessStatus(paneId);
        if (!info.PaneId.empty())
            return info;
    }

    winrt::throw_hresult(E_FAIL);
}

Protocol::SessionVariable TerminalProtocolComServer::GetSessionVariable(
    winrt::hstring const& paneId,
    winrt::hstring const& name)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        auto info = page.GetProtocolSessionVariable(paneId, name);
        if (!info.PaneId.empty())
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
    winrt::hstring const& windowId,
    winrt::hstring const& profile,
    winrt::hstring const& commandline,
    winrt::hstring const& title,
    bool suppressAppTitle,
    bool injectMcpCredentials,
    bool background)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    // Find target window.
    AppHost* targetHost = nullptr;
    if (!windowId.empty())
    {
        targetHost = s_emperor->GetWindowById(std::stoull(winrt::to_string(windowId)));
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
    if (!title.empty())
    {
        newTermArgs.TabTitle(title);
        if (suppressAppTitle)
            newTermArgs.SuppressApplicationTitle(true);
    }

    // Inject MCP credentials when requested.
    if (injectMcpCredentials)
    {
        const auto& token = s_emperor->GetMcpToken();
        if (!token.empty())
        {
            page.SetPendingProtocolEnv(L"WT_MCP_TOKEN", winrt::to_hstring(token));
            page.SetPendingProtocolEnv(L"WT_PIPE_NAME", winrt::hstring{ s_emperor->GetProtocolPipeName() });
            const auto& comClsid = s_emperor->GetComClsid();
            if (!comClsid.empty())
                page.SetPendingProtocolEnv(L"WT_COM_CLSID", winrt::hstring{ comClsid });
        }
    }

    auto cr = page.CreateProtocolTab(newTermArgs, background);
    THROW_HR_IF(E_FAIL, cr.TabId.empty());

    const auto& props = targetHost->Logic().WindowProperties();
    cr.WindowId = winrt::to_hstring(std::to_string(props.WindowId()));
    return cr;
}

Protocol::TabCreationResult TerminalProtocolComServer::SplitPane(
    winrt::hstring const& paneId,
    winrt::hstring const& direction,
    float size,
    winrt::hstring const& profile,
    winrt::hstring const& commandline,
    bool injectMcpCredentials,
    bool background)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, paneId.empty());

    // Map direction string to SplitDirection enum.
    auto splitDir = winrt::Microsoft::Terminal::Settings::Model::SplitDirection::Right;
    if (!direction.empty())
    {
        const auto dirStr = winrt::to_string(direction);
        if (dirStr == "left")
            splitDir = winrt::Microsoft::Terminal::Settings::Model::SplitDirection::Left;
        else if (dirStr == "up")
            splitDir = winrt::Microsoft::Terminal::Settings::Model::SplitDirection::Up;
        else if (dirStr == "down")
            splitDir = winrt::Microsoft::Terminal::Settings::Model::SplitDirection::Down;
    }

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

        // Inject MCP credentials when requested.
        if (injectMcpCredentials)
        {
            const auto& token = s_emperor->GetMcpToken();
            if (!token.empty())
            {
                page.SetPendingProtocolEnv(L"WT_MCP_TOKEN", winrt::to_hstring(token));
                page.SetPendingProtocolEnv(L"WT_PIPE_NAME", winrt::hstring{ s_emperor->GetProtocolPipeName() });
                const auto& comClsid = s_emperor->GetComClsid();
                if (!comClsid.empty())
                    page.SetPendingProtocolEnv(L"WT_COM_CLSID", winrt::hstring{ comClsid });
            }
        }

        auto cr = page.SplitProtocolPane(paneId, splitDir, size, newTermArgs, background);
        if (cr.TabId.empty())
            continue; // pane not in this window

        const auto& props = host->Logic().WindowProperties();
        cr.WindowId = winrt::to_hstring(std::to_string(props.WindowId()));
        return cr;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::ClosePane(winrt::hstring const& paneId)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, paneId.empty());

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.CloseProtocolPane(paneId))
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::SendInput(
    winrt::hstring const& paneId,
    winrt::hstring const& text)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, paneId.empty());
    THROW_HR_IF(E_INVALIDARG, text.empty());

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.SendProtocolInput(paneId, text))
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

void TerminalProtocolComServer::SetSessionVariable(
    winrt::hstring const& paneId,
    winrt::hstring const& name,
    winrt::hstring const& value)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);
    THROW_HR_IF(E_INVALIDARG, paneId.empty());
    THROW_HR_IF(E_INVALIDARG, name.empty());

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        if (page.SetProtocolSessionVariable(paneId, name, value))
            return;
    }

    winrt::throw_hresult(E_FAIL);
}

winrt::hstring TerminalProtocolComServer::SetSettings(
    winrt::hstring const& settingsContent)
{
    const auto contentStr = winrt::to_string(settingsContent);
    THROW_HR_IF(E_INVALIDARG, contentStr.empty());

    // Validate that it's valid JSON.
    Json::Value parsedSettings;
    THROW_HR_IF(E_INVALIDARG, !_parseJson(contentStr, parsedSettings));

    // Get the settings path and create a backup.
    const std::filesystem::path settingsPath{
        std::wstring_view{ winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings::SettingsPath() }
    };
    const auto settingsDir = settingsPath.parent_path();

    // Create timestamped backup.
    const auto now = std::chrono::system_clock::now();
    const auto time = std::chrono::system_clock::to_time_t(now);
    std::tm tm{};
    localtime_s(&tm, &time);

    wchar_t timeStr[64];
    wcsftime(timeStr, std::size(timeStr), L"%Y-%m-%dT%H-%M-%S", &tm);

    const auto backup = settingsDir / fmt::format(L"settings.backup.{}.json", timeStr);

    // Copy current settings to backup.
    std::error_code ec;
    std::filesystem::copy_file(settingsPath, backup, std::filesystem::copy_options::overwrite_existing, ec);

    // Clean up old backups — keep only the most recent 5.
    std::vector<std::filesystem::path> backups;
    for (const auto& entry : std::filesystem::directory_iterator(settingsDir, ec))
    {
        if (entry.is_regular_file() && entry.path().filename().wstring().starts_with(L"settings.backup."))
            backups.push_back(entry.path());
    }
    if (backups.size() > 5)
    {
        std::sort(backups.begin(), backups.end());
        for (size_t i = 0; i < backups.size() - 5; ++i)
            std::filesystem::remove(backups[i], ec);
    }

    // Write the new settings.
    til::io::write_utf8_string_to_file_atomic(settingsPath, contentStr);

    return winrt::hstring{ backup.wstring() };
}

// ============================================================================
// Interactive
// ============================================================================

Protocol::QuickPickResult TerminalProtocolComServer::QuickPick(
    winrt::hstring const& title,
    winrt::array_view<winrt::hstring const> choices,
    bool allowFreeInput)
{
    THROW_HR_IF(E_NOT_VALID_STATE, !s_emperor);

    // Serialize choices to JSON for ShowProtocolQuickPick.
    Json::Value choicesArr(Json::arrayValue);
    for (const auto& choice : choices)
    {
        choicesArr.append(winrt::to_string(choice));
    }
    Json::StreamWriterBuilder wb;
    wb["indentation"] = "";
    const auto choicesJson = winrt::to_hstring(Json::writeString(wb, choicesArr));

    const auto host = s_emperor->GetMostRecentWindow();
    THROW_HR_IF(E_FAIL, !host);

    const auto page = _getPage(host);
    THROW_HR_IF(E_FAIL, !page);

    const auto resultJson = winrt::to_string(
        page.ShowProtocolQuickPick(title, choicesJson, allowFreeInput));
    THROW_HR_IF(E_FAIL, resultJson.empty());

    Json::Value r;
    THROW_HR_IF(E_FAIL, !_parseJson(resultJson, r));

    Protocol::QuickPickResult result{};
    result.Cancelled = r.get("cancelled", true).asBool();
    result.Selected = winrt::to_hstring(r.get("selected", "").asString());
    return result;
}
