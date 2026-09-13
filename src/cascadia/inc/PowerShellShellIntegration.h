// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PowerShellShellIntegration.h
//
// PowerShell flavor of the shell integration installer. Exposes two
// concrete IShellFlavor classes — PowerShellFlavor (pwsh / PS 7+) and
// WindowsPowerShellFlavor (PS 5.1) — that the orchestrator drives.
//
// The two hosts use different $PROFILE subdirs (Documents/PowerShell vs
// Documents/WindowsPowerShell) but share script content, block format,
// marker recognizers, and the v0→v1 legacy dot-source migration path.

#pragma once

#include "ShellIntegrationCommon.h"

#include <algorithm>

namespace Microsoft::Terminal::ShellIntegration::Powershell
{
    enum class ExecutionPolicyStatus
    {
        Absent,
        Allowed,
        Blocked,
        Unknown,
    };

    struct PowerShellProcessResult
    {
        bool launched{ false };
        bool timedOut{ false };
        DWORD error{ ERROR_SUCCESS };
        DWORD exitCode{ STILL_ACTIVE };
        std::wstring output;
    };

    struct ExecutionPolicyProbeResult
    {
        Target target{ Target::Pwsh };
        ExecutionPolicyStatus status{ ExecutionPolicyStatus::Unknown };
        std::wstring executablePath;
        std::wstring policy;
        PowerShellProcessResult process;
    };

    struct ExecutionPolicyRemediationResult
    {
        ExecutionPolicyProbeResult pwshBefore;
        ExecutionPolicyProbeResult windowsPowerShellBefore;
        ExecutionPolicyProbeResult pwshAfter;
        ExecutionPolicyProbeResult windowsPowerShellAfter;
        PowerShellProcessResult pwshSetter;
        PowerShellProcessResult windowsPowerShellSetter;
        bool attempted{ false };
        bool verificationAttempted{ false };
        bool succeeded{ true };
    };

    namespace details
    {
        inline constexpr std::wstring_view QueryExecutionPolicyArguments{
            L"-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "
            L"Import-Module Microsoft.PowerShell.Security;"
            L"Remove-Item Env:PSExecutionPolicyPreference -ErrorAction SilentlyContinue;"
            L"Get-ExecutionPolicy"
        };
        inline constexpr std::wstring_view EnableRemoteSignedArguments{
            L"-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "
            L"Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned -Force -ErrorAction SilentlyContinue;"
            L"if((Get-ExecutionPolicy -Scope CurrentUser) -eq 'RemoteSigned'){exit 0}else{exit 1}"
        };

        inline std::wstring ParseExecutionPolicyOutput(std::string_view raw)
        {
            std::wstring result;
            for (const char c : raw)
            {
                if (c == '\r' || c == '\n')
                {
                    if (!result.empty())
                    {
                        break;
                    }
                    continue;
                }
                if (c >= 'A' && c <= 'Z')
                {
                    result.push_back(static_cast<wchar_t>(c + 0x20));
                }
                else if (c >= 'a' && c <= 'z')
                {
                    result.push_back(static_cast<wchar_t>(c));
                }
            }
            return result;
        }

        // Resolve pwsh only from explicit absolute PATH entries. Empty and
        // relative entries can resolve through the process current directory,
        // which must not select the executable that runs policy-management code.
        inline std::wstring ResolvePwshExecutableFromExplicitPath(std::wstring_view searchPath,
                                                                  std::wstring_view currentDirectory,
                                                                  DWORD* outError = nullptr) noexcept
        {
            if (outError)
            {
                *outError = ERROR_FILE_NOT_FOUND;
            }

            try
            {
                const std::filesystem::path currentPath{ currentDirectory };
                size_t entryStart = 0;
                while (entryStart <= searchPath.size())
                {
                    const auto separator = searchPath.find(L';', entryStart);
                    auto entry = searchPath.substr(
                        entryStart,
                        separator == std::wstring_view::npos ? std::wstring_view::npos : separator - entryStart);

                    const auto first = entry.find_first_not_of(L" \t");
                    if (first != std::wstring_view::npos)
                    {
                        const auto last = entry.find_last_not_of(L" \t");
                        entry = entry.substr(first, last - first + 1);
                        if (entry.size() >= 2 && entry.front() == L'"' && entry.back() == L'"')
                        {
                            entry = entry.substr(1, entry.size() - 2);
                        }
                        else if (entry.front() == L'"' || entry.back() == L'"')
                        {
                            entry = {};
                        }
                    }
                    else
                    {
                        entry = {};
                    }

                    if (!entry.empty())
                    {
                        auto expanded = wil::ExpandEnvironmentStringsW<std::wstring>(std::wstring{ entry }.c_str());
                        std::filesystem::path directory{ expanded };
                        if (directory.is_absolute())
                        {
                            directory = directory.lexically_normal();
                            std::error_code ec;
                            const bool isCurrentDirectory =
                                !currentPath.empty() &&
                                std::filesystem::equivalent(directory, currentPath, ec) &&
                                !ec;
                            if (!isCurrentDirectory)
                            {
                                const auto candidate = (directory / L"pwsh.exe").lexically_normal();
                                const auto attributes = GetFileAttributesW(candidate.c_str());
                                if (attributes != INVALID_FILE_ATTRIBUTES &&
                                    WI_IsFlagClear(attributes, FILE_ATTRIBUTE_DIRECTORY))
                                {
                                    if (outError)
                                    {
                                        *outError = ERROR_SUCCESS;
                                    }
                                    return candidate.wstring();
                                }
                            }
                        }
                    }

                    if (separator == std::wstring_view::npos)
                    {
                        break;
                    }
                    entryStart = separator + 1;
                }
            }
            catch (...)
            {
                if (outError)
                {
                    *outError = ERROR_UNHANDLED_EXCEPTION;
                }
            }
            return {};
        }

