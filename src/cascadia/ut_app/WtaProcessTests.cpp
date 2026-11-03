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
        TEST_METHOD(LaunchFailureIsNotTimeout);
        TEST_METHOD(CapturePreservesOutputAndExitCode);
        TEST_METHOD(ManagedResourcesAreOptIn);
        TEST_METHOD(ManagedResourceRetirementRequiresConfirmedJob);
        TEST_METHOD(RetirementClosesFirstResourceAdmission);

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

    static std::wstring _environmentValue(const std::wstring& block, const wchar_t* key)
    {
        const auto prefix = std::wstring{ key } + L"=";
        for (auto entry = block.c_str(); *entry; entry += wcslen(entry) + 1)
        {
            if (_wcsnicmp(entry, prefix.c_str(), prefix.size()) == 0)
            {
                return entry + prefix.size();
            }
        }
        return {};
    }

    void WtaProcessTests::ManagedResourcesAreOptIn()
    {
        Wta::ProcessResourceScope scope;
        auto job = scope.CreateJob();
        VERIFY_IS_NOT_NULL(job.get());
        std::wstring original = L"CUSTOM=preserved";
        original.push_back(L'\0');
        original.append(L"WTA_PROCESS_RESOURCE_SCOPE=caller-cannot-replace-owner");
        original.push_back(L'\0');
        original.push_back(L'\0');
        const auto environment = scope.Environment(original.c_str());
        VERIFY_ARE_EQUAL(std::wstring{ L"preserved" }, _environmentValue(environment, L"CUSTOM"));
        VERIFY_ARE_EQUAL(size_t{ 32 }, _environmentValue(environment, L"WTA_PROCESS_RESOURCE_SCOPE").size());
        wil::unique_handle shutdown{ CreateEventW(nullptr, TRUE, FALSE, nullptr) };
        wil::unique_handle exited{ CreateEventW(nullptr, TRUE, TRUE, nullptr) };
        scope.Attached();
        scope.RequestShutdown(shutdown.get(), exited.get(), 1);
        VERIFY_ARE_EQUAL(DWORD{ WAIT_TIMEOUT }, WaitForSingleObject(shutdown.get(), 0));
        const auto eventName = L"Local\\wta-resources-used-" + _environmentValue(environment, L"WTA_PROCESS_RESOURCE_SCOPE");
        wil::unique_handle requested{ OpenEventW(EVENT_MODIFY_STATE, FALSE, eventName.c_str()) };
        VERIFY_IS_NOT_NULL(requested.get());
        VERIFY_IS_TRUE(SetEvent(requested.get()));
        scope.RequestShutdown(shutdown.get(), exited.get(), 1);
        VERIFY_ARE_EQUAL(DWORD{ WAIT_OBJECT_0 }, WaitForSingleObject(shutdown.get(), 0));
    }

    void WtaProcessTests::RetirementClosesFirstResourceAdmission()
    {
        Wta::ProcessResourceScope scope;
        auto job = scope.CreateJob();
        VERIFY_IS_NOT_NULL(job.get());
        const auto environment = scope.Environment(nullptr);
        const auto id = _environmentValue(environment, L"WTA_PROCESS_RESOURCE_SCOPE");
        scope.Attached();
        VERIFY_IS_FALSE(scope.Retire(job.get()));
        wil::unique_handle ready{ OpenEventW(SYNCHRONIZE, FALSE, (L"Local\\wta-resources-ready-" + id).c_str()) };
        VERIFY_IS_NOT_NULL(ready.get());
        VERIFY_ARE_EQUAL(DWORD{ WAIT_TIMEOUT }, WaitForSingleObject(ready.get(), 0));
    }

    void WtaProcessTests::ManagedResourceRetirementRequiresConfirmedJob()
    {
        Wta::ProcessResourceScope scope;
        auto job = scope.CreateJob();
        VERIFY_IS_NOT_NULL(job.get());
        const auto seed = scope.Environment(nullptr);
        const auto id = _environmentValue(seed, L"WTA_PROCESS_RESOURCE_SCOPE");
        const auto local = std::filesystem::temp_directory_path() / (L"WtaResourceTest-" + id);
        const auto root = IntelligentTerminal::AcpRuntimeRoot(local);
        std::filesystem::create_directories(root);
        const auto removeFixture = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(local, error);
            LOG_HR_IF(E_FAIL, static_cast<bool>(error));
        });
        auto original = L"LOCALAPPDATA=" + local.wstring();
        original.push_back(L'\0');
        original.push_back(L'\0');
        scope.Environment(original.c_str());
        scope.Attached();
        wil::unique_handle requested{ OpenEventW(EVENT_MODIFY_STATE, FALSE, (L"Local\\wta-resources-used-" + id).c_str()) };
        VERIFY_IS_NOT_NULL(requested.get());
        VERIFY_IS_TRUE(SetEvent(requested.get()));
        const auto receipt = root / (L".retired-" + id);
        VERIFY_IS_FALSE(scope.Retire(INVALID_HANDLE_VALUE, 0));
        VERIFY_IS_FALSE(std::filesystem::exists(receipt));
        VERIFY_IS_TRUE(scope.Retire(job.get()));
        VERIFY_IS_TRUE(std::filesystem::is_regular_file(receipt));
        VERIFY_ARE_EQUAL(uintmax_t{ 0 }, std::filesystem::file_size(receipt));
    }

    void WtaProcessTests::CancellationBeforeLaunch()
    {
        std::atomic<bool> cancellation{ true };
        const auto result = Wta::RunWtaCapture(_systemExecutable(L"cmd.exe"), L"/d /c exit 0", 5000, nullptr, true, &cancellation);
        VERIFY_IS_TRUE(result.cancelled);
        VERIFY_IS_FALSE(result.timedOut);
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
        VERIFY_IS_FALSE(result.timedOut);
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
        VERIFY_IS_TRUE(result.timedOut);
        VERIFY_IS_TRUE(result.output.empty());
        VERIFY_IS_TRUE(duration < std::chrono::seconds{ 3 });
    }

    void WtaProcessTests::CapturePreservesOutputAndExitCode()
    {
        const auto command = _systemExecutable(L"cmd.exe");
        const auto result = Wta::RunWtaCapture(command, L"/d /c \"echo captured & echo hidden 1>&2 & exit /b 7\"", 5000, nullptr, false);
        VERIFY_IS_TRUE(result.completed);
        VERIFY_IS_FALSE(result.cancelled);
        VERIFY_IS_FALSE(result.timedOut);
        VERIFY_ARE_EQUAL(DWORD{ 7 }, result.exitCode);
        VERIFY_IS_TRUE(result.output.find("captured") != std::string::npos);
        VERIFY_IS_TRUE(result.output.find("hidden") == std::string::npos);
        std::atomic<bool> cancellation{ false };
        const auto merged = Wta::RunWtaCapture(command, L"/d /c \"echo captured & echo stderr 1>&2\"", 5000, nullptr, true, &cancellation);
        VERIFY_IS_TRUE(merged.completed);
        VERIFY_IS_FALSE(merged.cancelled);
        VERIFY_IS_FALSE(merged.timedOut);
        VERIFY_ARE_EQUAL(DWORD{ 0 }, merged.exitCode);
        VERIFY_IS_TRUE(merged.output.find("stderr") != std::string::npos);
    }

    void WtaProcessTests::LaunchFailureIsNotTimeout()
    {
        const auto result = Wta::RunWtaCapture({}, L"", 100);
        VERIFY_IS_FALSE(result.completed);
        VERIFY_IS_FALSE(result.cancelled);
        VERIFY_IS_FALSE(result.timedOut);
    }
}
