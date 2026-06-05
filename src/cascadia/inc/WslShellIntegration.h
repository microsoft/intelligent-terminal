// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// WslShellIntegration.h
//
// WSL flavor — per-distro bash shell integration. Reuses the Bash flavor
// entirely; the only WSL-specific work is:
//   1. Validate the distro name (strict allow-list — defends the
//      CreateProcessW command line against injection).
//   2. Probe $HOME inside the distro via one bounded wsl.exe spawn,
//      cached per-process so reconcile cycles after the first hit are
//      free.
//   3. Build a \\wsl$\<distro>\<wslHome>\... UNC path and delegate to
//      Bash::Install / Bash::Uninstall — ordinary fstream works
//      transparently over the WSL UNC mount.
//
// \\wsl$\ is Win10 1903+ (Build 18362); IT's WindowsTargetPlatformMinVersion
// is 10.0.18362.0, so this works on every supported host. The first
// access to \\wsl$\<dist>\ auto-starts the distro VM — so the $HOME
// probe pays the one-time cold-start cost.

#pragma once

#include "ShellIntegrationCommon.h"
#include "BashShellIntegration.h"

namespace Microsoft::Terminal::ShellIntegration::Wsl
{
    namespace details
    {
        // True for distro names that are safe to embed verbatim in a
        // `wsl.exe -d <name>` command line. WSL distro names allow
        // alphanumerics, `.`, `-`, `_`, and `+`; any other character
        // (especially `"`, `\`, `;`, `&`, `|`, newline, control bytes)
        // is rejected so we can never accidentally inject arbitrary
        // shell into the parent CreateProcessW command line.
        inline bool IsSafeDistroName(std::wstring_view name) noexcept
        {
            if (name.empty() || name.size() > 256)
            {
                return false;
            }
            for (const auto c : name)
            {
                const bool ok =
                    (c >= L'A' && c <= L'Z') ||
                    (c >= L'a' && c <= L'z') ||
                    (c >= L'0' && c <= L'9') ||
                    c == L'.' || c == L'-' || c == L'_' || c == L'+';
                if (!ok)
                {
                    return false;
                }
            }
            return true;
        }

        // True for posix-looking absolute paths we'd accept as a $HOME
        // reply from inside the distro. Rules:
        //   • must start with `/`
        //   • only ASCII letters, digits, `_`, `-`, `.`, `/`
        //   • no `//`, no trailing `/`
        //   • no segment composed entirely of `.` (rejects `..`, `.`,
        //     `...`, etc — fail-shut on traversal AND current-dir noise)
        inline bool IsSafeHome(std::string_view home) noexcept
        {
            if (home.size() < 2 || home.size() > 4096 || home.front() != '/' || home.back() == '/')
            {
                return false;
            }
            char prev = 0;
            for (const auto c : home)
            {
                const bool ok =
                    (c >= 'A' && c <= 'Z') ||
                    (c >= 'a' && c <= 'z') ||
                    (c >= '0' && c <= '9') ||
                    c == '_' || c == '-' || c == '.' || c == '/';
                if (!ok)
                {
                    return false;
                }
                if (c == '/' && prev == '/')
                {
                    return false;
                }
                prev = c;
            }
            // Reject any segment composed entirely of `.`.
            size_t segStart = 0;
            for (size_t i = 0; i <= home.size(); ++i)
            {
                if (i == home.size() || home[i] == '/')
                {
                    const auto seg = home.substr(segStart, i - segStart);
                    if (!seg.empty() && seg.find_first_not_of('.') == std::string_view::npos)
                    {
                        return false;
                    }
                    segStart = i + 1;
                }
            }
            return true;
        }

