#include <windows.h>
#include <string>
#include <vector>

int wmain(int argc, wchar_t** argv)
{
    // Startup inventory can discover this executable; it must never act as ACP.
    std::wstring session;
    bool resume = false;
    bool external = false;
    for (int i = 1; i < argc; ++i)
    {
        if (std::wstring(argv[i]) == L"--session-id" && i + 1 < argc)
            session = argv[++i];
#ifdef ITE2E_SHIM_RESUME_SESSION
        else if ((std::wstring(argv[i]) == L"--resume" || std::wstring(argv[i]) == L"--external-session") && i + 1 < argc)
        {
            resume = std::wstring(argv[i]) == L"--resume";
            external = !resume;
            session = argv[++i];
            if (session != ITE2E_SHIM_RESUME_SESSION)
                return 3;
        }
#endif
        else
        {
            fwprintf(stderr, L"ITE2E shim rejects noninteractive argument: %s\n", argv[i]);
            return 3;
        }
    }
    const std::wstring pwsh = ITE2E_SHIM_PWSH;
    const std::wstring fixture = ITE2E_SHIM_FIXTURE;
    const std::wstring log = ITE2E_SHIM_LOG;
    const std::wstring run = ITE2E_SHIM_RUN;
    const std::wstring wtcli = ITE2E_SHIM_WTCLI;
    if (session.empty() || pwsh.empty() || fixture.empty() || log.empty() || run.empty() || wtcli.empty())
    {
        fwprintf(stderr, L"ITE2E shim missing test prerequisites: session=%d pwsh=%d fixture=%d log=%d run=%d wtcli=%d\n",
            session.empty(), pwsh.empty(), fixture.empty(), log.empty(), run.empty(), wtcli.empty());
        return 4;
    }
    SetEnvironmentVariableW(L"ITE2E_SHIM_PID", std::to_wstring(GetCurrentProcessId()).c_str());
    SetEnvironmentVariableW(L"ITE2E_SHIM_ARGS", GetCommandLineW());
    std::wstring command = L"\"" + pwsh + L"\" -NoLogo -NoProfile -File \"" + fixture +
        L"\" -LogPath \"" + log + L"\" -RunId " + run + (external ? L" -External" : L" -Canonical") +
        (resume ? L" -Resume" : L"") + L" -SessionId " +
        session + L" -WtcliPath \"" + wtcli + L"\"";
#ifdef ITE2E_SHIM_SESSION_START_GATE
    command += L" -SessionStartGate \"" + std::wstring{ ITE2E_SHIM_SESSION_START_GATE } + L"\"";
#endif
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
    startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION process{};
    if (!CreateProcessW(pwsh.c_str(), command.data(), nullptr, nullptr, TRUE, 0, nullptr, nullptr, &startup, &process))
        return static_cast<int>(GetLastError());
    CloseHandle(process.hThread);
    WaitForSingleObject(process.hProcess, INFINITE);
    DWORD code = 1;
    GetExitCodeProcess(process.hProcess, &code);
    CloseHandle(process.hProcess);
    return static_cast<int>(code);
}
