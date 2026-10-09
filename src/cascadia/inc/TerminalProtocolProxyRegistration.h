// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <objbase.h>
#include <objidl.h>
#include <appmodel.h>
#include "ITerminalProtocol.h"
#include "ITerminalHandoff.h"
#include "IConsoleHandoff.h"

#include <array>
#include <mutex>
#include <string>

#include <wil/stl.h>
#include <wil/win32_helpers.h>
#include <wil/resource.h>
#include <wil/result.h>
#include <wrl/client.h>

struct tagProxyFileInfo;

namespace Microsoft::Terminal::Protocol
{
    namespace details
    {
        using GetProxyDllInfo = void(WINAPI*)(const tagProxyFileInfo***, const CLSID**);
        using DllGetClassObject = HRESULT(STDAPICALLTYPE*)(REFCLSID, REFIID, void**);

        inline constexpr std::array ProxyInterfaces{
            __uuidof(ITerminalProtocol),
            __uuidof(ITerminalProtocolEventSink),
            __uuidof(ITerminalProtocolNativeAgent),
            __uuidof(ITerminalHandoff),
            __uuidof(ITerminalHandoff2),
            __uuidof(ITerminalHandoff3),
            __uuidof(IConsoleHandoff),
            __uuidof(IDefaultTerminalMarker)
        };

#if defined(WT_BRANDING_RELEASE) || defined(WT_BRANDING_PREVIEW) || defined(WT_BRANDING_CANARY)
        inline constexpr bool AllowDevelopmentProxy = false;
#else
        inline constexpr bool AllowDevelopmentProxy = true;
#endif

        [[nodiscard]] inline std::wstring GetExecutableLocalProxyPath()
        {
            auto executablePath = wil::GetModuleFileNameW<std::wstring>(nullptr);
            const auto filenameOffset = executablePath.find_last_of(L"\\/");
            THROW_HR_IF(E_UNEXPECTED, filenameOffset == std::wstring::npos);
            executablePath.resize(filenameOffset + 1);
            executablePath.append(L"OpenConsoleProxy.dll");
            return executablePath;
        }

        [[nodiscard]] inline bool IsProcessElevated()
        {
            wil::unique_handle token;
            THROW_IF_WIN32_BOOL_FALSE(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, token.put()));
            TOKEN_ELEVATION elevation{};
            DWORD returnedSize{};
            THROW_IF_WIN32_BOOL_FALSE(GetTokenInformation(token.get(), TokenElevation, &elevation, sizeof(elevation), &returnedSize));
            return elevation.TokenIsElevated != 0;
        }

        [[nodiscard]] inline std::wstring GetTrustedProxyPath()
        {
            UINT32 length = 0;
            const auto identityResult = GetCurrentPackageFullName(&length, nullptr);
            // Elevated production clients can lack package identity. Only that
            // specific result permits a sibling DLL; other API errors fail closed.
            if (identityResult == APPMODEL_ERROR_NO_PACKAGE && (AllowDevelopmentProxy || IsProcessElevated()))
            {
                return GetExecutableLocalProxyPath();
            }
            THROW_WIN32_IF(identityResult, identityResult != ERROR_INSUFFICIENT_BUFFER);

            // Use the current package's original installation, inheriting its
            // existing signing guarantees (including unsigned Dev deployment).
            length = 0;
            const auto pathResult = GetCurrentPackagePath(&length, nullptr);
            THROW_WIN32_IF(pathResult, pathResult != ERROR_INSUFFICIENT_BUFFER);
            THROW_HR_IF(E_UNEXPECTED, length < 2);
            std::wstring expectedPath(length, L'\0');
            THROW_IF_WIN32_ERROR(GetCurrentPackagePath(&length, expectedPath.data()));
            expectedPath.resize(length - 1);
            expectedPath.append(L"\\OpenConsoleProxy.dll");
            // Reject external-location/sparse-package executables as well.
            const auto executableLocalPath = GetExecutableLocalProxyPath();
            THROW_HR_IF(E_ACCESSDENIED,
                        CompareStringOrdinal(expectedPath.c_str(), -1, executableLocalPath.c_str(), -1, TRUE) != CSTR_EQUAL);
            return expectedPath;
        }

