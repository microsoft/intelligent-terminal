// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <unknwn.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Microsoft.Terminal.Protocol.h>

#include "Formatting.h"

#include <CLI/CLI.hpp>

#include <chrono>
#include <cstdio>
#include <sstream>
#include <string>
#include <thread>

namespace Protocol = winrt::Microsoft::Terminal::Protocol;

// ── EventCallback — receives push-based events from Terminal ──

struct EventCallback : winrt::implements<EventCallback, Protocol::IProtocolEventCallback>
{
    EventCallback(std::function<void(winrt::hstring const&)> handler) :
        _handler(std::move(handler)) {}

    void OnEvent(winrt::hstring const& eventJson)
    {
        if (_handler)
            _handler(eventJson);
    }

private:
    std::function<void(winrt::hstring const&)> _handler;
};

// ── Helpers ──

static Protocol::IProtocolServer ConnectToTerminal(Protocol::AuthResult* outAuth = nullptr)
{
    wchar_t clsid[128]{};
    if (!GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)))
    {
        fprintf(stderr, "[wtcli] WT_COM_CLSID not set. Must run inside a Windows Terminal pane.\n");
        return nullptr;
    }

    CLSID cls{};
    if (FAILED(CLSIDFromString(clsid, &cls)))
    {
        fprintf(stderr, "[wtcli] Invalid CLSID: %ls\n", clsid);
        return nullptr;
    }

    try
    {
        auto server = winrt::create_instance<Protocol::IProtocolServer>(cls, CLSCTX_LOCAL_SERVER);
        auto authResult = server.Authenticate(L"");
        if (!authResult.Authenticated)
        {
            fprintf(stderr, "[wtcli] Authentication failed\n");
            return nullptr;
        }
        if (outAuth)
            *outAuth = authResult;
        return server;
    }
    catch (const winrt::hresult_error& e)
    {
        fprintf(stderr, "[wtcli] Connection failed: 0x%08X %ls\n",
                static_cast<uint32_t>(e.code()), e.message().c_str());
        return nullptr;
    }
}

static winrt::guid ResolveSessionId(const Protocol::IProtocolServer& server, const std::string& target)
{
    if (!target.empty())
    {
        // Accept both plain and braced GUID formats
        auto wstr = winrt::to_hstring(target);
        std::wstring guidStr{ wstr };
        if (!guidStr.empty() && guidStr[0] != L'{')
            guidStr = L"{" + guidStr + L"}";
        GUID g{};
        if (SUCCEEDED(CLSIDFromString(guidStr.c_str(), &g)))
            return winrt::guid{ g };
        fprintf(stderr, "[wtcli] Invalid session ID: %s\n", target.c_str());
        return {};
    }
    auto info = server.GetActivePane();
    return info.SessionId;
}

static std::string GuidToString(const winrt::guid& g)
{
    wchar_t buf[40]{};
    StringFromGUID2(g, buf, ARRAYSIZE(buf));
    std::wstring ws(buf);
    if (ws.size() > 2 && ws.front() == L'{' && ws.back() == L'}')
        ws = ws.substr(1, ws.size() - 2);
    return winrt::to_string(winrt::hstring{ ws });
}

static uint64_t GetFirstWindowId(const Protocol::IProtocolServer& server)
{
    auto windows = server.ListWindows();
    if (windows.size() > 0)
        return windows[0].WindowId;
    return 0;
}

static uint32_t GetFirstTabId(const Protocol::IProtocolServer& server, uint64_t windowId)
{
    auto tabs = server.ListTabs(windowId);
    if (tabs.size() > 0)
        return tabs[0].TabId;
    return UINT32_MAX;
}

// ── Main ──

