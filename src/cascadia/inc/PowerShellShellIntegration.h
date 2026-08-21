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

namespace Microsoft::Terminal::ShellIntegration::Powershell
{
    namespace details
    {
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
            if (outTimedOut)
            {
                *outTimedOut = false;
            }
            // This is a best-effort helper: any failure (CreateProcess, pipe,
            // read hang, OOM, …) must fail-open by returning an empty string
            // so the caller treats the policy as "not blocking" rather than
            // crashing the Terminal over a diagnostic probe.
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

                std::wstring cmdLine{ L"\"" };
                cmdLine += exe;
                cmdLine += L"\" -NoProfile -NonInteractive -Command Get-ExecutionPolicy";

                PROCESS_INFORMATION pi{};
                if (!CreateProcessW(nullptr,
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
                    return {};
                }
                wil::unique_handle process{ pi.hProcess };
                wil::unique_handle thread{ pi.hThread };

                writeEnd.reset();

                constexpr DWORD timeoutMs = 20000;
                const DWORD waitResult = WaitForSingleObject(process.get(), timeoutMs);
                if (waitResult != WAIT_OBJECT_0)
                {
                    // The child didn't exit on its own — either it timed out, or the
                    // wait itself failed (WAIT_FAILED / unexpected). In BOTH cases the
                    // child may still be running and still holds the pipe's write end,
                    // so the ReadFile below would block forever waiting for EOF — kill
                    // it first so the read returns promptly and we fail open. Only a
                    // real WAIT_TIMEOUT is reported as a timeout; a wait failure is an
                    // inconclusive probe (empty result), not a timeout.
                    if (waitResult == WAIT_TIMEOUT && outTimedOut)
                    {
                        *outTimedOut = true;
                    }
                    TerminateProcess(process.get(), 1);
                    WaitForSingleObject(process.get(), 1000);
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
            catch (...)
            {
                return {};
            }
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
            // PowerShell 7 (pwsh.exe) is an optional third-party install with no fixed
            // location, so it must be resolved via PATH.
            wchar_t buffer[MAX_PATH]{};
            const DWORD resolvedLen = SearchPathW(nullptr, L"pwsh.exe", nullptr, MAX_PATH, buffer, nullptr);
            if (resolvedLen == 0 || resolvedLen >= MAX_PATH)
            {
                // Not on PATH (resolvedLen == 0), or path too long for the buffer
                // (resolvedLen >= MAX_PATH leaves `buffer` unfilled/truncated).
                return false;
            }
            resolved.assign(buffer, resolvedLen);
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
    // v7: use transactional, versioned runtime state; host-write control marks
    // before the wrapped prompt; consume tracking state even when that prompt
    // throws; and restore failure semantics immediately before delegating so
    // status-sensitive prompts such as Oh My Posh still observe the user's
    // original failure.
    //
    // v8: recognize a directly submitted, static Oh My Posh initializer and
    // transactionally wrap the replacement prompt at the next ReadLine entry.
    // Prompt generations capture their downstream by identity so runtime
    // recovery never mutates an existing wrapper chain.
    // ───────────────────────────────────────────────────────────────────
    inline constexpr int kVersion = 8;

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

    // The shell integration script content. The version is carried by both
    // the filename and the runtime state/lifecycle signals.
    inline std::string ScriptContent()
    {
        return std::string{
            R"(# Shell Integration — non-invasive prompt wrapper
# Emits OSC 133 (command marks / exit code) and OSC 9;9 (CWD) escape
# sequences WITHOUT altering the visual appearance of the user's prompt.
#
# Compatible with Windows PowerShell 5.1+ and PowerShell 7+.
# Safe to source multiple times. Runtime rebinding is limited to a directly
# submitted, statically recognized Oh My Posh initializer.

$__ShellInteg_E = [char]0x1B
$__ShellInteg_B = [char]0x07
$__ShellInteg_ShellName =
    if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
Microsoft.PowerShell.Utility\Write-Host -Object (
    "${__ShellInteg_E}]9001;ShellType;${__ShellInteg_ShellName};$($PSVersionTable.PSVersion)${__ShellInteg_B}") `
    -NoNewline -InformationAction Continue
$__ShellInteg_StateVariable =
    Get-Variable -Name __ShellInteg_State -Scope Global -ErrorAction Ignore

if ($null -ne $__ShellInteg_StateVariable) {
    $__ShellInteg_ExistingState = $__ShellInteg_StateVariable.Value
    if ($__ShellInteg_ExistingState -is [hashtable] -and
        $__ShellInteg_ExistingState.Version -eq 8 -and
        $__ShellInteg_ExistingState.InstallComplete -eq $true -and
        $null -ne $__ShellInteg_ExistingState.ActiveGeneration -and
        $function:prompt -eq $__ShellInteg_ExistingState.ActiveGeneration.Wrapper -and
        ($null -eq $__ShellInteg_ExistingState.ReadLineWrapper -or
            $function:PSConsoleHostReadLine -eq $__ShellInteg_ExistingState.ReadLineWrapper)) {
        $__ShellInteg_ExistingState.RepairSignaled = $false
        Microsoft.PowerShell.Utility\Write-Host -Object (
            "${__ShellInteg_E}]9001;ShellIntegrationReady;${__ShellInteg_ShellName};8${__ShellInteg_B}") `
            -NoNewline -InformationAction Continue
    }
    elseif ($__ShellInteg_ExistingState -is [hashtable] -and
        $__ShellInteg_ExistingState.Version -eq 7) {
        Microsoft.PowerShell.Utility\Write-Host -Object (
            "${__ShellInteg_E}]9001;ShellIntegrationRepair;${__ShellInteg_ShellName};restart-required${__ShellInteg_B}") `
            -NoNewline -InformationAction Continue
    }
    else {
        Microsoft.PowerShell.Utility\Write-Host -Object (
            "${__ShellInteg_E}]9001;ShellIntegrationRepair;${__ShellInteg_ShellName};prompt-changed${__ShellInteg_B}") `
            -NoNewline -InformationAction Continue
    }
    Remove-Variable __ShellInteg_E, __ShellInteg_B, __ShellInteg_ShellName,
        __ShellInteg_StateVariable, __ShellInteg_ExistingState -ErrorAction Ignore
    return
}

# v6 cannot be migrated safely in a running shell: its mutable downstream
# pointer does not prove which prompt currently owns the chain.
$__ShellInteg_V6State =
    Get-Variable -Name __ShellInteg_Installed -Scope Global -ErrorAction Ignore
if ($null -ne $__ShellInteg_V6State -and $__ShellInteg_V6State.Value -eq $true) {
    Microsoft.PowerShell.Utility\Write-Host -Object (
        "${__ShellInteg_E}]9001;ShellIntegrationRepair;${__ShellInteg_ShellName};restart-required${__ShellInteg_B}") `
        -NoNewline -InformationAction Continue
    Remove-Variable __ShellInteg_E, __ShellInteg_B, __ShellInteg_ShellName,
        __ShellInteg_StateVariable, __ShellInteg_V6State -ErrorAction Ignore
    return
}

$__ShellInteg_OriginalPrompt =
    Get-Item -LiteralPath Function:\prompt -ErrorAction Ignore
$__ShellInteg_OriginalReadLine =
    if (Test-Path Function:\PSConsoleHostReadLine) {
        Get-Item -LiteralPath Function:\PSConsoleHostReadLine -ErrorAction Ignore
    }
    else {
        $null
    }
$__ShellInteg_CanInspectErrors =
    $ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage'

$__ShellInteg_GenerationFactory = {
    param($controller, [long]$generationId, $downstream)

    $downstreamScript = $downstream.ScriptBlock
    if ($null -eq $downstreamScript) {
        return $null
    }
    $generation = @{
        Id = $generationId
        Downstream = $downstreamScript
        Wrapper = $null
    }
    $wrapper = {
        # Capture status before controller or generation access can clobber it.
        $gle = if ($?) {
            0
        }
        elseif ($null -ne $LastExitCode -and $LastExitCode -ne 0) {
            $LastExitCode
        }
        else {
            -1
        }
        $savedLastExitCode = $global:LASTEXITCODE

        # An older generation may be retained by a prompt framework that
        # captured the prior prompt. It becomes a transparent pass-through.
        if ($controller.BindingInProgress -or
            $controller.ActiveGenerationId -ne $generation.Id) {
            $global:LASTEXITCODE = $savedLastExitCode
            if ($gle -ne 0) {
                Microsoft.PowerShell.Utility\Write-Error `
                    -Message '__it_status_restore__' -ErrorAction Ignore
            }
            return & $generation.Downstream @args
        }

        $controller.PromptDepth++
        try {
            # A downstream prompt can invoke prompt again. Only the outermost
            # active invocation owns terminal marks and shared tracking.
            if ($controller.PromptDepth -ne 1) {
                $global:LASTEXITCODE = $savedLastExitCode
                if ($gle -ne 0) {
                    Microsoft.PowerShell.Utility\Write-Error `
                        -Message '__it_status_restore__' -ErrorAction Ignore
                }
                return & $generation.Downstream @args
            }

            $submittedLine = $controller.LastSubmittedLine
            $submissionGeneration = $controller.SubmissionGeneration
            $errorRecord = $Error[0]
            $entry = Get-History -Count 1 -ErrorAction Ignore
            $loc = $executionContext.SessionState.Path.CurrentLocation
            $E = [char]0x1B
            $B = [char]0x07

            $historyAdvanced = $entry -and $entry.Id -gt $controller.LastHistoryId
            $newErrorRecord = $controller.CanInspectErrors -and
                $null -ne $errorRecord -and
                -not [object]::ReferenceEquals($errorRecord, $controller.LastErrorRecord)
            $inputHadParserError = $false
            if ($controller.CanInspectErrors -and
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

            $newUntrackedError =
                -not $historyAdvanced -and $gle -ne 0 -and $newErrorRecord
            $completionObserved =
                $historyAdvanced -or $newUntrackedError -or $inputHadParserError
            $emitCompletion =
                $completionObserved -and
                $submissionGeneration -gt $controller.LastEmittedSubmissionGeneration

            # Reserve completion state before invoking a downstream prompt
            # that may synchronously call prompt again.
            if ($historyAdvanced) {
                $controller.LastHistoryId = $entry.Id
            }
            $controller.LastErrorRecord = $errorRecord
            if ($controller.SubmissionGeneration -eq $submissionGeneration) {
                $controller.LastSubmittedLine = $null
            }
            if ($emitCompletion) {
                $controller.LastEmittedSubmissionGeneration = $submissionGeneration
            }

            $prefix = ''
            if ($emitCompletion) {
                $prefix += "${E}]133;D;${gle}${B}"
            }
            $prefix += "${E}]133;A${B}"
            $prefix += "${E}]9;9;`"${loc}`"${B}"
            $shellName =
                if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
            $prefix +=
                "${E}]9001;ShellType;${shellName};$($PSVersionTable.PSVersion)${B}"
            $suffix = "${E}]133;B${B}"

            Microsoft.PowerShell.Utility\Write-Host `
                -Object $prefix -NoNewline -InformationAction Continue
            $global:LASTEXITCODE = $savedLastExitCode
            if ($gle -ne 0) {
                Microsoft.PowerShell.Utility\Write-Error `
                    -Message '__it_status_restore__' -ErrorAction Ignore
            }

            try {
                $downstreamOutput = & $generation.Downstream @args
            }
            catch {
                Microsoft.PowerShell.Utility\Write-Host `
                    -Object $suffix -NoNewline -InformationAction Continue
                throw
            }
            finally {
                $controller.LastErrorRecord = $Error[0]
            }

            # Prompt callbacks are string-valued. Returning one combined record
            # keeps PSReadLine's transient-prompt path from falling back to PS>.
            return "${downstreamOutput}${suffix}"
        }
        finally {
            $controller.PromptDepth--
        }
    }.GetNewClosure()

    $generation.Wrapper = $wrapper
    return ,$generation
}.GetNewClosure()

$__ShellInteg_OmpMatcher = {
    param([string]$line)

    if ([string]::IsNullOrWhiteSpace($line)) {
        return $null
    }

    try {
        $tokens = $null
        $parseErrors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseInput(
            $line,
            [ref]$tokens,
            [ref]$parseErrors)
        if ($parseErrors.Count -ne 0 -or
            $null -eq $ast.EndBlock -or
            $ast.EndBlock.Statements.Count -ne 1) {
            return $null
        }

        $statement = $ast.EndBlock.Statements[0]
        if (-not ($statement -is [System.Management.Automation.Language.PipelineAst]) -or
            $statement.PipelineElements.Count -ne 2) {
            return $null
        }

        $right = $statement.PipelineElements[1]
        if (-not ($right -is [System.Management.Automation.Language.CommandAst]) -or
            $right.Redirections.Count -ne 0 -or
            $right.CommandElements.Count -ne 1) {
            return $null
        }
        $rightName = $right.GetCommandName()
        if ($null -eq $rightName -or
            ($rightName -ine 'Invoke-Expression' -and $rightName -ine 'iex')) {
            return $null
        }

        $leftCommands = @($statement.PipelineElements[0].FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst]
        }, $true))
        if ($leftCommands.Count -ne 1) {
            return $null
        }

        $left = $leftCommands[0]
        if ($left.Redirections.Count -ne 0 -or $left.CommandElements.Count -lt 3) {
            return $null
        }
        $leftName = $left.GetCommandName()
        if ($null -eq $leftName -or $leftName.IndexOfAny([char[]]'*?[]') -ge 0) {
            return $null
        }
        $verb = $left.CommandElements[1]
        $shell = $left.CommandElements[2]
        if (-not ($verb -is [System.Management.Automation.Language.StringConstantExpressionAst]) -or
            -not ($shell -is [System.Management.Automation.Language.StringConstantExpressionAst]) -or
            $verb.Value -ine 'init' -or
            ($shell.Value -ine 'pwsh' -and $shell.Value -ine 'powershell')) {
            return $null
        }

        # Get-Command without -CommandType follows the same precedence as
        # execution, so a same-name function or alias is rejected.
        $leftResolved = @(Microsoft.PowerShell.Core\Get-Command `
            -Name $leftName -ErrorAction Ignore)
        if ($leftResolved.Count -ne 1 -or
            -not ($leftResolved[0] -is [System.Management.Automation.ApplicationInfo])) {
            return $null
        }
        $leftLeaf = [System.IO.Path]::GetFileName($leftResolved[0].Path)
        if ($leftLeaf -ine 'oh-my-posh.exe') {
            return $null
        }

        $rightResolved = @(Microsoft.PowerShell.Core\Get-Command `
            -Name $rightName -ErrorAction Ignore)
        if ($rightResolved.Count -ne 1) {
            return $null
        }
        $rightCommand = $rightResolved[0]
        if ($rightCommand -is [System.Management.Automation.AliasInfo]) {
            $rightCommand = $rightCommand.ResolvedCommand
        }
        if (-not ($rightCommand -is [System.Management.Automation.CmdletInfo]) -or
            $rightCommand.Name -ine 'Invoke-Expression' -or
            $rightCommand.Source -ine 'Microsoft.PowerShell.Utility') {
            return $null
        }

        return @{
            Executable = $leftResolved[0].Path
        }
    }
    catch {
        return $null
    }
}

$__ShellInteg_RuntimeBind = {
    param($controller, $downstream)

    if ($controller.BindingInProgress -or
        $null -eq $downstream -or
        $function:PSConsoleHostReadLine -ne $controller.ReadLineWrapper) {
        return $false
    }

    $oldGeneration = $controller.ActiveGeneration
    $oldGenerationId = $controller.ActiveGenerationId
    $oldNextGenerationId = $controller.NextGenerationId
    $oldLastHistoryId = $controller.LastHistoryId
    $oldLastErrorRecord = $controller.LastErrorRecord
    $oldLastSubmittedLine = $controller.LastSubmittedLine
    $oldLastEmittedSubmissionGeneration =
        $controller.LastEmittedSubmissionGeneration
    $oldPendingRebind = $controller.PendingRebind
    $oldFailedRuntimePrompt = $controller.FailedRuntimePrompt
    $oldRepairSignaled = $controller.RepairSignaled
    $oldRuntimeFailureSignaled = $controller.RuntimeFailureSignaled
    $candidateId = $controller.NextGenerationId
    $candidate = & $controller.GenerationFactory $controller $candidateId $downstream
    if ($null -eq $candidate -or $null -eq $candidate.Wrapper) {
        return $false
    }

    $installed = $false
    $controller.BindingInProgress = $true
    try {
        Set-Item -LiteralPath Function:\global:prompt `
            -Value $candidate.Wrapper -ErrorAction Stop
        $installed = $true
        if ($function:prompt -ne $candidate.Wrapper) {
            throw 'Runtime prompt identity mismatch'
        }

        $controller.ActiveGeneration = $candidate
        $controller.ActiveGenerationId = $candidateId
        $controller.NextGenerationId = $candidateId + 1
        $entry = Get-History -Count 1 -ErrorAction Ignore
        if ($entry -and $entry.Id -gt $controller.LastHistoryId) {
            $controller.LastHistoryId = $entry.Id
        }
        $controller.LastErrorRecord = $Error[0]
        $controller.LastSubmittedLine = $null
        $controller.LastEmittedSubmissionGeneration =
            $controller.SubmissionGeneration
        $controller.PendingRebind = $null
        $controller.FailedRuntimePrompt = $null
        $controller.RepairSignaled = $false
        $controller.RuntimeFailureSignaled = $false
        return $true
    }
    catch {
        if ($installed -and $function:prompt -eq $candidate.Wrapper) {
            Set-Item -LiteralPath Function:\global:prompt `
                -Value $downstream.ScriptBlock -ErrorAction Ignore
        }
        $controller.ActiveGeneration = $oldGeneration
        $controller.ActiveGenerationId = $oldGenerationId
        $controller.NextGenerationId = $oldNextGenerationId
        $controller.LastHistoryId = $oldLastHistoryId
        $controller.LastErrorRecord = $oldLastErrorRecord
        $controller.LastSubmittedLine = $oldLastSubmittedLine
        $controller.LastEmittedSubmissionGeneration =
            $oldLastEmittedSubmissionGeneration
        $controller.PendingRebind = $oldPendingRebind
        $controller.FailedRuntimePrompt = $oldFailedRuntimePrompt
        $controller.RepairSignaled = $oldRepairSignaled
        $controller.RuntimeFailureSignaled = $oldRuntimeFailureSignaled
        return $false
    }
    finally {
        $controller.BindingInProgress = $false
    }
}

$__ShellInteg_CandidateState = @{
    Version = 8
    InstallComplete = $false
    OriginalReadLine =
        if ($null -ne $__ShellInteg_OriginalReadLine) {
            $__ShellInteg_OriginalReadLine.ScriptBlock
        }
        else {
            $null
        }
    ReadLineWrapper = $null
    CanInspectErrors = $__ShellInteg_CanInspectErrors
    GenerationFactory = $__ShellInteg_GenerationFactory
    OmpMatcher = $__ShellInteg_OmpMatcher
    RuntimeBind = $__ShellInteg_RuntimeBind
    ActiveGeneration = $null
    ActiveGenerationId = 0L
    NextGenerationId = 2L
    BindingInProgress = $false
    PromptDepth = 0
    InReadLine = $false
    PendingRebind = $null
    FailedRuntimePrompt = $null
    LastHistoryId = -1
    LastErrorRecord = $Error[0]
    LastSubmittedLine = $null
    SubmissionGeneration = 0L
    LastEmittedSubmissionGeneration = 0L
    RepairSignaled = $false
    RuntimeFailureSignaled = $false
}

$__ShellInteg_InitialGeneration =
    & $__ShellInteg_GenerationFactory `
        $__ShellInteg_CandidateState 1L $__ShellInteg_OriginalPrompt
$__ShellInteg_CandidateState.ActiveGeneration = $__ShellInteg_InitialGeneration
$__ShellInteg_CandidateState.ActiveGenerationId = 1L

$__ShellInteg_ReadLineWrapper = if ($__ShellInteg_CanInspectErrors -and
    $null -ne $__ShellInteg_OriginalReadLine) {
    {
        $state = $Global:__ShellInteg_State
        $active = $state.ActiveGeneration
        if ($function:prompt -ne $active.Wrapper) {
            $pending = $state.PendingRebind
            if ($null -ne $pending -and
                $pending.GenerationId -eq $state.ActiveGenerationId -and
                $pending.PromptWrapper -eq $active.Wrapper -and
                $function:PSConsoleHostReadLine -eq $state.ReadLineWrapper) {
                $history = Get-History -Count 1 -ErrorAction Ignore
                $historyFailed =
                    $history -and
                    $history.Id -gt $pending.HistoryBaseline -and
                    ($history.ExecutionStatus -eq 'Failed' -or
                        $history.ExecutionStatus -eq 'Stopped')
                $rebound = $false
                if (-not $historyFailed) {
                    $replacement =
                        Get-Item -LiteralPath Function:\prompt -ErrorAction Ignore
                    $rebound = & $state.RuntimeBind $state $replacement
                }

                $shellName =
                    if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
                $E = [char]0x1B
                $B = [char]0x07
                if ($rebound) {
                    Microsoft.PowerShell.Utility\Write-Host -Object (
                        "${E}]9001;ShellIntegrationRuntime;${shellName};rebound${B}") `
                        -NoNewline -InformationAction Continue
                }
                elseif (-not $state.RuntimeFailureSignaled) {
                    $state.FailedRuntimePrompt = $function:prompt
                    Microsoft.PowerShell.Utility\Write-Host -Object (
                        "${E}]9001;ShellIntegrationRuntime;${shellName};rebind-failed${B}") `
                        -NoNewline -InformationAction Continue
                    $state.RuntimeFailureSignaled = $true
                    $state.PendingRebind = $null
                }
            }
            elseif ($null -ne $state.FailedRuntimePrompt -and
                $function:prompt -eq $state.FailedRuntimePrompt) {
                # This exact prompt already produced a pane-local runtime
                # failure. Do not misclassify it as a persisted profile issue.
            }
            elseif (-not $state.RepairSignaled) {
                $state.FailedRuntimePrompt = $null
                $shellName =
                    if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
                $E = [char]0x1B
                $B = [char]0x07
                Microsoft.PowerShell.Utility\Write-Host -Object (
                    "${E}]9001;ShellIntegrationRepair;${shellName};prompt-changed${B}") `
                    -NoNewline -InformationAction Continue
                $state.RepairSignaled = $true
            }
        }

        $state.InReadLine = $true
        try {
            $line = & $state.OriginalReadLine @args
        }
        finally {
            $state.InReadLine = $false
        }

        $state.SubmissionGeneration++
        $state.LastSubmittedLine = if ($line -is [string]) { $line } else { $null }
        $match =
            if ($line -is [string]) { & $state.OmpMatcher $line } else { $null }
        if ($null -ne $match -and
            $function:prompt -eq $state.ActiveGeneration.Wrapper) {
            $history = Get-History -Count 1 -ErrorAction Ignore
            $state.PendingRebind = @{
                GenerationId = $state.ActiveGenerationId
                PromptWrapper = $state.ActiveGeneration.Wrapper
                HistoryBaseline = if ($history) { $history.Id } else { -1 }
            }
        }
        else {
            $state.PendingRebind = $null
        }
        return $line
    }.GetNewClosure()
}
else {
    $null
}

$__ShellInteg_CandidateState.ReadLineWrapper = $__ShellInteg_ReadLineWrapper

try {
    New-Variable -Name __ShellInteg_State -Scope Global -Value $__ShellInteg_CandidateState -Option ReadOnly -ErrorAction Stop
    if ($null -ne $__ShellInteg_ReadLineWrapper) {
        Set-Item -LiteralPath Function:\global:PSConsoleHostReadLine -Value $__ShellInteg_ReadLineWrapper -ErrorAction Stop
    }
    Set-Item -LiteralPath Function:\global:prompt -Value $__ShellInteg_InitialGeneration.Wrapper -ErrorAction Stop
    $__ShellInteg_CandidateState.InstallComplete = $true
}
catch {
    if ($function:prompt -eq $__ShellInteg_InitialGeneration.Wrapper) {
        Set-Item -LiteralPath Function:\global:prompt -Value $__ShellInteg_OriginalPrompt.ScriptBlock -ErrorAction Ignore
    }
    if ($null -ne $__ShellInteg_ReadLineWrapper -and
        $function:PSConsoleHostReadLine -eq $__ShellInteg_ReadLineWrapper) {
        Set-Item -LiteralPath Function:\global:PSConsoleHostReadLine -Value $__ShellInteg_OriginalReadLine.ScriptBlock -ErrorAction Ignore
    }
    $__ShellInteg_PublishedState = Get-Variable -Name __ShellInteg_State -Scope Global -ErrorAction Ignore
    if ($null -ne $__ShellInteg_PublishedState -and
        $__ShellInteg_PublishedState.Value -eq $__ShellInteg_CandidateState) {
        Remove-Variable -Name __ShellInteg_State -Scope Global -Force -ErrorAction Ignore
    }
    Microsoft.PowerShell.Utility\Write-Host -Object (
        "${__ShellInteg_E}]9001;ShellIntegrationRepair;${__ShellInteg_ShellName};bind-failed${__ShellInteg_B}") `
        -NoNewline -InformationAction Continue
    Remove-Variable __ShellInteg_E, __ShellInteg_B, __ShellInteg_ShellName,
        __ShellInteg_StateVariable, __ShellInteg_OriginalPrompt,
        __ShellInteg_OriginalReadLine, __ShellInteg_CanInspectErrors,
        __ShellInteg_V6State,
        __ShellInteg_GenerationFactory, __ShellInteg_OmpMatcher,
        __ShellInteg_RuntimeBind, __ShellInteg_CandidateState,
        __ShellInteg_InitialGeneration,
        __ShellInteg_ReadLineWrapper, __ShellInteg_PublishedState -ErrorAction Ignore
    return
}

Microsoft.PowerShell.Utility\Write-Host -Object (
    "${__ShellInteg_E}]9001;ShellIntegrationReady;${__ShellInteg_ShellName};8${__ShellInteg_B}") `
    -NoNewline -InformationAction Continue
Remove-Variable __ShellInteg_E, __ShellInteg_B, __ShellInteg_ShellName,
    __ShellInteg_StateVariable, __ShellInteg_OriginalPrompt,
    __ShellInteg_OriginalReadLine, __ShellInteg_CanInspectErrors,
    __ShellInteg_V6State,
    __ShellInteg_GenerationFactory, __ShellInteg_OmpMatcher,
    __ShellInteg_RuntimeBind, __ShellInteg_CandidateState,
    __ShellInteg_InitialGeneration,
    __ShellInteg_ReadLineWrapper -ErrorAction Ignore
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