        inline std::wstring ResolvePwshExecutableFromExplicitPath(DWORD* outError = nullptr) noexcept
        {
            if (outError)
            {
                *outError = ERROR_SUCCESS;
            }

            try
            {
                SetLastError(ERROR_SUCCESS);
                const DWORD pathLength = GetEnvironmentVariableW(L"PATH", nullptr, 0);
                if (pathLength == 0)
                {
                    const auto error = GetLastError();
                    if (outError)
                    {
                        *outError = error == ERROR_SUCCESS ? ERROR_FILE_NOT_FOUND : error;
                    }
                    return {};
                }

                std::wstring searchPath(pathLength, L'\0');
                const DWORD copiedPathLength = GetEnvironmentVariableW(L"PATH", searchPath.data(), pathLength);
                if (copiedPathLength == 0 || copiedPathLength >= pathLength)
                {
                    if (outError)
                    {
                        *outError = copiedPathLength == 0 ? GetLastError() : ERROR_INSUFFICIENT_BUFFER;
                    }
                    return {};
                }
                searchPath.resize(copiedPathLength);

                const DWORD currentDirectoryLength = GetCurrentDirectoryW(0, nullptr);
                if (currentDirectoryLength == 0)
                {
                    if (outError)
                    {
                        *outError = GetLastError();
                    }
                    return {};
                }
                std::wstring currentDirectory(currentDirectoryLength, L'\0');
                const DWORD copiedCurrentDirectoryLength =
                    GetCurrentDirectoryW(currentDirectoryLength, currentDirectory.data());
                if (copiedCurrentDirectoryLength == 0 ||
                    copiedCurrentDirectoryLength >= currentDirectoryLength)
                {
                    if (outError)
                    {
                        *outError = copiedCurrentDirectoryLength == 0 ?
                                        GetLastError() :
                                        ERROR_INSUFFICIENT_BUFFER;
                    }
                    return {};
                }
                currentDirectory.resize(copiedCurrentDirectoryLength);

                return ResolvePwshExecutableFromExplicitPath(searchPath, currentDirectory, outError);
            }
            catch (...)
            {
                if (outError)
                {
                    *outError = ERROR_UNHANDLED_EXCEPTION;
                }
                return {};
            }
        }

        inline PowerShellProcessResult RunPowerShellCommand(LPCWSTR exe,
                                                            std::wstring_view arguments,
                                                            DWORD timeoutMs = 20000) noexcept
        {
            PowerShellProcessResult result;
            try
            {
                SECURITY_ATTRIBUTES sa{};
                sa.nLength = sizeof(sa);
                sa.bInheritHandle = TRUE;

                HANDLE rawRead = nullptr;
                HANDLE rawWrite = nullptr;
                if (!CreatePipe(&rawRead, &rawWrite, &sa, 0))
                {
                    result.error = GetLastError();
                    return result;
                }
                wil::unique_handle readEnd{ rawRead };
                wil::unique_handle writeEnd{ rawWrite };
                if (!SetHandleInformation(readEnd.get(), HANDLE_FLAG_INHERIT, 0))
                {
                    result.error = GetLastError();
                    return result;
                }

                wil::unique_handle nullInput{
                    CreateFileW(L"NUL",
                                GENERIC_READ,
                                FILE_SHARE_READ | FILE_SHARE_WRITE,
                                &sa,
                                OPEN_EXISTING,
                                FILE_ATTRIBUTE_NORMAL,
                                nullptr)
                };
                if (!nullInput)
                {
                    result.error = GetLastError();
                    return result;
                }

                STARTUPINFOW si{};
                si.cb = sizeof(si);
                si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
                si.wShowWindow = SW_HIDE;
                si.hStdOutput = writeEnd.get();
                si.hStdError = writeEnd.get();
                si.hStdInput = nullInput.get();

                std::wstring cmdLine{ L"\"" };
                cmdLine += exe;
                cmdLine += L"\" ";
                cmdLine += arguments;

                PROCESS_INFORMATION pi{};
                if (!CreateProcessW(exe,
                                    cmdLine.data(),
                                    nullptr,
                                    nullptr,
                                    TRUE,
                                    CREATE_NO_WINDOW,
                                    nullptr,
                                    nullptr,
                                    &si,
                                    &pi))
                {
                    result.error = GetLastError();
                    return result;
                }
                result.launched = true;
                wil::unique_handle process{ pi.hProcess };
                wil::unique_handle thread{ pi.hThread };

                writeEnd.reset();
                nullInput.reset();

                std::string raw;
                const auto drainAvailableOutput = [&]() {
                    for (;;)
                    {
                        DWORD available = 0;
                        if (!PeekNamedPipe(readEnd.get(), nullptr, 0, nullptr, &available, nullptr) || available == 0)
                        {
                            break;
                        }

                        char buffer[256];
                        DWORD bytesRead = 0;
                        const DWORD toRead = std::min<DWORD>(available, sizeof(buffer));
                        if (!ReadFile(readEnd.get(), buffer, toRead, &bytesRead, nullptr) || bytesRead == 0)
                        {
                            break;
                        }
                        if (raw.size() < 4096)
                        {
                            raw.append(buffer, std::min<size_t>(bytesRead, 4096 - raw.size()));
                        }
                    }
                };

                const auto start = GetTickCount64();
                DWORD waitResult = WAIT_TIMEOUT;
                for (;;)
                {
                    drainAvailableOutput();
                    waitResult = WaitForSingleObject(process.get(), 25);
                    if (waitResult == WAIT_OBJECT_0)
                    {
                        break;
                    }
                    if (waitResult == WAIT_FAILED)
                    {
                        result.error = GetLastError();
                        break;
                    }
                    if (GetTickCount64() - start >= timeoutMs)
                    {
                        result.timedOut = true;
                        break;
                    }
                }

                if (waitResult != WAIT_OBJECT_0)
                {
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
                }
                drainAvailableOutput();

                if (!GetExitCodeProcess(process.get(), &result.exitCode))
                {
                    result.error = GetLastError();
                }
                result.output = ParseExecutionPolicyOutput(raw);
            }
            catch (...)
            {
                result.error = ERROR_UNHANDLED_EXCEPTION;
            }
            return result;
        }

