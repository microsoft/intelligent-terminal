// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

// Standalone, unpackaged test executable. No Terminal activation or registration
// changes outside this process. argv[1] is an absolute path to a built proxy DLL.
#include <windows.h>
#include <appmodel.h>
#include <objbase.h>
#include <objidl.h>
#define RPCPROXY_ENABLE_CPP_NO_CINTERFACE
#include <rpcproxy.h>
#include <algorithm>
#include <string>
#include <vector>
#include <wil/stl.h>
#include <wil/win32_helpers.h>
#include <wil/resource.h>
#include <wil/result.h>
#include <wrl/client.h>
#include <atomic>
#include <cstdio>
#include <thread>
#include <wrl/implements.h>

namespace PackageFixture
{
    static bool enabled = false;
    static std::wstring root;
    static LONG identityError = ERROR_SUCCESS;
    static LONG probeError = ERROR_SUCCESS;
    static LONG readError = ERROR_SUCCESS;
    static bool invalidLength = false;

    static LONG WINAPI FullName(UINT32* length, PWSTR value)
    {
        if (!enabled)
        {
            return GetCurrentPackageFullName(length, value);
        }
        *length = 16;
        return identityError ? identityError : ERROR_INSUFFICIENT_BUFFER;
    }

    static LONG WINAPI Path(UINT32* length, PWSTR value)
    {
        if (!enabled)
        {
            return GetCurrentPackagePath(length, value);
        }
        if (const auto error = value ? readError : probeError)
        {
            return error;
        }
        const auto required = static_cast<UINT32>(root.size() + 1);
        if (!value || *length < required)
        {
            *length = invalidLength ? 1 : required;
            return ERROR_INSUFFICIENT_BUFFER;
        }
        wcscpy_s(value, *length, root.c_str());
        *length = required;
        return ERROR_SUCCESS;
    }
}

// Substitute only the package API boundary. Path construction, sibling checks,
// LoadLibraryEx, and module verification are the actual production code.
#define GetCurrentPackageFullName PackageFixture::FullName
#define GetCurrentPackagePath PackageFixture::Path
#include "../../../cascadia/inc/TerminalProtocolProxyRegistration.h"
#undef GetCurrentPackagePath
#undef GetCurrentPackageFullName

namespace Protocol = Microsoft::Terminal::Protocol;

static void Check(const bool condition, const char* message)
{
    if (!condition)
    {
        std::fprintf(stderr, "%s\n", message);
    }
    THROW_HR_IF_MSG(E_UNEXPECTED, !condition, "%hs", message);
}

static constexpr GUID TestSession{ 0x12345678, 0x9abc, 0x4def, { 0x81, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef } };

static bool EqualBstr(BSTR value, const wchar_t* expected) noexcept
{
    return value && SysStringLen(value) == wcslen(expected) && wcscmp(value, expected) == 0;
}

class TestSink final : public Microsoft::WRL::RuntimeClass<Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::ClassicCom>, ITerminalProtocolEventSink, ITerminalProtocolNativeAgent>
{
public:
    std::atomic<bool> received{ false };
    std::atomic<bool> tabReceived{ false };
    std::atomic<bool> splitReceived{ false };

    HRESULT STDMETHODCALLTYPE OnEvent(BSTR text) noexcept override
    {
        received = text && wcscmp(text, L"package-local callback") == 0;
        return received ? S_OK : E_INVALIDARG;
    }

    HRESULT STDMETHODCALLTYPE CreateAgentCliTab(MIDL_uhyper windowId, BSTR profile, BSTR commandline, BSTR title, BSTR startingDirectory,
                                               boolean suppressAppTitle, boolean background, BSTR providerId, BSTR* json) noexcept override
    {
        RETURN_HR_IF_NULL(E_POINTER, json);
        *json = nullptr;
        tabReceived = windowId == 0x123456789abcdef0ULL &&
                      EqualBstr(profile, L"test profile") && EqualBstr(commandline, L"synthetic --argument \"two words\"") &&
                      EqualBstr(title, L"Agent \u03a9") && EqualBstr(startingDirectory, L"C:\\synthetic directory") &&
                      suppressAppTitle == 1 && background == 0 && EqualBstr(providerId, L"copilot");
        RETURN_HR_IF(E_INVALIDARG, !tabReceived);
        *json = SysAllocString(L"{\"synthetic\":\"tab\",\"id\":42}");
        return *json ? S_OK : E_OUTOFMEMORY;
    }

