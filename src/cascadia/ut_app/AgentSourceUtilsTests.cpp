// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../inc/AgentSourceUtils.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class AgentSourceUtilsTests
    {
        TEST_CLASS(AgentSourceUtilsTests);

        TEST_METHOD(ReadEnvironmentVariableSupportsLongValues);
        TEST_METHOD(PrefersPaneCwdOverWindowLaunchCwd);
        TEST_METHOD(SeparatesAgentCwdFromHelperLaunchCwd);
    };

    void AgentSourceUtilsTests::ReadEnvironmentVariableSupportsLongValues()
    {
        constexpr auto name = L"WT_AGENT_SOURCE_UTILS_LONG_ENV";
        SetLastError(ERROR_SUCCESS);
        const auto priorLength = GetEnvironmentVariableW(name, nullptr, 0);
        const auto priorMissing = priorLength == 0 && GetLastError() == ERROR_ENVVAR_NOT_FOUND;
        const auto priorValue = priorMissing ? std::wstring{} : Microsoft::Terminal::AgentSource::ReadEnvironmentVariable(name);
        const std::wstring expected(MAX_PATH + 32, L'x');
        VERIFY_WIN32_BOOL_SUCCEEDED(SetEnvironmentVariableW(name, expected.c_str()));
        const auto cleanup = wil::scope_exit([=]() {
            VERIFY_WIN32_BOOL_SUCCEEDED(SetEnvironmentVariableW(name, priorMissing ? nullptr : priorValue.c_str()));
        });

        VERIFY_ARE_EQUAL(expected, Microsoft::Terminal::AgentSource::ReadEnvironmentVariable(name));
    }

    void AgentSourceUtilsTests::PrefersPaneCwdOverWindowLaunchCwd()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\work" },
            AgentSource::ResolveCwd(
                L"C:\\work",
                L"C:\\Windows\\System32",
                L"C:\\profile",
                L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\window" },
            AgentSource::ResolveCwd({}, L"C:\\window", L"C:\\profile", L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\profile" },
            AgentSource::ResolveCwd({}, {}, L"C:\\profile", L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\Users\\user" },
            AgentSource::ResolveCwd({}, {}, {}, L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(std::wstring{}, AgentSource::ResolveCwd({}, {}, {}, {}));
    }

    void AgentSourceUtilsTests::SeparatesAgentCwdFromHelperLaunchCwd()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        const auto isWindowsDirectory = [](const std::wstring_view candidate) {
            return candidate == L"C:\\window" ||
                   candidate == L"C:\\profile" ||
                   candidate == L"C:\\Users\\user";
        };

        const auto wsl = AgentSource::ResolveAgentAndHelperWorkingDirectories(
            true,
            L"/home/user/project",
            L"C:\\window",
            L"C:\\profile",
            L"C:\\Users\\user",
            isWindowsDirectory);
        VERIFY_ARE_EQUAL(std::wstring{ L"/home/user/project" }, wsl.agent);
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\window" }, wsl.helper);

        const auto host = AgentSource::ResolveAgentAndHelperWorkingDirectories(
            false,
            L"/home/user/project",
            L"C:\\window",
            L"C:\\profile",
            L"C:\\Users\\user",
            isWindowsDirectory);
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\window" }, host.agent);
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\window" }, host.helper);

        const auto noWindowsDirectory = [](std::wstring_view) { return false; };
        const auto wslWithoutHelperCwd = AgentSource::ResolveAgentAndHelperWorkingDirectories(
            true, L"/home/user/project", {}, {}, {}, noWindowsDirectory);
        VERIFY_ARE_EQUAL(std::wstring{ L"/home/user/project" }, wslWithoutHelperCwd.agent);
        VERIFY_ARE_EQUAL(std::wstring{}, wslWithoutHelperCwd.helper);

        const auto hostWithoutWindowsCwd = AgentSource::ResolveAgentAndHelperWorkingDirectories(
            false, L"/home/user/project", {}, {}, {}, noWindowsDirectory);
        VERIFY_ARE_EQUAL(std::wstring{}, hostWithoutWindowsCwd.agent);
        VERIFY_ARE_EQUAL(std::wstring{}, hostWithoutWindowsCwd.helper);
    }
}