// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <Windows.h>
#include <oleauto.h>
#include <winrt/Windows.Foundation.h>
#include "ITerminalProtocol.h"
#include <limits>
#include <stdexcept>
#include <utility>

// Load SDK/WIL declarations before mocking the CLI's COM activation calls.
#include "..\..\..\cascadia\TerminalProtocol\ProtocolMarshaling.h"

namespace NativeMock
{
    HRESULT WINAPI GetActiveObject(REFCLSID, void*, IUnknown**);
    HRESULT WINAPI CoCreateInstance(REFCLSID, IUnknown*, DWORD, REFIID, void**);
    HRESULT LoadProxy(wil::unique_hmodule&);
    HRESULT RegisterProxy(const wil::unique_hmodule&);
}

#define LoadAndVerifyLocalProxyDll LoadProxy
#define RegisterProcessLocalProxyFactory RegisterProxy
namespace Microsoft::Terminal::Protocol
{
    using NativeMock::LoadProxy;
    using NativeMock::RegisterProxy;
}
#define GetActiveObject NativeMock::GetActiveObject
#define CoCreateInstance NativeMock::CoCreateInstance
#define wmain WtcliMain
#include "..\main.cpp"
#undef wmain
#undef CoCreateInstance
#undef GetActiveObject
#undef RegisterProcessLocalProxyFactory
#undef LoadAndVerifyLocalProxyDll

namespace NativeMock
{
    unsigned activeCalls = 0;
    unsigned activationCalls = 0;
    unsigned factoryCalls = 0;
    HRESULT activationResult = E_NOINTERFACE;
    bool registered = true;
    bool supportsFactory = true;
    HRESULT loadResult = S_OK;
    HRESULT registerResult = S_OK;
    unsigned loadCalls = 0;
    unsigned registerCalls = 0;
    ITerminalProtocol* queryServer = nullptr;

    struct ProfileServer : ITerminalProtocol
    {
        const char* capabilities = R"(["list_profiles"])";
        const char* profiles = R"([{"id":"{5784e726-2a17-4932-bd11-18a387ef9c02}","name":"Copilot","is_default":false,"agent_id":"copilot"}])";
        HRESULT queryResult = S_OK;
        unsigned queryCalls = 0;

        HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override
        {
            *object = nullptr;
            if (iid == __uuidof(IUnknown) || iid == __uuidof(ITerminalProtocol))
            {
                *object = static_cast<ITerminalProtocol*>(this);
                AddRef();
                return S_OK;
            }
            return E_NOINTERFACE;
        }
        ULONG STDMETHODCALLTYPE AddRef() override { return 2; }
        ULONG STDMETHODCALLTYPE Release() override { return 1; }
        HRESULT STDMETHODCALLTYPE Authenticate(BSTR, BSTR* json) override
        {
            *json = Bstr(R"({"authenticated":true,"protocol_version":"2.4"})");
            return S_OK;
        }
        HRESULT STDMETHODCALLTYPE GetCapabilities(BSTR* json) override
        {
            *json = Bstr(capabilities);
            return S_OK;
        }
        HRESULT STDMETHODCALLTYPE ListProfiles(BSTR* json) override
        {
            ++queryCalls;
            *json = SUCCEEDED(queryResult) ? Bstr(profiles) : nullptr;
            return queryResult;
        }
        HRESULT STDMETHODCALLTYPE GetActivePane(BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE ListWindows(BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE ListTabs(unsigned __int64, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE ListPanes(unsigned __int64, unsigned long, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE ReadPaneOutput(GUID, BSTR, long, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE GetProcessStatus(GUID, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE GetSessionVariable(GUID, BSTR, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE GetSettings(BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE CreateTab(unsigned __int64, BSTR, BSTR, BSTR, BSTR, boolean, boolean, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE SplitPane(GUID, BSTR, float, BSTR, BSTR, boolean, BSTR*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE ClosePane(GUID) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE SendInput(GUID, BSTR) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE FocusPane(GUID) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE SetSessionVariable(GUID, BSTR, BSTR) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE Subscribe(ITerminalProtocolEventSink*) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE Unsubscribe() override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE SendEvent(BSTR) override { return E_NOTIMPL; }
        HRESULT STDMETHODCALLTYPE GetPaneContext(GUID, boolean, long, long, BSTR*) override { return E_NOTIMPL; }
    };

    HRESULT LoadProxy(wil::unique_hmodule& module)
    {
        ++loadCalls;
        if (FAILED(loadResult))
        {
            module.reset();
            return loadResult;
        }
        return Microsoft::Terminal::Protocol::LoadAndVerifyLocalProxyDll(module);
    }

    HRESULT RegisterProxy(const wil::unique_hmodule& module)
    {
        ++registerCalls;
        return FAILED(registerResult) ? registerResult : Microsoft::Terminal::Protocol::RegisterProcessLocalProxyFactory(module);
    }

    struct Factory : IClassFactory
    {
        HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override
        {
            *object = nullptr;
            if (iid == __uuidof(IUnknown) || (supportsFactory && iid == __uuidof(IClassFactory)))
            {
                *object = static_cast<IClassFactory*>(this);
                AddRef();
                return S_OK;
            }
            return E_NOINTERFACE;
        }
        ULONG STDMETHODCALLTYPE AddRef() override { return 2; }
        ULONG STDMETHODCALLTYPE Release() override { return 1; }
        HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown*, REFIID iid, void** object) override
        {
            ++factoryCalls;
            *object = nullptr;
            if (iid != __uuidof(ITerminalProtocol))
                throw std::runtime_error("wrong protocol interface");
            return E_NOINTERFACE;
        }
        HRESULT STDMETHODCALLTYPE LockServer(BOOL) override { return S_OK; }
    } factory;

    HRESULT WINAPI GetActiveObject(REFCLSID, void*, IUnknown** object)
    {
        ++activeCalls;
        *object = registered ? &factory : nullptr;
        return registered ? S_OK : MK_E_UNAVAILABLE;
    }

    HRESULT WINAPI CoCreateInstance(REFCLSID, IUnknown*, DWORD, REFIID, void** object)
    {
        ++activationCalls;
        if (queryServer)
        {
            *object = queryServer;
            queryServer->AddRef();
            return S_OK;
        }
        *object = nullptr;
        return activationResult;
    }

    void Reset()
    {
        activeCalls = activationCalls = factoryCalls = 0;
        activationResult = E_NOINTERFACE;
        registered = supportsFactory = true;
        loadResult = registerResult = S_OK;
        loadCalls = registerCalls = 0;
        queryServer = nullptr;
    }

    void Require(bool condition, const char* message = "listener connection contract failed")
    {
        if (!condition)
            throw std::runtime_error(message);
    }

    void CheckListenTimestampFormatting()
    {
        using Result = wtcli::ListenEventFormatResult;

        Json::StreamWriterBuilder writer;
        writer["indentation"] = "";
        Json::Value event;
        event["type"] = "event";
        event["method"] = "agent_event";
        event["params"]["event"] = "agent.notification";
        event["params"]["pane_id"] = "owner";
        event["params"]["payload"]["message"] = "approve command";
        event["params"]["payload"]["tool_input"]["timestamp"] = 123;
        event["params"]["last_activity_at_ms"] = 456;
        const std::pair<Json::Int64, const char*> cases[]{
            { 1792473545608, "2026-10-20T05:19:05.608Z" },
            { 0, "1970-01-01T00:00:00.000Z" },
            { -1, "1969-12-31T23:59:59.999Z" },
            { 1709164800000, "2024-02-29T00:00:00.000Z" },
            { 253402300799999, "9999-12-31T23:59:59.999Z" },
        };
        for (const auto& [timestamp, expected] : cases)
        {
            event["params"]["payload"]["timestamp"] = timestamp;
            const auto raw = Json::writeString(writer, event);
            std::string formatted;
            Require(wtcli::FormatListenEvent(raw, false, formatted) == Result::Unchanged, "plain listen must preserve numeric timestamps");
            Require(wtcli::FormatListenEvent(raw, true, formatted) == Result::Normalized, "JSON listen must normalize numeric timestamps");
            Json::Value actual;
            Json::CharReaderBuilder reader;
            std::string errors;
            std::istringstream stream{ formatted };
            Require(Json::parseFromStream(reader, stream, &actual, &errors), "normalized output must remain valid JSON");
            event["params"]["payload"]["timestamp"] = expected;
            Require(actual == event, "normalization must produce UTC ISO 8601 and preserve other fields");
            Require(formatted.find('\n') == std::string::npos, "listen output must remain one JSON object per line");
            Require(wtcli::FormatListenEvent(formatted, true, formatted) == Result::Unchanged, "existing ISO timestamps must stay unchanged");
        }

        for (const auto& timestamp : {
                 Json::Value{ (std::numeric_limits<Json::UInt64>::max)() },
                 Json::Value{ Json::Int64{ 253402300800000 } },
                 Json::Value{ 1.5 },
             })
        {
            event["params"]["payload"]["timestamp"] = timestamp;
            std::string formatted;
            Require(wtcli::FormatListenEvent(Json::writeString(writer, event), true, formatted) == Result::InvalidTimestamp, "invalid timestamps must be reported instead of inventing dates");
        }
        for (const auto* unchanged : {
                 R"({"method":"agent_event","params":{"payload":{"timestamp":"2026-10-20T13:19:05.580+08:00"}}})",
                 R"({"method":"agent_event","params":{"payload":{}}})",
                 R"({"method":"connection_state","params":{"payload":{"timestamp":1792473545608}}})",
                 R"({"method":"agent_event","params":{"payload":false}})",
             })
        {
            std::string formatted;
            Require(wtcli::FormatListenEvent(unchanged, true, formatted) == Result::Unchanged, "unrelated events and existing timestamp strings must remain unchanged");
        }
        std::string formatted;
        Require(wtcli::FormatListenEvent("invalid JSON", true, formatted) == Result::InvalidJson, "JSON listen must report malformed events");
        Require(wtcli::FormatListenEvent("invalid JSON", false, formatted) == Result::Unchanged, "plain listen must remain a raw pass-through");
        puts("Listener timestamp formatting tests passed.");
    }

    int Listen(bool existingOnly)
    {
        wchar_t executable[] = L"wtcli";
        wchar_t json[] = L"--json";
        wchar_t skip[] = L"--skip-authenticate";
        wchar_t listen[] = L"listen";
        wchar_t flag[] = L"--existing-only";
        wchar_t tokenOption[] = L"--ready-token";
        wchar_t token[] = L"native-test";
        wchar_t* argv[]{ executable, json, skip, listen, tokenOption, token, flag };
        const auto result = WtcliMain(existingOnly ? 7 : 6, argv);
        winrt::uninit_apartment();
        return result;
    }

    int Publish(bool existingOnly, bool fromStdin = false)
    {
        wchar_t executable[] = L"wtcli";
        wchar_t json[] = L"--json";
        wchar_t skip[] = L"--skip-authenticate";
        wchar_t publish[] = L"publish";
        wchar_t payload[] = L"{\"method\":\"fixture.completed\"}";
        wchar_t stdinOption[] = L"--stdin";
        wchar_t flag[] = L"--existing-only";
        wchar_t* argv[]{ executable, json, skip, publish, fromStdin ? stdinOption : payload, flag };
        std::istringstream input{ "{\"method\":\"fixture.completed\"}" };
        auto* previousInput = std::cin.rdbuf(input.rdbuf());
        const auto result = WtcliMain(existingOnly ? 6 : 5, argv);
        std::cin.rdbuf(previousInput);
        std::cin.clear();
        winrt::uninit_apartment();
        return result;
    }

    void CheckProfileQueries()
    {
        Reset();
        ProfileServer server;
        queryServer = &server;
        const auto reset = wil::scope_exit([]() noexcept { queryServer = nullptr; });
        const auto query = []() {
            wchar_t executable[] = L"wtcli";
            wchar_t json[] = L"--json";
            wchar_t command[] = L"list-profiles";
            wchar_t* argv[]{ executable, json, command };
            const auto result = WtcliMain(3, argv);
            winrt::uninit_apartment();
            return result;
        };

        Require(query() == 0 && server.queryCalls == 1, "profile queries must reach the runtime server");
        server.profiles = "[]";
        Require(query() == 0 && server.queryCalls == 2, "an empty profile list must remain a successful query");
        server.queryResult = E_FAIL;
        Require(query() == 1 && server.queryCalls == 3, "profile query failures must not become empty lists");
        server.queryResult = S_OK;
        server.profiles = "{}";
        Require(query() == 1 && server.queryCalls == 4, "malformed profile results must fail explicitly");
        server.capabilities = "[]";
        Require(query() == 2 && server.queryCalls == 4, "older servers must not receive the appended profile query");
        server.capabilities = "{}";
        Require(query() == 1 && server.queryCalls == 4, "malformed capabilities must not reach the appended method");
        puts("Profile query compatibility tests passed.");
    }
}

int wmain()
{
    using namespace NativeMock;
    try
    {
        CheckListenTimestampFormatting();
        SetEnvironmentVariableW(L"WT_COM_CLSID", L"{11111111-1111-1111-1111-111111111111}");
        CheckProfileQueries();
        for (const auto failLoad : { false, true })
        {
            for (const auto existingOnly : { false, true })
            {
                for (const auto operation : { 0, 1, 2 })
                {
                    Reset();
                    const auto resetFailure = wil::scope_exit([]() noexcept { loadResult = registerResult = S_OK; });
                    (failLoad ? loadResult : registerResult) = E_OUTOFMEMORY;
                    Require((operation == 0 ? Listen(existingOnly) : Publish(existingOnly, operation == 2)) == 1);
                    Require(loadCalls == 1 && registerCalls == (failLoad ? 0u : 1u));
                    Require(activationCalls == 0 && activeCalls == 0 && factoryCalls == 0);
                }
            }
        }
        Reset();
        Require(Listen(false) == 1);
        Require(activationCalls == 1 && activeCalls == 0 && factoryCalls == 0);

        for (const auto factoryRegistered : { false, true })
        {
            Reset();
            activationResult = REGDB_E_CLASSNOTREG;
            registered = factoryRegistered;
            Require(Listen(false) == 1);
            Require(activationCalls == 1 && activeCalls == 1 && factoryCalls == (factoryRegistered ? 1u : 0u));
        }

        Reset();
        Require(Listen(true) == 1);
        Require(activationCalls == 0 && activeCalls == 1 && factoryCalls == 1);

        Reset();
        registered = false;
        Require(Listen(true) == 1);
        Require(activationCalls == 0 && activeCalls == 1 && factoryCalls == 0);

        Reset();
        supportsFactory = false;
        Require(Listen(true) == 1);
        Require(activationCalls == 0 && activeCalls == 1 && factoryCalls == 0);

        for (const auto fromStdin : { false, true })
        {
            Reset();
            Require(Publish(false, fromStdin) == 1);
            Require(activationCalls == 1 && activeCalls == 0 && factoryCalls == 0);

            for (const auto factoryRegistered : { false, true })
            {
                Reset();
                activationResult = REGDB_E_CLASSNOTREG;
                registered = factoryRegistered;
                Require(Publish(false, fromStdin) == 1);
                Require(activationCalls == 1 && activeCalls == 1 && factoryCalls == (factoryRegistered ? 1u : 0u));
            }

            Reset();
            Require(Publish(true, fromStdin) == 1);
            Require(activationCalls == 0 && activeCalls == 1 && factoryCalls == 1);

            Reset();
            registered = false;
            Require(Publish(true, fromStdin) == 1);
            Require(activationCalls == 0 && activeCalls == 1 && factoryCalls == 0);

            Reset();
            supportsFactory = false;
            Require(Publish(true, fromStdin) == 1);
            Require(activationCalls == 0 && activeCalls == 1 && factoryCalls == 0);
        }

        puts("Listener/publisher connection tests passed (no real COM server or agent).");
        return 0;
    }
    catch (const std::exception& error)
    {
        fprintf(stderr, "%s\n", error.what());
        return 1;
    }
}
