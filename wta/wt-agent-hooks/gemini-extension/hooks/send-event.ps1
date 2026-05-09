# send-event.ps1 — Forward Copilot CLI hook events to WTA via wtcli
#
# CLI-source identification:
#   The installer hard-codes which CLI invokes this script via the
#   `-CliSource` parameter (claude / copilot / gemini). That is the
#   ONLY reliable signal — env-var heuristics are unreliable because
#   Copilot CLI inherits Claude's plugin shape and sets CLAUDE_PLUGIN_ROOT,
#   making it indistinguishable from a real Claude run by env vars alone.
param(
    [string]$EventType = "agent.hook",
    [string]$CliSource = ""
)

# ─── diagnostic trace (round 13) ────────────────────────────────────────
# Every hook invocation appends one line so we can diagnose missing
# SessionEnd events on Ctrl+C without relying on wta seeing the message.
# Writes to %LOCALAPPDATA%\IntelligentTerminal\logs\hook-trace.log.
# Best-effort; never throws.
$traceWritten = $false
try {
    $traceDir = Join-Path $env:LOCALAPPDATA 'IntelligentTerminal\logs'
    if (-not (Test-Path -LiteralPath $traceDir)) {
        New-Item -ItemType Directory -Path $traceDir -Force | Out-Null
    }
    $tracePath = Join-Path $traceDir 'hook-trace.log'
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
    $line = "$stamp | ENTER cli=$CliSource event=$EventType envHint=$cliEnvHint wt=$wtSess pid=$PID"
    Add-Content -LiteralPath $tracePath -Value $line -ErrorAction SilentlyContinue
    $traceWritten = $true
} catch { }
# ────────────────────────────────────────────────────────────────────────

# Skip if not running inside Windows Terminal
if (-not $env:WT_COM_CLSID) {
    if ($traceWritten) {
        try {
            $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
            Add-Content -LiteralPath $tracePath -Value "$stamp | SKIP no WT_COM_CLSID (cli=$CliSource event=$EventType)" -ErrorAction SilentlyContinue
        } catch { }
    }
    exit 0
}

# Locate wtcli.exe. Order:
#   1. PATH (works if the package registers a wtcli AppExecutionAlias).
#   2. $env:WTCLI_PATH override (escape hatch for dev builds / debugging).
#   3. The Windows Terminal package InstallLocation (where the build drops it).
$wtcliPath = (Get-Command wtcli -ErrorAction SilentlyContinue).Source
if (-not $wtcliPath -and $env:WTCLI_PATH -and (Test-Path $env:WTCLI_PATH)) {
    $wtcliPath = $env:WTCLI_PATH
}
if (-not $wtcliPath) {
    try {
        $pkgs = Get-AppxPackage -Name "*Terminal*" -ErrorAction SilentlyContinue
        foreach ($pkg in $pkgs) {
            $candidate = Join-Path $pkg.InstallLocation "wtcli.exe"
            if (Test-Path $candidate) { $wtcliPath = $candidate; break }
        }
    } catch { }
}
if (-not $wtcliPath) { exit 0 }

# Read hook JSON from stdin (may be empty for events that don't carry a
# payload, e.g. some CLIs' AfterTool / SessionEnd. We still want those to
# reach WTA so the state can transition out of Working/Working back to Idle.)
$hookData = [Console]::In.ReadToEnd()
if (-not $hookData) { $hookData = "" }

# Wrap payload and send via ProcessStartInfo to avoid PowerShell argument mangling
try {
    # ConvertFrom-Json on empty/whitespace input throws; treat as no payload.
    $parsed = $null
    if ($hookData.Trim()) {
        try { $parsed = $hookData | ConvertFrom-Json } catch { $parsed = $null }
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
    if ($traceWritten) {
        try {
            $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
            $exitInfo = if ($exited) { "exit=$($proc.ExitCode)" } else { 'TIMEOUT_5s' }
            $stderrSnippet = ''
            try { $stderrSnippet = ($proc.StandardError.ReadToEnd() -replace "[\r\n]+", ' ').Trim() } catch { }
            if ($stderrSnippet.Length -gt 200) { $stderrSnippet = $stderrSnippet.Substring(0, 200) + '...' }
            $sessIdShort = if ($agentSessionId) { $agentSessionId.Substring(0, [Math]::Min(8, $agentSessionId.Length)) } else { '<none>' }
            Add-Content -LiteralPath $tracePath -Value "$stamp | OK cli=$cliSource event=$EventType $exitInfo sessId=$sessIdShort wtcli=$wtcliPath stderr=`"$stderrSnippet`"" -ErrorAction SilentlyContinue
        } catch { }
    }
} catch {
    if ($traceWritten) {
        try {
            $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
            $msg = ($_.Exception.Message -replace "[\r\n]+", ' ').Trim()
            Add-Content -LiteralPath $tracePath -Value "$stamp | ERROR cli=$CliSource event=$EventType ex=`"$msg`"" -ErrorAction SilentlyContinue
        } catch { }
    }
    # Silently ignore errors — hooks must not block the agent.
}
