// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxSessionQueryTests.cpp
//
// Abstract:
// - One-shot query output, diagnostics, bounds, cancellation and launch tests.

#include "precomp.h"

#include "../TerminalApp/TmuxSessionQuery.h"

#include <algorithm>
#include <chrono>
#include <memory>
#include <string>
#include <string_view>

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;
using namespace std::chrono_literals;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::wstring SystemDirectory()
        {
            std::wstring path(32768, L'\0');
            const auto length = GetSystemDirectoryW(path.data(), static_cast<UINT>(path.size()));
            THROW_LAST_ERROR_IF(length == 0 || length >= path.size());
            path.resize(length);
            return path;
        }

        std::wstring Cmd(const std::wstring_view script)
        {
            return L"\"" + SystemDirectory() + L"\\cmd.exe\" /d /s /c \"" + std::wstring{ script } + L"\"";
        }

        std::wstring PowerShell(const std::wstring_view script)
        {
            return L"\"" + SystemDirectory() +
                   L"\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -Command \"" + std::wstring{ script } + L"\"";
        }

        winrt::hstring Query(const std::wstring_view commandline)
        {
            return QuerySessionListAsync(winrt::hstring{ commandline }, winrt::hstring{ SystemDirectory() }).get();
        }

        winrt::hresult_error QueryFailure(const std::wstring_view commandline)
        {
            try
            {
                Query(commandline);
                VERIFY_FAIL(L"Expected the session query to fail.");
            }
            catch (const winrt::hresult_error& error)
            {
                return error;
            }
            return winrt::hresult_error{ E_UNEXPECTED };
        }
    }

    class TmuxSessionQueryTests
    {
        TEST_CLASS(TmuxSessionQueryTests);

        TEST_METHOD(ReturnsCompleteUtf8OutputWithoutStderr);
        TEST_METHOD(UsesExplicitWorkingDirectory);
        TEST_METHOD(ReportsNonzeroExitAndDiagnostic);
        TEST_METHOD(RecognizesOnlyMissingDefaultServer);
        TEST_METHOD(RejectsOtherServerAndSshFailures);
        TEST_METHOD(AcceptsExactOutputBounds);
        TEST_METHOD(ReportsOutputAndErrorOverflow);
        TEST_METHOD(RejectsMalformedUtf8);
        TEST_METHOD(CancelDoesNotJoinOnCallerThread);
        TEST_METHOD(ImmediateAndStaleCancellationAreSafe);
        TEST_METHOD(ReportsLaunchFailure);
    };

    void TmuxSessionQueryTests::ReturnsCompleteUtf8OutputWithoutStderr()
    {
        const auto output = Query(PowerShell(
            L"$b = [Text.Encoding]::UTF8.GetBytes('$0 main' + [char]10 + '$42 \u5f00\u53d1 \U0001f680' + [char]10); "
            L"$s = [Console]::OpenStandardOutput(); foreach ($c in $b) { $s.WriteByte($c) }; "
            L"[Console]::Error.Write('stderr-sentinel')"));
        VERIFY_ARE_EQUAL(winrt::hstring{ L"$0 main\n$42 \u5f00\u53d1 \U0001f680\n" }, output);
        VERIFY_ARE_EQUAL(winrt::hstring{}, Query(Cmd(L"exit /b 0")));
    }

    void TmuxSessionQueryTests::UsesExplicitWorkingDirectory()
    {
        VERIFY_ARE_EQUAL(winrt::hstring{ L"$0 cwd-ok\r\n" },
                         Query(Cmd(L"if exist cmd.exe (echo $0 cwd-ok) else (exit /b 7)")));
    }

    void TmuxSessionQueryTests::ReportsNonzeroExitAndDiagnostic()
    {
        const auto error = QueryFailure(Cmd(L"echo $0 partial&1>&2 echo stderr-sentinel&exit /b 23"));
        const std::wstring message{ error.message() };
        VERIFY_IS_TRUE(message.find(L"exit code 23") != std::wstring::npos);
        VERIFY_IS_TRUE(message.find(L"stderr-sentinel") != std::wstring::npos);
        VERIFY_IS_TRUE(message.find(L"$0 partial") == std::wstring::npos);

        const auto emptyError = QueryFailure(Cmd(L"exit /b 7"));
        VERIFY_IS_TRUE(std::wstring_view{ emptyError.message() }.find(L"exit code 7") != std::wstring_view::npos);
    }

    void TmuxSessionQueryTests::RecognizesOnlyMissingDefaultServer()
    {
        VERIFY_ARE_EQUAL(winrt::hstring{}, Query(Cmd(L"1>&2 echo no server running on /tmp/tmux-1000/default&exit /b 1")));
        VERIFY_ARE_EQUAL(winrt::hstring{}, Query(Cmd(L"1>&2 echo error connecting to /tmp/tmux-1000/default ^(No such file or directory^)&exit /b 1")));
        VERIFY_ARE_EQUAL(winrt::hstring{}, Query(PowerShell(L"[Console]::Error.Write('no server running on /custom tmux/tmux-42/default' + [char]10); exit 1")));
    }

    void TmuxSessionQueryTests::RejectsOtherServerAndSshFailures()
    {
        for (const auto script : {
                 L"1>&2 echo no server running on /tmp/tmux-1000/named&exit /b 1",
                 L"1>&2 echo no server running on relative/default&exit /b 1",
                 L"1>&2 echo no server running on /tmp/tmux-1000/default-extra&exit /b 1",
                 L"1>&2 echo error connecting to /tmp/tmux-1000/default ^(Permission denied^)&exit /b 1",
                 L"1>&2 echo error connecting to /tmp/tmux-1000/default ^(Connection timed out^)&exit /b 1",
                 L"1>&2 echo ssh: Permission denied ^(publickey^).&exit /b 255",
                 L"1>&2 echo sh: tmux: command not found&exit /b 127",
                 L"1>&2 echo warning&1>&2 echo no server running on /tmp/tmux-1000/default&exit /b 1",
                 L"1>&2 echo no server running on /tmp/tmux-1000/default&1>&2 echo second error&exit /b 1",
                 L"1>&2 echo no server running on /tmp/tmux-1000/default&exit /b 255",
                 L"echo $0 partial&1>&2 echo no server running on /tmp/tmux-1000/default&exit /b 1",
                 L"exit /b 1",
             })
        {
            const auto error = QueryFailure(Cmd(script));
            VERIFY_IS_TRUE(std::wstring_view{ error.message() }.find(L"exit code ") != std::wstring_view::npos);
        }
    }

    void TmuxSessionQueryTests::AcceptsExactOutputBounds()
    {
        const auto output = Query(PowerShell(L"[Console]::Out.Write(('x' * 65536)); [Console]::Error.Write(('y' * 16384))"));
        VERIFY_ARE_EQUAL(uint32_t{ 65536 }, output.size());
        VERIFY_IS_TRUE(std::all_of(output.begin(), output.end(), [](const wchar_t ch) { return ch == L'x'; }));
    }

    void TmuxSessionQueryTests::ReportsOutputAndErrorOverflow()
    {
        const auto outputError = QueryFailure(PowerShell(L"[Console]::Out.Write(('x' * 65537)); [Threading.Thread]::Sleep(60000)"));
        VERIFY_ARE_EQUAL(HRESULT_FROM_WIN32(ERROR_BUFFER_OVERFLOW), static_cast<HRESULT>(outputError.code()));
        VERIFY_IS_TRUE(std::wstring_view{ outputError.message() }.find(L"stdout exceeded 65536") != std::wstring_view::npos);

        const auto errorError = QueryFailure(PowerShell(L"[Console]::Error.Write(('x' * 16385)); [Threading.Thread]::Sleep(60000)"));
        VERIFY_ARE_EQUAL(HRESULT_FROM_WIN32(ERROR_BUFFER_OVERFLOW), static_cast<HRESULT>(errorError.code()));
        VERIFY_IS_TRUE(std::wstring_view{ errorError.message() }.find(L"stderr exceeded 16384") != std::wstring_view::npos);
    }

    void TmuxSessionQueryTests::RejectsMalformedUtf8()
    {
        const auto outputError = QueryFailure(PowerShell(L"$s = [Console]::OpenStandardOutput(); $s.WriteByte(0xc0); $s.WriteByte(0xaf)"));
        VERIFY_ARE_EQUAL(HRESULT_FROM_WIN32(ERROR_NO_UNICODE_TRANSLATION), static_cast<HRESULT>(outputError.code()));
        VERIFY_IS_TRUE(std::wstring_view{ outputError.message() }.find(L"invalid UTF-8 on stdout") != std::wstring_view::npos);

        const auto errorError = QueryFailure(PowerShell(L"[Console]::OpenStandardError().WriteByte(0xff); exit 1"));
        VERIFY_ARE_EQUAL(HRESULT_FROM_WIN32(ERROR_NO_UNICODE_TRANSLATION), static_cast<HRESULT>(errorError.code()));
        VERIFY_IS_TRUE(std::wstring_view{ errorError.message() }.find(L"invalid UTF-8 on stderr") != std::wstring_view::npos);
    }

    void TmuxSessionQueryTests::CancelDoesNotJoinOnCallerThread()
    {
        GUID guid;
        THROW_IF_FAILED(CoCreateGuid(&guid));
        wchar_t guidText[40]{};
        VERIFY_IS_TRUE(StringFromGUID2(guid, guidText, ARRAYSIZE(guidText)) > 0);
        const auto name = std::wstring{ L"Local\\TmuxSessionQueryTests-" } + guidText;
        const wil::unique_handle started{ CreateEventW(nullptr, TRUE, FALSE, name.c_str()) };
        THROW_LAST_ERROR_IF(!started);
        const auto completed = std::make_shared<wil::unique_handle>(CreateEventW(nullptr, TRUE, FALSE, nullptr));
        THROW_LAST_ERROR_IF(!*completed);

        const auto operation = QuerySessionListAsync(
            winrt::hstring{ PowerShell(L"$null = [Threading.EventWaitHandle]::OpenExisting('" + name + L"').Set(); [Threading.Thread]::Sleep(60000)") },
            winrt::hstring{ SystemDirectory() });
        const auto cancelOnExit = wil::scope_exit([&]() noexcept { operation.Cancel(); });
        operation.Completed([completed](const auto&, const auto&) {
            SetEvent(completed->get());
        });
        VERIFY_ARE_EQUAL(DWORD{ WAIT_OBJECT_0 }, WaitForSingleObject(started.get(), 10000));

        const auto cancelStart = std::chrono::steady_clock::now();
        operation.Cancel();
        VERIFY_IS_TRUE(std::chrono::steady_clock::now() - cancelStart < 250ms);
        VERIFY_ARE_EQUAL(DWORD{ WAIT_OBJECT_0 }, WaitForSingleObject(completed->get(), 5000));
        VERIFY_THROWS(operation.GetResults(), winrt::hresult_canceled);
    }

    void TmuxSessionQueryTests::ImmediateAndStaleCancellationAreSafe()
    {
        const auto canceled = QuerySessionListAsync(winrt::hstring{ Cmd(L"set /p wait=") }, winrt::hstring{ SystemDirectory() });
        canceled.Cancel();
        VERIFY_THROWS(canceled.get(), winrt::hresult_canceled);
        canceled.Cancel();

        const auto finished = QuerySessionListAsync(winrt::hstring{ Cmd(L"echo $3 first") }, winrt::hstring{ SystemDirectory() });
        VERIFY_ARE_EQUAL(winrt::hstring{ L"$3 first\r\n" }, finished.get());
        const auto next = QuerySessionListAsync(winrt::hstring{ Cmd(L"echo $4 second") }, winrt::hstring{ SystemDirectory() });
        finished.Cancel();
        canceled.Cancel();
        VERIFY_ARE_EQUAL(winrt::hstring{ L"$4 second\r\n" }, next.get());
    }

    void TmuxSessionQueryTests::ReportsLaunchFailure()
    {
        const auto error = QueryFailure(L"\"" + SystemDirectory() + L"\\TmuxSessionQueryTests-absent-{175C3D82-1CDD-415A-914C-C9923D0B2C78}.exe\"");
        VERIFY_ARE_EQUAL(HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND), static_cast<HRESULT>(error.code()));
        VERIFY_IS_TRUE(std::wstring_view{ error.message() }.find(L"Unable to launch tmux session query") != std::wstring_view::npos);
        VERIFY_THROWS(QuerySessionListAsync(winrt::hstring{ Cmd(L"echo unexpected") }, L"").get(), winrt::hresult_error);
    }
}
