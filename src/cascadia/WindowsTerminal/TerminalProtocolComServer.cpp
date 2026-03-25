// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "TerminalProtocolComServer.h"
#include "ProtocolRequestHandler.h"
#include "WindowEmperor.h"
#include "AppHost.h"

#include <json/json.h>
#include <til/io.h>

using namespace Microsoft::WRL;

// Static state — set once before registration, never mutated.
WindowEmperor* TerminalProtocolComServer::s_emperor = nullptr;
ProtocolRequestHandler* TerminalProtocolComServer::s_handler = nullptr;

static DWORD g_comRegistration = 0;
static std::shared_mutex g_mtx;

void TerminalProtocolComServer::s_setEmperor(WindowEmperor* emperor) noexcept
{
    s_emperor = emperor;
}

void TerminalProtocolComServer::s_setHandler(ProtocolRequestHandler* handler) noexcept
{
    s_handler = handler;
}

HRESULT TerminalProtocolComServer::s_StartListening()
try
{
    std::unique_lock lock{ g_mtx };

    const auto classFactory = Make<SimpleClassFactory<TerminalProtocolComServer>>();
    RETURN_LAST_ERROR_IF_NULL(classFactory);

    ComPtr<IUnknown> unk;
    RETURN_IF_FAILED(classFactory.As(&unk));

    RETURN_IF_FAILED(CoRegisterClassObject(
        __uuidof(TerminalProtocolComServer),
        unk.Get(),
        CLSCTX_LOCAL_SERVER,
        REGCLS_MULTIPLEUSE,
        &g_comRegistration));

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

    return S_OK;
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

// ============================================================================
// JSON fallback — delegates to ProtocolRequestHandler
// ============================================================================

STDMETHODIMP TerminalProtocolComServer::HandleRequest(BSTR requestJson, BSTR* responseJson)
try
{
    RETURN_HR_IF_NULL(E_POINTER, responseJson);
    *responseJson = nullptr;
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);
    RETURN_HR_IF_NULL(E_INVALIDARG, requestJson);

    const auto reqWide = std::wstring_view(requestJson, SysStringLen(requestJson));
    const auto reqUtf8 = winrt::to_string(reqWide);

    Json::Value request;
    if (!_parseJson(reqUtf8, request))
    {
        Json::Value errResp;
        errResp["type"] = "response";
        errResp["id"] = "";
        errResp["result"] = Json::nullValue;
        Json::Value err;
        err["code"] = "parse_error";
        err["message"] = "Failed to parse request JSON.";
        errResp["error"] = err;

        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        *responseJson = SysAllocString(winrt::to_hstring(Json::writeString(wb, errResp)).c_str());
        return S_OK;
    }

    const auto response = s_handler->HandleRequest(request, _authenticated);

    Json::StreamWriterBuilder wb;
    wb["indentation"] = "";
    *responseJson = SysAllocString(winrt::to_hstring(Json::writeString(wb, response)).c_str());
    return S_OK;
}
CATCH_RETURN()

// ============================================================================
// Meta
// ============================================================================

STDMETHODIMP TerminalProtocolComServer::Authenticate(BSTR token, BOOL* authenticated, BSTR* protocolVersion)
try
{
    RETURN_HR_IF_NULL(E_POINTER, authenticated);
    RETURN_HR_IF_NULL(E_POINTER, protocolVersion);
    *authenticated = FALSE;
    *protocolVersion = nullptr;

    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);

    const auto tokenStr = token ? winrt::to_string(std::wstring_view(token, SysStringLen(token))) : std::string{};

    // Build a JSON request and delegate to the existing handler.
    Json::Value params;
    params["token"] = tokenStr;
    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-auth";
    request["method"] = "authenticate";
    request["params"] = params;

    s_handler->HandleRequest(request, _authenticated);

    *authenticated = _authenticated ? TRUE : FALSE;
    *protocolVersion = SysAllocString(L"1.0");
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::GetCapabilities(BSTR* protocolVersion, BSTR* supportedMethodsJson)
try
{
    RETURN_HR_IF_NULL(E_POINTER, protocolVersion);
    RETURN_HR_IF_NULL(E_POINTER, supportedMethodsJson);

    *protocolVersion = SysAllocString(L"1.0");

    // Build JSON array of method names from the canonical list in ProtocolRequestHandler.
    // "quick_pick" is intentionally excluded — it blocks the UI thread and isn't
    // supported over the COM transport yet.
    Json::Value methods(Json::arrayValue);
    for (const auto& m : ProtocolRequestHandler::GetSupportedMethods())
    {
        if (m != "quick_pick")
            methods.append(m);
    }

    Json::StreamWriterBuilder wb;
    wb["indentation"] = "";
    *supportedMethodsJson = SysAllocString(winrt::to_hstring(Json::writeString(wb, methods)).c_str());
    return S_OK;
}
CATCH_RETURN()