        [[nodiscard]] inline wil::unique_hmodule LoadAndVerifyProxyDll(const std::wstring& expectedPath)
        {
            wil::unique_hmodule module{ LoadLibraryExW(expectedPath.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
            THROW_LAST_ERROR_IF_NULL(module);

            // Check the returned module, not just the path we asked Windows to load.
            const auto actualLoadedPath = wil::GetModuleFileNameW<std::wstring>(module.get());
            THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_INVALID_DLL),
                        CompareStringOrdinal(expectedPath.c_str(), -1, actualLoadedPath.c_str(), -1, TRUE) != CSTR_EQUAL);
            return module;
        }

        class ProxyRegistration
        {
        public:
            [[nodiscard]] HRESULT Register(HMODULE module) noexcept
            try
            {
                RETURN_HR_IF(E_INVALIDARG, !module);
                std::lock_guard lock{ _mutex };
                if (_cookie)
                {
                    return S_OK;
                }

                const auto getProxyDllInfo = reinterpret_cast<GetProxyDllInfo>(GetProcAddress(module, "GetProxyDllInfo"));
                RETURN_LAST_ERROR_IF_NULL(getProxyDllInfo);
                const auto dllGetClassObject = reinterpret_cast<DllGetClassObject>(GetProcAddress(module, "DllGetClassObject"));
                RETURN_LAST_ERROR_IF_NULL(dllGetClassObject);

                const tagProxyFileInfo** proxyFileList = nullptr;
                const CLSID* proxyClsid = nullptr;
                getProxyDllInfo(&proxyFileList, &proxyClsid);
                RETURN_HR_IF(E_UNEXPECTED, !proxyFileList || !proxyClsid);

                Microsoft::WRL::ComPtr<IPSFactoryBuffer> factory;
                RETURN_IF_FAILED(dllGetClassObject(*proxyClsid, IID_PPV_ARGS(factory.GetAddressOf())));

                // Revoking the factory does not destroy outstanding COM proxies.
                // Keep their code loaded until process exit, including after shutdown.
                HMODULE pinnedModule = nullptr;
                RETURN_IF_WIN32_BOOL_FALSE(GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                    reinterpret_cast<LPCWSTR>(module),
                    &pinnedModule));

                DWORD cookie = 0;
                RETURN_IF_FAILED(CoRegisterClassObject(
                    *proxyClsid,
                    factory.Get(),
                    CLSCTX_INPROC_SERVER,
                    REGCLS_MULTIPLEUSE,
                    &cookie));

                auto revokeOnFailure = wil::scope_exit([&]() noexcept {
                    LOG_IF_FAILED(CoRevokeClassObject(cookie));
                });
                // Handoff uses this same DLL. Leaving its IIDs ambient would
                // allow another package's proxy to be loaded in this process.
                for (const auto& iid : ProxyInterfaces)
                {
                    RETURN_IF_FAILED(CoRegisterPSClsid(iid, *proxyClsid));
                }

                _cookie = cookie;
                revokeOnFailure.release();
                return S_OK;
            }
            CATCH_RETURN()

            [[nodiscard]] HRESULT Unregister() noexcept
            {
                std::lock_guard lock{ _mutex };
                if (!_cookie)
                {
                    return S_OK;
                }

                const auto hr = CoRevokeClassObject(_cookie);
                if (SUCCEEDED(hr))
                {
                    _cookie = 0;
                }
                return hr;
            }

        private:
            std::mutex _mutex;
            DWORD _cookie = 0;
        };

        inline ProxyRegistration& ProxyRegistrationInstance() noexcept
        {
            static ProxyRegistration registration;
            return registration;
        }
    }

    // Packaged execution uses the current package's DLL, not another publisher's
    // or version's registration. Dev and actually elevated unpackaged processes
    // also permit their own sibling DLL. No registry/PATH or other-package fallback.
    // On failure the output handle is empty and COM registration is unchanged.
    [[nodiscard]] inline HRESULT LoadAndVerifyLocalProxyDll(wil::unique_hmodule& proxyDll) noexcept
    try
    {
        proxyDll.reset();
        proxyDll = details::LoadAndVerifyProxyDll(details::GetTrustedProxyPath());
        return S_OK;
    }
    CATCH_RETURN()

    // COM retains this DLL's factory and IID mappings process-locally for later
    // marshaling. Pin the DLL so existing proxies survive factory revocation.
    [[nodiscard]] inline HRESULT RegisterProcessLocalProxyFactory(const wil::unique_hmodule& proxyDll) noexcept
    {
        return details::ProxyRegistrationInstance().Register(proxyDll.get());
    }

    [[nodiscard]] inline HRESULT UnregisterTerminalProtocolProxy() noexcept
    {
        return details::ProxyRegistrationInstance().Unregister();
    }
}