        // Runs PowerShell with a Process-scope Bypass so its policy-management
        // module remains loadable even when the effective policy is AllSigned or
        // Restricted. After loading the module, the command removes the temporary
        // Process override and asks PowerShell for the real effective policy,
        // including its built-in default when every persistent scope is Undefined.
        // Returns an EMPTY string if it could not be determined — CreateProcess
        // failed, the host isn't installed, or the child timed out. Empty means
        // "unknown / probe failed", NOT "blocked" (the caller fails open on empty).
        //
        // `outTimedOut`, when provided, is set to true iff the wait hit the
        // timeout (vs. a CreateProcess/pipe failure) — for diagnostic logging.
        //
        // Timeout = 20s: the probe spawns a PowerShell host, and its COLD START
        // can take many seconds when the machine is busy — which is exactly the
        // FRE Save case (concurrent winget pre-warm + agent-hook install + the
        // other host's probe). The previous 5s budget was hit under that load,
        // the killed probe returned empty, and an empty result used to be misread
        // as "blocked", false-stopping FRE completion. 20s comfortably covers a
        // loaded cold start while still bounding the FRE Save so it can't hang.
        //
        // Without the bootstrap Bypass, PowerShell 7 can reject its own type-data
        // files before Get-ExecutionPolicy is available under AllSigned.
        inline std::wstring QueryExecutionPolicy(LPCWSTR exe, bool* outTimedOut = nullptr) noexcept
        {
            std::wstring resolved{ exe };
            if (resolved.find_first_of(L"\\/") == std::wstring::npos)
            {
                if (_wcsicmp(exe, L"pwsh") == 0 || _wcsicmp(exe, L"pwsh.exe") == 0)
                {
                    resolved = ResolvePwshExecutableFromExplicitPath();
                }
                else
                {
                    wchar_t buffer[MAX_PATH]{};
                    const DWORD resolvedLen = SearchPathW(nullptr, exe, nullptr, MAX_PATH, buffer, nullptr);
                    if (resolvedLen != 0 && resolvedLen < MAX_PATH)
                    {
                        resolved.assign(buffer, resolvedLen);
                    }
                    else
                    {
                        resolved.clear();
                    }
                }
                if (resolved.empty())
                {
                    if (outTimedOut)
                    {
                        *outTimedOut = false;
                    }
                    return {};
                }
            }

            const auto result = RunPowerShellCommand(
                resolved.c_str(),
                QueryExecutionPolicyArguments);
            if (outTimedOut)
            {
                *outTimedOut = result.timedOut;
            }
            return result.output;
        }

        inline bool PolicyNameBlocksUnsignedScripts(std::wstring_view name) noexcept
        {
            // Block only the two effective policies that actually refuse to run
            // unsigned local scripts — Restricted and AllSigned. Everything else
            // permits our (unsigned) shell-integration $PROFILE block to load:
            // RemoteSigned / Unrestricted / Bypass, the "undefined" no-restriction
            // marker, AND an empty/unknown result from an inconclusive probe (a
            // probe failure is NOT a restrictive policy, so it must not block).
            //
            // This is the contract the ShellIntegrationTests PolicyName_* unit
            // tests assert — the earlier allow-list form ("block unless
            // RemoteSigned/Unrestricted/Bypass") contradicted them by treating "",
            // "undefined" and unknown values as blocking.
            return name == L"restricted" || name == L"allsigned";
        }

        inline ExecutionPolicyStatus ClassifyExecutionPolicy(std::wstring_view policy) noexcept
        {
            if (policy.empty())
            {
                return ExecutionPolicyStatus::Unknown;
            }
            if (PolicyNameBlocksUnsignedScripts(policy))
            {
                return ExecutionPolicyStatus::Blocked;
            }
            if (policy == L"remotesigned" ||
                policy == L"unrestricted" ||
                policy == L"bypass" ||
                policy == L"undefined")
            {
                return ExecutionPolicyStatus::Allowed;
            }
            return ExecutionPolicyStatus::Unknown;
        }

        // Body-line recognizer for orphan-marker recovery — matches the
        // exact line prefixes the block builder emits. The `$__it_si`
        // / `Remove-Variable __it_si` prefixes are private to us so
        // collisions with user content are not a realistic concern.
        inline bool IsOrphanBodyLine(std::string_view candidate) noexcept
        {
            constexpr std::array<std::string_view, 6> bodyPrefixes = {
                std::string_view{ "# Auto-generated by Intelligent Terminal" },
                std::string_view{ "# Documents is resolved at runtime" },
                std::string_view{ "# Folder Move and is a silent no-op" },
                std::string_view{ "$__it_si " },
                std::string_view{ "if (Test-Path -LiteralPath $__it_si)" },
                std::string_view{ "Remove-Variable __it_si" },
            };
            for (const auto& prefix : bodyPrefixes)
            {
                if (candidate.size() >= prefix.size() &&
                    candidate.substr(0, prefix.size()) == prefix)
                {
                    return true;
                }
            }
            return false;
        }

        // Legacy detector: `. "...shell-integration*.ps1"` dot-source
        // line. Detection (and rewrite-on-install) is how existing
        // affected profiles get migrated to the modern block.
        //
        // Pattern: line begins with `.` + whitespace + a quoted path
        // whose FINAL filename component is `shell-integration*.ps1`.
        // The path-component check (preceded by `/`, `\`, or the
        // opening quote; followed only by non-separator chars before
        // `.ps1`) avoids false matches on directories that happen to
        // contain "shell-integration".
        //
        // `(^|\n)` substitutes for the C++17 `multiline` flag — MSVC's
        // STL does NOT define `std::regex_constants::multiline`. We
        // trim the consumed `\n` out of the returned range so callers
        // see only the dot-source line itself.
        inline std::optional<std::pair<size_t, size_t>> FindLegacyDotSource(std::string_view contents)
        {
            static const std::regex pattern{
                R"((^|\n)[ \t]*\.[ \t]+"(?:[^"]*[\\/])?shell-integration[^"\\/]*\.ps1".*)",
                std::regex_constants::ECMAScript
            };
            std::cmatch m;
            if (std::regex_search(contents.data(), contents.data() + contents.size(), m, pattern))
            {
                size_t start = static_cast<size_t>(m.position());
                size_t end = start + static_cast<size_t>(m.length());
                if (start < contents.size() && contents[start] == '\n')
                {
                    ++start;
                }
                while (end > start && contents[end - 1] == '\r')
                {
                    --end;
                }
                return std::make_pair(start, end);
            }
            return std::nullopt;
        }
    }