        // Spawn `wsl.exe -d <distName> -e bash -c 'echo $HOME'` and
        // return the trimmed POSIX home dir (e.g. "/home/yeelam"), or
        // "" on any failure (no WSL, distro stopped + can't auto-start,
        // bash missing, timeout, garbled output). Best-effort fail-shut:
        // empty return → caller skips this distro silently.
        //
        // `WSL_UTF8=1` forces wsl.exe to relay bash stdout as UTF-8
        // instead of UTF-16LE (the legacy conhost behavior), so we can
        // read raw bytes without an encoding probe.
        //
        // Bounded at 30s wall-clock to absorb cold-start (the first
        // wsl.exe invocation in a session spins up the WSL2 VM).
        inline std::string QueryWslHomeRaw(std::wstring_view distName) noexcept
        {
            if (!IsSafeDistroName(distName))
            {
                return {};
            }
            try
            {
                SECURITY_ATTRIBUTES sa{};
                sa.nLength = sizeof(sa);
                sa.bInheritHandle = TRUE;

                HANDLE rawRead = nullptr;
                HANDLE rawWrite = nullptr;
                if (!CreatePipe(&rawRead, &rawWrite, &sa, 0))
                {
                    return {};
                }
                wil::unique_handle readEnd{ rawRead };
                wil::unique_handle writeEnd{ rawWrite };
                SetHandleInformation(readEnd.get(), HANDLE_FLAG_INHERIT, 0);

                STARTUPINFOW si{};
                si.cb = sizeof(si);
                si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
                si.wShowWindow = SW_HIDE;
                si.hStdOutput = writeEnd.get();
                si.hStdError = writeEnd.get();
                si.hStdInput = GetStdHandle(STD_INPUT_HANDLE);

                std::wstring sysDir;
                if (FAILED(wil::GetSystemDirectoryW<std::wstring>(sysDir)))
                {
                    return {};
                }

                std::wstring cmdLine{ L"\"" };
                cmdLine += sysDir;
                cmdLine += L"\\wsl.exe\" -d ";
                cmdLine.append(distName);
                cmdLine += L" -e bash -c \"echo $HOME\"";

                // Pass WSL_UTF8=1 via a child-only environment block
                // instead of mutating the process-wide environment.
                // SetEnvironmentVariableW would let other threads in
                // this process inherit WSL_UTF8=1 in any CreateProcess*
                // they spawn during the 30-second window below — even
                // a narrowed Set/restore window cannot eliminate that
                // race. The child env block we hand to CreateProcessW
                // via lpEnvironment is private to the wsl.exe spawn:
                // no other thread sees WSL_UTF8, and we don't need
                // any save/restore dance.
                //
                // WSL_UTF8=1 makes wsl.exe relay child stdout as UTF-8
                // (otherwise the default is UTF-16LE for newer WSL).
                //
                // Fallback: if GetEnvironmentStringsW() fails (very rare
                // — only under low-memory pressure) we cannot safely
                // build a child env block (passing one with only
                // WSL_UTF8=1 would drop SystemRoot/Path and break the
                // wsl.exe spawn). In that case, pass nullptr to inherit
                // the parent env without the WSL_UTF8 override. Newer
                // WSL may then emit UTF-16LE stdout; the IsSafeHome
                // validator below rejects that and we return empty
                // (same as any other probe failure), which the cache
                // policy treats as retryable.
                bool useChildEnv = false;
                std::wstring childEnv;
                if (wchar_t* origEnvBlock = GetEnvironmentStringsW())
                {
                    bool wslUtf8Replaced = false;
                    for (wchar_t* p = origEnvBlock; *p != L'\0'; )
                    {
                        const std::wstring_view entry{ p };
                        p += entry.size() + 1;
                        // Strip any existing WSL_UTF8=... so we never
                        // emit two definitions. Case-insensitive match
                        // on the leading name (Windows env names are
                        // case-insensitive).
                        if (entry.size() >= 9 &&
                            _wcsnicmp(entry.data(), L"WSL_UTF8=", 9) == 0)
                        {
                            wslUtf8Replaced = true;
                            childEnv.append(L"WSL_UTF8=1");
                        }
                        else
                        {
                            childEnv.append(entry);
                        }
                        childEnv.push_back(L'\0');
                    }
                    FreeEnvironmentStringsW(origEnvBlock);
                    if (!wslUtf8Replaced)
                    {
                        childEnv.append(L"WSL_UTF8=1");
                        childEnv.push_back(L'\0');
                    }
                    childEnv.push_back(L'\0'); // env block terminates on \0\0
                    useChildEnv = true;
                }

                PROCESS_INFORMATION pi{};
                const DWORD creationFlags = CREATE_NO_WINDOW |
                                            (useChildEnv ? CREATE_UNICODE_ENVIRONMENT : 0u);
                const bool spawnOk = CreateProcessW(nullptr,
                                                    cmdLine.data(),
                                                    nullptr,
                                                    nullptr,
                                                    TRUE,
                                                    creationFlags,
                                                    useChildEnv ? childEnv.data() : nullptr,
                                                    nullptr,
                                                    &si,
                                                    &pi) != FALSE;
                if (!spawnOk)
                {
                    return {};
                }
                wil::unique_handle process{ pi.hProcess };
                wil::unique_handle thread{ pi.hThread };

                writeEnd.reset();

                if (WaitForSingleObject(process.get(), 30000) != WAIT_OBJECT_0)
                {
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
                    return {};
                }

                DWORD exitCode = 0;
                if (!GetExitCodeProcess(process.get(), &exitCode) || exitCode != 0)
                {
                    return {};
                }

                std::string raw;
                char buf[256];
                DWORD bytesRead = 0;
                while (raw.size() < 4096 &&
                       ReadFile(readEnd.get(), buf, sizeof(buf), &bytesRead, nullptr) &&
                       bytesRead > 0)
                {
                    raw.append(buf, bytesRead);
                }

                while (!raw.empty() &&
                       (raw.back() == '\n' || raw.back() == '\r' ||
                        raw.back() == ' ' || raw.back() == '\t'))
                {
                    raw.pop_back();
                }
                if (const auto lastLf = raw.find_last_of('\n'); lastLf != std::string::npos)
                {
                    raw.erase(0, lastLf + 1);
                }

                if (!IsSafeHome(raw))
                {
                    return {};
                }
                return raw;
            }
            catch (...)
            {
                return {};
            }
        }

