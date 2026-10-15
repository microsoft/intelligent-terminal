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
        TEST_METHOD(PaneCwdUsesReportedDirectoryOrHostStartup);
        TEST_METHOD(PaneCwdUsesWslLaunchDirectoryBeforeShellIntegration);
        TEST_METHOD(PaneCwdDoesNotInferUnknownWslDirectories);
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

    void AgentSourceUtilsTests::PaneCwdUsesReportedDirectoryOrHostStartup()
    {
        using Microsoft::Terminal::AgentSource::ResolvePaneCwd;
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\live" },
                         ResolvePaneCwd(L"C:\\live", true, L"pwsh.exe -NoProfile", L"C:\\start"));
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\start" },
                         ResolvePaneCwd(L"C:\\start", false, L"pwsh.exe -NoProfile", L"C:\\start"));
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\start" },
                         ResolvePaneCwd({}, false, L"pwsh.exe -NoProfile", L"C:\\start"));
        VERIFY_ARE_EQUAL(std::wstring{ L"/live" },
                         ResolvePaneCwd(L"/live", true, L"wsl.exe -d Ubuntu --cd /start --exec bash", L"C:\\launcher"));
        VERIFY_ARE_EQUAL(std::wstring{ L"C:\\start" },
                         ResolvePaneCwd(L"C:\\start", false, L"cmd.exe /c echo wsl --cd /not-a-launch", L"C:\\start"));
    }

    void AgentSourceUtilsTests::PaneCwdUsesWslLaunchDirectoryBeforeShellIntegration()
    {
        using Microsoft::Terminal::AgentSource::ResolvePaneCwd;
        const std::wstring cwd{ L"/work with spaces/\u6d4b\u8bd5" };
        const auto command = L"\"C:\\Windows\\System32\\wsl.exe\" -d Ubuntu --cd \"" + cwd + L"\" -- bash -lc \"exec agy\"";
        VERIFY_ARE_EQUAL(cwd, ResolvePaneCwd(L"C:\\launcher", false, command, L"C:\\launcher"));
        VERIFY_ARE_EQUAL(std::wstring{ L"/work" },
                         ResolvePaneCwd({}, false, L"wsl --distribution-id {1234} --user user --cd /work --exec bash", L"C:\\launcher"));
        VERIFY_ARE_EQUAL(std::wstring{ L"/start" },
                         ResolvePaneCwd(L"/start", false, L"wsl.exe -d Ubuntu --exec bash", L"/start"));
        VERIFY_ARE_EQUAL(std::wstring{ L"/start" },
                         ResolvePaneCwd(L"C:\\launcher", false, L"wsl.exe --cd /start --exec echo --cd /not-a-launch-option", L"C:\\launcher"));
    }

    void AgentSourceUtilsTests::PaneCwdDoesNotInferUnknownWslDirectories()
    {
        using Microsoft::Terminal::AgentSource::ResolvePaneCwd;
        for (const auto command : {
                 L"wsl.exe",
                 L"wsl.exe ~",
                 L"wsl.exe --cd",
                 L"wsl.exe --cd relative",
                 L"wsl.exe --cd ~",
                 L"wsl.exe --cd C:\\work",
                 L"wsl.exe --cd /first --cd /second",
                 L"wsl.exe -d --cd /work",
                 L"wsl.exe --user",
                 L"wsl.exe --unknown-option --cd /work",
                 L"wsl.exe --exec echo --cd /not-a-launch-option",
                 L"wsl.exe bash -lc \"echo --cd /not-a-launch-option\"" })
        {
            VERIFY_ARE_EQUAL(std::wstring{}, ResolvePaneCwd(L"C:\\launcher", false, command, L"C:\\launcher"));
        }
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolvePaneCwd(L"/start", false, L"wsl.exe --cd relative --exec bash", L"/start"));
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolvePaneCwd(L"/start", false, L"wsl.exe ~", L"/start"));
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolvePaneCwd(L"/start", false, L"wsl.exe --exec echo --cd /not-a-launch-option", L"/start"));
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolvePaneCwd(L"/start", false, L"C:\\not-system32\\wsl.exe", L"/start"));
    }
}