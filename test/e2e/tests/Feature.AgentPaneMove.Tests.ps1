#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #429: /move is a transient per-tab override and must restore agent input
# focus after rebuilding the split layout.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.PackageFamilyName -like 'IntelligentTerminal_*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: per-tab agent pane move' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            acpAgent         = 'copilot'
            agentPanePosition = 'bottom'
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
    }

    It 'Agent pane move is isolated per tab and preserves input focus' {
        $resolveTabId = {
            param([string]$OwnerPaneSessionId)
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 500
                Invoke-RunCommand -App $script:app -SessionId $OwnerPaneSessionId -Command 'echo ite2e-move-tab-probe' | Out-Null
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

        $shellA = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $agentA = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shellA.session_id -TimeoutSec 30
        Wait-AgentReady -App $script:app -PaneSessionId $agentA.PaneSessionId -TimeoutSec 90 |
            Should -BeTrue
        $tabA = & $resolveTabId $shellA.session_id

        $shellB = New-WtTab -App $script:app -Title 'move-isolation-b'
        Open-AgentPane -App $script:app | Out-Null
        $agentB = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shellB.session_id -ExcludePaneSessionId $agentA.PaneSessionId -TimeoutSec 30
        Wait-AgentReady -App $script:app -PaneSessionId $agentB.PaneSessionId -TimeoutSec 90 |
            Should -BeTrue
        $tabB = & $resolveTabId $shellB.session_id
        $tabA | Should -Not -Be $tabB

        Set-WtWindowForeground -App $script:app | Should -BeTrue
        Set-AgentPaneFocus -App $script:app | Out-Null
        Start-Sleep -Seconds 1
        Clear-AgentInput -App $script:app -PaneSessionId $agentB.PaneSessionId | Out-Null
        Initialize-LogOffsets -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $agentB.PaneSessionId -Text '/move right' | Out-Null

        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' `
            -Pattern "OnAgentStateChanged: tab_id=$([regex]::Escape($tabB)).*pane_position=right" -TimeoutSec 15
        Send-WtWindowKey -App $script:app -Vk 0xBF -RequireForeground | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $agentB.PaneSessionId `
            -Pattern '(?i)/help' -TimeoutSec 10
        Clear-AgentInput -App $script:app -PaneSessionId $agentB.PaneSessionId | Out-Null

        Set-WtPaneFocus -App $script:app -SessionId $shellA.session_id
        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' `
            -Pattern "OnAgentStateChanged: tab_id=$([regex]::Escape($tabA)).*pane_position=global" -TimeoutSec 15
        (Get-WtSetting -App $script:app -Key 'agentPanePosition') |
            Should -Be 'bottom' -Because '/move must not mutate the global setting'

        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtPaneFocus -App $script:app -SessionId $shellB.session_id
        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' `
            -Pattern "OnAgentStateChanged: tab_id=$([regex]::Escape($tabB)).*pane_position=right" -TimeoutSec 15
    }
}