    inline ExecutionPolicyProbeResult ProbeExecutionPolicy(Target target) noexcept
    {
        ExecutionPolicyProbeResult result;
        result.target = target;

        try
        {
            if (target == Target::WindowsPowerShell)
            {
                wchar_t system32[MAX_PATH]{};
                const UINT system32Len = GetSystemDirectoryW(system32, MAX_PATH);
                if (system32Len == 0 || system32Len >= MAX_PATH)
                {
                    result.process.error = system32Len == 0 ? GetLastError() : ERROR_INSUFFICIENT_BUFFER;
                    return result;
                }
                result.executablePath.assign(system32, system32Len);
                result.executablePath += L"\\WindowsPowerShell\\v1.0\\powershell.exe";

                const auto attributes = GetFileAttributesW(result.executablePath.c_str());
                if (attributes == INVALID_FILE_ATTRIBUTES)
                {
                    const auto error = GetLastError();
                    result.process.error = error;
                    result.status = error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND ?
                                        ExecutionPolicyStatus::Absent :
                                        ExecutionPolicyStatus::Unknown;
                    return result;
                }
            }
            else
            {
                DWORD resolutionError = ERROR_SUCCESS;
                result.executablePath = details::ResolvePwshExecutableFromExplicitPath(&resolutionError);
                if (result.executablePath.empty())
                {
                    result.process.error = resolutionError;
                    const bool resolutionMeansAbsent =
                        resolutionError == ERROR_SUCCESS ||
                        resolutionError == ERROR_FILE_NOT_FOUND ||
                        resolutionError == ERROR_PATH_NOT_FOUND ||
                        resolutionError == ERROR_ENVVAR_NOT_FOUND;
                    result.status = resolutionMeansAbsent ?
                                        ExecutionPolicyStatus::Absent :
                                        ExecutionPolicyStatus::Unknown;
                    return result;
                }
            }

            result.process = details::RunPowerShellCommand(
                result.executablePath.c_str(),
                details::QueryExecutionPolicyArguments);
            result.policy = result.process.output;
            result.status = result.process.launched &&
                                    !result.process.timedOut &&
                                    result.process.error == ERROR_SUCCESS &&
                                    result.process.exitCode == ERROR_SUCCESS ?
                                details::ClassifyExecutionPolicy(result.policy) :
                                ExecutionPolicyStatus::Unknown;
        }
        catch (...)
        {
            result.process.error = ERROR_UNHANDLED_EXCEPTION;
            result.status = ExecutionPolicyStatus::Unknown;
        }
        return result;
    }

    inline PowerShellProcessResult EnableRemoteSignedForCurrentUser(const ExecutionPolicyProbeResult& probe) noexcept
    {
        if (probe.status != ExecutionPolicyStatus::Blocked || probe.executablePath.empty())
        {
            PowerShellProcessResult result;
            result.error = ERROR_INVALID_STATE;
            return result;
        }

        return details::RunPowerShellCommand(
            probe.executablePath.c_str(),
            details::EnableRemoteSignedArguments);
    }

    inline bool CanAttemptExecutionPolicyRemediation(const ExecutionPolicyProbeResult& pwsh,
                                                     const ExecutionPolicyProbeResult& windowsPowerShell) noexcept
    {
        return pwsh.status != ExecutionPolicyStatus::Unknown &&
               windowsPowerShell.status != ExecutionPolicyStatus::Unknown;
    }

    inline bool ExecutionPolicyRemediationSucceeded(const ExecutionPolicyProbeResult& pwsh,
                                                    const ExecutionPolicyProbeResult& windowsPowerShell) noexcept
    {
        const auto allowedOrAbsent = [](ExecutionPolicyStatus status) {
            return status == ExecutionPolicyStatus::Allowed ||
                   status == ExecutionPolicyStatus::Absent;
        };
        return allowedOrAbsent(pwsh.status) && allowedOrAbsent(windowsPowerShell.status);
    }

    inline bool ExecutionPoliciesVerifiedForInstall(const ExecutionPolicyRemediationResult& remediation) noexcept
    {
        if (remediation.verificationAttempted)
        {
            return ExecutionPolicyRemediationSucceeded(
                remediation.pwshAfter,
                remediation.windowsPowerShellAfter);
        }
        return ExecutionPolicyRemediationSucceeded(
            remediation.pwshBefore,
            remediation.windowsPowerShellBefore);
    }

    inline ExecutionPolicyRemediationResult RemediateExecutionPoliciesForCurrentUser() noexcept
    {
        ExecutionPolicyRemediationResult result;
        result.pwshBefore = ProbeExecutionPolicy(Target::Pwsh);
        result.windowsPowerShellBefore = ProbeExecutionPolicy(Target::WindowsPowerShell);

        const bool pwshBlocked = result.pwshBefore.status == ExecutionPolicyStatus::Blocked;
        const bool windowsPowerShellBlocked = result.windowsPowerShellBefore.status == ExecutionPolicyStatus::Blocked;
        result.attempted = pwshBlocked || windowsPowerShellBlocked;
        if (!result.attempted)
        {
            return result;
        }

        if (!CanAttemptExecutionPolicyRemediation(result.pwshBefore, result.windowsPowerShellBefore))
        {
            result.succeeded = false;
            return result;
        }

        if (pwshBlocked)
        {
            result.pwshSetter = EnableRemoteSignedForCurrentUser(result.pwshBefore);
        }
        if (windowsPowerShellBlocked)
        {
            result.windowsPowerShellSetter = EnableRemoteSignedForCurrentUser(result.windowsPowerShellBefore);
        }

        result.verificationAttempted = true;
        result.pwshAfter = ProbeExecutionPolicy(Target::Pwsh);
        result.windowsPowerShellAfter = ProbeExecutionPolicy(Target::WindowsPowerShell);
        result.succeeded = ExecutionPolicyRemediationSucceeded(
            result.pwshAfter,
            result.windowsPowerShellAfter);
        return result;
    }