        // Per-process cache of distName → $HOME, populated lazily on
        // first access. Each distro pays the cold-start cost at most
        // once per process per successful probe. Only successful (non-
        // empty) probes are cached: a failed probe (distro stopped,
        // transient WSL startup race, or `$HOME` not readable) is NOT
        // memoized, so a fresh Install/Uninstall call from the user
        // (e.g. via Settings UI or FRE retry) re-probes from scratch
        // — the user can recover from transient failures without
        // restarting Windows Terminal. Reconcile runs only on settings
        // changes, not in a tight loop, so this won't thrash on a
        // legitimately stopped distro.
        // Mutex protects both the map AND each individual probe so two
        // racing callers for the same distro don't both spawn wsl.exe.
        inline std::string GetWslHomeCached(std::wstring_view distName)
        {
            static std::mutex cacheMu;
            static std::map<std::wstring, std::string, std::less<>> cache;

            std::lock_guard<std::mutex> g{ cacheMu };
            if (const auto it = cache.find(distName); it != cache.end())
            {
                return it->second;
            }
            auto home = QueryWslHomeRaw(distName);
            // Only cache successful probes. Failed probes (empty) are
            // re-attempted on the next call — see the comment above for
            // why this is the right trade-off.
            if (!home.empty())
            {
                cache.emplace(std::wstring{ distName }, home);
            }
            return home;
        }
    }

    // Build a Win32 UNC path for an arbitrary POSIX path inside a WSL
    // distro: \\wsl$\<distName>\<posixPath-with-forward-slashes-converted-to-backslashes>.
    // Both forward and backslash separators after the distro name are
    // accepted by the Win32 file APIs (the \\wsl$\ provider routes the
    // lookup through the distro's vfs either way), but we emit the
    // canonical backslash form so the produced path matches what other
    // Windows tools display and so it sails through any consumer that
    // treats them differently. A leading `/` in the posix path is
    // trimmed to avoid producing `\\wsl$\Ubuntu\\path` (double-sep).
    inline std::wstring UncPath(std::wstring_view distName, std::string_view posixPath)
    {
        std::wstring out{ L"\\\\wsl$\\" };
        out.append(distName);
        if (!posixPath.empty() && posixPath.front() == '/')
        {
            posixPath.remove_prefix(1);
        }
        out.push_back(L'\\');
        for (const auto c : posixPath)
        {
            out.push_back(c == '/' ? L'\\' : static_cast<wchar_t>(static_cast<unsigned char>(c)));
        }
        return out;
    }

    // Install bash shell integration into a WSL distro.
    //
    // Flow:
    //   1. Validate distro name (allow-list).
    //   2. Probe $HOME inside the distro (cached per-process).
    //   3. Build UNC paths for `.bashrc` and `.intelligent-terminal/`.
    //   4. Delegate to Bash::Install — all the heavy lifting is shared.
    //
    // Synchronous — call from a background thread. The first call for
    // each distro can block up to 30s on a cold-start; subsequent calls
    // return immediately from the cache.
    inline InstallResult Install(const std::wstring& distName)
    {
        if (!details::IsSafeDistroName(distName))
        {
            return { false, false, L"WSL distro name rejected (unsafe characters)" };
        }
        const auto home = details::GetWslHomeCached(distName);
        if (home.empty())
        {
            return { false, false, L"Could not probe $HOME inside WSL distro" };
        }
        const auto bashrcPath = UncPath(distName, home + "/.bashrc");
        const auto scriptDir = UncPath(distName, home + "/.intelligent-terminal");
        return Bash::Install(bashrcPath, scriptDir);
    }

    inline InstallResult Uninstall(const std::wstring& distName)
    {
        if (!details::IsSafeDistroName(distName))
        {
            return { false, false, L"WSL distro name rejected (unsafe characters)" };
        }
        const auto home = details::GetWslHomeCached(distName);
        if (home.empty())
        {
            // If we can't reach the distro there's nothing to remove —
            // treat as already-uninstalled so a toggle-off reconcile
            // doesn't flap into an error state every time the distro
            // is down.
            return { true, true, {} };
        }
        const auto bashrcPath = UncPath(distName, home + "/.bashrc");
        return Bash::Uninstall(bashrcPath);
    }
}
