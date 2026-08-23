// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <unknwn.h>
#include <winrt/Windows.Foundation.h>

#include "Formatting.h"
#include "wtcli_functions.h"

// Classic-COM Terminal protocol. Generated from
// src/host/proxy/ITerminalProtocol.idl; found via the OpenConsoleProxy IntDir
// added to this project's include path. Marshaled by the OpenConsoleProxy
// proxy/stub (NOT WinRT MBM), so activation/marshaling never hits the combase
// WinRT activation catalog.
#include "ITerminalProtocol.h"

#include <CLI/CLI.hpp>

#include <wil/resource.h>

#include <array>
#include <charconv>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <fcntl.h>
#include <io.h>
#include <iostream>
#include <string>
#include <thread>
#include <vector>

// ── EventSink — pure classic-COM event sink for `listen` ──
struct EventSink : ITerminalProtocolEventSink
{
    LONG _ref{ 1 };
    std::function<void(const std::string&)> _handler;

    explicit EventSink(std::function<void(const std::string&)> handler) :
        _handler(std::move(handler)) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override
    {
        if (!ppv)
            return E_POINTER;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(ITerminalProtocolEventSink))
        {
            *ppv = static_cast<ITerminalProtocolEventSink*>(this);
            AddRef();
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&_ref); }
    ULONG STDMETHODCALLTYPE Release() override
    {
        const auto r = InterlockedDecrement(&_ref);
        if (r == 0)
            delete this;
        return r;
    }
    HRESULT STDMETHODCALLTYPE OnEvent(BSTR eventJson) override
    {
        if (_handler)
            _handler(eventJson ? winrt::to_string(winrt::hstring{ eventJson }) : std::string{});
        return S_OK;
    }
};

// ── Helpers ──

static winrt::com_ptr<ITerminalProtocol> ConnectToTerminal(bool* outAuthenticated = nullptr,
                                                           std::string* outVersion = nullptr,
                                                           bool skipAuthenticate = false,
                                                           bool quiet = false)
{
    if (outAuthenticated)
        *outAuthenticated = false;
    if (outVersion)
        outVersion->clear();

    wchar_t clsid[128]{};
    if (!GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)))
    {
        if (!quiet)
            fprintf(stderr, "[wtcli] WT_COM_CLSID not set. Must run inside an Intelligent Terminal pane.\n");
        return nullptr;
    }

    CLSID cls{};
    if (FAILED(CLSIDFromString(clsid, &cls)))
    {
        if (!quiet)
            fprintf(stderr, "[wtcli] Invalid CLSID: %ls\n", clsid);
        return nullptr;
    }

    winrt::com_ptr<ITerminalProtocol> server;
    auto hr = CoCreateInstance(cls, nullptr, CLSCTX_LOCAL_SERVER, __uuidof(ITerminalProtocol), server.put_void());
    if (FAILED(hr))
    {
        if (!quiet)
            fprintf(stderr, "[wtcli] Connection failed: 0x%08X\n", static_cast<uint32_t>(hr));
        return nullptr;
    }
    if (skipAuthenticate)
    {
        return server;
    }

    BSTR rawAuth = nullptr;
    hr = server->Authenticate(nullptr, &rawAuth);
    bool parsed = false;
    bool authenticated = false;
    std::string version;
    if (SUCCEEDED(hr) && rawAuth)
    {
        Json::Value v;
        Json::CharReaderBuilder rb;
        std::string errs;
        auto s = winrt::to_string(winrt::hstring{ rawAuth });
        std::istringstream ss(s);
        if (Json::parseFromStream(rb, ss, &v, &errs))
        {
            parsed = true;
            authenticated = v["authenticated"].asBool();
            version = v["protocol_version"].asString();
        }
    }
    if (rawAuth)
        SysFreeString(rawAuth);

    if (FAILED(hr))
    {
        if (!quiet)
            fprintf(stderr, "[wtcli] Authentication failed: 0x%08X\n", static_cast<uint32_t>(hr));
        return nullptr;
    }
    if (!parsed)
    {
        // Success HRESULT but a null/malformed auth payload is a broken
        // server contract — don't misreport it as a server rejection.
        if (!quiet)
            fprintf(stderr, "[wtcli] Authentication response missing or malformed (server contract error)\n");
        return nullptr;
    }
    if (!authenticated)
    {
        if (!quiet)
            fprintf(stderr, "[wtcli] Authentication rejected by server\n");
        return nullptr;
    }

    if (outAuthenticated)
        *outAuthenticated = authenticated;
    if (outVersion)
        *outVersion = version;
    return server;
}

