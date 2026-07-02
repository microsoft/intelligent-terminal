#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §11 accessibility (C198) — the agent pane must be fully operable with the
# KEYBOARD alone (no mouse). Every step here uses keyboard input only (wtcli send-keys / raw ANSI
# to the pane's conpty — the same path a physical keyboard drives), never a winapp mouse click:
# type into the input, open the `/` command menu, move the selection with arrow keys, and dismiss
# with Esc. Each step self-asserts, so a regression that breaks keyboard operability fails here.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

Describe 'Feature §11 keyboard-only agent pane' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Keyboard-only agent pane works (type, open / menu, arrow-navigate, Esc — no mouse)' {
        # 1) Typing (keyboard): a typed draft must appear in the input line.
        Clear-AgentInput -App $script:app | Out-Null
        $marker = "kbd$(Get-Random -Maximum 99999)"
        $typed = $false
        for ($t = 0; $t -lt 3 -and -not $typed; $t++) {
            Clear-AgentInput -App $script:app | Out-Null
            Send-AgentPrompt -App $script:app -Text $marker -NoSubmit | Out-Null
            $typed = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 40) -match $marker }
        }
        $typed | Should -BeTrue -Because 'typing into the agent pane must work with the keyboard alone'
        Clear-AgentInput -App $script:app | Out-Null

        # 2) Open the `/` command menu with the keyboard.
        Open-AgentCommandMenu -App $script:app | Out-Null
        (Get-AgentPaneText -App $script:app -MaxLines 40) | Should -Match '/help|/clear|/new|/sessions' -Because 'the / command menu must open from the keyboard'

        # 3) Arrow-navigate: Down must move the selection to a different row.
        $sel1 = Get-AgentMenuSelection -App $script:app
        Send-AgentKey -App $script:app -Key Down | Out-Null
        $moved = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition { (Get-AgentMenuSelection -App $script:app) -ne $sel1 }
        $moved | Should -BeTrue -Because 'arrow keys must move the menu selection (keyboard navigation)'

        # 4) Esc dismisses the menu (keyboard).
        Send-AgentKey -App $script:app -Key Escape | Out-Null
        (Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                (Get-AgentPaneText -App $script:app -MaxLines 40) -notmatch '/help.*\n.*/clear'
            }) | Should -BeTrue -Because 'Esc must dismiss the command menu (keyboard back-navigation)'
        Clear-AgentInput -App $script:app | Out-Null
    }
}
