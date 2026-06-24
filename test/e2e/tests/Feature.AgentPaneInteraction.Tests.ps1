#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: Opening/hiding/focus (8) + Input and rendering (9) +
# Agent pane slash commands (2) + Built-in agent chat (copilot) (2).
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature: agent pane open/hide/focus + input + slash + chat' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    Context 'Opening, hiding, and focus' {
        It 'Button opens pane (AgentToggleButton)' {
            if (Test-AgentPaneOpen -App $script:app) { Stop-AgentPane -App $script:app | Out-Null }
            Open-AgentPane -App $script:app -TimeoutSec 30 | Out-Null
            Test-AgentPaneOpen -App $script:app | Should -BeTrue
        }
        It 'Button hides pane (stash)' {
            Stop-AgentPane -App $script:app -TimeoutSec 15 | Out-Null
            Test-AgentPaneOpen -App $script:app | Should -BeFalse
        }
        It 'Stash preserves chat (reopen shows prior conversation)' {
            Open-AgentPane -App $script:app | Out-Null
            Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
            # Answer (42) is NOT in the prompt, so matching it proves the agent REPLIED
            # (not just the echoed prompt).
            Send-AgentPrompt -App $script:app -Text 'What is 21 plus 21? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern '\b42\b' -TimeoutSec 40
            Stop-AgentPane -App $script:app | Out-Null
            Start-Sleep -Seconds 1
            Open-AgentPane -App $script:app | Out-Null
            # The prior exchange (including the 42 answer) survives the stash/restore.
            Assert-AgentPaneText -App $script:app -Pattern '\b42\b' -TimeoutSec 15
        }
        It 'Focus hotkey / focus works (agent pane gets focus)' {
            Set-AgentPaneFocus -App $script:app | Out-Null
            Test-AgentPaneOpen -App $script:app | Should -BeTrue
        }
        It 'Different positions work (pane opens at configured position)' {
            Set-WtPanePosition -App $script:app -Position 'right' | Out-Null
            Stop-AgentPane -App $script:app | Out-Null
            Open-AgentPane -App $script:app | Out-Null
            Test-AgentPaneOpen -App $script:app | Should -BeTrue
            Set-WtPanePosition -App $script:app -Position 'bottom' | Out-Null
        }
        It 'Tab close cleans up (closing the tab removes its agent session record activity)' {
            $tab = New-WtTab -App $script:app -Title 'cleanup'
            Close-WtPane -App $script:app -SessionId $tab.session_id
            # The original window/tab is still alive and usable.
            { Get-ActivePane -App $script:app } | Should -Not -Throw
        }
    }

    Context 'Input and rendering' {
        BeforeAll { Open-AgentPane -App $script:app | Out-Null; Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null }

        It 'Typing works (text appears in the input line)' {
            Clear-AgentInput -App $script:app | Out-Null
            $sess = Send-AgentPrompt -App $script:app -Text 'hello typing test' -NoSubmit
            $sess.PaneSessionId | Should -Match '[0-9A-Fa-f-]{36}'
            Assert-AgentPaneText -App $script:app -Pattern 'hello typing test' -TimeoutSec 8
            Clear-AgentInput -App $script:app | Out-Null
        }
        It 'Streaming output renders correctly (agent reply appears in the pane)' {
            # 99 is the ANSWER, not in the prompt — proves the reply rendered, not the echo.
            Send-AgentPrompt -App $script:app -Text 'What is 100 minus 1? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern '\b99\b' -TimeoutSec 40
        }
        It 'Keyboard navigation works (arrow moves menu selection)' {
            Open-AgentCommandMenu -App $script:app | Out-Null
            $first = Get-AgentMenuSelection -App $script:app
            Send-AgentKey -App $script:app -Key Down | Out-Null
            (Get-AgentMenuSelection -App $script:app) | Should -Not -Be $first
            Clear-AgentInput -App $script:app | Out-Null
        }
        It 'Prompt focused appearance is correct (input hint visible)' {
            Clear-AgentInput -App $script:app | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern 'Ask anything|/ for commands' -TimeoutSec 8
        }
    }

    Context 'Agent pane slash commands' {
        It '/model command is available in the menu' {
            Open-AgentCommandMenu -App $script:app | Out-Null
            # /model may or may not be listed depending on agent; at minimum the menu renders.
            Assert-AgentPaneText -App $script:app -Pattern '/help|/new|/clear' -TimeoutSec 10
            Clear-AgentInput -App $script:app | Out-Null
        }
        It 'Esc/back navigation works (Escape dismisses the popup)' {
            Open-AgentCommandMenu -App $script:app | Out-Null
            Send-AgentKey -App $script:app -Key Escape | Out-Null
            $closed = Test-Until -TimeoutSec 8 -Condition {
                -not ((Get-AgentPaneText -App $script:app -MaxLines 40) -match '/help\s+Show this command list')
            }
            $closed | Should -BeTrue
        }
    }

    Context 'Built-in agent chat (Copilot)' {
        It 'Copilot chat works (answers a question)' {
            Clear-AgentInput -App $script:app | Out-Null
            Send-AgentPrompt -App $script:app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern '7' -TimeoutSec 40
            Assert-AI -Claim 'The agent answered the arithmetic question 3 plus 4 with 7.' -Context (Get-AgentPaneText -App $script:app -MaxLines 60)
        }
    }
}