// ============================================================================
// Queries
// ============================================================================

STDMETHODIMP TerminalProtocolComServer::GetActivePane(PROTOCOL_PANE_INFO* result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    memset(result, 0, sizeof(*result));

    const auto host = s_emperor->GetMostRecentWindow();
    RETURN_HR_IF_NULL(E_FAIL, host);

    const auto page = _getPage(host);
    RETURN_HR_IF_NULL(E_FAIL, page);

    const auto jsonStr = winrt::to_string(page.GetProtocolActivePaneJson());
    if (jsonStr.empty())
        return E_FAIL;

    Json::Value v;
    if (!_parseJson(jsonStr, v))
        return E_FAIL;

    const auto& props = host->Logic().WindowProperties();
    const auto windowId = std::to_string(props.WindowId());

    result->PaneId = SysAllocString(winrt::to_hstring(v.get("pane_id", "").asString()).c_str());
    result->TabId = SysAllocString(winrt::to_hstring(v.get("tab_id", "").asString()).c_str());
    result->WindowId = SysAllocString(winrt::to_hstring(windowId).c_str());
    result->Title = SysAllocString(winrt::to_hstring(v.get("title", "").asString()).c_str());
    result->Profile = SysAllocString(winrt::to_hstring(v.get("profile", "").asString()).c_str());
    result->IsActive = TRUE;
    result->Pid = v.get("pid", 0u).asUInt();
    result->Rows = 0;
    result->Columns = 0;
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::ListWindows(UINT32* count, PROTOCOL_WINDOW_INFO** results)
try
{
    RETURN_HR_IF_NULL(E_POINTER, count);
    RETURN_HR_IF_NULL(E_POINTER, results);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    *count = 0;
    *results = nullptr;

    const auto mostRecent = s_emperor->GetMostRecentWindow();

    // Count windows first.
    std::vector<PROTOCOL_WINDOW_INFO> items;
    auto cleanupItems = wil::scope_exit([&]() {
        for (auto& i : items) { SysFreeString(i.WindowId); SysFreeString(i.Title); }
    });

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto logic = host->Logic();
        if (!logic)
            continue;

        const auto& props = logic.WindowProperties();
        PROTOCOL_WINDOW_INFO info{};
        info.WindowId = SysAllocString(winrt::to_hstring(std::to_string(props.WindowId())).c_str());
        info.Title = SysAllocString(props.WindowNameForDisplay().c_str());
        info.IsFocused = (host.get() == mostRecent) ? TRUE : FALSE;
        info.TabCount = logic.TabCount();
        items.push_back(info);
    }

    if (items.empty())
        return S_OK;

    *count = static_cast<UINT32>(items.size());
    *results = static_cast<PROTOCOL_WINDOW_INFO*>(CoTaskMemAlloc(items.size() * sizeof(PROTOCOL_WINDOW_INFO)));
    RETURN_HR_IF_NULL(E_OUTOFMEMORY, *results);
    memcpy(*results, items.data(), items.size() * sizeof(PROTOCOL_WINDOW_INFO));
    cleanupItems.release();
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::ListTabs(BSTR windowIdFilter, UINT32* count, PROTOCOL_TAB_INFO** results)
try
{
    RETURN_HR_IF_NULL(E_POINTER, count);
    RETURN_HR_IF_NULL(E_POINTER, results);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    *count = 0;
    *results = nullptr;

    const auto filter = windowIdFilter ? winrt::to_string(std::wstring_view(windowIdFilter, SysStringLen(windowIdFilter))) : std::string{};

    std::vector<PROTOCOL_TAB_INFO> items;
    auto cleanupItems = wil::scope_exit([&]() {
        for (auto& i : items) { SysFreeString(i.TabId); SysFreeString(i.WindowId); SysFreeString(i.Title); }
    });

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

        const auto tabsJson = winrt::to_string(page.GetProtocolTabsJson());
        Json::Value tabs;
        if (!_parseJson(tabsJson, tabs) || !tabs.isArray())
            continue;

        for (const auto& t : tabs)
        {
            PROTOCOL_TAB_INFO info{};
            info.TabId = SysAllocString(winrt::to_hstring(t.get("tab_id", "").asString()).c_str());
            info.WindowId = SysAllocString(winrt::to_hstring(windowIdStr).c_str());
            info.Title = SysAllocString(winrt::to_hstring(t.get("title", "").asString()).c_str());
            info.IsActive = t.get("is_active", false).asBool() ? TRUE : FALSE;
            info.PaneCount = t.get("pane_count", 0u).asUInt();
            items.push_back(info);
        }
    }

    if (items.empty())
        return S_OK;

    *count = static_cast<UINT32>(items.size());
    *results = static_cast<PROTOCOL_TAB_INFO*>(CoTaskMemAlloc(items.size() * sizeof(PROTOCOL_TAB_INFO)));
    RETURN_HR_IF_NULL(E_OUTOFMEMORY, *results);
    memcpy(*results, items.data(), items.size() * sizeof(PROTOCOL_TAB_INFO));
    cleanupItems.release();
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::ListPanes(BSTR windowIdFilter, BSTR tabIdFilter, UINT32* count, PROTOCOL_PANE_INFO** results)
try
{
    RETURN_HR_IF_NULL(E_POINTER, count);
    RETURN_HR_IF_NULL(E_POINTER, results);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    *count = 0;
    *results = nullptr;

    const auto winFilter = windowIdFilter ? winrt::to_string(std::wstring_view(windowIdFilter, SysStringLen(windowIdFilter))) : std::string{};
    const auto tabFilter = tabIdFilter ? winrt::to_string(std::wstring_view(tabIdFilter, SysStringLen(tabIdFilter))) : std::string{};

    std::vector<PROTOCOL_PANE_INFO> items;
    auto cleanupItems = wil::scope_exit([&]() {
        for (auto& i : items)
        {
            SysFreeString(i.PaneId); SysFreeString(i.TabId); SysFreeString(i.WindowId);
            SysFreeString(i.Title); SysFreeString(i.Profile);
        }
    });

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

        const auto panesJson = winrt::to_string(page.GetProtocolPanesJson(winrt::to_hstring(tabFilter)));
        Json::Value panes;
        if (!_parseJson(panesJson, panes) || !panes.isArray())
            continue;

        for (const auto& p : panes)
        {
            PROTOCOL_PANE_INFO info{};
            info.PaneId = SysAllocString(winrt::to_hstring(p.get("pane_id", "").asString()).c_str());
            info.TabId = SysAllocString(winrt::to_hstring(p.get("tab_id", "").asString()).c_str());
            info.WindowId = SysAllocString(winrt::to_hstring(windowIdStr).c_str());
            info.Title = SysAllocString(winrt::to_hstring(p.get("title", "").asString()).c_str());
            info.Profile = SysAllocString(winrt::to_hstring(p.get("profile", "").asString()).c_str());
            info.IsActive = p.get("is_active", false).asBool() ? TRUE : FALSE;
            info.Pid = p.get("pid", 0u).asUInt();
            info.Rows = p.isMember("size") ? p["size"].get("rows", 0).asInt() : 0;
            info.Columns = p.isMember("size") ? p["size"].get("columns", 0).asInt() : 0;
            items.push_back(info);
        }
    }

    if (items.empty())
        return S_OK;

    *count = static_cast<UINT32>(items.size());
    *results = static_cast<PROTOCOL_PANE_INFO*>(CoTaskMemAlloc(items.size() * sizeof(PROTOCOL_PANE_INFO)));
    RETURN_HR_IF_NULL(E_OUTOFMEMORY, *results);
    memcpy(*results, items.data(), items.size() * sizeof(PROTOCOL_PANE_INFO));
    cleanupItems.release();
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::ReadPaneOutput(BSTR paneId, BSTR source, INT32 maxLines, PROTOCOL_PANE_OUTPUT* result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    memset(result, 0, sizeof(*result));

    const auto paneIdStr = paneId ? winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId))) : std::string{};
    const auto sourceStr = source ? winrt::to_string(std::wstring_view(source, SysStringLen(source))) : std::string("scrollback");

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto jsonStr = winrt::to_string(page.ReadProtocolPaneOutput(
            winrt::to_hstring(paneIdStr), winrt::to_hstring(sourceStr), maxLines));
        if (jsonStr.empty())
            continue;

        Json::Value v;
        if (!_parseJson(jsonStr, v))
            continue;

        result->PaneId = SysAllocString(winrt::to_hstring(v.get("pane_id", "").asString()).c_str());
        result->Content = SysAllocString(winrt::to_hstring(v.get("content", "").asString()).c_str());
        result->LineCount = v.get("line_count", 0).asInt();
        result->Truncated = v.get("truncated", false).asBool() ? TRUE : FALSE;
        return S_OK;
    }

    return E_FAIL; // Pane not found
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::GetProcessStatus(BSTR paneId, PROTOCOL_PROCESS_STATUS* result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    memset(result, 0, sizeof(*result));

    const auto paneIdStr = paneId ? winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId))) : std::string{};

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto jsonStr = winrt::to_string(page.GetProtocolProcessStatus(winrt::to_hstring(paneIdStr)));
        if (jsonStr.empty())
            continue;

        Json::Value v;
        if (!_parseJson(jsonStr, v))
            continue;

        result->PaneId = SysAllocString(winrt::to_hstring(v.get("pane_id", "").asString()).c_str());
        result->State = SysAllocString(winrt::to_hstring(v.get("state", "unknown").asString()).c_str());
        result->Pid = v.get("pid", 0u).asUInt();
        result->ExitCode = v.get("exit_code", 0).asInt();
        result->HasExitCode = v.isMember("exit_code") && !v["exit_code"].isNull() ? TRUE : FALSE;
        return S_OK;
    }

    return E_FAIL;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::GetSessionVariable(BSTR paneId, BSTR name, PROTOCOL_SESSION_VARIABLE* result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_emperor);
    memset(result, 0, sizeof(*result));

    const auto paneIdStr = paneId ? winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId))) : std::string{};
    const auto nameStr = name ? winrt::to_string(std::wstring_view(name, SysStringLen(name))) : std::string{};

    for (const auto& host : s_emperor->GetWindows())
    {
        const auto page = _getPage(host.get());
        if (!page)
            continue;

        const auto jsonStr = winrt::to_string(page.GetProtocolSessionVariable(
            winrt::to_hstring(paneIdStr), winrt::to_hstring(nameStr)));
        if (jsonStr.empty())
            continue;

        Json::Value v;
        if (!_parseJson(jsonStr, v))
            continue;

        result->PaneId = SysAllocString(winrt::to_hstring(v.get("pane_id", "").asString()).c_str());
        result->Name = SysAllocString(winrt::to_hstring(v.get("name", "").asString()).c_str());
        result->Value = SysAllocString(winrt::to_hstring(v.get("value", "").asString()).c_str());
        result->Exists = v.get("exists", false).asBool() ? TRUE : FALSE;
        return S_OK;
    }

    return E_FAIL;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::GetSettings(BSTR* settingsJson)