// Call a method that returns a JSON BSTR; parse into `out`. Returns the HRESULT.
template<typename F>
static HRESULT CallJson(F&& call, Json::Value& out)
{
    BSTR raw = nullptr;
    HRESULT hr = call(&raw);
    if (SUCCEEDED(hr))
    {
        bool parsed = false;
        if (raw)
        {
            Json::CharReaderBuilder rb;
            std::string errs;
            auto s = winrt::to_string(winrt::hstring{ raw });
            std::istringstream ss(s);
            parsed = Json::parseFromStream(rb, ss, &out, &errs);
        }
        // A success HRESULT with a null BSTR or malformed JSON is a broken
        // server contract; surface it as an error so callers' FAILED(hr)
        // checks fire immediately instead of proceeding with a
        // default-constructed `out`.
        if (!parsed)
            hr = E_UNEXPECTED;
    }
    if (raw)
        SysFreeString(raw);
    return hr;
}

static std::string GuidToString(const GUID& g)
{
    wchar_t buf[40]{};
    StringFromGUID2(g, buf, ARRAYSIZE(buf));
    std::wstring ws(buf);
    if (ws.size() > 2 && ws.front() == L'{' && ws.back() == L'}')
        ws = ws.substr(1, ws.size() - 2);
    return winrt::to_string(winrt::hstring{ ws });
}

static GUID GuidFromString(const std::string& target, bool quiet = false)
{
    auto wstr = winrt::to_hstring(target);
    std::wstring guidStr{ wstr };
    if (!guidStr.empty() && guidStr[0] != L'{')
        guidStr = L"{" + guidStr + L"}";
    GUID g{};
    if (FAILED(CLSIDFromString(guidStr.c_str(), &g)))
    {
        if (!quiet && !target.empty())
            fprintf(stderr, "[wtcli] Invalid session ID: %s\n", target.c_str());
        return GUID{};
    }
    return g;
}

// Resolve a session id: an explicit GUID string, or the active pane's id.
static GUID ResolveSessionId(ITerminalProtocol* server, const std::string& target)
{
    if (!target.empty())
        return GuidFromString(target);

    Json::Value info;
    const auto hr = CallJson([&](BSTR* j) { return server->GetActivePane(j); }, info);
    if (FAILED(hr))
    {
        fprintf(stderr, "[wtcli] Could not resolve active pane (GetActivePane failed: 0x%08X)\n", static_cast<uint32_t>(hr));
        return GUID{};
    }
    const auto sessionId = info["session_id"].asString();
    if (sessionId.empty())
    {
        fprintf(stderr, "[wtcli] No active pane.\n");
        return GUID{};
    }
    return GuidFromString(sessionId);
}

static uint64_t GetFirstWindowId(ITerminalProtocol* server)
{
    Json::Value windows;
    CallJson([&](BSTR* j) { return server->ListWindows(j); }, windows);
    if (windows.isArray() && !windows.empty())
        return windows[0u]["window_id"].asUInt64();
    return 0;
}

static uint32_t GetFirstTabId(ITerminalProtocol* server, uint64_t windowId)
{
    Json::Value tabs;
    CallJson([&](BSTR* j) { return server->ListTabs(windowId, j); }, tabs);
    if (tabs.isArray() && !tabs.empty())
        return tabs[0u]["tab_id"].asUInt();
    return UINT32_MAX;
}

// Allocate a BSTR from a UTF-8 std::string.
static BSTR Bstr(const std::string& s)
{
    return SysAllocString(winrt::to_hstring(s).c_str());
}

static std::string EnvironmentValue(const wchar_t* name)
{
    const auto required = GetEnvironmentVariableW(name, nullptr, 0);
    if (required == 0)
    {
        return {};
    }

    std::wstring value(required, L'\0');
    const auto written = GetEnvironmentVariableW(name, value.data(), required);
    if (written == 0 || written >= required)
    {
        return {};
    }
    value.resize(written);
    return winrt::to_string(winrt::hstring{ value });
}

static std::string AgentSessionIdFromEnvironment(const std::string& cliSource)
{
    if (cliSource == "copilot")
        return EnvironmentValue(L"COPILOT_SESSION_ID");
    if (cliSource == "claude")
        return EnvironmentValue(L"CLAUDE_SESSION_ID");
    if (cliSource == "gemini")
        return EnvironmentValue(L"GEMINI_SESSION_ID");
    if (cliSource == "codex")
        return EnvironmentValue(L"CODEX_SESSION_ID");
    return {};
}

// Parse a base-10 unsigned 64-bit integer without throwing (unlike std::stoull,
// which aborts wtcli on non-numeric input). Returns false on empty, non-numeric,
// trailing-garbage, or overflowing input.
static bool TryParseU64(const std::string& s, uint64_t& out)
{
    if (s.empty())
        return false;
    uint64_t v = 0;
    const auto* first = s.data();
    const auto* last = s.data() + s.size();
    const auto [ptr, ec] = std::from_chars(first, last, v);
    if (ec != std::errc{} || ptr != last)
        return false;
    out = v;
    return true;
}

// ── Main ──

