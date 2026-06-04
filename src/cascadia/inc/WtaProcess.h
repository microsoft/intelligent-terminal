// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// WtaProcess.h
//
// Shared utilities for locating and spawning wta.exe from C++ code.
// Lives in src/cascadia/inc/ so both TerminalApp (FreOverlay) and
// TerminalSettingsEditor (AIAgentsViewModel) can use them without
// duplicating logic.
//
// Pure Win32 + STL, no WinRT dependency.

#pragma once

#include <filesystem>
#include <string>

namespace Microsoft::Terminal::WtaProcess
{
    // Locate wta.exe using the same strategy as TerminalPage::_DetectWtaPath:
    //   1. Co-located next to the running module (MSIX / packaged)
    //   2. Walk up from module dir looking for tools/wta/target/{debug,release}/wta.exe (dev build)
    //   3. SearchPathW fallback
    inline std::wstring ResolveWtaExePath()
    {
        const auto modulePath = std::filesystem::path{ wil::GetModuleFileNameW<std::wstring>(nullptr) };
        const auto moduleDir = modulePath.parent_path();
        std::error_code ec;

        // 1. Co-located
        {
            const auto sibling = moduleDir / L"wta.exe";
            if (std::filesystem::exists(sibling, ec))
            {
                return sibling.lexically_normal().wstring();
            }
        }

        // 2. Dev-tree walk. Relative path must match the canonical layout
        //    `tools\wta\target\<profile>\wta.exe` produced by
        //    `cargo build --manifest-path tools\wta\Cargo.toml` and
        //    matched by TerminalPage::_DetectWtaPath. Keep this list in
        //    sync with that function — diverging here silently makes
        //    `wta hooks status` / FRE "Install hooks" fall through to
        //    the PATH fallback in dev (unpackaged) builds.
        auto cursor = moduleDir;
        while (!cursor.empty())
        {
            for (const auto& relative : {
                     std::filesystem::path{ L"tools\\wta\\target\\debug\\wta.exe" },
                     std::filesystem::path{ L"tools\\wta\\target\\release\\wta.exe" },
                 })
            {
                const auto candidate = cursor / relative;
                if (std::filesystem::exists(candidate, ec))
                {
                    return candidate.lexically_normal().wstring();
                }
            }
            const auto parent = cursor.parent_path();
            if (parent == cursor) break;
            cursor = parent;
        }

        // 3. PATH fallback
        wchar_t buffer[MAX_PATH];
        if (SearchPathW(nullptr, L"wta", L".exe", MAX_PATH, buffer, nullptr) > 0)
        {
            return std::wstring{ buffer };
        }
        return {};
    }

    // Spawn `wta.exe <argsAfterExe>` and return its stdout on exit-0;
    // empty string otherwise. Synchronous — call from a background thread.
    // Optionally accepts a custom environment block (double-null-terminated
    // wide string); pass nullptr to inherit the current environment.
    inline std::string RunWtaCaptureStdout(const std::wstring& wtaPath,
                                           const std::wstring& argsAfterExe,
                                           DWORD timeoutMs,
                                           wchar_t* envBlock = nullptr)
    {
        if (wtaPath.empty())
        {
            return {};
        }

        SECURITY_ATTRIBUTES sa{};
        sa.nLength = sizeof(sa);
        sa.bInheritHandle = TRUE;

        wil::unique_handle readHandle;
        wil::unique_handle writeHandle;
        if (!CreatePipe(readHandle.addressof(), writeHandle.addressof(), &sa, 0))
        {
            return {};
        }
        if (!SetHandleInformation(readHandle.get(), HANDLE_FLAG_INHERIT, 0))
        {
            return {};
        }

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW | STARTF_USESTDHANDLES;
        si.wShowWindow = SW_HIDE;
        si.hStdOutput = writeHandle.get();
        si.hStdError = writeHandle.get();
        si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);

        std::wstring cmdline = L"\"" + wtaPath + L"\" " + argsAfterExe;
        std::wstring mutableCmd = cmdline;

        DWORD flags = CREATE_NO_WINDOW;
        if (envBlock)
        {
            flags |= CREATE_UNICODE_ENVIRONMENT;
        }

        PROCESS_INFORMATION pi{};
        const BOOL launched = CreateProcessW(
            wtaPath.c_str(),
            mutableCmd.data(),
            nullptr,
            nullptr,
            TRUE, // inherit handles
            flags,
            envBlock,
            nullptr,
            &si,
            &pi);
        if (!launched)
        {
            return {};
        }
        wil::unique_handle proc{ pi.hProcess };
        wil::unique_handle thread{ pi.hThread };