try
{
    RETURN_HR_IF_NULL(E_POINTER, settingsJson);
    *settingsJson = nullptr;

    const std::filesystem::path settingsPath{ std::wstring_view{ winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings::SettingsPath() } };
    const auto content = til::io::read_file_as_utf8_string_if_exists(settingsPath);

    *settingsJson = SysAllocString(winrt::to_hstring(content).c_str());
    return S_OK;
}
CATCH_RETURN()

// ============================================================================
// Mutations
// ============================================================================

STDMETHODIMP TerminalProtocolComServer::CreateTab(BSTR windowId, BSTR profile, BSTR commandline,
                                                   BSTR title, BOOL suppressAppTitle,
                                                   BOOL injectMcpCredentials, BOOL background,
                                                   PROTOCOL_TAB_CREATION_RESULT* result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);
    memset(result, 0, sizeof(*result));

    // Build JSON params and delegate to the existing handler.
    Json::Value params;
    if (windowId && SysStringLen(windowId) > 0)
        params["window_id"] = winrt::to_string(std::wstring_view(windowId, SysStringLen(windowId)));
    if (profile && SysStringLen(profile) > 0)
        params["profile"] = winrt::to_string(std::wstring_view(profile, SysStringLen(profile)));
    if (commandline && SysStringLen(commandline) > 0)
        params["commandline"] = winrt::to_string(std::wstring_view(commandline, SysStringLen(commandline)));
    if (title && SysStringLen(title) > 0)
        params["title"] = winrt::to_string(std::wstring_view(title, SysStringLen(title)));
    params["suppress_application_title"] = suppressAppTitle ? true : false;
    params["inject_mcp_credentials"] = injectMcpCredentials ? true : false;
    params["background"] = background ? true : false;

    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-create-tab";
    request["method"] = "create_tab";
    request["params"] = params;

    const auto response = s_handler->HandleRequest(request, _authenticated);
    const auto& r = response["result"];
    if (r.isNull())
        return E_FAIL;

    result->TabId = SysAllocString(winrt::to_hstring(r.get("tab_id", "").asString()).c_str());
    result->PaneId = SysAllocString(winrt::to_hstring(r.get("pane_id", "").asString()).c_str());
    result->WindowId = SysAllocString(winrt::to_hstring(r.get("window_id", "").asString()).c_str());
    result->Pid = r.get("pid", 0u).asUInt();
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::SplitPane(BSTR paneId, BSTR direction, float size,
                                                    BSTR profile, BSTR commandline,
                                                    BOOL injectMcpCredentials, BOOL background,
                                                    PROTOCOL_TAB_CREATION_RESULT* result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);
    memset(result, 0, sizeof(*result));

    Json::Value params;
    if (paneId && SysStringLen(paneId) > 0)
        params["pane_id"] = winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId)));
    if (direction && SysStringLen(direction) > 0)
        params["direction"] = winrt::to_string(std::wstring_view(direction, SysStringLen(direction)));
    params["size"] = size;
    if (profile && SysStringLen(profile) > 0)
        params["profile"] = winrt::to_string(std::wstring_view(profile, SysStringLen(profile)));
    if (commandline && SysStringLen(commandline) > 0)
        params["commandline"] = winrt::to_string(std::wstring_view(commandline, SysStringLen(commandline)));
    params["inject_mcp_credentials"] = injectMcpCredentials ? true : false;
    params["background"] = background ? true : false;

    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-split-pane";
    request["method"] = "split_pane";
    request["params"] = params;

    const auto response = s_handler->HandleRequest(request, _authenticated);
    const auto& r = response["result"];
    if (r.isNull())
        return E_FAIL;

    result->TabId = SysAllocString(winrt::to_hstring(r.get("tab_id", "").asString()).c_str());
    result->PaneId = SysAllocString(winrt::to_hstring(r.get("pane_id", "").asString()).c_str());
    result->WindowId = SysAllocString(winrt::to_hstring(r.get("window_id", "").asString()).c_str());
    result->Pid = r.get("pid", 0u).asUInt();
    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::ClosePane(BSTR paneId)
