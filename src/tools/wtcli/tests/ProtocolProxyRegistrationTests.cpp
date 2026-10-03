// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

// Standalone, unpackaged test executable. No Terminal activation or registration
// changes outside this process. argv[1] is an absolute path to a built proxy DLL.
#include "../../../cascadia/inc/TerminalProtocolProxyRegistration.h"
#include <atomic>
#include <cstdio>
#include <thread>
#include <wrl/implements.h>

namespace Protocol = Microsoft::Terminal::Protocol;

static void Check(const bool condition, const char* message)
{
    THROW_HR_IF_MSG(E_UNEXPECTED, !condition, "%hs", message);
}

class TestSink final : public Microsoft::WRL::RuntimeClass<Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::ClassicCom>, ITerminalProtocolEventSink>
{
public:
    std::atomic<bool> received{ false };

    HRESULT STDMETHODCALLTYPE OnEvent(BSTR text) noexcept override
    {
        received = text && wcscmp(text, L"package-local callback") == 0;
        return received ? S_OK : E_INVALIDARG;
    }
};

int wmain(int argc, wchar_t** argv)
try
{
    Check(argc == 2, "Expected a built proxy DLL path");
    const auto apartment = wil::CoInitializeEx(COINIT_MULTITHREADED);
    UINT32 length = 0;
    Check(GetCurrentPackageFullName(&length, nullptr) == APPMODEL_ERROR_NO_PACKAGE, "Run this test unpackaged");

    const auto dev = Protocol::details::AllowDevelopmentProxy;
    if (dev)
    {
        Check(Protocol::details::GetTrustedProxyPath() == Protocol::details::GetExecutableLocalProxyPath(), "Dev uses its own sibling path");
    }
    else
    {
        wil::unique_hmodule output{ LoadLibraryExW(L"version.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32) };
        THROW_LAST_ERROR_IF_NULL(output);
        Check(Protocol::LoadAndVerifyLocalProxyDll(output) == HRESULT_FROM_WIN32(APPMODEL_ERROR_NO_PACKAGE), "Unpackaged production must fail before loading");
        Check(!output, "Failure must clear the output handle");
    }

    Protocol::details::ProxyRegistration registration;
    Check(registration.Register(nullptr) == E_INVALIDARG, "Reject null module");
    THROW_IF_FAILED(registration.Unregister());
    wil::unique_hmodule nonProxy{ LoadLibraryExW(L"version.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32) };
    THROW_LAST_ERROR_IF_NULL(nonProxy);
    Check(FAILED(registration.Register(nonProxy.get())), "Reject DLL without proxy factory exports");

    // Exercise the registration mechanism independently of package policy using
    // only our specified build artifact, never a COM-selected installed proxy.
    auto module = Protocol::details::LoadAndVerifyProxyDll(argv[1]);
    const auto info = reinterpret_cast<Protocol::details::GetProxyDllInfo>(GetProcAddress(module.get(), "GetProxyDllInfo"));
    const auto getFactory = reinterpret_cast<Protocol::details::DllGetClassObject>(GetProcAddress(module.get(), "DllGetClassObject"));
    Check(info && getFactory, "Proxy exports exist");
    const tagProxyFileInfo** files = nullptr;
    const CLSID* clsid = nullptr;
    info(&files, &clsid);
    Check(files && clsid, "Proxy metadata exists");
    THROW_IF_FAILED(registration.Register(module.get()));
    const auto revoke = wil::scope_exit([&]() noexcept { LOG_IF_FAILED(registration.Unregister()); });
    THROW_IF_FAILED(registration.Register(module.get()));

    for (const auto& iid : Protocol::details::ProxyInterfaces)
    {
        CLSID actual{};
        THROW_IF_FAILED(CoGetPSClsid(iid, &actual));
        Check(actual == *clsid, "Protocol and handoff IIDs resolve to the explicitly loaded factory");
    }
    Microsoft::WRL::ComPtr<IPSFactoryBuffer> directFactory;
    Microsoft::WRL::ComPtr<IPSFactoryBuffer> comFactory;
    THROW_IF_FAILED(getFactory(*clsid, IID_PPV_ARGS(&directFactory)));
    THROW_IF_FAILED(CoGetClassObject(*clsid, CLSCTX_INPROC_SERVER, nullptr, IID_PPV_ARGS(&comFactory)));
    Check(directFactory.Get() == comFactory.Get(), "COM returned the factory from the loaded module");
    Check(Protocol::details::ProxyInterfaces.size() == 7, "Protocol and legacy/current handoff coverage");
    for (const auto& iid : Protocol::details::ProxyInterfaces)
    {
        Microsoft::WRL::ComPtr<IRpcProxyBuffer> buffer;
        void* interfacePointer = nullptr;
        THROW_IF_FAILED(comFactory->CreateProxy(nullptr, iid, &buffer, &interfacePointer));
        Check(interfacePointer != nullptr, "Local factory implements each registered interface");
        static_cast<IUnknown*>(interfacePointer)->Release();
    }

    auto sink = Microsoft::WRL::Make<TestSink>();
    Check(!!sink, "Allocate callback sink");
    Microsoft::WRL::ComPtr<IStream> stream;
    THROW_IF_FAILED(CoMarshalInterThreadInterfaceInStream(__uuidof(ITerminalProtocolEventSink), sink.Get(), &stream));
    HRESULT callbackResult = E_PENDING;
    std::thread worker([marshaled = stream.Detach(), &callbackResult]() noexcept {
        try
        {
            const auto sta = wil::CoInitializeEx(COINIT_APARTMENTTHREADED);
            Microsoft::WRL::ComPtr<ITerminalProtocolEventSink> proxy;
            THROW_IF_FAILED(CoGetInterfaceAndReleaseStream(marshaled, IID_PPV_ARGS(&proxy)));
            wil::unique_bstr message{ SysAllocString(L"package-local callback") };
            THROW_IF_NULL_ALLOC(message);
            callbackResult = proxy->OnEvent(message.get());
        }
        catch (...)
        {
            callbackResult = wil::ResultFromCaughtException();
        }
    });
    worker.join(); // The test runner imposes a process deadline.
    THROW_IF_FAILED(callbackResult);
    Check(sink->received, "Callback crossed apartments through the local proxy");
    THROW_IF_FAILED(registration.Unregister());
    THROW_IF_FAILED(registration.Unregister());
    std::puts("PASS: package policy, failure handling, local factory, IID mappings, callback marshaling");
    return 0;
}
catch (...)
{
    std::fprintf(stderr, "FAIL: 0x%08X\n", static_cast<unsigned>(wil::ResultFromCaughtException()));
    return 1;
}
