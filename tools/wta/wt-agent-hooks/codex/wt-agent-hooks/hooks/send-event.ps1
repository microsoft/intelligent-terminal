# send-event.ps1 — Telemetry hook for WTA agent session tracking.
#
# ── EXIT-CODE CONTRACT ──────────────────────────────────────────────────
# This script MUST exit 0 unconditionally. It is wired to lifecycle
# events where a non-zero exit has *semantic* consequences:
#   * Exit 2  → blocks the tool call / erases the user prompt /
#               forces to keep going past Stop
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
# `-CliSource` parameter (claude / codex / copilot / gemini). That is the
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
    # Appends one ENTER line per invocation so we can diagnose missing
    # SessionEnd events on Ctrl+C. Soft 5 MB rotation: the check fires
    # at the start of the NEXT hook after the threshold, so both the
    # active log and the `.1` backup can briefly exceed 5 MB.
    #
    # `-ErrorAction SilentlyContinue` on every filesystem cmdlet: the
    # hook CONTRACT forbids writing anything to stdout/stderr, and
    # New-Item / Get-Item / Move-Item emit non-terminating errors
    # (AV-locked file, ACL denied, hooks racing) that bypass the outer
    # try/catch and would otherwise leak into the parent CLI transcript.
    # A persistent failure (read-only / no disk) surfaces via unbounded
    # log growth, which is a visible signal.
    # Prefer the package-private log dir handed down by wta-master via
    # WTA_HOOK_LOG_DIR — it points at the LocalCache\Local store this script
    # can't resolve on its own (it only sees the un-redirected %LOCALAPPDATA%
    # and doesn't know the package family name). Fall back to the bare path
    # when the var is absent (unpackaged dev runs / older wta).
    $traceDir = if ($env:WTA_HOOK_LOG_DIR) {
        $env:WTA_HOOK_LOG_DIR
    } else {
        Join-Path $env:LOCALAPPDATA 'IntelligentTerminal\logs'
    }
    if (-not (Test-Path -LiteralPath $traceDir)) {
        New-Item -ItemType Directory -Path $traceDir -Force -ErrorAction SilentlyContinue | Out-Null
    }
    $tracePath = Join-Path $traceDir 'hook-trace.log'

    $traceItem = Get-Item -LiteralPath $tracePath -ErrorAction SilentlyContinue
    if (($traceItem -is [System.IO.FileInfo]) -and $traceItem.Length -ge 5MB) {
        Move-Item -LiteralPath $tracePath -Destination "$tracePath.1" -Force -ErrorAction SilentlyContinue
    }

    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
    $cliEnvHint =
        if ($env:COPILOT_SESSION_ID) { 'copilot' }
        elseif ($env:GEMINI_SESSION_ID) { 'gemini' }
        elseif ($env:CLAUDE_SESSION_ID) { 'claude' }
        elseif ($env:CODEX_SESSION_ID) { 'codex' }
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

    # Extract agent_session_id from stdin JSON (Claude/Gemini/Codex), env (Copilot), or empty.
    $agentSessionId = ""
    if ($parsed -and ($parsed.PSObject.Properties.Name -contains "session_id")) {
        $agentSessionId = [string]$parsed.session_id
    } elseif ($env:COPILOT_SESSION_ID) {
        $agentSessionId = $env:COPILOT_SESSION_ID
    } elseif ($env:CLAUDE_SESSION_ID) {
        $agentSessionId = $env:CLAUDE_SESSION_ID
    } elseif ($env:GEMINI_SESSION_ID) {
        $agentSessionId = $env:GEMINI_SESSION_ID
    } elseif ($env:CODEX_SESSION_ID) {
        $agentSessionId = $env:CODEX_SESSION_ID
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
        elseif ($env:CODEX_SESSION_ID)   { $CliSource = "codex" }
        elseif ($env:GEMINI_CLI)         { $CliSource = "gemini" }
        elseif ($env:COPILOT_CLI)        { $CliSource = "copilot" }
        elseif ($env:CLAUDE_PLUGIN_ROOT) { $CliSource = "claude" }
        else { $CliSource = "copilot" }
    }
    $cliSource = $CliSource

    # Drop large / model-bound fields wta never reads, so multi-KB tool output
    # and the user's prompt text don't ride the
    # hook -> wtcli -> COM -> wta pipeline for nothing.
    #
    # Why this matters even though dispatch is async (ShellExecuteEx, hidden
    # window): every prompt spawns a new wtcli.exe, and the wrapped JSON is
    # passed as a single CreateProcess argv. Windows caps the command-line
    # near 32 768 chars, so a long pasted prompt or a Write/Edit tool_input
    # carrying file contents can truncate the JSON (or silently fail to
    # spawn). The COM SendEvent broadcast also has to marshal the HString
    # to every listener (TerminalPage + every `wtcli listen` subscriber).
    #
    # Authoritative list of fields wta consumes lives in tools/wta/src/app.rs
    # (route_one_hook switch). Anything outside that list is pure overhead.
    if ($parsed -is [System.Management.Automation.PSCustomObject]) {
        # Always-strip: large tool-call results, the user's prompt text
        # (UserPromptSubmit / BeforeAgent — wta only needs the state flip,
        # never the body), and CLI-side metadata wta never reads (paths,
        # model info, permission mode, hook bookkeeping, etc).
        $alwaysStrip = @(
            'tool_result', 'tool_response', 'tool_output',
            'toolResult', 'toolResponse', 'toolOutput',
            'prompt', 'user_prompt', 'userPrompt',
            'transcript_path', 'transcriptPath',
            'hook_event_name', 'hookEventName',
            'permission_mode', 'permissionMode',
            'model', 'model_info', 'modelInfo',
            'output_style', 'outputStyle',
            'version', 'source', 'apiKeySource',
            # Large per-session context that some CLIs (notably Copilot)
            # bundle into SessionStart / Stop hook stdin when restoring or
            # snapshotting a conversation. None of these are consumed by
            # wta — the SessionStart/SessionEnd handlers only flip state
            # and need `session_id`. Without this strip a single long
            # conversation puts SessionStart over the CreateProcess argv
            # cap and the hook silently fails ("filename or extension is
            # too long").
            'transcript', 'messages', 'history', 'conversation',
            'systemPrompt', 'system_prompt', 'instructions', 'context',
            'files', 'attachments', 'events', 'chat', 'chatHistory'
        )
        # NOTE: `cwd` is intentionally NOT stripped. wta's route_one_hook
        # (app.rs ~582) reads `payload["cwd"]` to populate
        # SessionStarted.cwd and to derive a synthetic title (the cwd
        # basename) for events that arrive before the real
        # `agent.session.started` lands. Stripping it here would empty
        # the cwd field for every session whose first hook is not
        # SessionStart (every Copilot session, since prompt.submit fires
        # first in that CLI's run model) and produce blank rows in the
        # session-management picker.
        foreach ($key in $alwaysStrip) {
            if ($parsed.PSObject.Properties[$key]) {
                $parsed.PSObject.Properties.Remove($key)
            }
        }

        # tool_input is only consumed by wta when tool_name is a user-input
        # tool (mirrors `is_user_input_tool` in agent_sessions.rs, where wta
        # pulls `tool_input.{question,prompt,message}` to surface the
        # question text in a Notification). For every other tool — Bash
        # commands, Write/Edit file contents, MCP arg payloads — it's pure
        # overhead and can be many KB.
        $toolNameProp = $parsed.PSObject.Properties['tool_name']
        if (-not $toolNameProp) { $toolNameProp = $parsed.PSObject.Properties['toolName'] }
        $toolNameLower = if ($toolNameProp) { ([string]$toolNameProp.Value).ToLowerInvariant() } else { '' }
        $userInputTools = @(
            'ask_user', 'askuser', 'ask-user',
            'ask_question', 'askquestion', 'ask_for_clarification',
            'request_input', 'request_user_input', 'user_input',
            'prompt_user', 'clarification_request'
        )
        if (-not ($userInputTools -contains $toolNameLower)) {
            foreach ($key in @('tool_input', 'toolInput')) {
                if ($parsed.PSObject.Properties[$key]) {
                    $parsed.PSObject.Properties.Remove($key)
                }
            }
        }
    }

    $wrapper = @{
        cli_source       = $cliSource
        agent_session_id = $agentSessionId
        payload          = $parsed
    }

    $payload = $wrapper | ConvertTo-Json -Compress -Depth 5

    # Size guard — final defense against CreateProcess argv overflow.
    #
    # Even with the aggressive strip above, a CLI we don't know about can
    # bundle unexpectedly large state into stdin (a new Copilot experiment
    # ships full conversation context on SessionStart; a custom agent
    # might dump anything). When that happens, the argv-escaped JSON below
    # would push past Windows' ~32 768-char CreateProcess command-line cap
    # and the `Process.Start` call throws "filename or extension is too
    # long", silently dropping the event.
    #
    # When the wrapper is too big we keep the bare-minimum envelope —
    # cli_source + agent_session_id — which is all wta's SessionStart /
    # SessionEnd / agent.prompt.submit / agent.stop handlers actually
    # consume. Notification and agent.error lose their `message` /
    # `error` text in this rare case; that's a deliberate trade-off vs.
    # the alternative of dropping the event entirely.
    #
    # Threshold: 25 000 raw-JSON chars leaves ~7 KB of headroom for the
    # wtcli.exe path (~80), the surrounding `send-event -e <event>
    # -p "<pane>" "..."` framing (~80), and worst-case 2x growth from
    # the CommandLineToArgvW backslash-doubling escape below.
    $MAX_PAYLOAD_CHARS = 25000
    $payloadTruncated  = $false
    $originalSize      = $payload.Length
    if ($payload.Length -gt $MAX_PAYLOAD_CHARS) {
        $payloadTruncated = $true
        $wrapper = @{
            cli_source       = $cliSource
            agent_session_id = $agentSessionId
            payload          = @{
                _truncated     = $true
                _original_size = $originalSize
            }
        }
        $payload = $wrapper | ConvertTo-Json -Compress -Depth 3
    }

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
    # in the session management list the same (focused) pane GUID, so Enter on any live row
    # focuses the focused pane instead of its own pane.
    $paneArg = ''
    if ($env:WT_SESSION) {
        $paneArg = " -p `"$($env:WT_SESSION)`""
    }
    # Async dispatch: launch wtcli via ShellExecuteEx so the parent PowerShell
    # process can exit immediately without waiting for wtcli's COM round-trip.
    # The hook contract is "exit 0 quickly"; WTA is a fire-and-observe
    # listener, so we don't need wtcli's exit code or stderr.
    #
    # Why UseShellExecute=$true:
    #   - Child gets its own console (no inherited stdio handles), so this
    #     PowerShell can exit without waiting for the child's pipes to drain.
    #   - WindowStyle=Hidden -> wtcli runs invisibly (no flashing console).
    #   - No cmd.exe wrapper, no handle juggling, no WaitForExit timeout.
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $wtcliPath
    $psi.Arguments = "send-event -e $EventType$paneArg `"$escaped`""
    $psi.UseShellExecute = $true
    $psi.WindowStyle = 'Hidden'
    [void][System.Diagnostics.Process]::Start($psi)
    $stamp = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss.fff')
    $sessIdShort = if ($agentSessionId) { $agentSessionId.Substring(0, [Math]::Min(8, $agentSessionId.Length)) } else { '<none>' }
    $truncTag = if ($payloadTruncated) { " TRUNCATED orig=$originalSize" } else { "" }
    Add-Content -LiteralPath $tracePath -Value "$stamp | DISPATCHED cli=$cliSource event=$EventType sessId=$sessIdShort wtcli=$wtcliPath$truncTag" -ErrorAction SilentlyContinue
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
