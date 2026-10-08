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

namespace TokenFixture
{
    static bool enabled = false;
    static DWORD elevated = 0;
    static DWORD openError = ERROR_SUCCESS;
    static DWORD queryError = ERROR_SUCCESS;
    static unsigned openCalls = 0;
    static unsigned queryCalls = 0;

    static BOOL WINAPI Open(HANDLE process, DWORD access, PHANDLE token)
    {
        if (enabled)
        {
            ++openCalls;
            if (openError)
            {
                SetLastError(openError);
                return FALSE;
            }
        }
        // WIL owns and closes a real token, even under injection.
        return OpenProcessToken(process, access, token);
    }

    static BOOL WINAPI Query(HANDLE token, TOKEN_INFORMATION_CLASS kind, LPVOID value, DWORD size, PDWORD returned)
    {
        if (enabled)
        {
            ++queryCalls;
            if (queryError)
            {
                SetLastError(queryError);
                return FALSE;
            }
        }
        const auto result = GetTokenInformation(token, kind, value, size, returned);
        if (result && enabled && kind == TokenElevation)
        {
            static_cast<TOKEN_ELEVATION*>(value)->TokenIsElevated = elevated;
        }
        return result;
    }
}

namespace RegistrationFixture
{
    static bool enabled = false;
    static bool failClass = false;
    static size_t failMapping = 0;
    static unsigned classCalls = 0;
    static unsigned revokeCalls = 0;
    static DWORD revokedCookie = 0;
    static constexpr DWORD Cookie = 42;
    static std::vector<IID> mappings;

    static HRESULT WINAPI RegisterClass(REFCLSID clsid, LPUNKNOWN factory, DWORD context, DWORD flags, LPDWORD cookie)
    {
        if (!enabled)
        {
            return CoRegisterClassObject(clsid, factory, context, flags, cookie);
        }
        ++classCalls;
        Microsoft::WRL::ComPtr<IPSFactoryBuffer> buffer;
        RETURN_IF_FAILED(factory->QueryInterface(IID_PPV_ARGS(buffer.GetAddressOf())));
        *cookie = failClass ? 0 : Cookie;
        return failClass ? E_OUTOFMEMORY : S_OK;
    }

    static HRESULT WINAPI RegisterMapping(REFIID iid, REFCLSID clsid)
    {
        if (!enabled)
        {
            return CoRegisterPSClsid(iid, clsid);
        }
        mappings.push_back(iid);
        return mappings.size() == failMapping ? E_OUTOFMEMORY : S_OK;
    }

    static HRESULT WINAPI Revoke(DWORD cookie)
    {
        if (!enabled)
        {
            return CoRevokeClassObject(cookie);
        }
        ++revokeCalls;
        revokedCookie = cookie;
        return S_OK;
    }
}