int main()
{
    winrt::init_apartment(winrt::apartment_type::multi_threaded);

    CLI::App app{ "wtcli — Windows Terminal CLI" };
    app.require_subcommand(0, 1);

    // Global options
    bool jsonMode = false;
    int exitCode = 0;
    app.add_flag("--json", jsonMode, "Output raw JSON");

    // Helper: connect to Windows Terminal
    auto connect = [&]() -> Protocol::IProtocolServer {
        auto server = ConnectToTerminal();
        if (!server)
            exitCode = 1;
        return server;
    };

    // ── list-windows ──
    auto* listWindowsCmd = app.add_subcommand("list-windows", "List all windows")->alias("lsw");
    listWindowsCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto windows = server.ListWindows();
            if (jsonMode)
            {
                Json::Value arr(Json::objectValue);
                Json::Value list(Json::arrayValue);
                for (const auto& w : windows) list.append(WindowInfoToJson(w));
                arr["windows"] = list;
                PrintJson(arr);
            }
            else
            {
                FormatWindowsHuman(windows);
            }
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "ListWindows failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── list-tabs ──
    std::string listTabsWindowId;
    auto* listTabsCmd = app.add_subcommand("list-tabs", "List tabs in a window")->alias("lst");
    listTabsCmd->add_option("-w,--window-id", listTabsWindowId, "Window ID");
    listTabsCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            uint64_t wid = listTabsWindowId.empty() ? GetFirstWindowId(server) : std::stoull(listTabsWindowId);
            auto tabs = server.ListTabs(wid);
            if (jsonMode)
            {
                Json::Value arr(Json::objectValue);
                Json::Value list(Json::arrayValue);
                for (const auto& t : tabs) list.append(TabInfoToJson(t));
                arr["tabs"] = list;
                PrintJson(arr);
            }
            else
            {
                FormatTabsHuman(tabs);
            }
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "ListTabs failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
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
        try
        {
            uint64_t wid = listPanesWindowId.empty() ? 0 : std::stoull(listPanesWindowId);
            uint32_t tid = listPanesTabId.empty() ? UINT32_MAX : static_cast<uint32_t>(std::stoul(listPanesTabId));
            if (tid == UINT32_MAX)
            {
                if (wid == 0) wid = GetFirstWindowId(server);
                tid = GetFirstTabId(server, wid);
            }
            auto panes = server.ListPanes(wid, tid);
            if (jsonMode)
            {
                Json::Value arr(Json::objectValue);
                Json::Value list(Json::arrayValue);
                for (const auto& p : panes) list.append(PaneInfoToJson(p));
                arr["panes"] = list;
                PrintJson(arr);
            }
            else
            {
                FormatPanesHuman(panes);
            }
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "ListPanes failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── active-pane ──
    auto* activePaneCmd = app.add_subcommand("active-pane", "Show the currently active pane");
    activePaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto info = server.GetActivePane();
            if (jsonMode)
                PrintJson(PaneInfoToJson(info));
            else
                FormatActivePaneHuman(info);
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "GetActivePane failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
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
        try
        {
            auto sessionId = ResolveSessionId(server, capturePaneTarget);
            const auto sourceArg = captureLastPrompt ? L"last_prompt" : L"scrollback";
            auto output = server.ReadPaneOutput(sessionId, sourceArg, captureMaxLines);
            if (jsonMode)
            {
                PrintJson(PaneOutputToJson(output));
            }
            else
            {
                auto content = winrt::to_string(output.Content);
                printf("%s\n", content.c_str());
            }
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "ReadPaneOutput failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── pane-status ──
    std::string paneStatusTarget;
    auto* paneStatusCmd = app.add_subcommand("pane-status", "Show pane process status");
    paneStatusCmd->add_option("-t,--target", paneStatusTarget, "Session ID (GUID)");
    paneStatusCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto sessionId = ResolveSessionId(server, paneStatusTarget);
            auto status = server.GetProcessStatus(sessionId);
            if (jsonMode)
            {
                Json::Value v;
                v["session_id"] = GuidToString(status.SessionId);
                v["state"] = winrt::to_string(status.State);
                v["pid"] = static_cast<Json::UInt>(status.Pid);
                if (status.HasExitCode) v["exit_code"] = status.ExitCode;
                PrintJson(v);
            }
            else
            {
                FormatPaneStatusHuman(status);
            }
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "GetProcessStatus failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // send-keys removed: keystroke injection is now confined to per-wta
    // secure pipes (see TerminalProtocolPipeServer in TerminalApp). Any
    // caller that previously used `wtcli send-keys` must route through
    // the wta process WT spawned for it.

    // ── new-tab ──
    std::string newTabCommand, newTabTitle, newTabCwd;
    auto* newTabCmd = app.add_subcommand("new-tab", "Create a new tab")->alias("neww");
    newTabCmd->add_option("-c,--command", newTabCommand, "Command to run");
    newTabCmd->add_option("-n,--title", newTabTitle, "Tab title");
    newTabCmd->add_option("-d,--cwd", newTabCwd, "Starting directory");
    newTabCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto result = server.CreateTab(
                0, L"",
                winrt::to_hstring(newTabCommand),
                winrt::to_hstring(newTabTitle),
                winrt::to_hstring(newTabCwd),
                false, true);
            if (jsonMode)
                PrintJson(CreationResultToJson(result));
            else
                FormatCreatedTabHuman(result);
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "CreateTab failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── split-pane ──
    std::string splitPaneTarget, splitPaneCommand, splitPaneDirection;
    bool splitHorizontal = false, splitVertical = false;
    double splitSize = 0.5;
    auto* splitPaneCmd = app.add_subcommand("split-pane", "Split a pane")->alias("splitw");
    splitPaneCmd->add_option("-t,--target", splitPaneTarget, "Session ID (GUID)");
    splitPaneCmd->add_option("-d,--direction", splitPaneDirection, "Split direction: right|left|up|down|auto");
    splitPaneCmd->add_flag("-H,--horizontal", splitHorizontal, "Split horizontally (legacy alias for --direction down)");
    splitPaneCmd->add_flag("-v,--vertical", splitVertical, "Split vertically (legacy alias for --direction right)");
    splitPaneCmd->add_option("-s,--size", splitSize, "Size fraction");
    splitPaneCmd->add_option("-c,--command", splitPaneCommand, "Command to run");
    splitPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto sessionId = ResolveSessionId(server, splitPaneTarget);
            // --direction wins over the legacy boolean flags. If neither is
            // given, send "automatic" so the COM server picks the longer
            // dimension (matches the WT default for `splitPane`).
            std::wstring dir;
            if (!splitPaneDirection.empty())
                dir = winrt::to_hstring(splitPaneDirection).c_str();
            else if (splitHorizontal)
                dir = L"down";
            else if (splitVertical)
                dir = L"right";
            else
                dir = L"automatic";
            auto result = server.SplitPane(
                sessionId, winrt::hstring{ dir }, static_cast<float>(splitSize),
                L"", winrt::to_hstring(splitPaneCommand), true);
            if (jsonMode)
                PrintJson(CreationResultToJson(result));
            else
                FormatCreatedPaneHuman(result);
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "SplitPane failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── kill-pane ──
    std::string killPaneTarget;
    auto* killPaneCmd = app.add_subcommand("kill-pane", "Close a pane")->alias("killp");
    killPaneCmd->add_option("-t,--target", killPaneTarget, "Session ID (GUID)");
    killPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto sessionId = ResolveSessionId(server, killPaneTarget);
            server.ClosePane(sessionId);
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
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "ClosePane failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── focus-pane ──
    std::string focusPaneTarget;
    auto* focusPaneCmd = app.add_subcommand("focus-pane", "Switch focus to a pane")->alias("focusp");
    focusPaneCmd->add_option("-t,--target", focusPaneTarget, "Session ID (GUID)");
    focusPaneCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            auto sessionId = ResolveSessionId(server, focusPaneTarget);
            server.FocusPane(sessionId);
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
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "FocusPane failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── test-pipe ──
    auto* testPipeCmd = app.add_subcommand("test-pipe", "Test connection to Windows Terminal");
    testPipeCmd->callback([&]() {
        printf("Connecting to Windows Terminal...\n");
        auto server = connect();
        if (!server) { fprintf(stderr, "Connection failed.\n"); return; }
        printf("Connected and authenticated!\n\n");

        try
        {
            auto windows = server.ListWindows();
            Json::Value arr(Json::objectValue);
            Json::Value list(Json::arrayValue);
            for (const auto& w : windows) list.append(WindowInfoToJson(w));
            arr["windows"] = list;
            printf("list_windows:\n");
            PrintJson(arr);
        }
        catch (const winrt::hresult_error&) {}

        printf("\n");

        try
        {
            auto capsJson = server.GetCapabilities();
            Json::Value cap;
            Json::CharReaderBuilder rb;
            std::string errs;
            auto capsStr = winrt::to_string(capsJson);
            std::istringstream ss(capsStr);
            Json::parseFromStream(rb, ss, &cap, &errs);
            printf("get_capabilities:\n");
            PrintJson(cap);
        }
        catch (const winrt::hresult_error&) {}
    });

    // ── info ──
    auto* infoCmd = app.add_subcommand("info", "Show connection info");
    infoCmd->callback([&]() {
        wchar_t clsid[128]{};
        auto hasClsid = GetEnvironmentVariableW(L"WT_COM_CLSID", clsid, ARRAYSIZE(clsid)) > 0;

        Protocol::AuthResult authResult{};
        auto server = ConnectToTerminal(&authResult);
        auto version = server ? winrt::to_string(authResult.ProtocolVersion) : std::string{};

        Json::Value methods(Json::arrayValue);
        if (server)
        {
            try
            {
                auto capsJson = server.GetCapabilities();
                Json::Value cap;
                Json::CharReaderBuilder rb;
                std::string errs;
                auto capsStr = winrt::to_string(capsJson);
                std::istringstream ss(capsStr);
                if (Json::parseFromStream(rb, ss, &cap, &errs) && cap.isArray())
                    methods = cap;
            }
            catch (const winrt::hresult_error&) {}
        }

        if (jsonMode)
        {
            Json::Value v;
            if (hasClsid)
                v["com_clsid"] = winrt::to_string(winrt::hstring{ clsid });
            v["connected"] = (server != nullptr);
            if (!version.empty())
                v["protocol_version"] = version;
            v["methods"] = methods;
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
                if (methods.size() > 0)
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
        // Parse target as GUID
        auto sessionId = ResolveSessionId(server, waitForTarget);
        auto start = std::chrono::steady_clock::now();

        while (true)
        {
            try
            {
                auto status = server.GetProcessStatus(sessionId);
                auto state = winrt::to_string(status.State);
                if (state == "exited")
                {
                    if (jsonMode)
                    {
                        Json::Value v;
                        v["state"] = state;
                        v["exit_code"] = status.ExitCode;
                        PrintJson(v);
                    }
                    else
                    {
                        printf("Process exited");
                        if (status.HasExitCode)
                            printf(" (code %d)", status.ExitCode);
                        printf("\n");
                    }
                    return;
                }
            }
            catch (const winrt::hresult_error& e)
            {
                fprintf(stderr, "GetProcessStatus failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
                exitCode = 1;
                return;
            }

            if (waitTimeout > 0)
            {
                auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                    std::chrono::steady_clock::now() - start).count();
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

    // ── quick-pick ──
    std::string quickPickTitle;
    std::vector<std::string> quickPickChoices;
    bool quickPickFreeInput = false;
    auto* quickPickCmd = app.add_subcommand("quick-pick", "Show a quick-pick dialog in Windows Terminal");
    quickPickCmd->add_option("choices", quickPickChoices, "Choices to present")->required();
    quickPickCmd->add_option("--title", quickPickTitle, "Dialog title");
    quickPickCmd->add_flag("--free-input", quickPickFreeInput, "Allow freeform text input");
    quickPickCmd->callback([&]() {
        auto server = connect();
        if (!server) return;
        try
        {
            std::vector<winrt::hstring> hstringChoices;
            hstringChoices.reserve(quickPickChoices.size());
            for (const auto& c : quickPickChoices)
                hstringChoices.push_back(winrt::to_hstring(c));

            auto result = server.QuickPick(
                winrt::to_hstring(quickPickTitle),
                hstringChoices,
                quickPickFreeInput).get();

            if (jsonMode)
            {
                Json::Value v;
                v["cancelled"] = result.Cancelled;
                v["selected"] = winrt::to_string(result.Selected);
                PrintJson(v);
            }
            else
            {
                if (result.Cancelled)
                    printf("(cancelled)\n");
                else
                    printf("%s\n", winrt::to_string(result.Selected).c_str());
            }
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "QuickPick failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── publish ──
    // Low-level "pass this JSON through to IProtocolServer::SendEvent verbatim"
    // escape hatch, for event shapes that don't fit the legacy send-event
    // envelope (method=agent_event, params.event required). Examples:
    // autofix_state updates from WTA that the COM server dispatches directly
    // to TerminalPage rather than broadcasting.
    std::string publishJson;
    auto* publishCmd = app.add_subcommand("publish", "Forward raw JSON to IProtocolServer::SendEvent");
    publishCmd->add_option("json", publishJson, "Full event JSON (e.g. {\"method\":\"autofix_state\",\"params\":{...}})")->required();
    publishCmd->callback([&]() {
        auto server = connect();
        if (!server)
        {
            return;
        }
        try
        {
            server.SendEvent(winrt::to_hstring(publishJson));
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "publish failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── send-event ──
    std::string sendEventType, sendEventJson, sendEventPaneTarget;
    auto* sendEventCmd = app.add_subcommand("send-event", "Publish an event to all listeners")->alias("se");
    sendEventCmd->add_option("-p,--pane", sendEventPaneTarget, "Source session ID (GUID)");
    sendEventCmd->add_option("-e,--event", sendEventType, "Event type (e.g. agent.task.started)")->required();
    sendEventCmd->add_option("json", sendEventJson, "Event params as JSON object");
    sendEventCmd->callback([&]() {
        auto server = connect();
        if (!server)
            return;
        try
        {
            Json::Value evt;
            evt["type"] = "event";
            evt["method"] = "agent_event";

            Json::Value params;
            if (!sendEventJson.empty())
            {
                Json::CharReaderBuilder rb;
                std::string errs;
                std::istringstream ss(sendEventJson);
                if (!Json::parseFromStream(rb, ss, &params, &errs) || !params.isObject())
                {
                    fprintf(stderr, "Invalid JSON: expected an object\n");
                    exitCode = 1;
                    return;
                }
            }

            params["event"] = sendEventType;
            if (!sendEventPaneTarget.empty())
                params["session_id"] = sendEventPaneTarget;
            else
                params["session_id"] = GuidToString(ResolveSessionId(server, ""));

            evt["params"] = params;

            Json::StreamWriterBuilder wb;
            wb["indentation"] = "";
            server.SendEvent(winrt::to_hstring(Json::writeString(wb, evt)));
        }
        catch (const winrt::hresult_error& e)
        {
            fprintf(stderr, "SendEvent failed: 0x%08X\n", static_cast<uint32_t>(e.code()));
            exitCode = 1;
        }
    });

    // ── listen ──
    std::string listenTarget;
    std::string listenEventFilter;
    auto* listenCmd = app.add_subcommand("listen", "Stream real-time events from Windows Terminal");
    listenCmd->add_option("-t,--target", listenTarget, "Filter by session ID (GUID)");
    listenCmd->add_option("--event", listenEventFilter, "Filter by event type (supports trailing wildcard, e.g. agent.*)");
    listenCmd->callback([&]() {
        auto server = ConnectToTerminal();
        if (!server) { exitCode = 1; return; }

        // Set up Ctrl-C handler to unblock the wait.
        static HANDLE s_stopEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        SetConsoleCtrlHandler([](DWORD) -> BOOL {
            SetEvent(s_stopEvent);
            return TRUE;
        }, TRUE);

        if (!jsonMode)
            fprintf(stderr, "Listening for events... (Ctrl-C to stop)\n");

        auto callback = winrt::make<EventCallback>([&](winrt::hstring const& eventJson) {
            auto eventUtf8 = winrt::to_string(eventJson);

            // Optionally filter by pane_id and/or event type
            if (!listenTarget.empty() || !listenEventFilter.empty())
            {
                Json::Value ev;
                Json::CharReaderBuilder rb;
                std::string errs;
                std::istringstream ss(eventUtf8);
                if (Json::parseFromStream(rb, ss, &ev, &errs))
                {
                    if (!listenTarget.empty())
                    {
                        auto sessionId = ev["params"].get("session_id", "").asString();
                        if (sessionId != listenTarget)
                            return;
                    }

                    if (!listenEventFilter.empty())
                    {
                        auto eventType = ev["params"].get("event", "").asString();
                        // Support trailing wildcard: "agent.*" matches "agent.task.started"
                        if (listenEventFilter.back() == '*')
                        {
                            auto prefix = listenEventFilter.substr(0, listenEventFilter.size() - 1);
                            if (eventType.substr(0, prefix.size()) != prefix)
                                return;
                        }
                        else if (eventType != listenEventFilter)
                        {
                            return;
                        }
                    }
                }
            }

            printf("%s\n", eventUtf8.c_str());
            fflush(stdout);
        });

        try
        {
            server.Subscribe(callback);
        }
        catch (winrt::hresult_error const& e)
        {
            fprintf(stderr, "Subscribe failed: %ls\n", e.message().c_str());
            exitCode = 1;
            CloseHandle(s_stopEvent);
            return;
        }

        // Block until Ctrl-C.
        WaitForSingleObject(s_stopEvent, INFINITE);
        server.Unsubscribe();
        CloseHandle(s_stopEvent);
    });

    // ── Default (no subcommand) ──
    app.callback([&]() {
        if (app.get_subcommands().empty())
        {
            printf("wtcli — Windows Terminal CLI\n\n");
            printf("Usage: wtcli [--json] [--pipe-name NAME] <subcommand>\n\n");
            printf("Run 'wtcli --help' for available subcommands.\n");
        }
    });

    CLI11_PARSE(app, __argc, __argv);

    return exitCode;
}
