# send-event.ps1 — Telemetry hook for WTA agent session tracking.
#
# ── EXIT-CODE CONTRACT ──────────────────────────────────────────────────
# This script MUST exit 0 unconditionally. It is wired to Claude / Copilot
# PreToolUse, UserPromptSubmit, Stop, SubagentStop, and other lifecycle
# events where a non-zero exit has *semantic* consequences:
#   * Exit 2  → blocks the tool call / erases the user prompt /
#               forces Claude to keep going past Stop
#   * Other   → shows "<hook> hook error" + first line of stderr in the
#               transcript on every fire
# Two guarantees defend the contract:
#   1. `trap { exit 0 }` at the top — catches any terminating error that
#      escapes the outer try/catch (script init, throws from inside the
#      catch handler itself, etc).
#   2. The single outer try/catch wraps every action and broadly swallows
#      anything that fails inside it.
# Do NOT add `exit N` for non-zero N anywhere. Do NOT remove the trap.
#
# ── STDIO DISCIPLINE ────────────────────────────────────────────────────
# Write nothing to stdout or stderr. On UserPromptSubmit / SessionStart,
# stdout is added to the model's context — every byte leaks tokens and
# can be a prompt-injection vector. Diagnostics go to
# %LOCALAPPDATA%\IntelligentTerminal\logs\hook-trace.log only.
#
# ── CLI-source identification ───────────────────────────────────────────
# The installer hard-codes which CLI invokes this script via the
# `-CliSource` parameter (claude / copilot / gemini). That is the
# ONLY reliable signal — env-var heuristics are unreliable because
# Copilot CLI inherits Claude's plugin shape and sets CLAUDE_PLUGIN_ROOT,
# making it indistinguishable from a real Claude run by env vars alone.
param(
    [string]$EventType = "agent.hook",
    [string]$CliSource = ""
)

# Failsafe: see CONTRACT above. Last line of defense behind the outer
# try/catch. Triggers on any terminating error (including ones thrown
# from inside the catch handler itself).
trap { exit 0 }

# Skip if not running inside Windows Terminal.
# (Checked before the diagnostic trace so we don't spam hook-trace.log
# with ENTER lines on every tool event when WTA isn't in play — the
# hook has nothing useful to do without WT_COM_CLSID anyway.)
if (-not $env:WT_COM_CLSID) { exit 0 }

