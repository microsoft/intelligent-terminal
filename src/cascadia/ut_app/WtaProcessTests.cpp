// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <fstream>
#include <future>

#include "../inc/WtaProcess.h"

using namespace WEX::TestExecution;
namespace Wta = Microsoft::Terminal::WtaProcess;

namespace TerminalAppUnitTests
{
    class WtaProcessTests
    {
        TEST_CLASS(WtaProcessTests);
        TEST_METHOD(CancellationBeforeLaunch);
        TEST_METHOD(CancellationStopsIdleChild);
        TEST_METHOD(CancellationStopsWritingChild);
        TEST_METHOD(TimeoutStillStopsChild);
        TEST_METHOD(CapturePreservesOutputAndExitCode);

        void _cancelChild(bool writeContinuously);
    };

    static std::wstring _systemExecutable(const wchar_t* relativePath)
    {
        wchar_t systemDirectory[MAX_PATH]{};
        const auto length = GetSystemDirectoryW(systemDirectory, ARRAYSIZE(systemDirectory));
        THROW_LAST_ERROR_IF(length == 0);
        THROW_HR_IF(E_UNEXPECTED, length >= ARRAYSIZE(systemDirectory));
        return (std::filesystem::path{ systemDirectory } / relativePath).wstring();
    }

    void WtaProcessTests::CancellationBeforeLaunch()
    {
        std::atomic<bool> cancellation{ true };
        const auto result = Wta::RunWtaCapture(_systemExecutable(L"cmd.exe"), L"/d /c exit 0", 5000, nullptr, true, &cancellation);
        VERIFY_IS_TRUE(result.cancelled);
        VERIFY_IS_FALSE(result.completed);
        VERIFY_IS_TRUE(result.output.empty());
    }

    void WtaProcessTests::_cancelChild(const bool writeContinuously)
    {
        GUID guid{};
        VERIFY_SUCCEEDED(CoCreateGuid(&guid));
        wchar_t id[40]{};
        VERIFY_IS_TRUE(StringFromGUID2(guid, id, ARRAYSIZE(id)) > 0);
        const auto eventName = std::wstring{ L"Local\\WtaCaptureTest-" } + id;
        wil::unique_handle ready{ CreateEventW(nullptr, TRUE, FALSE, eventName.c_str()) };
        VERIFY_IS_NOT_NULL(ready.get());

        wchar_t temp[MAX_PATH]{};
        VERIFY_IS_TRUE(GetTempPathW(ARRAYSIZE(temp), temp) > 0);
        const auto pidPath = std::filesystem::path{ temp } / (std::wstring{ L"WtaCaptureTest-" } + id + L".pid");
        const auto cleanup = wil::scope_exit([&]() {
            if (!DeleteFileW(pidPath.c_str()) && GetLastError() != ERROR_FILE_NOT_FOUND)
            {
                LOG_LAST_ERROR();
            }
        });
        std::wstring quotedPath;
        for (const auto ch : pidPath.wstring())
        {
            quotedPath.push_back(ch);
            if (ch == L'\'')
            {
                quotedPath.push_back(ch);
            }
        }
        const auto script = L"-NoLogo -NoProfile -NonInteractive -Command \""
                            L"[IO.File]::WriteAllText('" +
                            quotedPath + L"', [string]$PID); "
                                         L"$ready = [Threading.EventWaitHandle]::OpenExisting('" +
                            eventName + L"'); " +
                            (writeContinuously ? L"[Console]::Write(('x' * 65536)); " : L"") +
                            L"$ready.Set() | Out-Null; " +
                            (writeContinuously ? L"while ($true) { [Console]::Write(('x' * 65536)) }" :
                                                 L"[Threading.Thread]::Sleep(30000)") +
                            L"\"";

        std::atomic<bool> cancellation{ false };
        wil::unique_handle child;
        DWORD readyWait = WAIT_FAILED;
        DWORD openError = ERROR_SUCCESS;
        auto cancel = std::async(std::launch::async, [&]() {
            readyWait = WaitForSingleObject(ready.get(), 10000);
            if (readyWait == WAIT_OBJECT_0)
            {
                DWORD pid{};
                std::ifstream{ pidPath } >> pid;
                if (pid != 0)
                {
                    child.reset(OpenProcess(SYNCHRONIZE, FALSE, pid));
                    if (!child)
                    {
                        openError = GetLastError();
                    }
                }
            }
            cancellation.store(true);
            return std::chrono::steady_clock::now();
        });
        const auto result = Wta::RunWtaCapture(_systemExecutable(L"WindowsPowerShell\\v1.0\\powershell.exe"),
                                               script,
                                               15000,
                                               nullptr,
                                               true,
                                               &cancellation);
        const auto finished = std::chrono::steady_clock::now();
        const auto cancelledAt = cancel.get();
        VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), readyWait);
        VERIFY_ARE_EQUAL(static_cast<DWORD>(ERROR_SUCCESS), openError);
        VERIFY_IS_NOT_NULL(child.get());
        VERIFY_IS_TRUE(result.cancelled);
        VERIFY_IS_FALSE(result.completed);
        VERIFY_IS_TRUE(result.output.empty());
        VERIFY_IS_TRUE(finished - cancelledAt < std::chrono::seconds{ 3 });
        VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(child.get(), 0));
    }

    void WtaProcessTests::CancellationStopsIdleChild()
    {
        _cancelChild(false);
    }

    void WtaProcessTests::CancellationStopsWritingChild()
    {
        _cancelChild(true);
    }

    void WtaProcessTests::TimeoutStillStopsChild()
    {
        const auto before = std::chrono::steady_clock::now();
        const auto result = Wta::RunWtaCapture(_systemExecutable(L"cmd.exe"), L"/d /q /c for /l %i in (1,0,2) do @echo output", 250);
        const auto duration = std::chrono::steady_clock::now() - before;
        VERIFY_IS_FALSE(result.completed);
        VERIFY_IS_FALSE(result.cancelled);
        VERIFY_IS_TRUE(result.output.empty());
        VERIFY_IS_TRUE(duration < std::chrono::seconds{ 3 });
    }

    void WtaProcessTests::CapturePreservesOutputAndExitCode()
    {
        const auto command = _systemExecutable(L"cmd.exe");
        const auto result = Wta::RunWtaCapture(command, L"/d /c \"echo captured & echo hidden 1>&2 & exit /b 7\"", 5000, nullptr, false);
        VERIFY_IS_TRUE(result.completed);
        VERIFY_IS_FALSE(result.cancelled);
        VERIFY_ARE_EQUAL(DWORD{ 7 }, result.exitCode);
        VERIFY_IS_TRUE(result.output.find("captured") != std::string::npos);
        VERIFY_IS_TRUE(result.output.find("hidden") == std::string::npos);
        std::atomic<bool> cancellation{ false };
        const auto merged = Wta::RunWtaCapture(command, L"/d /c \"echo captured & echo stderr 1>&2\"", 5000, nullptr, true, &cancellation);
        VERIFY_IS_TRUE(merged.completed);
        VERIFY_IS_FALSE(merged.cancelled);
        VERIFY_ARE_EQUAL(DWORD{ 0 }, merged.exitCode);
        VERIFY_IS_TRUE(merged.output.find("stderr") != std::string::npos);
    }
}
