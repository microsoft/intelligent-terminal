#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #425 / issue #419: all Copilot tabs share one agent CLI through wta-master.
# Closing one tab while its prompt is in flight must close only that tab's ACP
# session, without tearing down the shared connection or breaking a sibling tab.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = [bool](
            (Get-Command copilot -ErrorAction SilentlyContinue) -and
            (Test-WinAppAvailable)
        )
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature: shared agent CLI lifecycle' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent = 'copilot'
            # This suite tests shared-process lifecycle, not permission enforcement.
            # Current Copilot CLI builds cannot submit ACP prompts with Yolo disabled.
            'agentPane.yoloMode' = $true
        }

        $script:GetMaster = {
            @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" -ErrorAction SilentlyContinue |
                Where-Object {
                    $_.ParentProcessId -eq $script:app.Pid -and
                    $_.CommandLine -match '--master(\s|$|")' -and
                    $_.CommandLine -notmatch '--connect-master'
                }) | Select-Object -First 1
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
    }

    It 'Closing a tab mid-turn leaves sibling agent tabs working' {
        # Establish the survivor first and pin its exact helper pane.
        $survivorShell = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $survivorAgent = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $survivorShell.session_id -TimeoutSec 30).PaneSessionId

        # A second tab shares the same master and underlying Copilot CLI.
        $victimShell = New-WtTab -App $script:app -Title 'mid-turn-close-victim'
        Set-WtPaneFocus -App $script:app -SessionId $victimShell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $victimAgent = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $victimShell.session_id -TimeoutSec 30).PaneSessionId

        $masterReady = Test-Until -TimeoutSec 20 -IntervalSec 0.5 -Condition $script:GetMaster
        $masterReady | Should -BeTrue -Because 'the two helpers must share a running master'
        $masterBefore = & $script:GetMaster
        $masterBefore | Should -Not -BeNullOrEmpty
        Initialize-LogOffsets -App $script:app | Out-Null

        # Request enough output that the helper can be closed after forwarding but
        # before the turn completes. The master log is the deterministic in-flight
        # signal; no model text is used to decide when to close.
        Send-AgentPrompt -App $script:app -PaneSessionId $victimAgent -Text `
            'Write the integers from 1 through 500, one per line, without abbreviating or using a tool.' | Out-Null
        $forwarded = Test-Until -TimeoutSec 20 -IntervalSec 0.1 -Condition {
            (Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart) -match
                'forwarding prompt to agent CLI \(non-blocking\)'
        }
        $forwarded | Should -BeTrue -Because 'the victim prompt must be in flight before its tab closes'

        Close-WtPane -App $script:app -SessionId $victimShell.session_id

        Assert-Log -App $script:app -Name 'wta-main_master.log' `
            -Pattern 'closed ACP session resolved from destroyed tab.*cleanup=PhysicallyClosed' -TimeoutSec 20

        $masterAfter = & $script:GetMaster
        $masterAfter.ProcessId | Should -Be $masterBefore.ProcessId -Because 'closing one tab must not restart the shared master'
        (Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart) |
            Should -Not -Match 'agent CLI exited' -Because 'closing one session must not tear down the shared agent CLI'

        # Final user-visible contract: the original sibling helper is still connected
        # and can complete a fresh turn without /restart or rebuilding its pane.
        Set-WtPaneFocus -App $script:app -SessionId $survivorShell.session_id
        Send-AgentPrompt -App $script:app -PaneSessionId $survivorAgent -Text `
            'What is 6 plus 7? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $survivorAgent -Pattern '\b13\b' -TimeoutSec 60
    }
}