    HRESULT STDMETHODCALLTYPE SplitAgentCliPane(GUID sessionId, BSTR direction, float size, BSTR profile, BSTR commandline,
                                               boolean background, BSTR providerId, BSTR* json) noexcept override
    {
        RETURN_HR_IF_NULL(E_POINTER, json);
        *json = nullptr;
        splitReceived = sessionId == TestSession && EqualBstr(direction, L"right") && size == 0.375f &&
                        EqualBstr(profile, L"test profile") && EqualBstr(commandline, L"synthetic --argument \"two words\"") &&
                        background == 1 && EqualBstr(providerId, L"copilot");
        RETURN_HR_IF(E_INVALIDARG, !splitReceived);
        *json = SysAllocString(L"{\"synthetic\":\"pane\",\"id\":73}");
        return *json ? S_OK : E_OUTOFMEMORY;
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

    {
        PackageFixture::enabled = true;
        const auto resetFixture = wil::scope_exit([]() noexcept { PackageFixture::enabled = false; });
        const auto localProxy = Protocol::details::GetExecutableLocalProxyPath();
        PackageFixture::root = localProxy.substr(0, localProxy.find_last_of(L'\\'));
        auto verifyLoad = [&](const HRESULT expected) {
            wil::unique_hmodule output{ LoadLibraryExW(L"version.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32) };
            THROW_LAST_ERROR_IF_NULL(output);
            Check(Protocol::LoadAndVerifyLocalProxyDll(output) == expected, "Package fixture loader result");
            Check(!!output == SUCCEEDED(expected), "Failed package load clears a prepopulated handle");
            if (output)
            {
                const auto actual = wil::GetModuleFileNameW<std::wstring>(output.get());
                Check(CompareStringOrdinal(localProxy.c_str(), -1, actual.c_str(), -1, TRUE) == CSTR_EQUAL, "Loaded the package fixture's real sibling DLL");
            }
        };
        verifyLoad(S_OK);
        CharUpperBuffW(PackageFixture::root.data(), static_cast<DWORD>(PackageFixture::root.size()));
        verifyLoad(S_OK);
        PackageFixture::root += L"\\other-package";
        verifyLoad(E_ACCESSDENIED);
        PackageFixture::root = localProxy.substr(0, localProxy.find_last_of(L'\\'));
        PackageFixture::identityError = ERROR_BAD_ENVIRONMENT;
        verifyLoad(HRESULT_FROM_WIN32(ERROR_BAD_ENVIRONMENT));
        PackageFixture::identityError = ERROR_SUCCESS;
        PackageFixture::probeError = ERROR_INVALID_DATA;
        verifyLoad(HRESULT_FROM_WIN32(ERROR_INVALID_DATA));
        PackageFixture::probeError = ERROR_SUCCESS;
        PackageFixture::readError = ERROR_SHARING_VIOLATION;
        verifyLoad(HRESULT_FROM_WIN32(ERROR_SHARING_VIOLATION));
        PackageFixture::readError = ERROR_SUCCESS;
        PackageFixture::invalidLength = true;
        verifyLoad(E_UNEXPECTED);
        PackageFixture::invalidLength = false;
        verifyLoad(S_OK);
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
    std::vector<IID> metadataInterfaces;
    for (auto file = files; *file; ++file)
    {
        Check((*file)->pStubVtblList != nullptr, "Proxy metadata contains stub headers");
        for (unsigned short index = 0; index < (*file)->TableSize; ++index)
        {
            const auto stub = (*file)->pStubVtblList[index];
            Check(stub != nullptr, "Proxy metadata contains a stub");
            const auto header = RPCPROXY_GET_STUB_HEADER(stub);
            Check(header->piid != nullptr, "Proxy stub header contains an IID");
            const auto& iid = *header->piid;
            Check(std::find(metadataInterfaces.begin(), metadataInterfaces.end(), iid) == metadataInterfaces.end(), "Proxy metadata IIDs are unique");
            metadataInterfaces.push_back(iid);
        }
    }
    Check(!metadataInterfaces.empty(), "Proxy metadata has interfaces");
    // Process-local poison mappings prevent installed packages from masking an
    // omitted production override. No factory is registered for this fresh CLSID.
    CLSID sentinel{};
    THROW_IF_FAILED(CoCreateGuid(&sentinel));
    Check(sentinel != *clsid, "Sentinel differs from the loaded proxy factory");
    for (const auto& iid : metadataInterfaces)
    {
        THROW_IF_FAILED(CoRegisterPSClsid(iid, sentinel));
        CLSID actual{};
        THROW_IF_FAILED(CoGetPSClsid(iid, &actual));
        Check(actual == sentinel, "Each metadata IID starts with the sentinel mapping");
    }
    std::printf("Proxy metadata interfaces: %zu; production interfaces: %zu\n", metadataInterfaces.size(), Protocol::details::ProxyInterfaces.size());
    Check(metadataInterfaces.size() == Protocol::details::ProxyInterfaces.size(), "Production IID set must exactly match loaded proxy metadata");
    for (const auto& iid : metadataInterfaces)
    {
        Check(std::count(Protocol::details::ProxyInterfaces.begin(), Protocol::details::ProxyInterfaces.end(), iid) == 1,
              "Each loaded proxy IID must occur exactly once in the production list");
    }
    THROW_IF_FAILED(registration.Register(module.get()));
    const auto revoke = wil::scope_exit([&]() noexcept { LOG_IF_FAILED(registration.Unregister()); });
    THROW_IF_FAILED(registration.Register(module.get()));

    for (const auto& iid : metadataInterfaces)
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
    for (const auto& iid : metadataInterfaces)
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
    THROW_IF_FAILED(CoMarshalInterThreadInterfaceInStream(__uuidof(ITerminalProtocolEventSink), static_cast<ITerminalProtocolEventSink*>(sink.Get()), &stream));
    HRESULT callbackResult = E_PENDING;
    std::thread worker([marshaled = stream.Detach(), &callbackResult]() noexcept {
        try
        {
            const auto sta = wil::CoInitializeEx(COINIT_APARTMENTTHREADED);
            Microsoft::WRL::ComPtr<ITerminalProtocolEventSink> proxy;
            THROW_IF_FAILED(CoGetInterfaceAndReleaseStream(marshaled, IID_PPV_ARGS(&proxy)));
            wil::unique_bstr message{ SysAllocString(L"package-local callback") };
            THROW_IF_NULL_ALLOC(message);
            THROW_IF_FAILED(proxy->OnEvent(message.get()));
            Microsoft::WRL::ComPtr<ITerminalProtocolNativeAgent> nativeAgent;
            THROW_IF_FAILED(proxy.As(&nativeAgent));
            const auto allocate = [](const wchar_t* value) {
                wil::unique_bstr result{ SysAllocString(value) };
                THROW_IF_NULL_ALLOC(result);
                return result;
            };
            auto profile = allocate(L"test profile");
            auto commandline = allocate(L"synthetic --argument \"two words\"");
            auto title = allocate(L"Agent \u03a9");
            auto directory = allocate(L"C:\\synthetic directory");
            auto provider = allocate(L"copilot");
            auto direction = allocate(L"right");
            wil::unique_bstr tabJson;
            THROW_IF_FAILED(nativeAgent->CreateAgentCliTab(0x123456789abcdef0ULL, profile.get(), commandline.get(), title.get(),
                                                         directory.get(), 1, 0, provider.get(), tabJson.put()));
            Check(EqualBstr(tabJson.get(), L"{\"synthetic\":\"tab\",\"id\":42}"), "Native agent tab JSON crossed apartments");
            wil::unique_bstr splitJson;
            THROW_IF_FAILED(nativeAgent->SplitAgentCliPane(TestSession, direction.get(), 0.375f, profile.get(), commandline.get(),
                                                         1, provider.get(), splitJson.put()));
            Check(EqualBstr(splitJson.get(), L"{\"synthetic\":\"pane\",\"id\":73}"), "Native agent pane JSON crossed apartments");
            callbackResult = S_OK;
        }
        catch (...)
        {
            callbackResult = wil::ResultFromCaughtException();
        }
    });
    worker.join(); // The test runner imposes a process deadline.
    THROW_IF_FAILED(callbackResult);
    Check(sink->received, "Callback crossed apartments through the local proxy");
    Check(sink->tabReceived && sink->splitReceived, "Both native agent calls crossed apartments with intact arguments");
    THROW_IF_FAILED(registration.Unregister());
    THROW_IF_FAILED(registration.Unregister());
    std::puts("PASS: package policy, failure handling, local factory, exact metadata IID set, sentinel overrides, callback and native agent marshaling");
    return 0;
}
catch (...)
{
    std::fprintf(stderr, "FAIL: 0x%08X\n", static_cast<unsigned>(wil::ResultFromCaughtException()));
    return 1;
}
