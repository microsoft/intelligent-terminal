// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <windows.h>
#include <oleauto.h>

#include <filesystem>
#include <initializer_list>

#include <wil/com.h>
#include <wil/resource.h>
#include <wil/win32_helpers.h>

// The exported proxy metadata is opaque here; rpcproxy.h requires C-style COM.
struct tagProxyFileInfo;

namespace Microsoft::Terminal::Protocol::Activation
{
    // Keep the registration alive until all protocol proxies/stubs have been
    // released, and destroy it before the owning COM apartment shuts down.
    // Host/client bootstrap is one-shot: neither activates protocol objects
    // after initialization fails.
    class ProxyRegistration
    {
    public:
        [[nodiscard]] HRESULT Initialize(const std::initializer_list<IID> interfaces,
                                         std::filesystem::path path = {}) noexcept
        try
        {
            RETURN_HR_IF(E_UNEXPECTED, _registration.get() != 0);
            if (path.empty())
            {
                const auto executable = wil::GetModuleFileNameW<wil::unique_cotaskmem_string>(nullptr);
                path = std::filesystem::path{ executable.get() }.parent_path() / L"OpenConsoleProxy.dll";
            }

            wil::unique_hmodule module{ LoadLibraryExW(path.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
            RETURN_LAST_ERROR_IF_NULL(module);

            using GetProxyInfo = void(STDAPICALLTYPE*)(const tagProxyFileInfo***, const CLSID**);
            using GetClassObject = HRESULT(STDAPICALLTYPE*)(REFCLSID, REFIID, void**);
            const auto getInfo = reinterpret_cast<GetProxyInfo>(GetProcAddress(module.get(), "GetProxyDllInfo"));
            RETURN_LAST_ERROR_IF_NULL(getInfo);
            const auto getClassObject = reinterpret_cast<GetClassObject>(GetProcAddress(module.get(), "DllGetClassObject"));
            RETURN_LAST_ERROR_IF_NULL(getClassObject);

            const tagProxyFileInfo** files{};
            const CLSID* proxyClsid{};
            getInfo(&files, &proxyClsid);
            RETURN_HR_IF(HRESULT_FROM_WIN32(ERROR_BAD_FORMAT), !proxyClsid);

            wil::com_ptr<IUnknown> factory;
            RETURN_IF_FAILED(getClassObject(*proxyClsid, IID_PPV_ARGS(factory.put())));
            wil::unique_com_class_object_cookie registration;
            RETURN_IF_FAILED(CoRegisterClassObject(*proxyClsid, factory.get(), CLSCTX_INPROC_SERVER, REGCLS_MULTIPLEUSE, registration.put()));
            for (const auto& iid : interfaces)
            {
                RETURN_IF_FAILED(CoRegisterPSClsid(iid, *proxyClsid));
            }

            // COM may retain marshaled objects after factory revocation during
            // shutdown. Their method pointers must remain valid until process exit.
            HMODULE pinnedModule{};
            RETURN_IF_WIN32_BOOL_FALSE(GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN, path.c_str(), &pinnedModule));
            _module = std::move(module);
            _registration = std::move(registration);
            return S_OK;
        }
        CATCH_RETURN()

    private:
        wil::unique_hmodule _module;
        wil::unique_com_class_object_cookie _registration;
    };

    // Packaged COM class lookup is scoped by application identity when elevated.
    // The ROT exposes a live factory across those identities, while retaining
    // Windows' user, desktop, and integrity-level access checks. Never use
    // ROTFLAGS_ALLOWANYCLIENT or relax COM access permissions here.
    class RunningFactoryRegistration
    {
    public:
        RunningFactoryRegistration() = default;
        RunningFactoryRegistration(const RunningFactoryRegistration&) = delete;
        RunningFactoryRegistration& operator=(const RunningFactoryRegistration&) = delete;

        ~RunningFactoryRegistration()
        {
            if (_cookie)
            {
                LOG_IF_FAILED(RevokeActiveObject(_cookie, nullptr));
            }
        }

        [[nodiscard]] HRESULT Initialize(IClassFactory* factory) noexcept
        {
            RETURN_HR_IF(E_UNEXPECTED, _cookie != 0);
            RETURN_HR_IF_NULL(E_POINTER, factory);
            RETURN_IF_FAILED(CoCreateGuid(&_clsid));
            return RegisterActiveObject(factory, _clsid, ACTIVEOBJECT_STRONG, &_cookie);
        }

        [[nodiscard]] const GUID& Clsid() const noexcept
        {
            return _clsid;
        }

    private:
        GUID _clsid{};
        DWORD _cookie{};
    };

    [[nodiscard]] inline HRESULT CreateInstance(REFCLSID clsid, REFIID iid, void** result) noexcept
    {
        RETURN_HR_IF_NULL(E_POINTER, result);
        *result = nullptr;

        // Prefer the registered class over any active object under the same
        // CLSID. Only an unregistered class may use the live endpoint path.
        const auto activationHr = CoCreateInstance(clsid, nullptr, CLSCTX_LOCAL_SERVER, iid, result);
        if (activationHr != REGDB_E_CLASSNOTREG)
        {
            return activationHr;
        }

        wil::com_ptr<IUnknown> object;
        const auto hr = GetActiveObject(clsid, nullptr, object.put());
        if (hr == MK_E_UNAVAILABLE)
        {
            return activationHr;
        }
        RETURN_IF_FAILED(hr);
        wil::com_ptr<IClassFactory> factory;
        RETURN_IF_FAILED(object->QueryInterface(IID_PPV_ARGS(factory.put())));
        return factory->CreateInstance(nullptr, iid, result);
    }
}
