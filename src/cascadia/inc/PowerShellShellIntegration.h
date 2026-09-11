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

    namespace details
    {
        inline constexpr std::wstring_view QueryExecutionPolicyArguments{
            L"-NoProfile -NonInteractive -Command Get-ExecutionPolicy"
        };
        inline constexpr std::wstring_view EnableRemoteSignedArguments{
            L"-NoProfile -NonInteractive -Command Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy RemoteSigned -Force"
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

                        char buf[256];
                        DWORD bytesRead = 0;
                        const DWORD toRead = std::min<DWORD>(available, sizeof(buf));
                        if (!ReadFile(readEnd.get(), buf, toRead, &bytesRead, nullptr) || bytesRead == 0)
                        {
                            break;
                        }
                        if (raw.size() < 4096)
                        {
                            raw.append(buf, std::min<size_t>(bytesRead, 4096 - raw.size()));
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

        // Runs `<exe> -NoProfile -NonInteractive -Command Get-ExecutionPolicy`
        // synchronously and returns the lowercased effective policy name from
        // stdout (e.g. "restricted"), or an EMPTY string if it could not be
        // determined — CreateProcess failed, the host isn't installed, or the
        // child didn't finish within the timeout. Empty therefore means "unknown
        // / probe failed", NOT "blocked" (the caller fails open on empty).
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
        // `-Command <expr>` runs an inline expression that is NOT subject to the
        // .ps1 execution policy, so this works even when the answer is Restricted
        // / AllSigned. We deliberately do NOT pass `-ExecutionPolicy` because that
        // would set the Process scope and override the value we're trying to read.
        inline std::wstring QueryExecutionPolicy(LPCWSTR exe, bool* outTimedOut = nullptr) noexcept
        {
            std::wstring resolved{ exe };
            if (resolved.find_first_of(L"\\/") == std::wstring::npos)
            {
                wchar_t buffer[MAX_PATH]{};
                const DWORD resolvedLen = SearchPathW(nullptr, exe, nullptr, MAX_PATH, buffer, nullptr);
                if (resolvedLen == 0 || resolvedLen >= MAX_PATH)
                {
                    if (outTimedOut)
                    {
                        *outTimedOut = false;
                    }
                    return {};
                }
                resolved.assign(buffer, resolvedLen);
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
            wchar_t buffer[MAX_PATH]{};
            SetLastError(ERROR_SUCCESS);
            const DWORD resolvedLen = SearchPathW(nullptr, L"pwsh.exe", nullptr, MAX_PATH, buffer, nullptr);
            if (resolvedLen == 0)
            {
                const auto error = GetLastError();
                result.process.error = error;
                result.status = error == ERROR_SUCCESS ||
                                        error == ERROR_FILE_NOT_FOUND ||
                                        error == ERROR_PATH_NOT_FOUND ||
                                        error == ERROR_ENVVAR_NOT_FOUND ?
                                    ExecutionPolicyStatus::Absent :
                                    ExecutionPolicyStatus::Unknown;
                return result;
            }
            if (resolvedLen >= MAX_PATH)
            {
                result.process.error = ERROR_INSUFFICIENT_BUFFER;
                return result;
            }
            result.executablePath.assign(buffer, resolvedLen);
        }

        result.process = details::RunPowerShellCommand(
            result.executablePath.c_str(),
            details::QueryExecutionPolicyArguments);
        result.policy = result.process.output;
        result.status = result.process.launched &&
                                !result.process.timedOut &&
                                result.process.exitCode == ERROR_SUCCESS ?
                            details::ClassifyExecutionPolicy(result.policy) :
                            ExecutionPolicyStatus::Unknown;
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
        auto result = ProbeExecutionPolicy(target);
        if (outTimedOut)
        {
            *outTimedOut = result.process.timedOut;
        }
        if (outPolicy)
        {
            *outPolicy = std::move(result.policy);
        }
        return result.status == ExecutionPolicyStatus::Blocked;
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
    // ───────────────────────────────────────────────────────────────────
    inline constexpr int kVersion = 6;

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
    $Global:__ShellInteg_CanInspectErrors =
        $ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage'
    $Global:__ShellInteg_Installed      = $true

    # PowerShell 7 drops parser failures before prompt runs, leaving no
    # observable status there. Retain the submitted line at the PSReadLine
    # boundary without changing what is returned to the engine.
    if ($Global:__ShellInteg_CanInspectErrors -and (Test-Path Function:\PSConsoleHostReadLine)) {
        $Global:__ShellInteg_OriginalPSConsoleHostReadLine = $function:PSConsoleHostReadLine
        function Global:PSConsoleHostReadLine {
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

        # ── Delegate to the user's ORIGINAL prompt — visual output is theirs ──
        $originalOutput = & $Global:__ShellInteg_OriginalPrompt

        $Global:__ShellInteg_LastHistoryId = if ($entry) { $entry.Id } else { -1 }
        $Global:__ShellInteg_LastErrorRecord = $Error[0]
        $Global:__ShellInteg_LastSubmittedLine = $null

        return "${prefix}${originalOutput}${suffix}"
    }
}
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
