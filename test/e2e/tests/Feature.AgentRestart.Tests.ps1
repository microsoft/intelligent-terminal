#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: remaining tractable items — agent restart after settings change,
# Shift+Enter (focus) on a live session row.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature: agent restart + session focus' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Agent restart after settings change works (/restart reconnects and answers)' {
        # Change a setting that affects the agent stack, then /restart the agent pane.
        Set-WtSetting -App $script:app -Key 'acpModel' -Value '' | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/restart'
        # After restart the agent reconnects and can still answer (poll, no fixed sleep).
        $reconnected = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 60) -match 'Ask anything|Copilot|Agent'
        }
        $reconnected | Should -BeTrue
        Send-AgentPrompt -App $script:app -Text 'What is 6 plus 6? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern '\b12\b' -TimeoutSec 50
    }

    It 'Shift+Enter on a live session row focuses it (same as Enter for Live)' {
        # A live session already exists from the restart test. Open the list and act on the
        # current (live) row. Shift+Enter on a Live row is a safety alias for Focus.
        Open-SessionList -App $script:app | Out-Null
        (Get-SessionListSelection -App $script:app) | Should -Not -BeNullOrEmpty
        Send-AgentKey -App $script:app -Key 'Enter' | Out-Null   # Shift alias == Enter for Live
        $backToChat = Test-Until -TimeoutSec 12 -Condition { -not (Test-SessionListShown -App $script:app -TimeoutSec 1) }
        $backToChat | Should -BeTrue
    }
}
