// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../inc/WtaProcess.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class ProtocolMarshalingTests
    {
        TEST_CLASS(ProtocolMarshalingTests);
        TEST_METHOD(AllInterfacesUseExecutableAdjacentProxy);
        TEST_METHOD(MissingExecutableAdjacentProxyFails);
        TEST_METHOD(ProductionElevationGateMatchesProcessToken);

        void _runProbe(const bool missing, const wchar_t* arguments);
    };

    void ProtocolMarshalingTests::_runProbe(const bool missing, const wchar_t* arguments)
    {
        // GetModuleFileName(nullptr) in the helper refers to this child EXE,
        // not TE.exe or the test DLL. Each child also isolates COM mappings.
        HMODULE testModule{};
        // Resolve the fixtures relative to this test DLL, even with a root TE.exe.
        VERIFY_WIN32_BOOL_SUCCEEDED(GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                                                       L"Terminal.App.Unit.Tests.dll",
                                                       &testModule));
        const auto root = std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>(testModule) }.parent_path() / L"ProtocolMarshaling";
        const auto directory = root / (missing ? L"Missing" : L"Present");
        const auto executable = directory / L"ProtocolMarshalingProbe.exe";
        VERIFY_IS_TRUE(std::filesystem::exists(executable));
        VERIFY_ARE_EQUAL(!missing, std::filesystem::exists(directory / L"OpenConsoleProxy.dll"));
        const auto result = Microsoft::Terminal::WtaProcess::RunWtaCapture(executable.wstring(), arguments, 10000);
        WEX::Logging::Log::Comment(WEX::Common::String(result.output.c_str()));
        VERIFY_IS_TRUE(result.completed);
        VERIFY_IS_FALSE(result.timedOut);
        VERIFY_ARE_EQUAL(0u, result.exitCode);
    }

    void ProtocolMarshalingTests::AllInterfacesUseExecutableAdjacentProxy()
    {
        _runProbe(false, L"");
    }

    void ProtocolMarshalingTests::MissingExecutableAdjacentProxyFails()
    {
        _runProbe(true, L"--missing");
    }

    void ProtocolMarshalingTests::ProductionElevationGateMatchesProcessToken()
    {
        _runProbe(true, L"--gate");
    }
}