try
{
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);

    Json::Value params;
    if (paneId && SysStringLen(paneId) > 0)
        params["pane_id"] = winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId)));

    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-close-pane";
    request["method"] = "close_pane";
    request["params"] = params;

    const auto response = s_handler->HandleRequest(request, _authenticated);
    if (!response["result"].isNull() && response["error"].isNull())
        return S_OK;
    return E_FAIL;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::SendInput(BSTR paneId, BSTR text)
try
{
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);

    Json::Value params;
    if (paneId && SysStringLen(paneId) > 0)
        params["pane_id"] = winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId)));
    if (text && SysStringLen(text) > 0)
        params["text"] = winrt::to_string(std::wstring_view(text, SysStringLen(text)));

    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-send-input";
    request["method"] = "send_input";
    request["params"] = params;

    const auto response = s_handler->HandleRequest(request, _authenticated);
    if (!response["result"].isNull() && response["error"].isNull())
        return S_OK;
    return E_FAIL;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::SetSessionVariable(BSTR paneId, BSTR name, BSTR value)
try
{
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);

    Json::Value params;
    if (paneId && SysStringLen(paneId) > 0)
        params["pane_id"] = winrt::to_string(std::wstring_view(paneId, SysStringLen(paneId)));
    if (name && SysStringLen(name) > 0)
        params["name"] = winrt::to_string(std::wstring_view(name, SysStringLen(name)));
    if (value && SysStringLen(value) > 0)
        params["value"] = winrt::to_string(std::wstring_view(value, SysStringLen(value)));

    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-set-session-var";
    request["method"] = "set_session_variable";
    request["params"] = params;

    const auto response = s_handler->HandleRequest(request, _authenticated);
    if (!response["result"].isNull() && response["error"].isNull())
        return S_OK;
    return E_FAIL;
}
CATCH_RETURN()

STDMETHODIMP TerminalProtocolComServer::SetSettings(BSTR settingsContent, BSTR* backupPath)
try
{
    RETURN_HR_IF_NULL(E_POINTER, backupPath);
    RETURN_HR_IF_NULL(E_NOT_VALID_STATE, s_handler);
    *backupPath = nullptr;

    const auto contentStr = settingsContent
        ? winrt::to_string(std::wstring_view(settingsContent, SysStringLen(settingsContent)))
        : std::string{};

    Json::Value params;
    params["settings"] = contentStr;

    Json::Value request;
    request["type"] = "request";
    request["id"] = "com-set-settings";
    request["method"] = "set_settings";
    request["params"] = params;

    const auto response = s_handler->HandleRequest(request, _authenticated);
    const auto& r = response["result"];
    if (r.isNull())
        return E_FAIL;

    *backupPath = SysAllocString(winrt::to_hstring(r.get("backup_path", "").asString()).c_str());
    return S_OK;
}
CATCH_RETURN()
