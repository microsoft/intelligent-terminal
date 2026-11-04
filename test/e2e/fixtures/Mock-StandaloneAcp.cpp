// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <windows.h>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

int wmain(const int argc, wchar_t**)
{
    if (argc != 1)
    {
        std::cerr << "The ACP fixture does not accept provider commands.\n";
        return 2;
    }
    std::wstring module(32768, L'\0');
    const auto length = GetModuleFileNameW(nullptr, module.data(), static_cast<DWORD>(module.size()));
    if (length == 0 || length >= module.size())
    {
        return 3;
    }
    module.resize(length);
    std::ifstream configuration{ std::filesystem::path{ module }.parent_path() / L"fixture-command.txt" };
    std::string encoded;
    std::getline(configuration, encoded);
    if (encoded.empty() || encoded.size() > 24000 ||
        encoded.find_first_not_of("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=") != std::string::npos)
    {
        std::cerr << "The ACP fixture requires its bounded encoded test command.\n";
        return 4;
    }
    auto command = L"pwsh.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand " +
                   std::wstring{ encoded.begin(), encoded.end() };
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
    startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
    startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    PROCESS_INFORMATION process{};
    if (!CreateProcessW(nullptr, command.data(), nullptr, nullptr, TRUE, CREATE_NO_WINDOW, nullptr, nullptr, &startup, &process))
    {
        return static_cast<int>(GetLastError());
    }
    CloseHandle(process.hThread);
    const auto waited = WaitForSingleObject(process.hProcess, INFINITE);
    DWORD code = 1;
    if (waited == WAIT_OBJECT_0)
    {
        GetExitCodeProcess(process.hProcess, &code);
    }
    CloseHandle(process.hProcess);
    return static_cast<int>(code);
}
