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
        Open/toggle the agent pane via the bottom-bar AgentToggleButton (UIA), falling
        back to the command palette. Self-verifies the agent pane is shown.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 30)
    process {
        if (Test-AgentPaneOpen -App $App) { return $App }
        $opened = $false
        try { Invoke-UiElement -App $App -Selector 'AgentToggleButton' -TimeoutSec 8 | Out-Null; $opened = $true }
        catch { Write-ItLog -Level WARN -Message "AgentToggleButton invoke failed: $_" }
        if (-not $opened) {
            $active = Get-ActivePane -App $App
            Send-WtKeys -App $App -SessionId $active.session_id -Keys @('C-S-p')
            Start-Sleep -Milliseconds 400
            Send-WtInput -App $App -SessionId $active.session_id -Text '>Toggle AI assistant'
            Send-WtKeys -App $App -SessionId $active.session_id -Keys @('Enter')
        }
        Wait-Until -TimeoutSec $TimeoutSec -Because "agent pane to open" -Condition { Test-AgentPaneOpen -App $App } | Out-Null
        $App
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

function Wait-AgentReady {
    <#
    .SYNOPSIS
        Open the agent pane and wait until its helper's ACP session is connected (anti-flake
        gate before autofix/prompt assertions). Signals (best-effort, any-of): an
        agent_state_changed event reaching a ready state, or a helper-log connect marker.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 60)
    process {
        Open-AgentPane -App $App | Out-Null
        $listener = Start-WtEventListener -App $App -EventFilter 'agent*'
        try {
            $ready = Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 0.5 -Quiet -Because "agent ACP connected" -Condition {
                # Any agent_event means the helper/ACP pipeline is live; or a helper-log marker.
                if (Get-WtEvents -Listener $listener -Predicate { $_.method -eq 'agent_event' }) { return $true }
                $log = Get-ItLogText -App $App -Name 'wta-main_helper-*.log' -SinceStart
                if ($log -match 'session/new|acp_initialize|Connected') { return $true }
                $false
            }
            if (-not $ready) { Write-ItLog -Level WARN -Message "Wait-AgentReady: no explicit connect signal within ${TimeoutSec}s." }
            $ready
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}

function Get-AgentPaneSession {
    <#
    .SYNOPSIS
        Resolve the CURRENT agent pane's identity from agent-pane-sessions.jsonl:
          - PaneSessionId : the WT pane session GUID (use with send-keys / focus-pane /
                            pane-status). The agent pane is NOT in `list-panes` when
                            stashed, but it DOES respond to its pane session id.
          - AcpSessionId  : the ACP conversation id (use to resume the session).
        Returns the newest record whose pane session is still 'running', or $null.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        $jsonl = Join-Path $App.LocalStateDir 'IntelligentTerminal\agent-pane-sessions.jsonl'
        if (-not (Test-Path $jsonl)) { return $null }
        $records = Get-Content -LiteralPath $jsonl | Where-Object { $_.Trim() } |
            ForEach-Object { $_ | ConvertFrom-JsonSafe } | Where-Object { $_ -and $_.pane_session_id }
        # Newest first; pick the first whose pane is still alive.
        for ($i = $records.Count - 1; $i -ge 0; $i--) {
            $r = $records[$i]
            $alive = $false
            try { $st = Get-WtPaneStatus -App $App -SessionId $r.pane_session_id; $alive = ($st -and $st.state -match 'run') } catch { $alive = $false }
            if ($alive) {
                return [pscustomobject]@{
                    PaneSessionId = $r.pane_session_id
                    AcpSessionId  = $r.session_id
                    StartedAt     = $r.started_at
                }
            }
        }
        $null
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
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Text, [switch]$NoSubmit)
    process {
        Open-AgentPane -App $App | Out-Null
        $sess = Wait-Until -TimeoutSec 20 -IntervalSec 0.5 -Because "agent pane session id" -Condition { Get-AgentPaneSession -App $App }
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
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$MaxLines = 100)
    process {
        $sess = Get-AgentPaneSession -App $App
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
