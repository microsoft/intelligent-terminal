# Agent.ps1 — agent-pane primitives.
#
# IMPORTANT: the agent pane is a XAML `AgentPaneContent` area, NOT a wtcli/protocol pane.
# It does NOT appear in `list-panes` and has no protocol session_id, so detection and
# focus go through the UI (winapp ui), not wtcli. Detect "open" by the agent UI elements
# (AgentLabelText / AgentLogo) that exist only while the pane is shown.

$script:ItAgentOpenSelector = 'AgentLabelText'

function Test-AgentPaneOpen {
    <# Is the agent pane currently shown? (UI detection — not a protocol pane.) #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 2)
    process { Test-UiElementExists -App $App -Selector $script:ItAgentOpenSelector -TimeoutSec $TimeoutSec }
}

function Open-AgentPane {
    <#
    .SYNOPSIS
        Open/restore the agent pane via the bottom-bar AgentToggleButton (UIA), re-invoking
        if it doesn't appear in a slice. Self-verifies the agent pane is shown.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 45)
    process {
        if (Test-AgentPaneOpen -App $App) { return $App }
        # Invoke the toggle, then poll; if the pane hasn't appeared within a slice, re-invoke
        # (the first click can land on a busy frame / before the pre-warm helper is ready,
        # especially after several consecutive launches under load).
        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        $attempt = 0
        do {
            $attempt++
            # The only reliable open is the UIA toggle button; WT's command-palette
            # accelerator is not reachable from this harness (send-keys reaches the conpty,
            # not WT's keybinding handler), so there is no send-keys fallback. Re-invoke if
            # the pane hasn't appeared (the first click can land on a busy frame / before
            # the pre-warm helper is ready, especially after several launches under load).
            try { Invoke-UiElement -App $App -Selector 'AgentToggleButton' -TimeoutSec 8 | Out-Null }
            catch { Write-ItLog -Level WARN -Message "AgentToggleButton invoke failed (attempt $attempt): $_" }
            if (Test-Until -TimeoutSec 12 -IntervalSec 1 -Condition { Test-AgentPaneOpen -App $App }) { return $App }
            # If a stray toggle closed it, the next loop's invoke re-opens it.
        } while ((Get-Date) -lt $deadline)
        throw "Open-AgentPane: agent pane did not open within ${TimeoutSec}s ($attempt attempts)."
    }
}

function Stop-AgentPane {
    <# Toggle the agent pane closed (stash). #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 15)
    process {
        if (-not (Test-AgentPaneOpen -App $App)) { return $App }
        Invoke-UiElement -App $App -Selector 'AgentToggleButton' -TimeoutSec 8 | Out-Null
        Wait-Until -TimeoutSec $TimeoutSec -Quiet -Because "agent pane to stash" -Condition { -not (Test-AgentPaneOpen -App $App) } | Out-Null
        $App
    }
}
Set-Alias -Name Restore-AgentPane -Value Open-AgentPane

function Set-AgentPaneFocus {
    <# Focus the agent pane via its own session id (restores it if stashed). #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        Open-AgentPane -App $App | Out-Null
        $sess = Get-AgentPaneSession -App $App
        if ($sess) { try { Invoke-WtCli -App $App -Arguments @('focus-pane', '-t', $sess.PaneSessionId) | Out-Null } catch { } }
        $App
    }
}

function Get-WtReswTextValues {
    <#
    .SYNOPSIS
        Distinct <value> strings for a C++ .resw resource key across EVERY bundled TerminalApp AND
        TerminalSettingsEditor locale. Returns @() when the resources/key can't be read. Cached per
        key. Underpins Get-WtReswTextRegex (all-locales match) and locale-robust name SEARCHES (a
        caller can try each localized value as a winapp search query, since search needs a literal).
    #>
    [CmdletBinding()] param([Parameter(Mandatory)][string]$Key)
    if (-not $script:WtReswValsCache) { $script:WtReswValsCache = @{} }
    if ($script:WtReswValsCache.ContainsKey($Key)) { return $script:WtReswValsCache[$Key] }
    $vals = @()
    try {
        $resDirs = @(
            (Join-Path $PSScriptRoot '..\..\..\..\src\cascadia\TerminalApp\Resources'),
            (Join-Path $PSScriptRoot '..\..\..\..\src\cascadia\TerminalSettingsEditor\Resources')
        ) | Where-Object { Test-Path $_ }
        $raw = foreach ($resDir in $resDirs) {
            foreach ($f in Get-ChildItem -Path $resDir -Filter 'Resources.resw' -Recurse -ErrorAction SilentlyContinue) {
                try {
                    $xml = [xml](Get-Content -LiteralPath $f.FullName -Raw)
                    $node = $xml.root.data | Where-Object { $_.name -eq $Key } | Select-Object -First 1
                    if ($node) { $v = "$($node.value)".Trim(); if ($v) { $v } }
                }
                catch {}
            }
        }
        $vals = @($raw | Where-Object { $_ } | Select-Object -Unique)
    }
    catch { $vals = @() }
    $script:WtReswValsCache[$Key] = $vals
    $vals
}