        // Close write end so pipe sees EOF when child exits (if no grandchildren).
        writeHandle.reset();

        // Poll loop: drain available data, then wait for process exit.
        // A plain blocking ReadFile-until-EOF would hang if grandchildren
        // (e.g. npx → node) inherit the pipe write end and outlive the child.
        std::string captured;
        captured.reserve(4096);
        char buf[4096];

        const auto drainAvailable = [&]() {
            for (;;)
            {
                DWORD available = 0;
                if (!PeekNamedPipe(readHandle.get(), nullptr, 0, nullptr, &available, nullptr) || available == 0)
                    break;
                DWORD bytesRead = 0;
                if (!ReadFile(readHandle.get(), buf, (std::min)(available, (DWORD)sizeof(buf)), &bytesRead, nullptr) || bytesRead == 0)
                    break;
                captured.append(buf, bytesRead);
            }
        };

        const DWORD startTick = GetTickCount();
        for (;;)
        {
            drainAvailable();
            if (WaitForSingleObject(proc.get(), 50) == WAIT_OBJECT_0)
            {
                drainAvailable();
                break;
            }
            if (GetTickCount() - startTick > timeoutMs)
            {
                TerminateProcess(proc.get(), 1);
                WaitForSingleObject(proc.get(), 1000);
                return {};
            }
        }
        DWORD exitCode = 1;
        GetExitCodeProcess(proc.get(), &exitCode);
        if (exitCode != 0)
        {
            return {};
        }
        return captured;
    }

    // Build an environment block that extends PATH with WinGet Links and npm
    // global directories. Needed after a fresh winget install so newly-installed
    // CLIs are discoverable by child processes. Returns a double-null-terminated
    // wide string suitable for CreateProcessW's lpEnvironment parameter.
    inline std::wstring BuildExtendedPathEnvBlock()
    {
        std::wstring envBlock;
        auto currentEnv = GetEnvironmentStringsW();
        if (!currentEnv) return {};

        std::wstring newPath;
        const wchar_t* q = currentEnv;
        while (*q)
        {
            auto varLen = wcslen(q);
            std::wstring_view var{ q, varLen };
            if (var.substr(0, 5) == L"PATH=" || var.substr(0, 5) == L"Path=")
            {
                newPath = std::wstring(var);
                wchar_t localAppData[MAX_PATH]{};
                wchar_t appData[MAX_PATH]{};
                GetEnvironmentVariableW(L"LOCALAPPDATA", localAppData, MAX_PATH);
                GetEnvironmentVariableW(L"APPDATA", appData, MAX_PATH);
                if (localAppData[0])
                {
                    newPath += L";";
                    newPath += localAppData;
                    newPath += L"\\Microsoft\\WinGet\\Links";
                }
                if (appData[0])
                {
                    newPath += L";";
                    newPath += appData;
                    newPath += L"\\npm";
                }
            }
            else
            {
                envBlock += var;
                envBlock += L'\0';
            }
            q += varLen + 1;
        }
        if (!newPath.empty())
        {
            envBlock += newPath;
            envBlock += L'\0';
        }
        envBlock += L'\0'; // double-null terminator
        FreeEnvironmentStringsW(currentEnv);
        return envBlock;
    }

    // Merge registry PATH entries into the current process's PATH.
    // Reads system + user PATH from the registry, then appends any
    // directories not already present. Preserves session-specific
    // entries inherited from the parent process.
    // Pure Win32 + std::wstring — no til/env or WinRT dependency.
    inline void RefreshProcessPath()
    {
        auto readRegPath = [](HKEY root, const wchar_t* subkey) -> std::wstring {
            HKEY hk{};
            if (RegOpenKeyExW(root, subkey, 0, KEY_READ, &hk) != ERROR_SUCCESS)
                return {};
            DWORD size = 0, kind = 0;
            if (RegQueryValueExW(hk, L"Path", nullptr, &kind, nullptr, &size) != ERROR_SUCCESS || size == 0)
            {
                RegCloseKey(hk);
                return {};
            }
            // Only process string types
            if (kind != REG_SZ && kind != REG_EXPAND_SZ)
            {
                RegCloseKey(hk);
                return {};
            }
            // Round up to whole wchar_t count to guard against odd byte sizes
            const DWORD wcharCount = (size + sizeof(wchar_t) - 1) / sizeof(wchar_t);
            std::wstring buf(wcharCount, L'\0');
            if (RegQueryValueExW(hk, L"Path", nullptr, &kind,
                                 reinterpret_cast<BYTE*>(buf.data()), &size) != ERROR_SUCCESS)
            {
                RegCloseKey(hk);
                return {};
            }
            RegCloseKey(hk);
            // Trim trailing null(s)
            while (!buf.empty() && buf.back() == L'\0')
                buf.pop_back();
            // Expand %VAR% references if REG_EXPAND_SZ
            if (kind == REG_EXPAND_SZ)
            {
                DWORD needed = ExpandEnvironmentStringsW(buf.c_str(), nullptr, 0);
                if (needed > 0)
                {
                    std::wstring expanded(needed, L'\0');
                    DWORD written = ExpandEnvironmentStringsW(buf.c_str(), expanded.data(), needed);
                    if (written > 0 && written <= needed)
                    {
                        while (!expanded.empty() && expanded.back() == L'\0')
                            expanded.pop_back();
                        return expanded;
                    }
                    // Expansion failed — fall through to return unexpanded buf
                }
            }
            return buf;
        };

        // Case-insensitive check: is `entry` already somewhere in
        // the semicolon-delimited `path`?
        auto pathContains = [](const std::wstring& path, const std::wstring& entry) -> bool {
            if (entry.empty())
                return true;
            // Walk the semicolon-delimited string without allocating
            size_t start = 0;
            while (start <= path.size())
            {
                auto pos = path.find(L';', start);
                if (pos == std::wstring::npos)
                    pos = path.size();
                if (pos - start == entry.size() &&
                    _wcsnicmp(path.c_str() + start, entry.c_str(), entry.size()) == 0)
                {
                    return true;
                }
                start = pos + 1;
            }
            return false;
        };

        auto sysPath = readRegPath(HKEY_LOCAL_MACHINE,
                                   LR"(SYSTEM\CurrentControlSet\Control\Session Manager\Environment)");
        auto usrPath = readRegPath(HKEY_CURRENT_USER, L"Environment");

        // Get current process PATH
        wchar_t currentBuf[32767]{};
        GetEnvironmentVariableW(L"PATH", currentBuf, 32767);
        std::wstring currentPath{ currentBuf };

        // Append registry entries not already in the process PATH
        bool changed = false;
        auto mergeFrom = [&](const std::wstring& regPath) {
            size_t start = 0;
            while (start < regPath.size())
            {
                auto pos = regPath.find(L';', start);
                if (pos == std::wstring::npos)
                    pos = regPath.size();
                auto entry = regPath.substr(start, pos - start);
                if (!entry.empty() && !pathContains(currentPath, entry))
                {
                    if (!currentPath.empty() && currentPath.back() != L';')
                        currentPath += L';';
                    currentPath += entry;
                    changed = true;
                }
                start = pos + 1;
            }
        };

        mergeFrom(sysPath);
        mergeFrom(usrPath);

        if (changed)
        {
            SetEnvironmentVariableW(L"PATH", currentPath.c_str());
        }
    }

    // Spawn `wta.exe <argsAfterExe>` and wait for completion (no stdout capture).
    // Returns true on exit-0.
    inline bool RunWtaAndWait(const std::wstring& wtaPath,
                              const std::wstring& argsAfterExe,
                              DWORD timeoutMs,
                              wchar_t* envBlock = nullptr)
    {
        if (wtaPath.empty())
        {
            return false;
        }

        std::wstring cmdline = L"\"" + wtaPath + L"\" " + argsAfterExe;
        std::wstring mutableCmd = cmdline;

        DWORD flags = CREATE_NO_WINDOW;
        if (envBlock)
        {
            flags |= CREATE_UNICODE_ENVIRONMENT;
        }

        STARTUPINFOW si{};
        si.cb = sizeof(si);
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_HIDE;
        PROCESS_INFORMATION pi{};

        if (!CreateProcessW(nullptr, mutableCmd.data(), nullptr, nullptr, FALSE,
                            flags, envBlock, nullptr, &si, &pi))
        {
            return false;
        }
        wil::unique_handle proc{ pi.hProcess };
        wil::unique_handle thread{ pi.hThread };

        if (WaitForSingleObject(proc.get(), timeoutMs) != WAIT_OBJECT_0)
        {
            TerminateProcess(proc.get(), 1);
            WaitForSingleObject(proc.get(), 1000);
            return false;
        }
        DWORD exitCode = 1;
        GetExitCodeProcess(proc.get(), &exitCode);
        return exitCode == 0;
    }
}
