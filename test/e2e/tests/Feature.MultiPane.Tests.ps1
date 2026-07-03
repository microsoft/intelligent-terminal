#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §7 Multi-pane and multi-window behavior — the items that are automatable
# in a single window via the protocol (split / new-tab / focus-pane) plus agent-pane chat.
# Deterministic where possible; one consolidated chat round proves the per-tab session isolation.
#
# Not covered here (genuinely not harness-injectable): "Move tab to new window" and the
# multi-window cross-route items — tearing a tab to a new window is a drag gesture WT exposes only
# through its own window chrome, which winapp/send-keys can't drive.

BeforeDiscovery {
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature §7 multi-pane and multi-tab' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the agent pane must connect before the multi-pane suite runs'
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Split pane does not break chat (agent pane still answers after a split)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $split = Split-WtPane -App $script:app -SessionId $sid -Direction right
        $split.session_id | Should -Match '[0-9A-Fa-f-]{36}'
        Start-Sleep -Seconds 2
        # The agent pane (unchanged by the terminal split) must still take a prompt and answer.
        Send-AgentPrompt -App $script:app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
        $answered = Test-Until -TimeoutSec 40 -IntervalSec 2 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 80) -match '\b7\b'
        }
        $answered | Should -BeTrue -Because 'splitting a terminal pane must not break the agent pane chat'
        Close-WtPane -App $script:app -SessionId $split.session_id
    }

    It 'Multiple tabs each get an independent agent session (conversations do not mix)' {
        # Seed a unique marker into THIS tab's agent conversation.
        Send-AgentPrompt -App $script:app -Text 'Reply with exactly the single word ZEBRA7 and nothing else.' | Out-Null
        $seeded = Test-Until -TimeoutSec 40 -IntervalSec 2 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 80) -match 'ZEBRA7'
        }
        $seeded | Should -BeTrue -Because 'the first tab agent pane should answer with the marker'

        # A new tab gets its OWN pre-warmed helper/session — opening its agent pane shows a fresh
        # conversation that does NOT contain the first tab's marker. This is the §7 "does not mix
        # conversations" guarantee. (Same-tab chat preservation across stash/restore is covered by
        # the 'Stash preserves chat' case in the open/hide suite.)
        $tab2 = New-WtTab -App $script:app -Title 'multi-pane-tab2'
        Set-WtPaneFocus -App $script:app -SessionId $tab2.session_id
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the second tab pre-warmed helper must connect'
        $tab2Text = Get-AgentPaneText -App $script:app -MaxLines 80
        $tab2Text | Should -Not -Match 'ZEBRA7' -Because 'the second tab agent session is independent and must not show the first tab conversation'
    }

    It 'Close target tab cleans up (closing an agent-pane tab leaves other tabs working)' {
        $tab = New-WtTab -App $script:app -Title 'multi-pane-closeme'
        Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
        Open-AgentPane -App $script:app | Out-Null
        # Closing the tab's shell pane tears the tab (and its agent pane/helper) down cleanly.
        { Close-WtPane -App $script:app -SessionId $tab.session_id } | Should -Not -Throw
        # The protocol surface is still alive and another pane is reachable.
        { Get-ActivePane -App $script:app } | Should -Not -Throw
        (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue) | Should -Not -BeNullOrEmpty
    }
}
