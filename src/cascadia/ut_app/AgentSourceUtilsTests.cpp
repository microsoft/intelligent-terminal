// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../inc/AgentSourceUtils.h"
#include "../WinRTUtils/inc/WtExeUtils.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class AgentSourceUtilsTests
    {
        TEST_CLASS(AgentSourceUtilsTests);

        TEST_METHOD(ReadEnvironmentVariableSupportsLongValues);
        TEST_METHOD(PrefersPaneCwdOverWindowLaunchCwd);
        TEST_METHOD(SeparatesAgentCwdFromHelperLaunchCwd);
        TEST_METHOD(RecognizesGeneratedSshSessionsSource);
        TEST_METHOD(RecognizesDirectSshSessionsSource);
        TEST_METHOD(SeparatesSshSessionsUserAndPort);
        TEST_METHOD(IgnoresNonSshSessionsProfiles);
        TEST_METHOD(RejectsUnsupportedSshSessionsProfiles);
        TEST_METHOD(RejectsMalformedSshSessionsProfiles);
        TEST_METHOD(BuildsSshSessionsHelperArguments);
        TEST_METHOD(WritesSshSessionsRuntimeMetadata);
        TEST_METHOD(BuildsManagedSshLaunchWithoutChangingSourceIdentity);
        TEST_METHOD(PreservesManagedSshPtyWithoutPinningGlobalHooksPolicy);
        TEST_METHOD(LeavesUnsupportedSshLaunchesUnchanged);
        TEST_METHOD(RecognizesManagedSshSessionSource);
        TEST_METHOD(RejectsMalformedManagedSshSessionSource);
        TEST_METHOD(RecognizesTmuxSshSessionsSource);
        TEST_METHOD(RejectsAmbiguousTmuxSshSessionsSource);
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

    void AgentSourceUtilsTests::RecognizesGeneratedSshSessionsSource()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        const auto source = AgentSource::ResolveSessionsSshSource(
            L"Windows.Terminal.SSH",
            LR"("%SystemRoot%\System32\OpenSSH\ssh.exe" wsl-ssh)");
        VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::ValidTarget,
                       L"Generated SSH identity depends on namespace and commandline, not a renameable display name");
        VERIFY_ARE_EQUAL(std::wstring{ L"wsl-ssh" }, source.destination);
        VERIFY_IS_FALSE(source.port.has_value());
        VERIFY_IS_TRUE(source.error.empty());

        const auto casePreserved = AgentSource::ResolveSessionsSshSource(
            L"Windows.Terminal.SSH",
            LR"("%SystemRoot%\System32\OpenSSH\ssh.exe" Production-Alias)");
        VERIFY_ARE_EQUAL(std::wstring{ L"Production-Alias" }, casePreserved.destination);

        for (const auto commandline : { L"", L"pwsh.exe", L"cmd.exe /c ssh wsl-ssh" })
        {
            const auto modified = AgentSource::ResolveSessionsSshSource(L"Windows.Terminal.SSH", commandline);
            VERIFY_IS_TRUE(modified.kind == AgentSource::SessionsSshKind::UnsupportedSsh);
            VERIFY_IS_FALSE(modified.error.empty());
            VERIFY_IS_TRUE(modified.destination.empty());
        }
    }

    void AgentSourceUtilsTests::RecognizesDirectSshSessionsSource()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto commandline : {
                 L"ssh Production-Alias",
                 L"SSH.EXE Production-Alias",
                 LR"("C:\Program Files\OpenSSH\SSH.exe" "Production-Alias")",
                 L" \tssh.exe\tProduction-Alias",
                 L"ssh -t -T -tt Production-Alias",
                 L"ssh -- Production-Alias",
             })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, commandline);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::ValidTarget);
            VERIFY_ARE_EQUAL(std::wstring{ L"Production-Alias" }, source.destination);
            VERIFY_IS_FALSE(source.port.has_value());
        }
    }

    void AgentSourceUtilsTests::RecognizesTmuxSshSessionsSource()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto commandline : {
                 L"ssh.exe -T -o BatchMode=yes wsl-ubuntu tmux -C a -t test-s",
                 L"ssh -oBatchMode=yes -- wsl-ubuntu tmux -C new-session -A -s work",
                 LR"(ssh.exe wsl-ubuntu "tmux -C attach-session -t work")",
                 L"ssh wsl-ubuntu /usr/bin/tmux -C a",
             })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, commandline, AgentSource::SessionsSshCommand::Tmux);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::ValidTarget);
            VERIFY_ARE_EQUAL(std::wstring{ L"wsl-ubuntu" }, source.destination);
            VERIFY_IS_FALSE(source.port.has_value());
            VERIFY_IS_TRUE(AgentSource::ResolveSessionsSshSource({}, commandline).kind == AgentSource::SessionsSshKind::UnsupportedSsh,
                           L"Ordinary SSH profiles must retain their strict remote-command boundary");
        }
        const auto source = AgentSource::ResolveSessionsSshSource(
            {}, L"ssh -p2222 -l user Host-Alias tmux -C a", AgentSource::SessionsSshCommand::Tmux);
        const auto ordinary = AgentSource::ResolveSessionsSshSource({}, L"ssh -p 2222 user@Host-Alias");
        VERIFY_ARE_EQUAL(ordinary.destination, source.destination);
        VERIFY_ARE_EQUAL(ordinary.port.value(), source.port.value());
        const auto ipv6 = AgentSource::ResolveSessionsSshSource(
            {}, L"ssh -T user@[::1] tmux -C a", AgentSource::SessionsSshCommand::Tmux);
        VERIFY_IS_TRUE(ipv6.kind == AgentSource::SessionsSshKind::ValidTarget);
        VERIFY_ARE_EQUAL(std::wstring{ L"user@[::1]" }, ipv6.destination);
    }

    void AgentSourceUtilsTests::RejectsAmbiguousTmuxSshSessionsSource()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto commandline : {
                 L"ssh host",
                 L"ssh host sh -lc tmux",
                 L"ssh host ssh other tmux -C a",
                 L"ssh host tmux -C a ; ssh other tmux -C a",
                 L"ssh host tmux -C a || ssh other tmux -C a",
                 L"ssh -oHostName=other host tmux -C a",
                 L"ssh -o User=other host tmux -C a",
                 L"ssh -F other-config host tmux -C a",
                 L"ssh -o",
                 L"ssh -p0 host tmux -C a",
                 L"ssh -l one two@host tmux -C a",
                 L"wta ssh --destination host --remote-command tmux",
             })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, commandline, AgentSource::SessionsSshCommand::Tmux);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::UnsupportedSsh);
            VERIFY_IS_TRUE(source.destination.empty());
        }
        for (const auto commandline : { L"wsl.exe -- tmux -C a", L"cmd.exe /c ssh host tmux -C a", L"opaque-backend" })
        {
            VERIFY_IS_TRUE(AgentSource::ResolveSessionsSshSource({}, commandline, AgentSource::SessionsSshCommand::Tmux).kind ==
                           AgentSource::SessionsSshKind::NonSsh);
        }
    }

    void AgentSourceUtilsTests::SeparatesSshSessionsUserAndPort()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        struct TestCase
        {
            const wchar_t* commandline;
            const wchar_t* destination;
            uint16_t port;
        };
        constexpr TestCase cases[]{
            { L"ssh yuazha@127.0.0.1", L"yuazha@127.0.0.1", 0 },
            { L"ssh -p 2222 yuazha@127.0.0.1", L"yuazha@127.0.0.1", 2222 },
            { L"ssh -p2222 -lyuazha 127.0.0.1", L"yuazha@127.0.0.1", 2222 },
            { L"ssh -l yuazha -p 22 Wsl-SSH", L"yuazha@Wsl-SSH", 22 },
            { L"ssh -l yuazha Wsl-SSH", L"yuazha@Wsl-SSH", 0 },
            { L"ssh -p1 server", L"server", 1 },
            { L"ssh -p65535 server", L"server", 65535 },
            { L"ssh -p00022 server", L"server", 22 },
            { L"ssh -p2222 user@[::1]", L"user@[::1]", 2222 },
            { L"ssh user@::1", L"user@::1", 0 },
        };
        for (const auto& test : cases)
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, test.commandline);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::ValidTarget);
            VERIFY_ARE_EQUAL(std::wstring{ test.destination }, source.destination);
            VERIFY_ARE_EQUAL(test.port, source.port.value_or(0));
            VERIFY_ARE_EQUAL(test.port != 0, source.port.has_value());
        }
    }

    void AgentSourceUtilsTests::IgnoresNonSshSessionsProfiles()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        struct TestCase
        {
            const wchar_t* source;
            const wchar_t* commandline;
        };
        constexpr TestCase cases[]{
            { L"", L"" },
            { L"", L"powershell.exe" },
            { L"Windows.Terminal.PowershellCore", L"pwsh.exe" },
            { L"Windows.Terminal.Wsl", L"wsl.exe -d Ubuntu" },
            { L"", LR"("%SystemRoot%\System32\wsl.exe" -d ssh)" },
            { L"", L"cmd.exe /k ssh wsl-ssh" },
            { L"", L"pwsh.exe -Command ssh wsl-ssh" },
            { L"", L"my-ssh.exe wsl-ssh" },
        };
        for (const auto& test : cases)
        {
            const auto source = AgentSource::ResolveSessionsSshSource(test.source, test.commandline);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::NonSsh);
            VERIFY_IS_TRUE(AgentSource::BuildSessionsSshHelperArguments(source).empty());
            Json::Value params;
            AgentSource::WriteSessionsSshMetadata(params, source);
            VERIFY_IS_TRUE(params.isMember("sessions_ssh"));
            VERIFY_IS_TRUE(params["sessions_ssh"].isNull());
        }
    }

    void AgentSourceUtilsTests::RejectsUnsupportedSshSessionsProfiles()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto commandline : {
                 L"ssh -i private-key server",
                 L"ssh -J jump-host server",
                 L"ssh -F alternate-config server",
                 L"ssh -o ProxyCommand=command server",
                 L"ssh -oPort=2222 server",
                 L"ssh -L 8080:localhost:80 server",
                 L"ssh -R 8080:localhost:80 server",
                 L"ssh -D 8080 server",
                 L"ssh -S control-socket server",
                 L"ssh -W target:22 server",
                 L"ssh -4 server",
                 L"ssh -6 server",
                 L"ssh -N server",
                 L"ssh -f server",
                 L"ssh -tp2222 server",
                 L"ssh server uname -a",
                 L"ssh server -p2222",
                 L"ssh server && other-command",
                 L"ssh -p22 -p2222 server",
                 L"ssh -p22 -p22 server",
                 L"ssh -l alice -l bob server",
                 L"ssh -l alice alice@server",
                 L"ssh -l alice bob@server",
             })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, commandline);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::UnsupportedSsh);
            VERIFY_IS_TRUE(source.destination.empty());
            VERIFY_IS_FALSE(source.port.has_value());
            VERIFY_ARE_NOT_EQUAL(std::wstring::npos, source.error.find(L"Host alias in OpenSSH config"));
            VERIFY_ARE_EQUAL(std::wstring::npos, source.error.find(L"private-key"));
            VERIFY_ARE_EQUAL(std::wstring::npos, source.error.find(L"ProxyCommand=command"));
        }
    }

    void AgentSourceUtilsTests::RejectsMalformedSshSessionsProfiles()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto commandline : {
                 L"ssh",
                 L"ssh -p",
                 L"ssh -l",
                 L"ssh -p0 server",
                 L"ssh -p65536 server",
                 L"ssh -p999999999999999999999999999 server",
                 L"ssh -p-22 server",
                 L"ssh -p+22 server",
                 L"ssh -p22x server",
                 L"ssh -p \"\" server",
                 L"ssh -l \"\" server",
                 L"ssh -l -p server",
                 L"ssh \"\"",
                 L"ssh --",
                 L"ssh -- -server",
                 L"ssh user@-server",
                 L"ssh @server",
                 L"ssh user@",
                 L"ssh user@other@server",
                 L"ssh \"server",
                 L"ssh \"server name\"",
                 L"ssh \"server;command\"",
                 L"ssh \"server&command\"",
                 L"ssh \"server|command\"",
                 L"ssh \"$(command)\"",
                 L"ssh \"server`command\"",
                 L"ssh \"%COMSPEC%\"",
                 L"ssh [::1",
                 L"ssh ::1]",
                 L"ssh user@[server]",
                 L"ssh ssh://user@server",
             })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, commandline);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::UnsupportedSsh);
            VERIFY_IS_TRUE(source.destination.empty());
            VERIFY_IS_FALSE(source.error.empty());
        }

        const auto embeddedNull = std::wstring{ L"ssh server" } + L'\0' + L"other-command";
        const auto source = AgentSource::ResolveSessionsSshSource({}, embeddedNull);
        VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::UnsupportedSsh);
    }

    void AgentSourceUtilsTests::BuildsSshSessionsHelperArguments()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        const auto direct = AgentSource::ResolveSessionsSshSource({}, L"ssh -p2222 -l yuazha Wsl-SSH");
        const auto directArgs = AgentSource::BuildSessionsSshHelperArguments(direct);
        VERIFY_ARE_EQUAL(size_t{ 2 }, directArgs.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"--sessions-ssh-target" }, directArgs[0].first);
        VERIFY_ARE_EQUAL(std::wstring{ L"yuazha@Wsl-SSH" }, directArgs[0].second);
        VERIFY_ARE_EQUAL(std::wstring{ L"--sessions-ssh-port" }, directArgs[1].first);
        VERIFY_ARE_EQUAL(std::wstring{ L"2222" }, directArgs[1].second);

        const auto alias = AgentSource::ResolveSessionsSshSource(L"Windows.Terminal.SSH", L"ssh wsl-ssh");
        const auto aliasArgs = AgentSource::BuildSessionsSshHelperArguments(alias);
        VERIFY_ARE_EQUAL(size_t{ 1 }, aliasArgs.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"--sessions-ssh-target" }, aliasArgs[0].first);
        VERIFY_ARE_EQUAL(std::wstring{ L"wsl-ssh" }, aliasArgs[0].second);

        const auto unsupported = AgentSource::ResolveSessionsSshSource({}, L"ssh -i private-key wsl-ssh");
        const auto errorArgs = AgentSource::BuildSessionsSshHelperArguments(unsupported);
        VERIFY_ARE_EQUAL(size_t{ 1 }, errorArgs.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"--sessions-ssh-error" }, errorArgs[0].first);
        VERIFY_ARE_EQUAL(unsupported.error, errorArgs[0].second);

        for (const auto& source : { direct, alias, unsupported, AgentSource::SessionsSshSource{} })
        {
            const auto args = AgentSource::BuildSessionsSshHelperArguments(source);
            std::wstring commandline{ L"wta.exe" };
            for (const auto& [flag, value] : args)
            {
                commandline.append(L" ").append(flag).append(L" ");
                QuoteAndEscapeCommandlineArg(value, commandline);
            }
            int argc = 0;
            const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(commandline.c_str(), &argc) };
            VERIFY_IS_NOT_NULL(argv.get());
            VERIFY_ARE_EQUAL(args.size() * 2 + 1, static_cast<size_t>(argc));
            for (size_t i = 0; i < args.size(); ++i)
            {
                VERIFY_ARE_EQUAL(args[i].first, std::wstring{ argv[i * 2 + 1] });
                VERIFY_ARE_EQUAL(args[i].second, std::wstring{ argv[i * 2 + 2] });
            }
        }
    }

    void AgentSourceUtilsTests::BuildsManagedSshLaunchWithoutChangingSourceIdentity()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        const std::wstring wta{ LR"(C:\Program Files\IT; Dev\wta.exe)" };
        const auto source = AgentSource::ResolveSessionsSshSource(
            L"Windows.Terminal.SSH", LR"("%SystemRoot%\System32\OpenSSH\ssh.exe" -p2222 -l user Work-Host)");
        const auto command = AgentSource::BuildManagedSshCommandline(
            source, wta, LR"(C:\Windows\System32\OpenSSH\ssh.exe)");
        VERIFY_IS_TRUE(command.has_value());
        int argc{};
        const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(command->c_str(), &argc) };
        VERIFY_IS_NOT_NULL(argv.get());
        VERIFY_ARE_EQUAL(6, argc);
        VERIFY_ARE_EQUAL(wta, std::wstring{ argv[0] });
        VERIFY_ARE_EQUAL(std::wstring{ L"ssh" }, std::wstring{ argv[1] });
        VERIFY_ARE_EQUAL(std::wstring{ L"--destination" }, std::wstring{ argv[2] });
        VERIFY_ARE_EQUAL(std::wstring{ L"user@Work-Host" }, std::wstring{ argv[3] });
        VERIFY_ARE_EQUAL(std::wstring{ L"--port" }, std::wstring{ argv[4] });
        VERIFY_ARE_EQUAL(std::wstring{ L"2222" }, std::wstring{ argv[5] });
        VERIFY_ARE_EQUAL(std::wstring{ L"user@Work-Host" }, source.destination);
        VERIFY_ARE_EQUAL(std::wstring{ L"%SystemRoot%\\System32\\OpenSSH\\ssh.exe" }, source.executable);
        Json::Value metadata;
        AgentSource::WriteSessionsSshMetadata(metadata, source);
        VERIFY_ARE_EQUAL(std::string{ "user@Work-Host" }, metadata["sessions_ssh"]["destination"].asString());
        VERIFY_ARE_EQUAL(2222u, metadata["sessions_ssh"]["port"].asUInt());
    }

    void AgentSourceUtilsTests::PreservesManagedSshPtyWithoutPinningGlobalHooksPolicy()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto input : { L"ssh host", L"ssh -t host", L"ssh -T -tt host" })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, input);
            VERIFY_IS_TRUE(source.requestPty);
            const auto command = AgentSource::BuildManagedSshCommandline(source, L"wta.exe", {});
            VERIFY_IS_TRUE(command.has_value());
            VERIFY_ARE_EQUAL(std::wstring::npos, command->find(L"--no-pty"));
            VERIFY_ARE_EQUAL(std::wstring::npos, command->find(L"--no-hooks"));
        }
        for (const auto input : { L"ssh -T host", L"ssh -tt -T host", L"ssh -ttT host" })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, input);
            VERIFY_IS_FALSE(source.requestPty);
            const auto command = AgentSource::BuildManagedSshCommandline(source, L"wta.exe", {});
            VERIFY_IS_TRUE(command.has_value());
            VERIFY_ARE_NOT_EQUAL(std::wstring::npos, command->find(L"--no-pty"));
            VERIFY_ARE_EQUAL(std::wstring::npos, command->find(L"--no-hooks"));
        }
        const auto system = AgentSource::ResolveSessionsSshSource({}, LR"("C:/Windows/System32/OpenSSH/ssh.exe" host)");
        VERIFY_IS_TRUE(AgentSource::BuildManagedSshCommandline(
                           system, L"wta.exe", LR"(C:\Windows\System32\OpenSSH\ssh.exe)")
                           .has_value());
    }

    void AgentSourceUtilsTests::LeavesUnsupportedSshLaunchesUnchanged()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto input : {
                 L"pwsh.exe", L"cmd.exe /c ssh host", L"ssh host uname -a", L"ssh -i key host", LR"("C:\Custom\ssh.exe" host)", L"wta.exe ssh --destination host" })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, input);
            VERIFY_IS_FALSE(AgentSource::BuildManagedSshCommandline(
                                source, L"wta.exe", LR"(C:\Windows\System32\OpenSSH\ssh.exe)")
                                .has_value());
        }
        const auto source = AgentSource::ResolveSessionsSshSource({}, L"ssh host");
        VERIFY_IS_FALSE(AgentSource::BuildManagedSshCommandline(source, {}, {}).has_value());
    }

    void AgentSourceUtilsTests::RecognizesManagedSshSessionSource()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto input : {
                 L"wta.exe ssh --destination user@host --port 2222",
                 L"wta.exe ssh --destination=user@host --port=2222 --no-hooks",
                 LR"("C:\Program Files\IT\wta.exe" ssh --destination user@host --port 2222 --remote-command "cd '/repo with spaces' && exec copilot --resume sid")" })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, input);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::ValidTarget);
            VERIFY_IS_TRUE(source.managedLaunch);
            VERIFY_ARE_EQUAL(std::wstring{ L"user@host" }, source.destination);
            VERIFY_ARE_EQUAL(uint16_t{ 2222 }, source.port.value());
            VERIFY_IS_TRUE(source.requestPty);
            VERIFY_IS_FALSE(AgentSource::BuildManagedSshCommandline(source, L"wta.exe", {}).has_value());
        }
        const auto noPty = AgentSource::ResolveSessionsSshSource(
            L"Windows.Terminal.SSH", L"wta.exe ssh --destination host --no-pty");
        VERIFY_IS_TRUE(noPty.kind == AgentSource::SessionsSshKind::ValidTarget);
        VERIFY_IS_FALSE(noPty.requestPty);
    }

    void AgentSourceUtilsTests::RejectsMalformedManagedSshSessionSource()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        for (const auto input : {
                 L"wta.exe ssh", L"wta.exe ssh --destination", L"wta.exe ssh --destination=", L"wta.exe ssh --destination host --destination other", L"wta.exe ssh --destination host --port 0", L"wta.exe ssh --destination host --port=", L"wta.exe ssh --destination host --port 22 --port 23", L"wta.exe ssh --destination host --unknown value", L"wta.exe ssh --destination \"host; command\"", L"wta.exe ssh --destination host --remote-command first --remote-command second" })
        {
            const auto source = AgentSource::ResolveSessionsSshSource({}, input);
            VERIFY_IS_TRUE(source.kind == AgentSource::SessionsSshKind::UnsupportedSsh);
            VERIFY_IS_FALSE(source.error.empty());
        }
    }

    void AgentSourceUtilsTests::WritesSshSessionsRuntimeMetadata()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        Json::Value params;
        params["tab_id"] = "owning-tab";
        params["window_id"] = "42";
        params["view"] = "sessions";
        params["pane_open"] = true;

        AgentSource::WriteSessionsSshMetadata(params, AgentSource::ResolveSessionsSshSource({}, L"ssh -p2222 yuazha@Wsl-SSH"));
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 5 }, params.size());
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 2 }, params["sessions_ssh"].size());
        VERIFY_ARE_EQUAL(std::string{ "yuazha@Wsl-SSH" }, params["sessions_ssh"]["destination"].asString());
        VERIFY_IS_TRUE(params["sessions_ssh"]["port"].isIntegral());
        VERIFY_ARE_EQUAL(2222u, params["sessions_ssh"]["port"].asUInt());

        AgentSource::WriteSessionsSshMetadata(params, AgentSource::ResolveSessionsSshSource({}, L"ssh wsl-ssh"));
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 2 }, params["sessions_ssh"].size());
        VERIFY_ARE_EQUAL(std::string{ "wsl-ssh" }, params["sessions_ssh"]["destination"].asString());
        VERIFY_IS_TRUE(params["sessions_ssh"].isMember("port"));
        VERIFY_IS_TRUE(params["sessions_ssh"]["port"].isNull());

        const auto unsupported = AgentSource::ResolveSessionsSshSource({}, L"ssh -J jump-host wsl-ssh");
        AgentSource::WriteSessionsSshMetadata(params, unsupported);
        VERIFY_ARE_EQUAL(Json::ArrayIndex{ 1 }, params["sessions_ssh"].size());
        VERIFY_ARE_EQUAL(winrt::to_string(unsupported.error), params["sessions_ssh"]["error"].asString());
        VERIFY_IS_FALSE(params["sessions_ssh"].isMember("destination"));
        VERIFY_IS_FALSE(params["sessions_ssh"].isMember("port"));

        AgentSource::WriteSessionsSshMetadata(params, AgentSource::ResolveSessionsSshSource({}, L"pwsh.exe"));
        VERIFY_IS_TRUE(params.isMember("sessions_ssh"));
        VERIFY_IS_TRUE(params["sessions_ssh"].isNull());
        VERIFY_ARE_EQUAL(std::string{ "owning-tab" }, params["tab_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "42" }, params["window_id"].asString());
        VERIFY_ARE_EQUAL(std::string{ "sessions" }, params["view"].asString());
        VERIFY_IS_TRUE(params["pane_open"].asBool());
    }
}