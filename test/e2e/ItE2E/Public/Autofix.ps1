# Autofix.ps1 — autofix primitives.
#
# Observable signals on the `wtcli listen` stream (envelope is always
# {method,params,type:"event"} — the event NAME is `.method`, not `.type`):
#   * the failure trigger:  method=vt_sequence, params.sequence ~ "osc:133;D;<nonzero>"
#   * the autofix request:  method=agent_event whose params.payload.initial_prompt ~
#                           "A command failed. Diagnose..." — this rides on
#                           params.event="agent.session.start" (NOT "agent.prompt.submit",
#                           which may not carry the prompt). Wait-Autofix matches on the
#                           initial_prompt text and does not require a specific params.event.
# (This build emits NO dedicated `autofix_state` event; detect via the above.)

function Invoke-FailingCommand {
    <#
    .SYNOPSIS
        Run a command guaranteed to fail in a shell-integrated pane (to trigger autofix).
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
        Wait for an OSC 133;D shell-integration mark with a non-zero exit code (the autofix
        trigger). Requires a listener started before the failing command.
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

function Wait-Autofix {
    <#
    .SYNOPSIS
        Wait for the autofix request to be submitted to the agent (the real observable
        signal). Requires a listener started before the failing command.
    .NOTES
        The autofix prompt's `initial_prompt` ("A command failed. Diagnose...") rides on the
        `agent.session.start` agent_event, NOT `agent.prompt.submit` (which carries no
        prompt). So we key on the prompt content across any agent_event.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$Listener, [string]$TabId, [int]$TimeoutSec = 45)
    process {
        Wait-WtEvent -Listener $Listener -TimeoutSec $TimeoutSec -Predicate {
            $_.method -eq 'agent_event' -and
            ("$($_.params.payload.initial_prompt)" -match 'command failed|Diagnose the error') -and
            (-not $TabId -or "$($_.params.tab_id)" -eq "$TabId")
        }
    }
}

function Send-AutofixState {
    <#
    .SYNOPSIS
        Inject an autofix_state event for deterministic UI testing. NOTE: this build does
        not consume a dedicated autofix_state event, so this is a best-effort hook.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$ParamsJson = '{"state":"suggested"}', [string]$SourcePane)
    process { Send-WtEvent -App $App -EventType 'autofix_state' -ParamsJson $ParamsJson -SourcePane $SourcePane | Out-Null; $App }
}
