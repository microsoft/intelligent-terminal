// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <windows.h>
#include <cstdio>
#include <cwchar>
#include <filesystem>
#include "../../TerminalProtocol/ProtocolMarshaling.h"

using Microsoft::Terminal::Protocol::ScopedMarshaling;

#define CHECK_RESULT(expression)                                                                  \
    do                                                                                            \
    {                                                                                             \
        const auto result = (expression);                                                         \
        if (FAILED(result))                                                                       \
        {                                                                                         \
            std::printf("%s failed: 0x%08lX\n", #expression, static_cast<unsigned long>(result)); \
            return 1;                                                                             \
        }                                                                                         \
    } while (false)

int wmain(int argc, wchar_t** argv)
{
    CHECK_RESULT(CoInitializeEx(nullptr, COINIT_MULTITHREADED));
    const auto apartment = wil::scope_exit([] { CoUninitialize(); });
    ScopedMarshaling registration;
    if (argc == 2 && std::wcscmp(argv[1], L"--gate") == 0)
    {
        wil::unique_handle token;
        CHECK_RESULT(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, token.put()) ? S_OK : HRESULT_FROM_WIN32(GetLastError()));
        TOKEN_ELEVATION elevation{};
        DWORD returnedSize{};
        CHECK_RESULT(GetTokenInformation(token.get(), TokenElevation, &elevation, sizeof(elevation), &returnedSize) ? S_OK : HRESULT_FROM_WIN32(GetLastError()));
        const IID interfaces[]{ __uuidof(ITerminalProtocol), __uuidof(ITerminalProtocolEventSink) };
        CLSID before[2]{};
        HRESULT beforeResult[2]{};
        for (size_t i = 0; i < ARRAYSIZE(interfaces); ++i)
        {
            beforeResult[i] = CoGetPSClsid(interfaces[i], &before[i]);
        }
        const auto result = registration.InitializeForElevatedProcess();
        if (elevation.TokenIsElevated)
        {
            std::printf("Elevated production gate HRESULT: 0x%08lX\n", static_cast<unsigned long>(result));
            return result == HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND) ? 0 : 1;
        }
        if (result != S_OK || GetModuleHandleW(L"OpenConsoleProxy.dll"))
        {
            return 1;
        }
        for (size_t i = 0; i < ARRAYSIZE(interfaces); ++i)
        {
            CLSID after{};
            const auto afterResult = CoGetPSClsid(interfaces[i], &after);
            if (afterResult != beforeResult[i] || (SUCCEEDED(afterResult) && !IsEqualCLSID(before[i], after)))
            {
                return 1;
            }
        }
        std::puts("Normal production gate preserved both mappings without loading the missing proxy");
        return 0;
    }
    if (argc == 2 && std::wcscmp(argv[1], L"--missing") == 0)
    {
        // Only the child changes CWD. A valid DLL is available there, but the
        // loader must still fail because it is absent beside this executable.
        const auto present = std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>() }.parent_path().parent_path() / L"Present";
        if (!std::filesystem::exists(present / L"OpenConsoleProxy.dll"))
        {
            std::puts("Missing the valid CWD proxy negative control");
            return 1;
        }
        CHECK_RESULT(SetCurrentDirectoryW(present.c_str()) ? S_OK : HRESULT_FROM_WIN32(GetLastError()));
        const auto result = registration.InitializeFromExecutableDirectory();
        std::printf("Missing adjacent DLL HRESULT: 0x%08lX\n", static_cast<unsigned long>(result));
        return result == HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND) ? 0 : 1;
    }
    if (argc != 1)
    {
        return 1;
    }

    wil::com_ptr<IPSFactoryBuffer> retainedFactory;
    {
        ScopedMarshaling scope;
        CHECK_RESULT(scope.InitializeFromExecutableDirectory());
        CHECK_RESULT(scope.InitializeFromExecutableDirectory());
        const auto module = GetModuleHandleW(L"OpenConsoleProxy.dll");
        if (!module)
        {
            return 1;
        }
        using GetProxyDllInfoFn = void(STDAPICALLTYPE*)(const tagProxyFileInfo***, const CLSID**);
        const auto getInfo = reinterpret_cast<GetProxyDllInfoFn>(GetProcAddress(module, "GetProxyDllInfo"));
        if (!getInfo)
        {
            return 1;
        }
        const tagProxyFileInfo** files{};
        const CLSID* expected{};
        getInfo(&files, &expected);
        if (!files || !expected)
        {
            return 1;
        }
        for (const auto iid : { __uuidof(ITerminalProtocol), __uuidof(ITerminalProtocolEventSink) })
        {
            CLSID actual{};
            CHECK_RESULT(CoGetPSClsid(iid, &actual));
            if (!IsEqualCLSID(actual, *expected))
            {
                std::puts("Interface mapped to the wrong branded proxy CLSID");
                return 1;
            }
            wil::com_ptr<IPSFactoryBuffer> factory;
            CHECK_RESULT(CoGetClassObject(actual, CLSCTX_INPROC_SERVER, nullptr, __uuidof(IPSFactoryBuffer), factory.put_void()));
            wil::com_ptr<IRpcStubBuffer> stub;
            CHECK_RESULT(factory->CreateStub(iid, nullptr, stub.put()));
            if (!stub)
            {
                return 1;
            }
            retainedFactory = std::move(factory);
        }
    }
    // A COM object retained past cookie revocation must still have live code.
    wil::com_ptr<IRpcStubBuffer> stub;
    CHECK_RESULT(retainedFactory->CreateStub(__uuidof(ITerminalProtocolEventSink), nullptr, stub.put()));
    std::puts("Both interface mappings, stubs, idempotency and retained factory verified");
    return 0;
}
