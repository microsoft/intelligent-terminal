// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <Windows.h>
#include <oleauto.h>
#include <winrt/Windows.Foundation.h>
#include "ITerminalProtocol.h"
#include <stdexcept>

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
    }

    void Require(bool condition)
    {
        if (!condition)
            throw std::runtime_error("listener connection contract failed");
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
}

int wmain()
{
    using namespace NativeMock;
    try
    {
        SetEnvironmentVariableW(L"WT_COM_CLSID", L"{11111111-1111-1111-1111-111111111111}");
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