// Substitute only API boundaries. Path construction, sibling checks,
// LoadLibraryEx, and module verification are the actual production code.
#define GetCurrentPackageFullName PackageFixture::FullName
#define GetCurrentPackagePath PackageFixture::Path
#define OpenProcessToken TokenFixture::Open
#define GetTokenInformation TokenFixture::Query
#define CoRegisterClassObject RegistrationFixture::RegisterClass
#define CoRegisterPSClsid RegistrationFixture::RegisterMapping
#define CoRevokeClassObject RegistrationFixture::Revoke
#include "../../../cascadia/inc/TerminalProtocolProxyRegistration.h"
#undef CoRevokeClassObject
#undef CoRegisterPSClsid
#undef CoRegisterClassObject
#undef GetTokenInformation
#undef OpenProcessToken
#undef GetCurrentPackagePath
#undef GetCurrentPackageFullName
#include "../../../cascadia/TerminalProtocol/ProtocolMarshaling.h"

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
    const auto elevated = Protocol::details::IsProcessElevated();
    std::printf("Actual process elevation: %s\n", elevated ? "yes" : "no");
    if (dev || elevated)
    {
        Check(Protocol::details::GetTrustedProxyPath() == Protocol::details::GetExecutableLocalProxyPath(), "Dev or elevated unpackaged process uses its own sibling path");
        wil::unique_hmodule output;
        THROW_IF_FAILED(Protocol::LoadAndVerifyLocalProxyDll(output));
        Check(!!output, "Allowed unpackaged process loads its sibling DLL");
    }
    else
    {
        wil::unique_hmodule output{ LoadLibraryExW(L"version.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32) };
        THROW_LAST_ERROR_IF_NULL(output);
        Check(Protocol::LoadAndVerifyLocalProxyDll(output) == HRESULT_FROM_WIN32(APPMODEL_ERROR_NO_PACKAGE), "Non-elevated unpackaged production must fail before loading");
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
        PackageFixture::identityError = APPMODEL_ERROR_NO_PACKAGE;
        verifyLoad(dev || elevated ? S_OK : HRESULT_FROM_WIN32(APPMODEL_ERROR_NO_PACKAGE));
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

        TokenFixture::enabled = true;
        const auto resetToken = wil::scope_exit([]() noexcept {
            TokenFixture::enabled = false;
            TokenFixture::openError = TokenFixture::queryError = ERROR_SUCCESS;
        });
        auto verifyPolicy = [&](LONG identity, DWORD elevation, DWORD openError, DWORD queryError,
                                HRESULT expected, unsigned opens, unsigned queries) {
            PackageFixture::identityError = identity;
            TokenFixture::elevated = elevation;
            TokenFixture::openError = openError;
            TokenFixture::queryError = queryError;
            TokenFixture::openCalls = TokenFixture::queryCalls = 0;
            verifyLoad(expected);
            Check(TokenFixture::openCalls == opens && TokenFixture::queryCalls == queries, "Elevation API call ledger");
        };
        const auto calls = dev ? 0u : 1u;
        verifyPolicy(APPMODEL_ERROR_NO_PACKAGE, 1, 0, 0, S_OK, calls, calls);
        verifyPolicy(APPMODEL_ERROR_NO_PACKAGE, 0, 0, 0, dev ? S_OK : HRESULT_FROM_WIN32(APPMODEL_ERROR_NO_PACKAGE), calls, calls);
        verifyPolicy(APPMODEL_ERROR_NO_PACKAGE, 1, ERROR_ACCESS_DENIED, 0, dev ? S_OK : E_ACCESSDENIED, calls, 0);
        verifyPolicy(APPMODEL_ERROR_NO_PACKAGE, 1, 0, ERROR_INVALID_DATA, dev ? S_OK : HRESULT_FROM_WIN32(ERROR_INVALID_DATA), calls, calls);
        verifyPolicy(ERROR_SUCCESS, 1, ERROR_ACCESS_DENIED, ERROR_INVALID_DATA, S_OK, 0, 0);
        verifyPolicy(ERROR_BAD_ENVIRONMENT, 1, ERROR_ACCESS_DENIED, ERROR_INVALID_DATA, HRESULT_FROM_WIN32(ERROR_BAD_ENVIRONMENT), 0, 0);
        PackageFixture::probeError = ERROR_INVALID_DATA;
        verifyPolicy(ERROR_SUCCESS, 1, ERROR_ACCESS_DENIED, ERROR_INVALID_DATA, HRESULT_FROM_WIN32(ERROR_INVALID_DATA), 0, 0);
        PackageFixture::probeError = ERROR_SUCCESS;
        PackageFixture::readError = ERROR_SHARING_VIOLATION;
        verifyPolicy(ERROR_SUCCESS, 1, ERROR_ACCESS_DENIED, ERROR_INVALID_DATA, HRESULT_FROM_WIN32(ERROR_SHARING_VIOLATION), 0, 0);
        PackageFixture::readError = ERROR_SUCCESS;
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
    {
        using namespace RegistrationFixture;
        enabled = true;
        const auto resetRegistration = wil::scope_exit([]() noexcept { enabled = false; });
        Check(Protocol::details::ProxyInterfaces.size() == 8, "Eight protocol and handoff mappings");
        for (const size_t failure : { size_t{ 0 }, size_t{ 1 }, size_t{ 4 }, size_t{ 8 } })
        {
            Protocol::details::ProxyRegistration attempt;
            classCalls = revokeCalls = 0;
            mappings.clear();
            failClass = failure == 0;
            failMapping = failure;
            Check(attempt.Register(module.get()) == E_OUTOFMEMORY, "Injected registration failure propagates");
            Check(classCalls == 1 && mappings.size() == failure, "Registration stops at failing API");
            Check(revokeCalls == (failClass ? 0u : 1u), "Mapping failure attempts factory revocation");
            Check(failClass || revokedCookie == Cookie, "Revoke uses returned class cookie");
            const auto previousRevokes = revokeCalls;
            THROW_IF_FAILED(attempt.Unregister());
            Check(revokeCalls == previousRevokes, "Failed registration commits no success cookie");
            failClass = false;
            failMapping = 0;
            mappings.clear();
            THROW_IF_FAILED(attempt.Register(module.get()));
            Check(classCalls == 2 && mappings.size() == 8, "Retry registers factory and all eight mappings");
            Check(std::equal(mappings.begin(), mappings.end(), Protocol::details::ProxyInterfaces.begin()), "Retry maps every production IID");
            THROW_IF_FAILED(attempt.Register(module.get()));
            Check(classCalls == 2 && mappings.size() == 8, "Successful registration is idempotent");
            THROW_IF_FAILED(attempt.Unregister());
            Check(revokeCalls == previousRevokes + 1, "Successful retry owns its cookie");
        }
        classCalls = revokeCalls = 0;
        mappings.clear();
        failMapping = 4;
        {
            Protocol::ScopedMarshaling scoped;
            Check(scoped.InitializeFromExecutableDirectory() == E_OUTOFMEMORY, "Scoped initialization propagates failure");
            failMapping = 0;
            mappings.clear();
            THROW_IF_FAILED(scoped.InitializeFromExecutableDirectory());
            Check(classCalls == 2 && mappings.size() == 8, "Failed scoped initialization remains retryable");
            THROW_IF_FAILED(scoped.InitializeFromExecutableDirectory());
            Check(classCalls == 2, "Successful scoped initialization is cached");
        }
        Check(revokeCalls == 2, "Scoped failure and successful destruction each revoke");
    }
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
    std::puts("PASS: package and injected token policy, registration failure/retry ledger, scoped retry, local factory, exact metadata IID set, sentinel overrides, callback and native agent marshaling");
    return 0;
}
catch (...)
{
    std::fprintf(stderr, "FAIL: 0x%08X\n", static_cast<unsigned>(wil::ResultFromCaughtException()));
    return 1;
}