function Get-WtReswTextRegex {
    <#
    .SYNOPSIS
        Case-insensitive regex alternation of a C++ .resw resource key's <value> across EVERY
        bundled TerminalApp locale (src/cascadia/TerminalApp/Resources/*/Resources.resw), so
        assertions on localized WT/FRE UI text (e.g. FreOverlay_AgentStatusInstalled) work on
        non-en-US machines. Returns $null when the resources/key can't be read so callers can fall
        back to an en-US literal. Cached per key.
    #>
    [CmdletBinding()] param([Parameter(Mandatory)][string]$Key)
    if (-not $script:WtReswRegexCache) { $script:WtReswRegexCache = @{} }
    if ($script:WtReswRegexCache.ContainsKey($Key)) { return $script:WtReswRegexCache[$Key] }
    $result = $null
    $pats = @(Get-WtReswTextValues -Key $Key | ForEach-Object { [regex]::Escape($_) })
    if ($pats.Count) { $result = '(?i)(' + ($pats -join '|') + ')' }
    $script:WtReswRegexCache[$Key] = $result
    $result
}

function Get-WtaLocalizedTextRegex {
    <#
    .SYNOPSIS
        Case-insensitive regex matching the value of a wta localization key across EVERY bundled
        locale (tools/wta/locales/*.yml), so assertions on localized UI text work on non-en-US
        machines (the running binary renders one locale; matching any locale's value covers it).
        Each value is normalized by stripping a trailing " (…)" key-hint and trailing "." run
        (e.g. "Select model (↑ ↓ • Enter • Esc)" -> "Select model", "Ask anything, / for
        commands.." -> "Ask anything, / for commands"). Cached per key. Returns $null when the
        bundle/key can't be read so callers can fall back to an en-US literal.
    #>
    [CmdletBinding()] param([Parameter(Mandatory)][string]$Key)
    if (-not $script:WtaLocaleRegexCache) { $script:WtaLocaleRegexCache = @{} }
    if ($script:WtaLocaleRegexCache.ContainsKey($Key)) { return $script:WtaLocaleRegexCache[$Key] }
    $result = $null
    try {
        $localeDir = Join-Path $PSScriptRoot '..\..\..\..\tools\wta\locales'
        if (Test-Path $localeDir) {
            $escKey = [regex]::Escape($Key)
            $pats = Select-String -Path (Join-Path $localeDir '*.yml') -Pattern ('^\s*' + $escKey + ':\s*(\S.*)$') |
                ForEach-Object {
                    $raw = $_.Matches[0].Groups[1].Value.Trim()
                    # YAML scalar: double-quoted (ignoring a trailing # comment / Locked hint),
                    # single-quoted (with '' escaping), or a bare scalar (strip a trailing # comment).
                    if ($raw -match '^"((?:[^"\\]|\\.)*)"') {
                        # Unescape YAML double-quoted escapes (\" \\ \n \t \r) so the regex matches
                        # the RENDERED text, not literal backslashes (e.g. setup.subtitle.* use \").
                        $val = [regex]::Replace($Matches[1], '\\(.)', {
                            param($m)
                            switch ($m.Groups[1].Value) {
                                'n' { "`n" } 't' { "`t" } 'r' { "`r" }
                                default { $m.Groups[1].Value }  # \" -> " , \\ -> \ , \x -> x
                            }
                        })
                    }
                    elseif ($raw -match "^'((?:[^']|'')*)'") { $val = ($Matches[1] -replace "''", "'") }
                    else { $val = ($raw -replace '\s+#.*$', '').Trim() }
                    # Drop a trailing " (…)" key-hint and "." run. Require whitespace BEFORE the
                    # parenthesized hint (\s+\() so a value that is ENTIRELY parenthesized — e.g.
                    # agents.footer_hint "(↑ ↓ … Esc to exit …)" — is preserved intact instead of
                    # being stripped to an empty string.
                    $val -replace '\s+\([^)]*\)\s*$', '' -replace '\.+\s*$', ''
                } |
                Where-Object { $_ } | Select-Object -Unique | ForEach-Object { [regex]::Escape($_) }
            $pats = @($pats)
            if ($pats.Count) { $result = '(?i)(' + ($pats -join '|') + ')' }
        }
    }
    catch { $result = $null }
    $script:WtaLocaleRegexCache[$Key] = $result
    $result
}

function Get-RecommendationCardRegex {
    <#
    .SYNOPSIS
        Regex matching EITHER recommendation/autofix-card button label ("[ Run command ]" /
        "Insert in Terminal"), localized across ALL bundled wta locales (en-US fallback). Used to
        detect that a card is shown, so card-presence checks work on non-en-US machines.
    #>
    [CmdletBinding()] param()
    $parts = @(
        (Get-WtaLocalizedTextRegex -Key 'recommendations.button_run_command'),
        (Get-WtaLocalizedTextRegex -Key 'recommendations.button_insert_in_terminal')
    ) | Where-Object { $_ }
    if ($parts.Count) { ($parts -join '|') } else { 'Run command|Insert in Terminal' }
}

function Get-AgentConnectedPlaceholderRegex {
    <#
    .SYNOPSIS
        Case-insensitive regex matching the connected input placeholder of ANY bundled wta
        locale, so Wait-AgentReady works on non-en-US machines (the placeholder is localized
        via input.placeholder.connected — see tools/wta/locales/*.yml + ui/input.rs). Degrades
        to the en-US literal if the bundle can't be read (e.g. running outside a repo checkout).
    #>
    [CmdletBinding()] param()
    $re = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
    if ($re) { $re } else { '(?i)Ask anything.*for commands' }
}

function Get-AgentCliStatus {
    <#
    .SYNOPSIS
        Classify a built-in agent CLI as not-installed, probe-timeout,
        installed-unauthenticated, or authed using its non-interactive print mode.
    #>
    [CmdletBinding()] param(
        [Parameter(Mandatory)][ValidateSet('claude', 'codex', 'gemini')][string]$Agent,
        [int]$TimeoutSec = 50
    )
    if (-not (Get-Command $Agent -ErrorAction SilentlyContinue)) { return 'not-installed' }

    $job = Start-Job -ArgumentList $Agent -ScriptBlock {
        param($AgentId)
        switch ($AgentId) {
            'claude' { claude -p 'Reply with only the token AUTHOK' 2>&1 }
            'codex'  { $null | codex exec 'Reply with only the token AUTHOK' 2>&1 }
            'gemini' { gemini -p 'Reply with only the token AUTHOK' 2>&1 }
        }
    }
    $out = ''
    try {
        $finished = Wait-Job $job -Timeout $TimeoutSec
        if (-not $finished) {
            Stop-Job $job -ErrorAction SilentlyContinue
            return 'probe-timeout'
        }
        $out = ((Receive-Job $job 2>&1) -join "`n")
    }
    finally {
        Remove-Job $job -Force -ErrorAction SilentlyContinue
    }
    if ($out -match 'AUTHOK') { 'authed' } else { 'installed-unauthenticated' }
}

function Wait-AgentReady {
    <#
    .SYNOPSIS
        Wait until the agent pane is USER-VISIBLY connected and ready for input: its chat input
        shows the connected placeholder ("Ask anything, / for commands.."), which the TUI
        renders ONLY in ConnectionState::Connected (tools/wta/src/ui/input.rs:62). Returns
        $true when ready, $false on a logged auth/fatal failure or on timeout.
    .DESCRIPTION
        Readiness is judged by the rendered, user-visible ready state — NOT by an internal
        session-registry artifact (agent-pane-sessions.jsonl). Gating on the registry would be
        "verifying a feature with that same feature": if the registry breaks, the gate would
        false-ready or hang and mask the bug. The connecting ("connecting...") and disconnected
        ("disconnected") placeholders are distinct strings, so matching the connected one is a
        clean Connected-only signal. It returns the instant the connected input is observed —
        deterministic, not a fixed delay — and covers both the initial connect and a reconnect
        after /restart or a settings-driven rebuild. A logged auth/fatal failure short-circuits
        so a genuinely-failed connect fails fast instead of burning the whole timeout.
    #>
    [CmdletBinding()] param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [int]$TimeoutSec = 90,
        [string]$PaneSessionId
    )
    process {
        Open-AgentPane -App $App | Out-Null
        $readyRe = Get-AgentConnectedPlaceholderRegex
        $deadline = (Get-Date).AddSeconds($TimeoutSec)
        $nextLogCheck = Get-Date
        do {
            # The connected input placeholder is the user-visible "ready to chat" signal. It is
            # localized (input.placeholder.connected), so $readyRe matches the connected
            # placeholder of ANY bundled wta locale — never just the en-US string — to stay
            # robust on non-en-US machines. The connecting/disconnected placeholders are distinct
            # per locale, so this remains a clean Connected-only signal.
            if ((Get-AgentPaneText -App $App -MaxLines 50 -PaneSessionId $PaneSessionId) -match $readyRe) { return $true }
            # Throttle the fail-fast log read to every ~2s (UI placeholder is still polled at
            # 500ms): Get-ItLogText re-reads the whole appended slice each call and the helper log
            # grows while connecting, so reading it every loop would be O(n²) IO on long waits.
            if ((Get-Date) -ge $nextLogCheck) {
                # Read the NEWEST helper log file in FULL (not -SinceStart). The helper is pre-warmed
                # during tab init, so an auth/fatal failure can be logged BEFORE Start-Terminal
                # captures the -SinceStart offset — -SinceStart would then miss it and we'd burn the
                # whole timeout. The newest wta-main_helper-*.log is this launch's helper (fresh PID
                # → fresh file, since Stop-StaleItInstances killed any prior terminal), so reading it
                # from the top catches the early failure without false-matching a previous run's log.
                $log = ''
                $dir = Get-ItLogDir -App $App
                if ($dir) {
                    $helperLog = Get-ChildItem $dir -Filter 'wta-main_helper-*.log' -ErrorAction SilentlyContinue |
                        Sort-Object LastWriteTime -Descending | Select-Object -First 1
                    if ($helperLog) {
                        # Shared read: the helper has the file open for writing, so use a
                        # FileShare.ReadWrite stream (plain Get-Content -Raw can hit a sharing
                        # violation), matching how Get-ItLogText reads live logs. Disposing the
                        # StreamReader also disposes the underlying FileStream.
                        try {
                            $fs = [System.IO.FileStream]::new($helperLog.FullName, 'Open', 'Read', 'ReadWrite')
                            try {
                                $sr = [System.IO.StreamReader]::new($fs)
                                try { $log = $sr.ReadToEnd() } finally { $sr.Dispose() }
                            }
                            finally { $fs.Dispose() }  # also disposes $fs if the StreamReader ctor threw
                        }
                        catch { $log = '' }
                    }
                }
                # Fail-fast on a logged fatal connect failure. Match the STRUCTURED tracing fields,
                # not bare substrings: the helper logs the typed failure as `class=auth_required`
                # (app.rs; tracing may quote the &str value → `class="auth_required"`, so the quote
                # is optional) and `non_compliant_auth=true` (failure.rs string-fallback). Requiring
                # the `class=`/`…=true` prefix avoids a false-trigger from those tokens appearing in
                # an unrelated field/value. NOT the message "agent failure" (also fires for a benign
                # cancel). The helper process exit is `exiting with error`.
                if ($log -match 'exiting with error|class="?auth_required|non_compliant_auth=true') {
                    Write-ItLog -Level WARN -Message "Wait-AgentReady: helper logged an auth/fatal connect failure; not ready."
                    return $false
                }
                $nextLogCheck = (Get-Date).AddSeconds(2)
            }
            Start-Sleep -Milliseconds 500
        } while ((Get-Date) -lt $deadline)
        Write-ItLog -Level WARN -Message "Wait-AgentReady: agent pane never showed the connected input within ${TimeoutSec}s."
        return $false
    }
}

function Get-AgentPaneSessions {
    <#
    .SYNOPSIS
        Resolve every live agent pane created by THIS run from agent-pane-sessions.jsonl.
    .DESCRIPTION
        Returns newest-first, de-duplicated by PaneSessionId. This is the multi-tab-safe
        primitive; callers can pin a pane before another tab creates or rebuilds its helper.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        $jsonl = Join-Path $App.LocalStateDir 'IntelligentTerminal\agent-pane-sessions.jsonl'
        if (-not (Test-Path $jsonl)) { return }
        $preIds = if ($App.PSObject.Properties.Name -contains 'PreExistingAgentPaneIds') { $App.PreExistingAgentPaneIds } else { $null }
        $records = @(Get-Content -LiteralPath $jsonl -Tail 256 | Where-Object { $_.Trim() } |
                ForEach-Object { $_ | ConvertFrom-JsonSafe } | Where-Object { $_ -and $_.pane_session_id })
        if ($preIds) {
            $records = @($records | Where-Object { -not $preIds.Contains([string]$_.pane_session_id) })
        }

        $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
        for ($i = $records.Count - 1; $i -ge 0; $i--) {
            $r = $records[$i]
            $paneId = [string]$r.pane_session_id
            if (-not $seen.Add($paneId)) { continue }
            $alive = $false
            try { $st = Get-WtPaneStatus -App $App -SessionId $paneId; $alive = ($st -and $st.state -match 'run') } catch { $alive = $false }
            if ($alive) {
                [pscustomobject]@{
                    PaneSessionId   = $paneId
                    AcpSessionId    = $r.session_id
                    StartedAt       = $r.started_at
                    HelperProcessId = $st.pid
                }
            }
        }
    }
}

function Resolve-AgentOwnerTabId {
    param(
        [Parameter(Mandatory)]$App,
        [Parameter(Mandatory)][string]$OwnerPaneSessionId
    )

    $listener = Start-WtEventListener -App $App
    try {
        Start-Sleep -Milliseconds 500
        Invoke-RunCommand -App $App -SessionId $OwnerPaneSessionId -Command 'echo ite2e-tab-id-probe' | Out-Null
        $event = Wait-WtEvent -Listener $listener -TimeoutSec 15 -Predicate {
            $_.method -eq 'vt_sequence' -and
            "$($_.params.pane_id)" -eq $OwnerPaneSessionId -and
            $_.params.tab_id
        }
        [string]$event.params.tab_id
    }
    finally {
        Stop-WtEventListener -Listener $listener
    }
}

function Get-AgentPaneSession {
    <#
    .SYNOPSIS
        Resolve THIS run's newest live agent pane, a specific pinned pane, or a tab's pane:
          - PaneSessionId : the WT pane session GUID (use with send-keys / focus-pane /
                            pane-status / capture-pane). The agent pane is XAML chrome around
                            the helper's TermControl and is NEVER in list-panes (open OR
                            stashed), so the jsonl is the only way to find it.
          - AcpSessionId  : the ACP conversation id (use to resume the session).
          - TabId         : the owner tab StableId GUID (not the numeric protocol tab index).
          - OwnerPaneSessionId : a shell pane in the owner tab; its VT event supplies the StableId.
    .DESCRIPTION
        WT is single-instance: one monarch owns the COM server, and agent-pane-sessions.jsonl is
        a SHARED, append-only file accumulating records across every window AND every prior test
        run. Picking the globally "newest alive" record therefore frequently resolved to the
        WRONG pane — another window/tab or a leftover prior-run instance — which made session-view
        assertions (Open-SessionList / Get-AgentPaneText) flake nondeterministically.

        Fix: only consider records whose pane_session_id is NEW since this app launched
        ($App.PreExistingAgentPaneIds, snapshotted before activation). Those are exactly the
        agent pane(s) this run created; among them return the newest still-alive one.
    #>
    [CmdletBinding()] param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [string]$PaneSessionId,
        [string]$TabId,
        [string]$OwnerPaneSessionId
    )
    process {
        $sessions = @(Get-AgentPaneSessions -App $App)
        if ($PaneSessionId) {
            return $sessions | Where-Object { $_.PaneSessionId -eq $PaneSessionId } | Select-Object -First 1
        }
        if ($OwnerPaneSessionId) {
            $TabId = Resolve-AgentOwnerTabId -App $App -OwnerPaneSessionId $OwnerPaneSessionId
        }
        if ($TabId) {
            $normalizedTabId = $TabId.Trim('{}')
            $tabPattern = '--owner-tab-id\s+"?\{?' + [regex]::Escape($normalizedTabId) + '\}?(?:"|\s|$)'
            $helperPids = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" -ErrorAction SilentlyContinue |
                    Where-Object { $_.CommandLine -match '--connect-master' -and $_.CommandLine -match $tabPattern } |
                    ForEach-Object { [int]$_.ProcessId })
            return $sessions |
                Where-Object { [int]$_.HelperProcessId -in $helperPids } |
                Select-Object -First 1
        }
        $sessions | Select-Object -First 1
    }
}

function Wait-NewAgentPaneSession {
    <# Wait for a tab's live pane, or the newest live pane, excluding any prior pane ids. #>
    [CmdletBinding()] param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [string[]]$ExcludePaneSessionId = @(),
        [string]$TabId,
        [string]$OwnerPaneSessionId,
        [int]$TimeoutSec = 30
    )
    process {
        if ($OwnerPaneSessionId) {
            $TabId = Resolve-AgentOwnerTabId -App $App -OwnerPaneSessionId $OwnerPaneSessionId
        }
        Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 0.5 -Because 'a new agent pane session id' -Condition {
            if ($TabId) {
                Get-AgentPaneSession -App $App -TabId $TabId |
                    Where-Object { $_.PaneSessionId -notin $ExcludePaneSessionId }
            }
            else {
                Get-AgentPaneSessions -App $App |
                    Where-Object { $_.PaneSessionId -notin $ExcludePaneSessionId } |
                    Select-Object -First 1
            }
        }
    }
}

function Send-AgentPrompt {
    <#
    .SYNOPSIS
        Type a prompt into the agent pane and submit it (Enter). Routes through the agent
        pane's OWN session id (from agent-pane-sessions.jsonl) via wtcli send-keys — the
        reliable path, since the agent pane is a TermControl with no UIA input element and
        is absent from `list-panes`.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$Text,
        [switch]$NoSubmit,
        [string]$PaneSessionId
    )
    process {
        Open-AgentPane -App $App | Out-Null
        $sess = Wait-Until -TimeoutSec 20 -IntervalSec 0.5 -Because "agent pane session id" -Condition {
            Get-AgentPaneSession -App $App -PaneSessionId $PaneSessionId
        }
        Write-ItLog -Level INFO -Message "Send-AgentPrompt -> agent pane $($sess.PaneSessionId)"
        Invoke-WtCli -App $App -Arguments @('send-keys', '--raw', '-t', $sess.PaneSessionId, '--', $Text) | Out-Null
        if (-not $NoSubmit) {
            Start-Sleep -Milliseconds 300
            Invoke-WtCli -App $App -Arguments @('send-keys', '-t', $sess.PaneSessionId, '--', 'Enter') | Out-Null
        }
        $sess
    }
}

function Get-AgentPaneText {
    <# Capture the agent pane's rendered buffer text (its conpty/TUI), via its session id. #>
    [CmdletBinding()] param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [int]$MaxLines = 100,
        [string]$PaneSessionId
    )
    process {
        $sess = Get-AgentPaneSession -App $App -PaneSessionId $PaneSessionId
        if (-not $sess) { return '' }
        try { Get-WtCapture -App $App -SessionId $sess.PaneSessionId -MaxLines $MaxLines } catch { '' }
    }
}

function Wait-AgentState {
    <#
    .SYNOPSIS
        Wait for an agent activity event. -State maps to the agent_event `event` field:
        Working ~ agent.prompt.submit, Idle ~ agent.stop, Start ~ agent.session.start,
        End ~ agent.session.end. Pass a regex to match the raw event name directly.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$State, [int]$TimeoutSec = 60)
    process {
        $map = @{ Working = 'agent\.prompt\.submit'; Idle = 'agent\.stop'; Start = 'agent\.session\.start'; End = 'agent\.session\.end' }
        $pattern = if ($map.ContainsKey($State)) { $map[$State] } else { $State }
        $listener = Start-WtEventListener -App $App -EventFilter 'agent*'
        try {
            Wait-WtEvent -Listener $listener -TimeoutSec $TimeoutSec -Predicate {
                $_.method -eq 'agent_event' -and "$($_.params.event)" -match $pattern
            }
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