    // True when the effective PowerShell execution policy for `target` refuses
    // to run unsigned local scripts. Asks PowerShell itself rather than walking
    // the registry / Group Policy hives — `Get-ExecutionPolicy` returns the
    // effective policy after considering every scope plus the built-in default.
    //
    // Re-queried on every call so that after the user fixes the policy outside
    // (e.g. `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`) and clicks
    // Save again, the Terminal picks up the new policy.
    //
    // Pure query — no logging / no I/O side effects. The optional out-params let
    // the caller (the FRE shell-integration sweep) record diagnostics:
    //   * `outPolicy`   — the raw effective policy we read ("" when the probe was
    //                     inconclusive, e.g. it timed out).
    //   * `outTimedOut` — true iff the probe was killed at its timeout (so an
    //                     empty `outPolicy` is a probe failure, NOT a restrictive
    //                     policy).
    inline bool ExecutionPolicyBlocksShellIntegration(Target target,
                                                      std::wstring* outPolicy = nullptr,
                                                      bool* outTimedOut = nullptr) noexcept
    {
        if (outPolicy)
        {
            outPolicy->clear();
        }
        if (outTimedOut)
        {
            *outTimedOut = false;
        }
        // Resolve the host to a FULL path and probe THAT exact binary (not the bare
        // name), so a PATH change between resolution and the probe can't make us run a
        // different executable, and so the probe isn't susceptible to PATH-order
        // hijacking. If the host can't be resolved to a trustworthy path we fail open
        // (return false): a missing/unresolvable host — e.g. pwsh.exe on machines
        // without PowerShell 7 — must not false-positive as "EP blocked". A PRESENT
        // host whose probe comes back empty/inconclusive (which can happen in the
        // packaged-app context) ALSO does not block: only a definitively restrictive
        // policy (Restricted/AllSigned) does — see PolicyNameBlocksUnsignedScripts.
        std::wstring resolved;
        if (target == Target::WindowsPowerShell)
        {
            // Windows PowerShell ships in the OS at a FIXED system location, so pin it
            // to %SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe instead of
            // trusting PATH — exactly as WslDistroGenerator pins wsl.exe to System32
            // (GH#11096) to defeat path hijacking of a system binary.
            wchar_t system32[MAX_PATH]{};
            const UINT system32Len = GetSystemDirectoryW(system32, MAX_PATH);
            if (system32Len == 0 || system32Len >= MAX_PATH)
            {
                return false;
            }
            resolved.assign(system32, system32Len);
            resolved += L"\\WindowsPowerShell\\v1.0\\powershell.exe";
            // A genuinely absent system powershell.exe (extremely unusual) fails open.
            if (GetFileAttributesW(resolved.c_str()) == INVALID_FILE_ATTRIBUTES)
            {
                return false;
            }
        }
        else
        {
            // PowerShell 7 is optional and has no fixed location. Search only
            // explicit absolute PATH entries so the current directory cannot
            // supply the executable that runs our policy-management commands.
            resolved = details::ResolvePwshExecutableFromExplicitPath();
            if (resolved.empty())
            {
                return false;
            }
        }
        bool timedOut = false;
        auto policy = details::QueryExecutionPolicy(resolved.c_str(), &timedOut);
        const bool blocked = details::PolicyNameBlocksUnsignedScripts(policy);
        if (outTimedOut)
        {
            *outTimedOut = timedOut;
        }
        if (outPolicy)
        {
            *outPolicy = std::move(policy);
        }
        return blocked;
    }

    // Discover the PowerShell $PROFILE path.
    // Uses SHGetKnownFolderPath for the Documents folder instead of spawning
    // a shell process, which hangs indefinitely in packaged-app environments.
    // SHGetKnownFolderPath respects OneDrive redirection and group policy.
    inline std::wstring DiscoverProfilePath(Target target)
    {
        wil::unique_cotaskmem_string documentsPath;
        if (FAILED(SHGetKnownFolderPath(FOLDERID_Documents, 0, nullptr, &documentsPath)) || !documentsPath)
        {
            return {};
        }
        std::filesystem::path profilePath{ documentsPath.get() };
        profilePath /= (target == Target::Pwsh) ? L"PowerShell" : L"WindowsPowerShell";
        profilePath /= L"Microsoft.PowerShell_profile.ps1";
        return profilePath.wstring();
    }

    // ───────────────────────────────────────────────────────────────────
    // SINGLE SOURCE OF TRUTH for the PowerShell shell-integration script
    // version. The version is carried by the filename
    // (`shell-integration_vN.ps1`) — installs detect any prior
    // `shell-integration*.ps1` reference in $PROFILE and rewrite it to
    // point at the current version. Older script files left on disk are
    // inert (never referenced). To roll out a new version, bump this.
    //
    // v2: added OSC 9001;ShellType emission (shell self-reports identity
    // each prompt). Bumped from v1 so existing users — whose $PROFILE
    // already references the v1 script byte-for-byte — get the new script
    // rewritten in; without the bump the orchestrator's block-match early-
    // out would leave the stale v1 script (no ShellType) in place.
    //
    // v3: fixed __ShellInteg_GetLastExitCode so PowerShell-level errors
    // (invalid -match regex, [int]::Parse, 1/0, ...) report a non-zero
    // OSC 133;D exit code on Windows PowerShell 5.1. 5.1 stamps
    // InvocationInfo.HistoryId = -1 on these .NET-exception-class errors, so
    // the old HistoryId-match check missed them and emitted the stale 0 from
    // the prior command, causing autofix to treat the failure as success.
    // Bumped so existing users get the corrected script rewritten in.
    //
    // v4: command-not-found errors can leave $LastExitCode null because no
    // native process was started. Treat null like the stale zero used by
    // PowerShell-level errors so OSC 133;D always carries a numeric non-zero
    // failure code.
    //
    // v5: track newly observed ErrorRecords as a fallback for failures that
    // do not enter Get-History, and consume errors raised by custom prompt
    // rendering so prompt redraws do not emit duplicate command-finished
    // marks.
    //
    // v6: PowerShell 7 discards parser failures before prompt runs: $? is
    // true, $Error[0] is null, and Get-History has no entry. Wrap
    // PSConsoleHostReadLine to retain the submitted line, then parse it lazily
    // in prompt only when no normal completion signal exists.
    //
    // v7: re-arm after a third-party prompt replacement. Through v6 the wrapper
    // snapshotted $function:prompt once, at install time, so any prompt
    // renderer loaded AFTERWARDS replaced it and silently removed shell
    // integration - including `oh-my-posh init | Invoke-Expression` placed at
    // the END of $PROFILE, which is the placement Oh My Posh's own
    // documentation shows. v7 keeps ownership of `prompt` but re-arms: at the
    // PSConsoleHostReadLine boundary (off the prompt path, so $? is
    // unaffected) it detects by object identity that something took the slot,
    // adopts that renderer as the prompt it wraps, and reinstalls itself. At
    // most one prompt render is unmarked after a takeover. Also new in v7:
    // restores a success/failure signal for the wrapped prompt - through v6 it
    // always saw success, which broke Oh My Posh's status segment - guards
    // against prompt modules that chain back into us, and renders fail-safe,
    // degrading to the wrapped prompt without marks instead of throwing on
    // every prompt.
    // ───────────────────────────────────────────────────────────────────
    inline constexpr int kVersion = 7;

