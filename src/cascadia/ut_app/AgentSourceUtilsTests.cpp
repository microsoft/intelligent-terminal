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
        TEST_METHOD(SplitCwdValidatesHostDirectories);
        TEST_METHOD(SplitCwdKeepsWslNamespacesSeparate);
        TEST_METHOD(SplitCwdRejectsUnsafeWslArguments);
        TEST_METHOD(SplitCwdQuotesWindowsRootsForWsl);
    };

    void AgentSourceUtilsTests::SplitCwdValidatesHostDirectories()
    {
        using Microsoft::Terminal::AgentSource::ResolveSplitCwd;
        const auto valid = [](std::wstring_view path) { return path == L"C:\\work with spaces"; };
        for (const auto source : { L"pwsh.exe", L"copilot.exe", L"cmd.exe /c claude.cmd" })
        {
            for (const auto target : { L"pwsh.exe", L"claude.exe", L"wsl.exe -d Ubuntu" })
            {
                VERIFY_ARE_EQUAL(std::wstring{ L"C:\\work with spaces" },
                                 ResolveSplitCwd(L"C:\\work with spaces", source, target, valid));
                VERIFY_ARE_EQUAL(std::wstring{}, ResolveSplitCwd(L"C:\\missing", source, target, valid));
                VERIFY_ARE_EQUAL(std::wstring{}, ResolveSplitCwd({}, source, target, valid));
            }
        }
        for (const auto relative : { L".", L"project", L"C:project", L"\\project" })
        {
            VERIFY_ARE_EQUAL(std::wstring{},
                             ResolveSplitCwd(relative, L"copilot.exe", L"pwsh.exe", [](std::wstring_view) { return true; }));
        }
    }

    void AgentSourceUtilsTests::SplitCwdKeepsWslNamespacesSeparate()
    {
        using Microsoft::Terminal::AgentSource::ResolveSplitCwd;
        const auto noWindowsDirectory = [](std::wstring_view) { return false; };
        VERIFY_ARE_EQUAL(std::wstring{ L"/work with spaces" },
                         ResolveSplitCwd(L"/work with spaces", L"wsl.exe -d Ubuntu --exec bash", L"wsl.exe --distribution Ubuntu --exec claude", noWindowsDirectory));
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolveSplitCwd(L"/work", L"wsl.exe", L"wsl.exe --exec bash", noWindowsDirectory));
        for (const auto target : { L"wsl.exe", L"wsl.exe -d Ubuntu", L"wsl.exe -d Debian" })
        {
            VERIFY_ARE_EQUAL(std::wstring{},
                             ResolveSplitCwd(L"/work", L"wsl.exe --exec bash", target, noWindowsDirectory));
        }
        VERIFY_ARE_EQUAL(std::wstring{ L"/work" },
                         ResolveSplitCwd(L"/work", L"wsl.exe --distribution-id {1234}", L"wsl.exe --distribution-id {1234} -e bash", noWindowsDirectory));
        for (const auto target : { L"pwsh.exe", L"copilot.exe", L"wsl.exe", L"wsl.exe -d Debian", L"wsl.exe --distribution-id {1234}", L"wsl.exe -d Ubuntu --cd /explicit", L"wsl.exe -d Ubuntu ~", L"wsl.exe --unknown -d Ubuntu", L"wsl.exe -d", L"wsl.exe -d Ubuntu -d Debian", L"cmd.exe /c wsl.exe -d Ubuntu" })
        {
            VERIFY_ARE_EQUAL(std::wstring{},
                             ResolveSplitCwd(L"/work", L"wsl.exe -d Ubuntu", target, noWindowsDirectory));
        }
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolveSplitCwd(L"/work", L"claude.exe", L"wsl.exe -d Ubuntu", noWindowsDirectory));
        VERIFY_ARE_EQUAL(std::wstring{ L"/work" },
                         ResolveSplitCwd(L"/work", L"wsl.exe ~ -d Debian", L"wsl.exe -d Debian", noWindowsDirectory));
        VERIFY_ARE_EQUAL(std::wstring{ L"/work" },
                         ResolveSplitCwd(L"/work", L"wsl.exe ~ --distribution-id {1234}", L"wsl.exe --distribution-id {1234}", noWindowsDirectory));
        for (const auto target : { L"wsl.exe", L"wsl.exe -d Ubuntu", L"wsl.exe ~ -d Debian" })
        {
            VERIFY_ARE_EQUAL(std::wstring{},
                             ResolveSplitCwd(L"/work", L"wsl.exe ~ -d Debian", target, noWindowsDirectory));
        }
    }

    void AgentSourceUtilsTests::SplitCwdRejectsUnsafeWslArguments()
    {
        using Microsoft::Terminal::AgentSource::ResolveSplitCwd;
        const auto noWindowsDirectory = [](std::wstring_view) { return false; };
        for (const auto directory : {
                 L"/tmp/\" --exec printf injected \"",
                 L"/tmp/back\\slash",
                 L"/tmp/trailing\\",
                 L"/tmp/%TEMP%",
                 L"/tmp/new\n"
                 L"line",
                 L"/tmp/new\r"
                 L"line" })
        {
            VERIFY_ARE_EQUAL(std::wstring{},
                             ResolveSplitCwd(directory, L"wsl.exe -d Ubuntu", L"wsl.exe -d Ubuntu", noWindowsDirectory));
        }
        const std::wstring embeddedNul{ L"/tmp/valid\0suffix", 17 };
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolveSplitCwd(embeddedNul, L"wsl.exe -d Ubuntu", L"wsl.exe -d Ubuntu", noWindowsDirectory));
        const std::wstring hostNul{ L"C:\\work\0suffix", 14 };
        VERIFY_ARE_EQUAL(std::wstring{},
                         ResolveSplitCwd(hostNul, L"pwsh.exe", L"pwsh.exe", [](std::wstring_view) { return true; }));

        const std::wstring directory{ L"/work with spaces/\u6d4b\u8bd5" };
        VERIFY_ARE_EQUAL(directory, ResolveSplitCwd(directory, L"wsl.exe -d Ubuntu", L"wsl.exe -d Ubuntu --exec bash", noWindowsDirectory));
        const auto [command, hostDirectory] = Microsoft::Console::Utils::MangleStartingDirectoryForWSL(L"wsl.exe --exec bash", directory);
        int argc{};
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(command.c_str(), &argc) };
        VERIFY_IS_NOT_NULL(argv.get());
        VERIFY_ARE_EQUAL(5, argc);
        VERIFY_ARE_EQUAL(std::wstring{ L"--cd" }, std::wstring{ argv.get()[1] });
        VERIFY_ARE_EQUAL(directory, std::wstring{ argv.get()[2] });
        VERIFY_ARE_EQUAL(std::wstring{ L"--exec" }, std::wstring{ argv.get()[3] });
        VERIFY_ARE_EQUAL(std::wstring{ L"bash" }, std::wstring{ argv.get()[4] });
        VERIFY_IS_TRUE(hostDirectory.empty());
    }

    void AgentSourceUtilsTests::SplitCwdQuotesWindowsRootsForWsl()
    {
        using Microsoft::Terminal::AgentSource::ResolveSplitCwd;
        for (const auto directory : { LR"(C:\)", LR"(C:\work with spaces\)", LR"(\\wsl$\Ubuntu\)" })
        {
            const auto inherited = ResolveSplitCwd(directory, L"pwsh.exe", L"wsl.exe -d Ubuntu", [](std::wstring_view) { return true; });
            VERIFY_ARE_EQUAL(std::wstring{ directory }, inherited);
            const auto [command, hostDirectory] = Microsoft::Console::Utils::MangleStartingDirectoryForWSL(L"wsl.exe -d Ubuntu", inherited);
            int argc{};
            const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(command.c_str(), &argc) };
            VERIFY_IS_NOT_NULL(argv.get());
            VERIFY_ARE_EQUAL(5, argc);
            VERIFY_ARE_EQUAL(std::wstring{ L"--cd" }, std::wstring{ argv.get()[1] });
            VERIFY_ARE_EQUAL(inherited, std::wstring{ argv.get()[2] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-d" }, std::wstring{ argv.get()[3] });
            VERIFY_ARE_EQUAL(std::wstring{ L"Ubuntu" }, std::wstring{ argv.get()[4] });
            VERIFY_IS_TRUE(hostDirectory.empty());
        }
    }

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