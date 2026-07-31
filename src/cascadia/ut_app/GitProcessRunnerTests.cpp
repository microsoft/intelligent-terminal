// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/GitProcessRunner.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::RepoAwareness;

namespace TerminalAppUnitTests
{
    class GitProcessRunnerTests
    {
        TEST_CLASS(GitProcessRunnerTests);

        TEST_METHOD(RejectsRelativeOrMissingExecutable);
        TEST_METHOD(CapturesOutputWithoutAWindow);
        TEST_METHOD(RemovesRepositoryRedirectEnvironment);
        TEST_METHOD(HonorsCancellationTimeoutAndOutputLimit);

        static std::filesystem::path _cmdPath();
    };

    std::filesystem::path GitProcessRunnerTests::_cmdPath()
    {
        std::wstring systemDirectory(MAX_PATH, L'\0');
        const auto length = GetSystemDirectoryW(systemDirectory.data(), static_cast<UINT>(systemDirectory.size()));
        VERIFY_IS_GREATER_THAN(length, 0u);
        VERIFY_IS_LESS_THAN(length, static_cast<UINT>(systemDirectory.size()));
        systemDirectory.resize(length);
        return std::filesystem::path{ systemDirectory } / L"cmd.exe";
    }

    void GitProcessRunnerTests::RejectsRelativeOrMissingExecutable()
    {
        VERIFY_IS_FALSE(GitProcessRunner{ L"git.exe" }.IsValid());
        VERIFY_IS_FALSE(GitProcessRunner{ std::filesystem::temp_directory_path() / L"missing-git.exe" }.IsValid());
    }

    void GitProcessRunnerTests::CapturesOutputWithoutAWindow()
    {
        const GitProcessRunner runner{ _cmdPath() };
        const auto result = runner.Run(
            std::filesystem::temp_directory_path(),
            { L"/d", L"/s", L"/c", L"echo runner-ok" });

        VERIFY_ARE_EQUAL(GitProcessStatus::Succeeded, result.status);
        VERIFY_ARE_EQUAL(0ul, result.exitCode);
        VERIFY_ARE_EQUAL(std::string{ "runner-ok\r\n" }, result.standardOutput);
        VERIFY_IS_TRUE(result.standardError.empty());
    }

    void GitProcessRunnerTests::RemovesRepositoryRedirectEnvironment()
    {
        wchar_t originalValue[32767];
        const auto originalLength = GetEnvironmentVariableW(L"GIT_DIR", originalValue, ARRAYSIZE(originalValue));
        const auto restore = wil::scope_exit([&] {
            if (originalLength == 0)
            {
                SetEnvironmentVariableW(L"GIT_DIR", nullptr);
            }
            else
            {
                SetEnvironmentVariableW(L"GIT_DIR", originalValue);
            }
        });
        VERIFY_WIN32_BOOL_SUCCEEDED(SetEnvironmentVariableW(L"GIT_DIR", L"C:\\must-not-leak"));

        const GitProcessRunner runner{ _cmdPath() };
        const auto result = runner.Run(
            std::filesystem::temp_directory_path(),
            { L"/d", L"/s", L"/c", L"if defined GIT_DIR (exit /b 42) else (exit /b 0)" });

        VERIFY_ARE_EQUAL(GitProcessStatus::Succeeded, result.status);
        VERIFY_ARE_EQUAL(0ul, result.exitCode);
    }

    void GitProcessRunnerTests::HonorsCancellationTimeoutAndOutputLimit()
    {
        const GitProcessRunner runner{ _cmdPath() };
        std::atomic_bool cancelled{ true };

        GitProcessOptions cancelledOptions;
        cancelledOptions.cancelled = &cancelled;
        const auto cancelledResult = runner.Run(
            std::filesystem::temp_directory_path(),
            { L"/d", L"/s", L"/c", L"ping -n 3 127.0.0.1 >nul" },
            cancelledOptions);
        VERIFY_ARE_EQUAL(GitProcessStatus::Cancelled, cancelledResult.status);

        GitProcessOptions timeoutOptions;
        timeoutOptions.timeout = std::chrono::milliseconds{ 1 };
        const auto timeoutResult = runner.Run(
            std::filesystem::temp_directory_path(),
            { L"/d", L"/s", L"/c", L"ping -n 3 127.0.0.1 >nul" },
            timeoutOptions);
        VERIFY_ARE_EQUAL(GitProcessStatus::TimedOut, timeoutResult.status);

        GitProcessOptions outputOptions;
        outputOptions.maxOutputBytes = 8;
        const auto outputResult = runner.Run(
            std::filesystem::temp_directory_path(),
            { L"/d", L"/s", L"/c", L"echo this-is-more-than-eight-bytes" },
            outputOptions);
        VERIFY_ARE_EQUAL(GitProcessStatus::OutputLimitExceeded, outputResult.status);
    }
}
