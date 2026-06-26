#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: agent restart after a settings change (/restart reconnects and answers).
#   Invoke-Pester test/e2e/tests -Tag Feature
#
# NOTE: the former "Shift+Enter on a live session row" case was removed. Its premise was wrong —
# it asserted the session view dismisses back to chat, but Shift+Enter on a Live row dispatches
# FocusPane (wtcli focus-pane → move WT focus to that session's pane); it does NOT close the
# view. That contract is covered deterministically by the Rust unit test
# `shift_enter_on_class_a_live_row_focuses` (tools/wta/src/app.rs). The focus-pane semantics
# aren't stably observable from E2E here (the MVP picker shows only Class B shell sessions, whose
# panes are typically already closed → Focus returns NotFound), so E2E adds no reliable signal.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature: agent restart' -Tag 'Feature' -Skip:(-not $script:Ready) {
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
        # The settings change rebuilds the agent stack; wait for the helper's session to be
        # usable again BEFORE driving the menu, so opening it doesn't race the reconnect
        # (deterministic readiness gate, not a fixed sleep).
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue -Because 'the agent stack must reconnect after the settings change before driving the menu'
        Invoke-AgentMenuItem -App $script:app -Name '/restart'
        # After /restart the agent stack rebuilds; Wait-AgentReady is the deterministic
        # reconnect-and-ready signal (the connected input placeholder), so gate on it before
        # sending the next prompt — no fixed sleep, and locale-robust.
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue -Because 'the agent must reconnect and be ready again after /restart before sending a prompt'
        Send-AgentPrompt -App $script:app -Text 'What is 6 plus 6? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern '\b12\b' -TimeoutSec 50
    }
}