# Single outer try/catch wraps every action this script takes. Catch is
# intentionally broad — see CONTRACT above. We deliberately do NOT
# narrow exception types here: this script must never propagate any
# failure to the parent agent CLI.
$tracePath = $null
try {
    # ── diagnostic trace + 5 MB rotation ────────────────────────────────
    # Every successful invocation appends one ENTER line so we can
    # diagnose missing SessionEnd events on Ctrl+C without relying on
    # wta seeing the message. Rotates at 5 MB to hook-trace.log.1
    # (one backup kept, ~10 MB ceiling).
    #
    # No external logging library here on purpose — every hook event
    # spawns a fresh PowerShell process, so importing PSFramework /
    # Microsoft.Extensions.Logging would add module-load latency to
    # every tool call. Rolling ~10 lines is the right tradeoff.
    $traceDir = Join-Path $env:LOCALAPPDATA 'IntelligentTerminal\logs'
    if (-not (Test-Path -LiteralPath $traceDir)) {
        New-Item -ItemType Directory -Path $traceDir -Force | Out-Null
    }
    $tracePath = Join-Path $traceDir 'hook-trace.log'

    # 5 MB rotation. Move-Item -Force atomically overwrites the .1.
    # Recoverable failures (file locked by AV / another hook racing us)
    # are absorbed by the outer catch; next invocation retries. A
    # persistent failure (ACL / read-only) surfaces via unbounded
    # growth, which is a visible signal.
    if ((Test-Path -LiteralPath $tracePath) -and
        (Get-Item -LiteralPath $tracePath).Length -ge 5MB) {
        Move-Item -LiteralPath $tracePath -Destination "$tracePath.1" -Force
    }

    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
    $cliEnvHint =
        if ($env:COPILOT_SESSION_ID) { 'copilot' }
        elseif ($env:GEMINI_SESSION_ID) { 'gemini' }
        elseif ($env:CLAUDE_SESSION_ID) { 'claude' }
        elseif ($env:GEMINI_CLI)   { 'gemini' }
        elseif ($env:COPILOT_CLI)  { 'copilot' }
        elseif ($env:CLAUDE_PLUGIN_ROOT) { 'claude' }
        else { '<unknown>' }
    $wtSess = if ($env:WT_SESSION) { $env:WT_SESSION } else { '<no-WT_SESSION>' }
    Add-Content -LiteralPath $tracePath -Value "$stamp | ENTER cli=$CliSource event=$EventType envHint=$cliEnvHint wt=$wtSess pid=$PID" -ErrorAction SilentlyContinue

    # ── Locate wtcli.exe ────────────────────────────────────────────────
    # Order: PATH (works if the package registers a wtcli AppExecutionAlias),
    # then $env:WTCLI_PATH override (escape hatch for dev builds /
    # debugging), then the Windows Terminal package InstallLocation
    # (where the build drops it).
    $wtcliPath = (Get-Command wtcli -ErrorAction SilentlyContinue).Source
    if (-not $wtcliPath -and $env:WTCLI_PATH -and (Test-Path $env:WTCLI_PATH)) {
        $wtcliPath = $env:WTCLI_PATH
    }
    if (-not $wtcliPath) {
        $pkgs = Get-AppxPackage -Name "*Terminal*" -ErrorAction SilentlyContinue
        foreach ($pkg in $pkgs) {
            $candidate = Join-Path $pkg.InstallLocation "wtcli.exe"
            if (Test-Path $candidate) { $wtcliPath = $candidate; break }
        }
    }
    if (-not $wtcliPath) { exit 0 }

    # ── Read hook JSON from stdin ───────────────────────────────────────
    # May be empty for events that don't carry a payload, e.g. some CLIs'
    # AfterTool / SessionEnd. We still want those to reach WTA so the
    # state can transition out of Working back to Idle.
    $hookData = [Console]::In.ReadToEnd()
    if (-not $hookData) { $hookData = "" }

    # ConvertFrom-Json on empty/whitespace input throws; skip the call so
    # the outer catch isn't triggered for a benign empty payload.
    # Malformed (non-empty) JSON is rare in practice and will fall through
    # to the outer catch, dropping the event entirely — that's acceptable
    # given the single-try-catch design.
    $parsed = $null
    if ($hookData.Trim()) {
        $parsed = $hookData | ConvertFrom-Json
    }

    # Extract agent_session_id from stdin JSON (Claude/Gemini), env (Copilot), or empty.
    $agentSessionId = ""
    if ($parsed -and ($parsed.PSObject.Properties.Name -contains "session_id")) {
        $agentSessionId = [string]$parsed.session_id
    } elseif ($env:COPILOT_SESSION_ID) {
        $agentSessionId = $env:COPILOT_SESSION_ID
    } elseif ($env:CLAUDE_SESSION_ID) {
        $agentSessionId = $env:CLAUDE_SESSION_ID
    } elseif ($env:GEMINI_SESSION_ID) {
        $agentSessionId = $env:GEMINI_SESSION_ID
    }

    # Detect CLI source — priority order:
    #   1. The `-CliSource` script parameter (set by the installer per-CLI;
    #      most reliable: hard-coded at install time, not affected by
    #      env-var leakage between CLIs that share Claude's plugin shape).
    #   2. WTA_CLI_SOURCE env var (manual override / bash hooks).
    #   3. CLI-specific session-id env vars (only that CLI sets each one).
    #   4. CLI-specific marker env vars.
    #   5. CLAUDE_PLUGIN_ROOT — last resort BEFORE the default.
    #   6. Default "copilot" — LEGACY fallback; should never be hit when
    #      installer plumbing is correct.
    if (-not $CliSource) { $CliSource = $env:WTA_CLI_SOURCE }
    if (-not $CliSource) {
        if     ($env:COPILOT_SESSION_ID) { $CliSource = "copilot" }
        elseif ($env:GEMINI_SESSION_ID)  { $CliSource = "gemini" }
        elseif ($env:CLAUDE_SESSION_ID)  { $CliSource = "claude" }
        elseif ($env:GEMINI_CLI)         { $CliSource = "gemini" }
        elseif ($env:COPILOT_CLI)        { $CliSource = "copilot" }
        elseif ($env:CLAUDE_PLUGIN_ROOT) { $CliSource = "claude" }
        else { $CliSource = "copilot" }
    }
    $cliSource = $CliSource

    $wrapper = @{
        cli_source       = $cliSource
        agent_session_id = $agentSessionId
        payload          = $parsed
    }

    $payload = $wrapper | ConvertTo-Json -Compress -Depth 5

    # CommandLineToArgvW-correct escape for a quoted argument:
    #   * Every backslash run that precedes a `"` (or end of string) is doubled.
    #   * Every `"` is preceded by a single extra backslash.
    # This is required so messages containing Windows paths (e.g. permission
    # prompts: 'Get-Acl -Path "C:\Windows\..."') don't have their JSON truncated
    # by the child process's argv parser.
    $sb = New-Object System.Text.StringBuilder
    $bsRun = 0
    foreach ($ch in $payload.ToCharArray()) {
        if ($ch -eq '\') {
            $bsRun++
        } elseif ($ch -eq '"') {
            [void]$sb.Append([string]'\' * ($bsRun * 2 + 1))
            [void]$sb.Append('"')
            $bsRun = 0
        } else {
            if ($bsRun -gt 0) { [void]$sb.Append([string]'\' * $bsRun); $bsRun = 0 }
            [void]$sb.Append($ch)
        }
    }
    if ($bsRun -gt 0) { [void]$sb.Append([string]'\' * ($bsRun * 2)) }
    $escaped = $sb.ToString()

    # Pass our pane GUID via -p so wtcli stamps the event with this pane's
    # session_id. Without -p, wtcli falls back to GetActivePane() which is
    # whichever pane the user is currently focused on — that gives every row
    # in the F2 list the same (focused) pane GUID, so Enter on any live row
    # focuses the focused pane instead of its own pane.
    $paneArg = ''
    if ($env:WT_SESSION) {
        $paneArg = " -p `"$($env:WT_SESSION)`""
    }
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $wtcliPath
    $psi.Arguments = "send-event -e $EventType$paneArg `"$escaped`""
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardError = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $exited = $proc.WaitForExit(5000)

    # OK breadcrumb. If StandardError.ReadToEnd throws (rare; process
    # exited abnormally) the outer catch absorbs it and we get the ERROR
    # breadcrumb instead — either way we record something.
    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
    $exitInfo = if ($exited) { "exit=$($proc.ExitCode)" } else { 'TIMEOUT_5s' }
    $stderrSnippet = ($proc.StandardError.ReadToEnd() -replace "[\r\n]+", ' ').Trim()
    if ($stderrSnippet.Length -gt 200) { $stderrSnippet = $stderrSnippet.Substring(0, 200) + '...' }
    $sessIdShort = if ($agentSessionId) { $agentSessionId.Substring(0, [Math]::Min(8, $agentSessionId.Length)) } else { '<none>' }
    Add-Content -LiteralPath $tracePath -Value "$stamp | OK cli=$cliSource event=$EventType $exitInfo sessId=$sessIdShort wtcli=$wtcliPath stderr=`"$stderrSnippet`"" -ErrorAction SilentlyContinue
} catch {
    # Single error sink. Best-effort ERROR breadcrumb; if Add-Content
    # itself throws, the `trap { exit 0 }` at the top catches it.
    # $tracePath may be unset if we crashed before reaching the trace
    # dir setup — guard before touching it.
    if ($tracePath) {
        $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
        $msg = ($_.Exception.Message -replace "[\r\n]+", ' ').Trim()
        Add-Content -LiteralPath $tracePath -Value "$stamp | ERROR cli=$CliSource event=$EventType ex=`"$msg`"" -ErrorAction SilentlyContinue
    }
}

# Explicit exit 0 per CONTRACT above. Without this, PowerShell's default
# exit code reflects whatever $LASTEXITCODE was set to by the most recent
# native command (e.g. wtcli's own exit code) — which we do NOT want to
# propagate to the parent CLI.
exit 0