// `wmain` — deliberately NOT `main`. Almost every string this tool forwards to
// the terminal arrives as an argument: `new-tab -d` starting directories,
// `-n` tab titles, `-c` command lines, `send-keys` payloads and the JSON blobs
// of `publish` / `send-event`. All of them may hold non-ASCII text.
//
// The CRT's narrow `__argv` is transcoded from the real UTF-16 command line
// through the *process ANSI code page*, so anything outside that code page is
// destroyed before we ever see it — on ACP 936 `D:\Obsidian\我的笔记` turns
// into GBK bytes that `Bstr()` below then decodes as UTF-8 (mojibake), and on
// a Latin ACP it degrades to literal `?`. Either way `CreateProcessW` on the
// terminal side fails the starting directory with 0x8007010b ERROR_DIRECTORY
// (GH#641), and titles / keystrokes / JSON silently lose characters.
//
// Taking `wchar_t**` keeps the original UTF-16 and lets CLI11's wide `parse`
// overload narrow it with `codecvt_utf8_utf16`, so every `std::string` bound
// to an option really is UTF-8 — which is exactly what `Bstr()` and
// `winrt::to_hstring` already assume.
int wmain(int argc, wchar_t** argv)
{
    winrt::init_apartment(winrt::apartment_type::multi_threaded);

    CLI::App app{ "wtcli - Windows Terminal CLI" };
    app.require_subcommand(0, 1);

    bool jsonMode = false;
    bool skipAuthenticate = false;
    int exitCode = 0;
    app.add_flag("--json", jsonMode, "Output raw JSON");
    app.add_flag("--skip-authenticate", skipAuthenticate, "Skip the compatibility handshake (testing only)");

    auto connect = [&]() -> winrt::com_ptr<ITerminalProtocol> {
        auto server = ConnectToTerminal(nullptr, nullptr, skipAuthenticate);
        if (!server)
            exitCode = 1;
        return server;
    };

    // ── list-windows ──
    auto* listWindowsCmd = app.add_subcommand("list-windows", "List all windows")->alias("lsw");
    listWindowsCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        Json::Value windows;
        auto hr = CallJson([&](BSTR* j) { return server->ListWindows(j); }, windows);
        if (FAILED(hr)) { fprintf(stderr, "ListWindows failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value arr(Json::objectValue);
            arr["windows"] = windows;
            PrintJson(arr);
        }
        else
        {
            FormatWindowsHuman(windows);
        }
    });

    // ── list-tabs ──
    std::string listTabsWindowId;
    auto* listTabsCmd = app.add_subcommand("list-tabs", "List tabs in a window")->alias("lst");
    listTabsCmd->add_option("-w,--window-id", listTabsWindowId, "Window ID");
    listTabsCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        uint64_t wid = 0;
        if (listTabsWindowId.empty())
        {
            wid = GetFirstWindowId(server.get());
            if (wid == 0)
            {
                // 0 is the server's "no filter" sentinel, not a real window id;
                // bail rather than silently listing tabs for ALL windows.
                fprintf(stderr, "[wtcli] Could not resolve a window (no windows or ListWindows failed)\n");
                exitCode = 1;
                return;
            }
        }
        else if (!TryParseU64(listTabsWindowId, wid))
        {
            fprintf(stderr, "[wtcli] Invalid --window-id: %s\n", listTabsWindowId.c_str());
            exitCode = 1;
            return;
        }
        Json::Value tabs;
        auto hr = CallJson([&](BSTR* j) { return server->ListTabs(wid, j); }, tabs);
        if (FAILED(hr)) { fprintf(stderr, "ListTabs failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value arr(Json::objectValue);
            arr["tabs"] = tabs;
            PrintJson(arr);
        }
        else
        {
            FormatTabsHuman(tabs);
        }
    });

    // ── list-panes ──
    std::string listPanesTabId, listPanesWindowId;
    auto* listPanesCmd = app.add_subcommand("list-panes", "List panes in a tab")->alias("lsp");
    listPanesCmd->add_option("-t,--tab-id", listPanesTabId, "Tab ID");
    listPanesCmd->add_option("-w,--window-id", listPanesWindowId, "Window ID");
    listPanesCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        uint64_t wid = 0;
        if (!listPanesWindowId.empty() && !TryParseU64(listPanesWindowId, wid))
        {
            fprintf(stderr, "[wtcli] Invalid --window-id: %s\n", listPanesWindowId.c_str());
            exitCode = 1;
            return;
        }
        uint32_t tid = UINT32_MAX;
        if (!listPanesTabId.empty())
        {
            uint64_t t = 0;
            if (!TryParseU64(listPanesTabId, t) || t > UINT32_MAX)
            {
                fprintf(stderr, "[wtcli] Invalid --tab-id: %s\n", listPanesTabId.c_str());
                exitCode = 1;
                return;
            }
            tid = static_cast<uint32_t>(t);
        }
        if (tid == UINT32_MAX)
        {
            if (wid == 0)
            {
                wid = GetFirstWindowId(server.get());
                if (wid == 0)
                {
                    fprintf(stderr, "[wtcli] Could not resolve a window (no windows or ListWindows failed)\n");
                    exitCode = 1;
                    return;
                }
            }
            tid = GetFirstTabId(server.get(), wid);
            if (tid == UINT32_MAX)
            {
                fprintf(stderr, "[wtcli] Could not resolve a tab (no tabs or ListTabs failed)\n");
                exitCode = 1;
                return;
            }
        }
        Json::Value panes;
        auto hr = CallJson([&](BSTR* j) { return server->ListPanes(wid, tid, j); }, panes);
        if (FAILED(hr)) { fprintf(stderr, "ListPanes failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value arr(Json::objectValue);
            arr["panes"] = panes;
            PrintJson(arr);
        }
        else
        {
            FormatPanesHuman(panes);
        }
    });

    // ── active-pane ──
    auto* activePaneCmd = app.add_subcommand("active-pane", "Show the currently active pane");
    activePaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        Json::Value info;
        auto hr = CallJson([&](BSTR* j) { return server->GetActivePane(j); }, info);
        if (FAILED(hr)) { fprintf(stderr, "GetActivePane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(info);
        else
            FormatActivePaneHuman(info);
    });

    // ── capture-pane ──
    std::string capturePaneTarget;
    int captureMaxLines = 200;
    bool captureLastPrompt = false;
    auto* capturePaneCmd = app.add_subcommand("capture-pane", "Capture pane output")->alias("capturep");
    capturePaneCmd->add_option("-t,--target", capturePaneTarget, "Session ID (GUID)");
    capturePaneCmd->add_option("-l,--max-lines", captureMaxLines, "Max lines");
    capturePaneCmd->add_flag("--last-prompt", captureLastPrompt,
        "Only return the most recent completed shell prompt (command + output, requires OSC 133 shell integration)");
    capturePaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), capturePaneTarget);
        wil::unique_bstr src{ Bstr(captureLastPrompt ? "last_prompt" : "scrollback") };
        Json::Value output;
        auto hr = CallJson([&](BSTR* j) { return server->ReadPaneOutput(sessionId, src.get(), captureMaxLines, j); }, output);
        if (FAILED(hr)) { fprintf(stderr, "ReadPaneOutput failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(output);
        else
            printf("%s\n", output["content"].asString().c_str());
    });

    // ── pane-status ──
    std::string paneStatusTarget;
    auto* paneStatusCmd = app.add_subcommand("pane-status", "Show pane process status");
    paneStatusCmd->add_option("-t,--target", paneStatusTarget, "Session ID (GUID)");
    paneStatusCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), paneStatusTarget);
        Json::Value status;
        auto hr = CallJson([&](BSTR* j) { return server->GetProcessStatus(sessionId, j); }, status);
        if (FAILED(hr)) { fprintf(stderr, "GetProcessStatus failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(status);
        else
            FormatPaneStatusHuman(status);
    });

    // ── new-tab ──
    std::string newTabCommand, newTabTitle, newTabCwd, newTabProfile;
    auto* newTabCmd = app.add_subcommand("new-tab", "Create a new tab")->alias("neww");
    newTabCmd->add_option("-c,--command", newTabCommand, "Command to run");
    newTabCmd->add_option("-n,--title", newTabTitle, "Tab title");
    newTabCmd->add_option("-d,--cwd", newTabCwd, "Starting directory");
    newTabCmd->add_option("-p,--profile", newTabProfile, "Profile");
    newTabCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        wil::unique_bstr profile{ Bstr(newTabProfile) }, command{ Bstr(newTabCommand) }, title{ Bstr(newTabTitle) }, cwd{ Bstr(newTabCwd) };
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) {
            return server->CreateTab(0, profile.get(), command.get(), title.get(), cwd.get(), false, true, j);
        }, result);
        if (FAILED(hr)) { fprintf(stderr, "CreateTab failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(result);
        else
            FormatCreatedTabHuman(result);
    });

    // ── split-pane ──
    std::string splitPaneTarget, splitPaneCommand, splitPaneDirection, splitPaneProfile;
    bool splitHorizontal = false, splitVertical = false;
    double splitSize = 0.5;
    auto* splitPaneCmd = app.add_subcommand("split-pane", "Split a pane")->alias("splitw");
    splitPaneCmd->add_option("-t,--target", splitPaneTarget, "Session ID (GUID)");
    splitPaneCmd->add_option("-d,--direction", splitPaneDirection, "Split direction: right|left|up|down|auto");
    splitPaneCmd->add_flag("-H,--horizontal", splitHorizontal, "Split horizontally (legacy alias for --direction down)");
    splitPaneCmd->add_flag("-v,--vertical", splitVertical, "Split vertically (legacy alias for --direction right)");
    splitPaneCmd->add_option("-s,--size", splitSize, "Size fraction");
    splitPaneCmd->add_option("-c,--command", splitPaneCommand, "Command to run");
    splitPaneCmd->add_option("-p,--profile", splitPaneProfile, "Profile");
    splitPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), splitPaneTarget);
        std::string dir;
        if (!splitPaneDirection.empty())
            dir = splitPaneDirection;
        else if (splitHorizontal)
            dir = "down";
        else if (splitVertical)
            dir = "right";
        else
            dir = "automatic";
        wil::unique_bstr dirB{ Bstr(dir) }, profile{ Bstr(splitPaneProfile) }, command{ Bstr(splitPaneCommand) };
        Json::Value result;
        auto hr = CallJson([&](BSTR* j) {
            return server->SplitPane(sessionId, dirB.get(), static_cast<float>(splitSize), profile.get(), command.get(), true, j);
        }, result);
        if (FAILED(hr)) { fprintf(stderr, "SplitPane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
            PrintJson(result);
        else
            FormatCreatedPaneHuman(result);
    });

    // ── kill-pane ──
    std::string killPaneTarget;
    auto* killPaneCmd = app.add_subcommand("kill-pane", "Close a pane")->alias("killp");
    killPaneCmd->add_option("-t,--target", killPaneTarget, "Session ID (GUID)");
    killPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), killPaneTarget);
        auto hr = server->ClosePane(sessionId);
        if (FAILED(hr)) { fprintf(stderr, "ClosePane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value v;
            v["ok"] = true;
            v["session_id"] = GuidToString(sessionId);
            PrintJson(v);
        }
        else
        {
            printf("Session %s closed.\n", GuidToString(sessionId).c_str());
        }
    });

    // ── send-keys ──
    std::string sendKeysTarget;
    std::vector<std::string> sendKeysArgs;
    bool sendKeysRaw = false;
    auto* sendKeysCmd = app.add_subcommand("send-keys", "Send keys to a pane")->alias("send");
    sendKeysCmd->add_option("-t,--target", sendKeysTarget, "Session ID (GUID)");
    sendKeysCmd->add_flag("--raw", sendKeysRaw,
                          "Treat the payload as literal UTF-8 text — skip tmux-style "
                          "token translation (Enter/Tab/Escape/BSpace/C-x). Use this when "
                          "forwarding arbitrary agent-supplied text.");
    sendKeysCmd->add_option("keys", sendKeysArgs, "Keys to send")->required();
    sendKeysCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), sendKeysTarget);
        auto text = sendKeysRaw
            ? wtcli::JoinAsUtf16(sendKeysArgs)
            : wtcli::TranslateKeys(sendKeysArgs);
        wil::unique_bstr textB{ SysAllocString(text.c_str()) };
        auto hr = server->SendInput(sessionId, textB.get());
        if (FAILED(hr)) { fprintf(stderr, "SendInput failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value v;
            v["ok"] = true;
            v["session_id"] = GuidToString(sessionId);
            PrintJson(v);
        }
    });

    // ── focus-pane ──
    std::string focusPaneTarget;
    auto* focusPaneCmd = app.add_subcommand("focus-pane", "Switch focus to a pane")->alias("focusp");
    focusPaneCmd->add_option("-t,--target", focusPaneTarget, "Session ID (GUID)");
    focusPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), focusPaneTarget);
        auto hr = server->FocusPane(sessionId);
        if (FAILED(hr)) { fprintf(stderr, "FocusPane failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; return; }
        if (jsonMode)
        {
            Json::Value v;
            v["ok"] = true;
            v["session_id"] = GuidToString(sessionId);
            PrintJson(v);
        }
        else
        {
            printf("Focused pane %s.\n", GuidToString(sessionId).c_str());
        }
    });

    // ── test-pipe ──
    auto* testPipeCmd = app.add_subcommand("test-pipe", "Test connection to Windows Terminal");
    testPipeCmd->callback([&]() {
        printf("Connecting to Windows Terminal...\n");
        auto server = connect();
        if (!server) { fprintf(stderr, "Connection failed.\n"); return; }
        printf(skipAuthenticate ? "Connected without compatibility handshake!\n\n" : "Connected and authenticated!\n\n");

        Json::Value windows;
        if (SUCCEEDED(CallJson([&](BSTR* j) { return server->ListWindows(j); }, windows)))
        {
            Json::Value arr(Json::objectValue);
            arr["windows"] = windows;
            printf("list_windows:\n");
            PrintJson(arr);
        }
        printf("\n");

        Json::Value caps;
        if (SUCCEEDED(CallJson([&](BSTR* j) { return server->GetCapabilities(j); }, caps)))
        {
            printf("get_capabilities:\n");
            PrintJson(caps);
        }
    });

    // ── info ──
    auto* infoCmd = app.add_subcommand("info", "Show connection info");
    infoCmd->callback([&]() {
        wchar_t clsid[128]{};
        auto hasClsid = GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)) > 0;

        std::string version;
        auto server = ConnectToTerminal(nullptr, &version, skipAuthenticate);

        Json::Value methods(Json::arrayValue);
        if (server)
        {
            CallJson([&](BSTR* j) { return server->GetCapabilities(j); }, methods);
        }

        if (jsonMode)
        {
            Json::Value v;
            if (hasClsid)
                v["com_clsid"] = winrt::to_string(winrt::hstring{ clsid });
            v["connected"] = (server != nullptr);
            if (!version.empty())
                v["protocol_version"] = version;
            v["methods"] = methods.isArray() ? methods : Json::Value(Json::arrayValue);
            PrintJson(v);
        }
        else
        {
            printf("Windows Terminal Protocol Info\n");
            printf("========================================\n");
            if (hasClsid)
                printf("  COM CLSID:  %ls\n", clsid);
            else
                printf("  COM CLSID:  (not set)\n");
            printf("\n");
            if (!server)
            {
                printf("  Connection: FAILED\n");
            }
            else
            {
                printf("  Connection: OK\n");
                if (!version.empty())
                    printf("  Protocol:   %s\n", version.c_str());
                printf("\n");
                if (methods.isArray() && methods.size() > 0)
                {
                    printf("  Methods:    %u supported\n", methods.size());
                    for (const auto& m : methods)
                        printf("              - %s\n", m.asString().c_str());
                }
            }
        }

        if (!server)
            exitCode = 1;
    });

    // ── wait-for ──
    std::string waitForTarget;
    int waitInterval = 500;
    int waitTimeout = 0;
    auto* waitForCmd = app.add_subcommand("wait-for", "Wait for a pane to exit");
    waitForCmd->add_option("-t,--target", waitForTarget, "Session ID (GUID)")->required();
    waitForCmd->add_option("--interval", waitInterval, "Poll interval (ms)");
    waitForCmd->add_option("--timeout", waitTimeout, "Timeout (seconds, 0=forever)");
    waitForCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        auto sessionId = ResolveSessionId(server.get(), waitForTarget);
        auto start = std::chrono::steady_clock::now();

        while (true)
        {
            Json::Value status;
            auto hr = CallJson([&](BSTR* j) { return server->GetProcessStatus(sessionId, j); }, status);
            if (FAILED(hr))
            {
                fprintf(stderr, "GetProcessStatus failed: 0x%08X\n", static_cast<uint32_t>(hr));
                exitCode = 1;
                return;
            }
            if (status["state"].asString() == "exited")
            {
                if (jsonMode)
                {
                    Json::Value v;
                    v["state"] = "exited";
                    if (status.isMember("exit_code"))
                        v["exit_code"] = status["exit_code"].asInt();
                    PrintJson(v);
                }
                else
                {
                    printf("Process exited");
                    if (status.isMember("has_exit_code") ? status["has_exit_code"].asBool() : status.isMember("exit_code"))
                        printf(" (code %d)", status["exit_code"].asInt());
                    printf("\n");
                }
                return;
            }

            if (waitTimeout > 0)
            {
                auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                                   std::chrono::steady_clock::now() - start)
                                   .count();
                if (elapsed >= waitTimeout)
                {
                    fprintf(stderr, "Timeout waiting for pane %s\n", waitForTarget.c_str());
                    exitCode = 1;
                    return;
                }
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(waitInterval));
        }
    });

    // ── set-env ──
    std::string setEnvShell = "powershell";
    auto* setEnvCmd = app.add_subcommand("set-env", "Print env setup commands")->alias("setenv");
    setEnvCmd->add_option("-s,--shell", setEnvShell, "Shell: powershell, bash, cmd");
    setEnvCmd->callback([&]() {
        wchar_t clsid[128]{};
        GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid));
        auto cl = winrt::to_string(winrt::hstring{ clsid });

        if (setEnvShell == "powershell" || setEnvShell == "pwsh")
        {
            if (!cl.empty()) printf("$env:WT_COM_CLSID = '%s'\n", cl.c_str());
        }
        else if (setEnvShell == "bash" || setEnvShell == "sh" || setEnvShell == "zsh")
        {
            if (!cl.empty()) printf("export WT_COM_CLSID='%s'\n", cl.c_str());
        }
        else if (setEnvShell == "cmd")
        {
            if (!cl.empty()) printf("set WT_COM_CLSID=%s\n", cl.c_str());
        }
    });

    // ── publish ──
    // Low-level "pass this JSON through to SendEvent verbatim" escape hatch.
    std::string publishJson;
    bool publishFromStdin = false;
    auto* publishCmd = app.add_subcommand("publish", "Forward raw JSON to SendEvent");
    auto* publishJsonOption = publishCmd->add_option("json", publishJson, "Full event JSON (e.g. {\"method\":\"autofix_state\",\"params\":{...}})");
    auto* publishStdinOption = publishCmd->add_flag("--stdin", publishFromStdin, "Read the full UTF-8 event JSON from stdin");
    publishJsonOption->excludes(publishStdinOption);
    publishCmd->require_option(1, 1);
    publishCmd->callback([&]() {
        if (publishFromStdin)
        {
            if (_setmode(_fileno(stdin), _O_BINARY) == -1)
            {
                fprintf(stderr, "[wtcli] publish: failed to configure stdin for binary input.\n");
                exitCode = 1;
                return;
            }
            std::string input;
            std::array<char, 8192> buffer;
            while (std::cin.read(buffer.data(), buffer.size()) || std::cin.gcount() > 0)
            {
                input.append(buffer.data(), static_cast<size_t>(std::cin.gcount()));
            }
            if (!std::cin.eof())
            {
                fprintf(stderr, "[wtcli] publish: failed to read JSON from stdin.\n");
                exitCode = 1;
                return;
            }
            publishJson = std::move(input);
        }
        if (publishJson.empty())
        {
            fprintf(stderr, "[wtcli] publish: JSON input must not be empty.\n");
            exitCode = 1;
            return;
        }
        auto server = connect();
        if (!server) return;
        wil::unique_bstr evt{ Bstr(publishJson) };
        auto hr = server->SendEvent(evt.get());
        if (FAILED(hr)) { fprintf(stderr, "publish failed: 0x%08X\n", static_cast<uint32_t>(hr)); exitCode = 1; }
    });

    // ── send-event ──
    //
    // Also the transport every hook bundle installed before the native
    // `agent-hook` bridge still uses. Those bundles live in each CLI's own
    // plugin cache, which a Terminal upgrade never rewrites and whose
    // auto-refresh can fail outright, so they keep calling this subcommand
    // indefinitely. Removing it as superseded would silently stop tracking
    // every user whose hooks never refreshed.
    // Guarded by Feature.LegacyHookBundle.Tests.ps1 (C270).
    std::string sendEventType, sendEventJson, sendEventPaneTarget;
    auto* sendEventCmd = app.add_subcommand("send-event", "Publish an event to all listeners")->alias("se");
    sendEventCmd->add_option("-p,--pane", sendEventPaneTarget, "Source session ID (GUID); omit when the source pane is unknown");
    sendEventCmd->add_option("-e,--event", sendEventType, "Event type (e.g. agent.task.started)")->required();
    sendEventCmd->add_option("json", sendEventJson, "Event params as JSON object");
    sendEventCmd->callback([&]() {
        auto server = connect();
        if (!server)
            return;
        // No `--pane` publishes an empty `pane_id`, meaning "this event has no
        // known source pane". It deliberately does NOT fall back to the focused
        // pane: the callers that omit `--pane` are hook bridges whose process
        // never inherited WT_SESSION, and the focused pane is simply wherever
        // the user happens to be looking, so attributing to it is wrong in a
        // way that corrupts state rather than merely losing it. A legacy bundle
        // firing `agent.session.start` would evict the focused pane's real
        // session (the orphan-handover branch in agent_sessions.rs demotes the
        // previous owner of a reused pane to Ended) and rebind the pane to an
        // agent that was never in it, so Enter in the session list then focuses
        // a stranger's pane. Empty is the case WTA already models: `pane_known`
        // is false, so it skips the handover, leaves `active_by_pane` alone,
        // and routes by cli_source instead.
        Json::Value evt;
        if (!wtcli::BuildSendEventJson(sendEventType, sendEventJson, sendEventPaneTarget, evt))
        {
            fprintf(stderr, "Invalid JSON for --json: value must be a JSON object (e.g. '{\"key\":\"val\"}')\n");
            exitCode = 1;
            return;
        }
        Json::StreamWriterBuilder wb;
        wb["indentation"] = "";
        wil::unique_bstr evtB{ Bstr(Json::writeString(wb, evt)) };
        auto hr = server->SendEvent(evtB.get());
        if (FAILED(hr))
        {
            fprintf(stderr, "SendEvent failed: 0x%08X\n", static_cast<uint32_t>(hr));
            exitCode = 1;
        }
    });

    // ── agent-hook ──
    // Native hook bridge. It deliberately never writes to stdout/stderr and
    // never returns a failure because hook failures can block agent actions.
    std::string agentHookEventType, agentHookCliSource;
    auto* agentHookCmd = app.add_subcommand("agent-hook", "Forward an agent lifecycle hook from stdin");
    agentHookCmd->add_option("-e,--event", agentHookEventType, "WTA agent event type")->required();
    agentHookCmd->add_option("-c,--cli-source", agentHookCliSource, "Source agent CLI")->required();
    agentHookCmd->callback([&]() {
        try
        {
            // A shell-pane session always has both values. The shared ACP
            // process has no pane identity and is already tracked over ACP, so
            // dropping it here also prevents events from binding to the active
            // user pane.
            const auto paneGuid = GuidFromString(EnvironmentValue(L"WT_SESSION"), true);
            if (EnvironmentValue(L"WT_COM_CLSID").empty() || IsEqualGUID(paneGuid, GUID{}))
            {
                return;
            }
            const auto paneId = GuidToString(paneGuid);

            // Read a bounded amount. `BuildAgentHookEventJson` caps what goes
            // on the wire, but that happens *after* the whole of stdin is in
            // memory and parsed, so an oversized hook payload still costs
            // memory and parse time first. The ceiling is deliberately far
            // above the event budget rather than equal to it: payloads larger
            // than the budget are normal and still produce a useful event
            // (truncated body, routing fields intact), so capping at the
            // budget would drop events that work today.
            constexpr std::streamsize kMaxHookStdinChars = 1024 * 1024;
            std::string input(static_cast<size_t>(kMaxHookStdinChars) + 1, '\0');
            std::cin.read(input.data(), static_cast<std::streamsize>(input.size()));
            const auto readChars = std::cin.gcount();
            if (readChars > kMaxHookStdinChars)
            {
                // Anything this large is a broken or hostile producer, not a
                // hook. Drop it rather than parse it; the callback still exits
                // 0 so a fail-closed CLI is unaffected.
                return;
            }
            input.resize(static_cast<size_t>(readChars));

            Json::Value event;
            if (!wtcli::BuildAgentHookEventJson(
                    agentHookEventType,
                    agentHookCliSource,
                    input,
                    paneId,
                    AgentSessionIdFromEnvironment(agentHookCliSource),
                    event))
            {
                return;
            }

            auto server = ConnectToTerminal(nullptr, nullptr, skipAuthenticate, true);
            if (!server)
            {
                return;
            }

            Json::StreamWriterBuilder writer;
            writer["indentation"] = "";
            wil::unique_bstr eventJson{ Bstr(Json::writeString(writer, event)) };
            if (eventJson)
            {
                server->SendEvent(eventJson.get());
            }
        }
        catch (...)
        {
            // Session tracking must never affect the parent agent CLI.
        }
    });

    // ── listen ──
    std::string listenTarget;
    std::string listenEventFilter;
    DWORD listenParentPid = 0;
    auto* listenCmd = app.add_subcommand("listen", "Stream real-time events from Windows Terminal");
    listenCmd->add_option("-t,--target", listenTarget, "Filter by session ID (GUID)");
    listenCmd->add_option("--event", listenEventFilter, "Filter by event type (supports trailing wildcard, e.g. agent.*)");
    listenCmd->add_option("--parent-pid", listenParentPid, "Exit when the specified parent process exits");
    listenCmd->callback([&]() {
        wil::unique_handle parentProcess;
        if (listenParentPid != 0)
        {
            parentProcess.reset(OpenProcess(SYNCHRONIZE, FALSE, listenParentPid));
            if (!parentProcess)
            {
                fprintf(stderr, "[wtcli] listen: failed to open parent process %lu (0x%08X)\n", listenParentPid, GetLastError());
                exitCode = 1;
                return;
            }
        }

        auto server = connect();
        if (!server)
        {
            exitCode = 1;
            return;
        }

        static HANDLE s_stopEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!s_stopEvent)
        {
            fprintf(stderr, "[wtcli] listen: failed to create stop event (0x%08X)\n", GetLastError());
            exitCode = 1;
            return;
        }
        SetConsoleCtrlHandler([](DWORD) -> BOOL {
            SetEvent(s_stopEvent);
            return TRUE;
        }, TRUE);

        if (!jsonMode)
            fprintf(stderr, "Listening for events... (Ctrl-C to stop)\n");

        // EventSink is born with _ref == 1, so attach() (adopt, no AddRef) hands
        // that reference to the com_ptr. RAII then Releases on every exit path --
        // exception-safe and robust against future early-returns, no manual
        // Release to forget.
        winrt::com_ptr<ITerminalProtocolEventSink> sink;
        sink.attach(new EventSink([&](const std::string& eventUtf8) {
            if (!wtcli::MatchesEventFilter(eventUtf8, listenTarget, listenEventFilter))
                return;
            printf("%s\n", eventUtf8.c_str());
            fflush(stdout);
        }));

        auto hr = server->Subscribe(sink.get());
        if (FAILED(hr))
        {
            fprintf(stderr, "Subscribe failed: 0x%08X\n", static_cast<uint32_t>(hr));
            exitCode = 1;
            return;
        }

        DWORD waitResult = WAIT_OBJECT_0;
        if (parentProcess)
        {
            const HANDLE waitHandles[]{ s_stopEvent, parentProcess.get() };
            waitResult = WaitForMultipleObjects(ARRAYSIZE(waitHandles), waitHandles, FALSE, INFINITE);
        }
        else
        {
            waitResult = WaitForSingleObject(s_stopEvent, INFINITE);
        }
        if (waitResult == WAIT_FAILED)
        {
            fprintf(stderr, "[wtcli] listen: wait failed (0x%08X)\n", GetLastError());
            exitCode = 1;
        }
        server->Unsubscribe();
        // s_stopEvent is intentionally NOT closed: it is static and still
        // referenced by the registered Ctrl-C handler (a non-capturing lambda
        // that can only reach it via the static), so closing it would leave the
        // handler pointing at an invalid handle. It is reclaimed at process exit.
    });

    // ── Default (no subcommand) ──
    app.callback([&]() {
        if (app.get_subcommands().empty())
        {
            printf("wtcli - Windows Terminal CLI\n\n");
            printf("Usage: wtcli [--json] <subcommand>\n\n");
            printf("Run 'wtcli --help' for available subcommands.\n");
        }
    });

    try
    {
        // CLI11's wide overload; see the comment on `wmain` above.
        app.parse(argc, argv);
    }
    catch (const CLI::ParseError& e)
    {
        return app.exit(e);
    }

    return exitCode;
}
