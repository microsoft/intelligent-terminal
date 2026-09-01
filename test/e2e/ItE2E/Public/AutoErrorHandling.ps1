# AutoErrorHandling.ps1 — Auto-error-handling primitives.
#
# Observable signals on the `wtcli listen` stream (envelope is always
# {method,params,type:"event"} — the event NAME is `.method`, not `.type`):
#   * the failure trigger:  method=vt_sequence, params.sequence ~ "osc:133;D;<nonzero>"
#   * the agent request:    method=agent_event whose params.payload.initial_prompt ~
#                           "A command failed. Diagnose..." — this rides on
#                           params.event="agent.session.start" (NOT "agent.prompt.submit",
#                           which may not carry the prompt). Wait-AutoErrorHandling matches on the
#                           initial_prompt text and does not require a specific params.event.
# WTA sends UI state through method=auto_error_handling_state. That route is consumed directly
# by TerminalPage rather than broadcast to `wtcli listen`, so tests observe it through the UI or
# inject it with Send-AutoErrorHandlingState.

function Invoke-FailingCommand {
    <#
    .SYNOPSIS
        Run a command guaranteed to fail in a shell-integrated pane.
        Returns the captured output.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [string]$SessionId,
        [string]$Command = 'ggit status'   # typo'd command -> nonzero exit
    )
    process {
        if (-not $SessionId) { $SessionId = (Get-ActivePane -App $App).session_id }
        Invoke-RunCommand -App $App -SessionId $SessionId -Command $Command
    }
}

function Wait-WtCommandFailure {
    <#
    .SYNOPSIS
        Wait for an OSC 133;D shell-integration mark with a non-zero exit code. Requires a
        listener started before the failing command.
    .PARAMETER PaneId
        Scope to a specific pane. The vt_sequence event's `pane_id` equals the pane's session_id
        (= Get-ActivePane.session_id), so pass that to ignore unrelated OSC 133 marks from other
        panes/startup. Prefer this over -TabId: the event's `tab_id` is a GUID, whereas
        Get-ActivePane/Get-WtTabs expose tab_id as a numeric INDEX, so -TabId can't be satisfied
        from those without a separate GUID lookup.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$Listener, [string]$TabId, [string]$PaneId, [int]$TimeoutSec = 20)
    process {
        Wait-WtEvent -Listener $Listener -TimeoutSec $TimeoutSec -Predicate {
            $_.method -eq 'vt_sequence' -and
            $_.params.sequence -match '(?i)osc:133;D;(?!0(\b|;|$))(-?\d+)' -and
            (-not $TabId -or "$($_.params.tab_id)" -eq "$TabId") -and
            (-not $PaneId -or "$($_.params.pane_id)" -eq "$PaneId")
        }
    }
}

function Wait-AutoErrorHandling {
    <#
    .SYNOPSIS
        Wait for an Auto-error-handling request to be submitted to the agent. Prefers the
        protocol event and falls back to the helper's structured log because current
        production builds do not consistently forward the agent_event to wtcli listeners.
    .NOTES
        The Auto-error-handling prompt's `initial_prompt` ("A command failed. Diagnose...") rides on the
        `agent.session.start` agent_event, NOT `agent.prompt.submit` (which carries no
        prompt). So we key on the prompt content across any agent_event.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$Listener, [string]$TabId, [int]$TimeoutSec = 45)
    process {
        Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 0.4 -Because 'an Auto-error-handling request event or helper log' -Condition {
            $event = @(Get-WtEvents -Listener $Listener -Predicate {
                    $_.method -eq 'agent_event' -and
                    ("$($_.params.payload.initial_prompt)" -match 'command failed|Diagnose the error') -and
                    (-not $TabId -or "$($_.params.tab_id)" -eq "$TabId")
                }) | Select-Object -First 1
            if ($event) { return $event }

            $log = Get-ItLogText -App $Listener.App -Name 'wta-main_helper-*.log' -SinceStart
            $pattern = if ($TabId) {
                'sending Auto-error-handling prompt.*tab_id=' + [regex]::Escape($TabId)
            }
            else {
                'sending Auto-error-handling prompt'
            }
            if ($log -match $pattern) {
                return [pscustomobject]@{ method = 'helper_log'; line = $Matches[0] }
            }
            $null
        }
    }
}

function Wait-AutoErrorHandlingDetection {
    <#
    .SYNOPSIS
        Wait until the helper enters detect-only mode for a command failure without contacting
        the agent. This is the deterministic oracle for the middle enum state and policy
        degradation from send-to-agent to detect-only.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$Listener, [string]$PaneId, [int]$TimeoutSec = 25)
    process {
        Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 0.4 -Because 'Auto-error-handling detect-only state' -Condition {
            $log = Get-ItLogText -App $Listener.App -Name 'wta-main_helper-*.log' -SinceStart
            $line = @($log -split '\r?\n' | Where-Object {
                    $_ -match 'detect-only mode.*Detected pill.*no agent call' -and
                    (-not $PaneId -or $_ -match ('pane_id="?{0}"?' -f [regex]::Escape($PaneId)))
                }) | Select-Object -First 1
            if ($line) {
                return [pscustomobject]@{ method = 'helper_log'; line = $line }
            }
            $null
        }
    }
}

function Send-AutoErrorHandlingState {
    <#
    .SYNOPSIS
        Inject an auto_error_handling_state event for deterministic UI testing.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$ParamsJson = '{"state":"detected"}', [string]$SourcePane)
    process { Send-WtEvent -App $App -EventType 'auto_error_handling_state' -ParamsJson $ParamsJson -SourcePane $SourcePane | Out-Null; $App }
}
