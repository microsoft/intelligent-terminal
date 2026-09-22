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
                       L"Generated SSH identity depends on namespace and commandline, not the display name");
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
            { L"ssh user@[fe80::1%eth0]", L"user@[fe80::1%eth0]", 0 },
            { L"ssh -p2222 user@[fe80::1%12]", L"user@[fe80::1%12]", 2222 },
            { L"ssh user@fe80::1%eth0", L"user@fe80::1%eth0", 0 },
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
                 L"ssh user@[fe80::1%]",
                 L"ssh user@[fe80::1%eth0%other]",
                 L"ssh user@[fe80::1%-scope]",
                 L"ssh user@server%eth0",
                 L"ssh user@[fe80::1%scope;command]",
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

        const auto scoped = AgentSource::ResolveSessionsSshSource({}, L"ssh user@[fe80::1%12]");
        VERIFY_IS_TRUE(scoped.kind == AgentSource::SessionsSshKind::ValidTarget);
        for (const auto& source : { direct, alias, scoped, unsupported, AgentSource::SessionsSshSource{} })
        {
            const auto args = AgentSource::BuildSessionsSshHelperArguments(source);
            std::wstring commandline{ L"wta.exe" };
            for (const auto& [flag, value] : args)
            {
                commandline.append(L" ").append(flag).append(L" ");
                QuoteAndEscapeCommandlineArg(value, commandline);
            }
            int argc = 0;
            const auto expanded = wil::ExpandEnvironmentStringsW<std::wstring>(commandline.c_str());
            const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(expanded.c_str(), &argc) };
            VERIFY_IS_NOT_NULL(argv.get());
            VERIFY_ARE_EQUAL(args.size() * 2 + 1, static_cast<size_t>(argc));
            for (size_t i = 0; i < args.size(); ++i)
            {
                VERIFY_ARE_EQUAL(args[i].first, std::wstring{ argv[i * 2 + 1] });
                VERIFY_ARE_EQUAL(args[i].second, std::wstring{ argv[i * 2 + 2] });
            }
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