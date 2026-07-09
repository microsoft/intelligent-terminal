#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §10 (C190) — the wt-agent-hooks bridge writes a diagnostic trace (hook-trace.log
# in the versioned log dir) every time a hooked agent CLI fires a lifecycle event. Submitting a
# prompt in the agent pane makes the copilot hook fire `agent.prompt.submit`, which must land in
# hook-trace.log. This is what lets a bug report explain session-tracking / autofix hook flow.
#
# Precondition: copilot must have the wt-agent-hooks plugin installed+enabled (hooks live in the
# user profile, not settings.json, so they survive the harness settings wipe). If not installed on
# this machine the case SKIPS — the trace is only written when a hook actually fires.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

Describe 'Feature §10 hook trace log' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        # Gate on copilot hooks being installed+enabled (the trace only writes when a hook fires).
        $script:copilotHooked = $false
        try {
            $st = (Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 40 -Raw).StdOut | ConvertFrom-Json
            $cop = $st.clis | Where-Object { $_.name -eq 'copilot' }
            $script:copilotHooked = [bool]($cop.plugin_installed -and $cop.plugin_enabled)
        }
        catch { $script:copilotHooked = $false }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Hook trace log is written (an agent-pane prompt makes the copilot hook trace agent.prompt.submit)' {
        if (-not $script:copilotHooked) { Set-ItResult -Skipped -Because 'copilot wt-agent-hooks plugin is not installed/enabled on this machine — no hook fires, so no trace is written'; return }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        Initialize-LogOffsets -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
        # The hook bridge appends an ENTER/DISPATCHED line for the copilot event to hook-trace.log.
        (Test-Until -TimeoutSec 30 -IntervalSec 1 -Condition {
                (Get-ItLogText -App $script:app -Name 'hook-trace.log' -SinceStart) -match '(?i)cli=copilot.*event=agent\.|event=agent\.prompt\.submit|DISPATCHED'
            }) | Should -BeTrue -Because 'a hooked copilot prompt must be traced in hook-trace.log'
    }
}