    inline std::wstring ScriptFileName()
    {
        return L"shell-integration_v" + std::to_wstring(kVersion) + L".ps1";
    }

    // Build the $PROFILE block. The block resolves Documents at runtime
    // via [Environment]::GetFolderPath('MyDocuments') so it:
    //   • survives OneDrive Known Folder Move enabled AFTER install
    //   • is a silent no-op (via Test-Path guard) on roamed profiles
    //     reaching a machine without Intelligent Terminal installed
    //   • respects Group Policy folder redirection to a network share
    inline std::string BuildBlock(std::wstring_view profileSubdir, std::string_view eol)
    {
        const auto fileName = til::u16u8(ScriptFileName());
        const auto subdir = til::u16u8(std::wstring{ profileSubdir });

        std::string block;
        block += kShellIntegrationBlockOpenMarker;                                          block += eol;
        block += "# Auto-generated by Intelligent Terminal. Do not edit between markers.";  block += eol;
        block += "# Documents is resolved at runtime so this survives OneDrive Known";      block += eol;
        block += "# Folder Move and is a silent no-op on machines without IT installed.";   block += eol;
        block += "$__it_si = Join-Path ([Environment]::GetFolderPath('MyDocuments')) '";
        block += subdir;
        block += "\\";
        block += fileName;
        block += "'";                                                                       block += eol;
        block += "if (Test-Path -LiteralPath $__it_si) { . $__it_si }";                     block += eol;
        block += "Remove-Variable __it_si -ErrorAction SilentlyContinue";                   block += eol;
        block += kShellIntegrationBlockCloseMarker;
        return block;
    }

