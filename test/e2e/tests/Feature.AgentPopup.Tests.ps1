#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# FEATURE TEST: agent pane popup / menu interaction.
# Proves we can (a) trigger a popup, (b) ASSERT it appeared, (c) SELECT an item, (d)
# TRIGGER the selected option — all on the TUI agent pane. Deterministic (no LLM call).
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: agent pane popup + menu' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'opens the / command popup and we can ASSERT it is shown' {
        Open-AgentCommandMenu -App $script:app | Out-Null
        # Assert the popup appeared (TUI text, not a UIA dialog).
        Test-AgentPopupShown -App $script:app -Pattern '/help' | Should -BeTrue
        Assert-AgentPaneText -App $script:app -Pattern '/clear'
        Assert-AgentPaneText -App $script:app -Pattern '/new'
    }

    It 'SELECTS a menu item with arrow keys (selection marker moves)' {
        Open-AgentCommandMenu -App $script:app | Out-Null
        $first = Get-AgentMenuSelection -App $script:app          # starts on /help
        $first | Should -Match '/help'
        Send-AgentKey -App $script:app -Key Down -Count 1 | Out-Null
        (Get-AgentMenuSelection -App $script:app) | Should -Not -Match '/help'  # moved
        # Dismiss the popup so the next test starts clean.
        Send-AgentKey -App $script:app -Key Escape | Out-Null
    }

    It 'TRIGGERS the selected option (/clear) and the popup closes' {
        Invoke-AgentMenuItem -App $script:app -Name '/clear'
        # After triggering /clear, the popup is gone (input returns to the prompt hint).
        $closed = Test-Until -TimeoutSec 10 -Condition {
            -not ((Get-AgentPaneText -App $script:app -MaxLines 40) -match '/help\s+Show this command list')
        }
        $closed | Should -BeTrue -Because 'the / popup should close after the command runs'
    }
}
