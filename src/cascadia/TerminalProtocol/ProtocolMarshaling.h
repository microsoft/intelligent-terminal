// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <objbase.h>
#include <objidl.h>
#include <string>
#include <utility>
#include <wil/com.h>
#include <wil/resource.h>
#include <wil/stl.h>
#include <wil/win32_helpers.h>

#include "ITerminalProtocol.h"

struct tagProxyFileInfo;

namespace Microsoft::Terminal::Protocol
{
    class ScopedMarshaling
    {
    public:
        [[nodiscard]] HRESULT InitializeForElevatedProcess() noexcept
        try
        {
            if (_registration)
            {
                return S_OK;
            }

            wil::unique_handle token;
            RETURN_IF_WIN32_BOOL_FALSE(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, token.put()));
            TOKEN_ELEVATION elevation{};
            DWORD returnedSize{};
            RETURN_IF_WIN32_BOOL_FALSE(GetTokenInformation(token.get(), TokenElevation, &elevation, sizeof(elevation), &returnedSize));
            if (!elevation.TokenIsElevated)
            {
                return S_OK;
            }

            auto path = wil::GetModuleFileNameW<std::wstring>();
            const auto separator = path.find_last_of(L'\\');
            RETURN_HR_IF(HRESULT_FROM_WIN32(ERROR_BAD_PATHNAME), separator == std::wstring::npos);
            path.resize(separator + 1);
            path.append(L"OpenConsoleProxy.dll");
            wil::unique_hmodule module{ LoadLibraryExW(path.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
            RETURN_LAST_ERROR_IF_NULL(module);

            using GetProxyDllInfoFn = void(STDAPICALLTYPE*)(const tagProxyFileInfo***, const CLSID**);
            using GetClassObjectFn = HRESULT(STDAPICALLTYPE*)(REFCLSID, REFIID, void**);
            const auto getInfo = reinterpret_cast<GetProxyDllInfoFn>(GetProcAddress(module.get(), "GetProxyDllInfo"));
            RETURN_LAST_ERROR_IF_NULL(getInfo);
            const auto getClassObject = reinterpret_cast<GetClassObjectFn>(GetProcAddress(module.get(), "DllGetClassObject"));
            RETURN_LAST_ERROR_IF_NULL(getClassObject);

            const tagProxyFileInfo** proxyFiles{};
            const CLSID* proxyClsid{};
            getInfo(&proxyFiles, &proxyClsid);
            RETURN_HR_IF(E_UNEXPECTED, !proxyFiles || !proxyClsid);

            wil::com_ptr<IPSFactoryBuffer> factory;
            RETURN_IF_FAILED(getClassObject(*proxyClsid, __uuidof(IPSFactoryBuffer), factory.put_void()));

            // COM can retain proxies after this registration's scope ends.
            HMODULE pinnedModule{};
            RETURN_IF_WIN32_BOOL_FALSE(GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, path.c_str(), &pinnedModule));

            wil::unique_com_class_object_cookie registration;
            RETURN_IF_FAILED(CoRegisterClassObject(*proxyClsid, factory.get(), CLSCTX_INPROC_SERVER, REGCLS_MULTIPLEUSE, registration.put()));
            RETURN_IF_FAILED(CoRegisterPSClsid(__uuidof(ITerminalProtocol), *proxyClsid));
            RETURN_IF_FAILED(CoRegisterPSClsid(__uuidof(ITerminalProtocolEventSink), *proxyClsid));
            _registration = std::move(registration);
            return S_OK;
        }
        CATCH_RETURN()

    private:
        wil::unique_com_class_object_cookie _registration;
    };
}