    // The shell integration script content. The version is carried by the
    // filename, not embedded inside the script body.
    inline std::string ScriptContent()
    {
        return std::string{
            R"(# Shell Integration — non-invasive prompt wrapper
# Emits OSC 133 (command marks / exit code) and OSC 9;9 (CWD) escape
# sequences WITHOUT altering the visual appearance of the user's prompt.
#
# Compatible with Windows PowerShell 5.1+ and PowerShell 7+.
# Safe to source multiple times (idempotent guard).

if (-not $Global:__ShellInteg_Installed) {

    # ── Escape characters (PS 5.1 doesn't support `e / `a literals) ──
    $Global:__ShellInteg_ESC = [char]0x1B   # ESC
    $Global:__ShellInteg_BEL = [char]0x07   # BEL (OSC string terminator)

    # ── Snapshot the user's current prompt before we touch it ──────────
    $Global:__ShellInteg_OriginalPrompt = $function:prompt
    $Global:__ShellInteg_LastHistoryId  = -1
    $Global:__ShellInteg_LastErrorRecord = $Error[0]
    $Global:__ShellInteg_LastSubmittedLine = $null
    $Global:__ShellInteg_PrevPrompt = $null
    $Global:__ShellInteg_Rendering = $false
    $Global:__ShellInteg_CanInspectErrors =
        $ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage'
    $Global:__ShellInteg_Installed      = $true

    # PowerShell 7 drops parser failures before prompt runs, leaving no
    # observable status there. Retain the submitted line at the PSReadLine
    # boundary without changing what is returned to the engine.
    #
    # This boundary is also where we re-arm (see __ShellInteg_Rearm): it runs
    # once per command, BEFORE the user's command executes, so it cannot
    # disturb the $? that the next prompt reads.
    if ($Global:__ShellInteg_CanInspectErrors -and (Test-Path Function:\PSConsoleHostReadLine)) {
        $Global:__ShellInteg_OriginalPSConsoleHostReadLine = $function:PSConsoleHostReadLine
        function Global:PSConsoleHostReadLine {
            __ShellInteg_Rearm
            $line = & $Global:__ShellInteg_OriginalPSConsoleHostReadLine @args
            $Global:__ShellInteg_LastSubmittedLine =
                if ($line -is [string]) { $line } else { $null }
            return $line
        }
    }

    function Global:__ShellInteg_GetLastExitCode {
        # $? still reflects the *user's* last command here because this
        # is the very first call inside the prompt function.
        if ($? -eq $True) { return 0 }
        # $? is False -> the last command failed. Preserve a real non-zero
        # native exit code. PowerShell-level errors leave the previous value
        # untouched, while command-not-found can leave it null because no
        # native process started; zero/null therefore use a numeric sentinel.
        if ($null -ne $LastExitCode -and $LastExitCode -ne 0) {
            return $LastExitCode
        }
        return -1
    }

    function prompt {
        # ── Capture exit code FIRST — before anything else can clobber $? ──
        $gle           = $(__ShellInteg_GetLastExitCode)

        # A prompt module may CHAIN rather than replace: it captures the
        # current prompt (ours) and calls it from inside its own. Once we
        # adopt it, delegating calls us back. Serve the prompt it displaced
        # so its text survives, instead of recursing.
        if ($Global:__ShellInteg_Rendering) {
            if ($Global:__ShellInteg_PrevPrompt) {
                return (& $Global:__ShellInteg_PrevPrompt)
            }
            # Nothing displaced left to serve. Return an empty string rather
            # than inventing prompt text: this wrapper never contributes
            # visible output of its own, and the chaining module renders its
            # own text around whatever we return.
            return ''
        }

        try {
            $submittedLine = $Global:__ShellInteg_LastSubmittedLine
            $errorRecord   = $Error[0]
            $entry         = Get-History -Count 1
            $loc           = $executionContext.SessionState.Path.CurrentLocation
            $E             = $Global:__ShellInteg_ESC
            $B             = $Global:__ShellInteg_BEL

            $prefix = ''
            $suffix = ''

            # ── Previous command finished (OSC 133;D with exit code) ──
            $historyAdvanced = $entry -and $entry.Id -ne $Global:__ShellInteg_LastHistoryId
            $newErrorRecord = $Global:__ShellInteg_CanInspectErrors -and
                $null -ne $errorRecord -and
                -not [object]::ReferenceEquals($errorRecord, $Global:__ShellInteg_LastErrorRecord)
            $inputHadParserError = $false
            if ($Global:__ShellInteg_CanInspectErrors -and
                $gle -eq 0 -and
                $null -ne $submittedLine -and
                ($newErrorRecord -or -not $historyAdvanced)) {
                $tokens = $null
                $parseErrors = $null
                [void][System.Management.Automation.Language.Parser]::ParseInput(
                    $submittedLine,
                    [ref]$tokens,
                    [ref]$parseErrors)
                $inputHadParserError = $parseErrors.Count -gt 0
            }
            if ($inputHadParserError -and $gle -eq 0) {
                $gle = -1
            }
            $newUntrackedError = -not $historyAdvanced -and $gle -ne 0 -and $newErrorRecord
            if ($historyAdvanced -or $newUntrackedError -or $inputHadParserError) {
                $prefix += "${E}]133;D;${gle}${B}"
            }

            # ── Prompt started (OSC 133;A) ──
            $prefix += "${E}]133;A${B}"

            # ── Report current working directory (OSC 9;9) ──
            $prefix += "${E}]9;9;`"${loc}`"${B}"

            # ── Report shell identity (OSC 9001;ShellType) ──
            # Emitted every prompt so the terminal always knows which shell owns
            # the pane, even after a nested shell (e.g. wsl) exits and PowerShell
            # repaints its prompt. PSEdition 'Core' is pwsh 7+, 'Desktop' is
            # Windows PowerShell 5.1.
            $shellName = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
            $prefix += "${E}]9001;ShellType;${shellName};$($PSVersionTable.PSVersion)${B}"

            # ── Prompt ended, command input starts (OSC 133;B) ──
            $suffix = "${E}]133;B${B}"

            # ── Restore a success/failure signal for the prompt we wrap ──
            # Our work above has overwritten $?, so a prompt that reads it (Oh My
            # Posh's status segment, starship, posh-git) would wrongly see success.
            # $? cannot be assigned, but it mirrors the last statement: looking up
            # a variable that does not exist fails WITHOUT touching $Error, and a
            # trivial assignment succeeds. This must be the statement immediately
            # before the delegation.
            #
            # The value restored is derived from $gle, not the literal $? the
            # engine had: parser-error detection above can turn a $?-true command
            # into $gle -1. That is deliberate - $gle is the same failure signal
            # reported in OSC 133;D, so the wrapped prompt and the terminal agree
            # on whether the command failed.
            $Global:__ShellInteg_Rendering = $true
            try {
            if ($gle -eq 0) { $null = $true }
            else { Get-Variable '__ShellInteg_NoSuchVariable__' -ErrorAction Ignore }
            # ── Delegate to the user's ORIGINAL prompt — visual output is theirs ──
            # Failures are caught HERE, at the delegation, so the wrapped prompt
            # is invoked exactly once per render. Letting it escape to the outer
            # catch would run it a second time and duplicate its side effects.
            # An empty result is correct: the host then supplies its own prompt.
            try     { $originalOutput = & $Global:__ShellInteg_OriginalPrompt }
            catch   { $originalOutput = '' }
            }
            finally { $Global:__ShellInteg_Rendering = $false }

            $Global:__ShellInteg_LastHistoryId = if ($entry) { $entry.Id } else { -1 }
            $Global:__ShellInteg_LastErrorRecord = $Error[0]
            $Global:__ShellInteg_LastSubmittedLine = $null

            return "${prefix}${originalOutput}${suffix}"
            }
        catch {
            # FAIL SAFE for failures in OUR OWN code above (Get-History, the
            # parser probe, string building). The wrapped prompt has its own
            # catch at the delegation, so reaching here means it has NOT yet
            # been invoked for this render - delegating now keeps it at exactly
            # one invocation.
            #
            # The reentrancy flag must be held here too. The normal path clears
            # it in its finally before this catch runs, so an adopted CHAINING
            # prompt would call back into us with the flag false and recurse
            # until the engine's call-depth limit - the very cycle the guard
            # above exists to prevent.
            $Global:__ShellInteg_Rendering = $true
            try { return (& $Global:__ShellInteg_OriginalPrompt) }
            catch {
                # The wrapped prompt itself threw. Return an empty string and
                # let the HOST fall back to its own built-in prompt. Do not
                # fabricate one: this wrapper never alters the visible prompt,
                # and a hand-rolled "PS <cwd>> " is only an imitation of the
                # real default, which differs by host and edition.
                return ''
            }
            finally { $Global:__ShellInteg_Rendering = $false }
        }
    }

    # Remember our own prompt scriptblock so re-arming can recognise it by
    # reference. Created once per session: re-creating it would make the
    # previous copy look like a third-party takeover.
    $Global:__ShellInteg_Wrapper = $function:prompt
}

# Any prompt renderer loaded AFTER us (Oh My Posh, starship, a module) replaces
# our wrapper and silently removes shell integration. Detect that by object
# identity, adopt the newcomer as the prompt we wrap, and reinstall ourselves.
# Runs on every source and once per command from the PSReadLine boundary.
function Global:__ShellInteg_Rearm {
    # No wrapper of our own to install. This happens when an OLDER version of
    # this script is already installed in the session: it set
    # __ShellInteg_Installed, so the block above was skipped and never created
    # our wrapper. Do nothing. Installing a null would clobber `prompt`, and
    # adopting the older version's wrapper as the prompt we wrap would make it
    # delegate to itself and overflow the call stack. The upgrade completes in
    # the next shell, which sources only this version.
    if ($null -eq $Global:__ShellInteg_Wrapper) { return }

    # Identity is compared with -eq rather than a static reference-equality
    # method. This function is called at script load and, when the readline
    # wrapper is installed, once per command. Under Constrained Language Mode a
    # static method call raises "Method invocation is supported only on core
    # types", which aborts the enclosing script even from inside try/catch -
    # breaking profile sourcing or the input path. ScriptBlock does not override
    # Equals, so -eq is reference identity here (verified: two script blocks with
    # identical text compare False), and the operator is permitted under
    # Constrained Language Mode.
    #
    # Scope of that: -eq makes this function SAFE to invoke under Constrained
    # Language Mode. It does not by itself make re-arming active there - the
    # readline wrapper above is gated on $Global:__ShellInteg_CanInspectErrors,
    # which is false in that mode, so the per-command driver is not installed and
    # integration degrades to "marks work, no self-healing after a takeover".
    $current = $function:prompt
    if ($null -eq $current) { return }
    if ($current -eq $Global:__ShellInteg_Wrapper) { return }

    # Best-effort. This runs from PSConsoleHostReadLine, so a throw here would
    # break the input path, and at script load, where it would break profile
    # sourcing. `prompt` can legitimately be ReadOnly or Constant in a hardened
    # profile, which makes Set-Item raise SessionStateUnauthorizedAccessException.
    # Shell integration is a convenience; never let it take the shell down.
    try {
        if ($current -ne $Global:__ShellInteg_OriginalPrompt) {
            # Retain the renderer being displaced, in case the newcomer chains
            # back into us.
            $Global:__ShellInteg_PrevPrompt = $Global:__ShellInteg_OriginalPrompt
            $Global:__ShellInteg_OriginalPrompt = $current
        }
        Set-Item Function:\global:prompt $Global:__ShellInteg_Wrapper -ErrorAction Stop
    }
    catch {
        # Leave whatever prompt is installed alone; try again next command.
    }
}

__ShellInteg_Rearm
)"
        };
    }

    // Shared base for the two concrete PowerShell flavors. Holds the
    // profile path and the subdir name embedded in the generated block;
    // every IShellFlavor method other than ProfilePath / the subdir is
    // identical between pwsh and Windows PowerShell.
    //
    // Not part of the public API surface — call sites construct the
    // concrete PowerShellFlavor or WindowsPowerShellFlavor.
    class PowerShellFlavorBase : public IShellFlavor
    {
    public:
        std::wstring          ProfilePath() const override          { return _profilePath; }
        std::filesystem::path ScriptDir() const override            { return std::filesystem::path{ _profilePath }.parent_path(); }
        std::wstring          ScriptFileName() const override       { return Powershell::ScriptFileName(); }
        std::string           ScriptContent() const override        { return Powershell::ScriptContent(); }
        std::wstring          ProfileFriendlyName() const override  { return L"PowerShell profile"; }
        LineEndingPolicy      LineEndings() const override          { return LineEndingPolicy::Auto; }

        std::string ScriptBlock(std::string_view eol) const override
        {
            return Powershell::BuildBlock(_profileSubdir, eol);
        }

        std::optional<std::pair<size_t, size_t>>
        FindExistingScriptBlock(std::string_view contents) const override
        {
            return ::Microsoft::Terminal::ShellIntegration::details::FindBlock(
                contents,
                &details::IsOrphanBodyLine,
                &details::FindLegacyDotSource);
        }

    protected:
        PowerShellFlavorBase(std::wstring profilePath, std::wstring profileSubdir) :
            _profilePath{ std::move(profilePath) },
            _profileSubdir{ std::move(profileSubdir) }
        {
        }

    private:
        std::wstring _profilePath;
        std::wstring _profileSubdir; // "PowerShell" or "WindowsPowerShell"
    };

    // PowerShell 7+ ($PROFILE under Documents\PowerShell\).
    //
    // The subdir name baked into the generated block is derived from
    // the profile path's parent dir (so a test that points at
    // `<tmp>\PowerShell\…` produces a block referencing PowerShell
    // and a test pointing at `<tmp>\WindowsPowerShell\…` produces one
    // referencing WindowsPowerShell). This matches the pre-refactor
    // behavior the FindBlock + BuildBlock tests rely on.
    class PowerShellFlavor : public PowerShellFlavorBase
    {
    public:
        explicit PowerShellFlavor(std::wstring profilePath) :
            PowerShellFlavorBase{ std::move(profilePath), _SubdirFromPath(profilePath) }
        {
        }

    private:
        static std::wstring _SubdirFromPath(const std::wstring& profilePath)
        {
            const auto subdir = std::filesystem::path{ profilePath }.parent_path().filename().wstring();
            return subdir.empty() ? std::wstring{ L"PowerShell" } : subdir;
        }
    };

    // Windows PowerShell 5.1 ($PROFILE under Documents\WindowsPowerShell\).
    class WindowsPowerShellFlavor : public PowerShellFlavorBase
    {
    public:
        explicit WindowsPowerShellFlavor(std::wstring profilePath) :
            PowerShellFlavorBase{ std::move(profilePath), _SubdirFromPath(profilePath) }
        {
        }

    private:
        static std::wstring _SubdirFromPath(const std::wstring& profilePath)
        {
            const auto subdir = std::filesystem::path{ profilePath }.parent_path().filename().wstring();
            return subdir.empty() ? std::wstring{ L"WindowsPowerShell" } : subdir;
        }
    };

    // Path-taking convenience used by both the FRE / Settings code
    // paths and the umbrella Install / Uninstall flat aliases that
    // the tests call. Picks the right concrete flavor based on the
    // profile's parent dir name.
    inline InstallResult Install(const std::wstring& profilePathW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        const auto subdir = std::filesystem::path{ profilePathW }.parent_path().filename().wstring();
        if (subdir == L"WindowsPowerShell")
        {
            WindowsPowerShellFlavor flavor{ profilePathW };
            return orchestrator::Install(flavor);
        }
        PowerShellFlavor flavor{ profilePathW };
        return orchestrator::Install(flavor);
    }

    inline InstallResult Uninstall(const std::wstring& profilePathW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        const auto subdir = std::filesystem::path{ profilePathW }.parent_path().filename().wstring();
        if (subdir == L"WindowsPowerShell")
        {
            WindowsPowerShellFlavor flavor{ profilePathW };
            return orchestrator::Uninstall(flavor);
        }
        PowerShellFlavor flavor{ profilePathW };
        return orchestrator::Uninstall(flavor);
    }

    // Convenience: discover + install. Probes execution policy first so
    // a Restricted host fails up front with a specific error rather than
    // succeeding-then-silently-erroring on every shell start.
    inline InstallResult InstallForTarget(Target target)
    {
        if (ExecutionPolicyBlocksShellIntegration(target))
        {
            return { false, false, L"PowerShell execution policy blocks scripts", true };
        }
        auto profilePath = DiscoverProfilePath(target);
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover PowerShell profile path" };
        }
        return Install(profilePath);
    }

    inline InstallResult UninstallForTarget(Target target)
    {
        auto profilePath = DiscoverProfilePath(target);
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover PowerShell profile path" };
        }
        return Uninstall(profilePath);
    }
}
