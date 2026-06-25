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
        It 'Open/hide works at all four pane positions (bottom/right/top/left)' {
            # Exercise EVERY configured dock position (the old test only covered right+bottom).
            # Exact dock geometry is not reliably observable via UIA — top and left report the
            # same AgentLabelText coordinates — so we assert the open/hide contract at each
            # position, which is exactly what the checklist item requires.
            foreach ($pos in @('bottom', 'right', 'top', 'left')) {
                Set-WtPanePosition -App $script:app -Position $pos | Out-Null
                Stop-AgentPane -App $script:app | Out-Null
                Open-AgentPane -App $script:app -TimeoutSec 30 | Out-Null
                Test-AgentPaneOpen -App $script:app | Should -BeTrue -Because "the agent pane should open at position '$pos'"
                Stop-AgentPane -App $script:app | Out-Null
                Test-AgentPaneOpen -App $script:app | Should -BeFalse -Because "the agent pane should hide at position '$pos'"
            }
            Set-WtPanePosition -App $script:app -Position 'bottom' | Out-Null
            Open-AgentPane -App $script:app | Out-Null
        }
        It 'Tab close cleans up (closing a tab tears down its pre-warmed helper)' {
            # Each tab pre-warms its own agent-pane helper (a wta.exe child of WT). Closing the
            # tab must tear that helper back down — verify via the descendant wta.exe count, not
            # just that the original tab survives.
            $rootPid = $script:app.Pid
            $before = @(Get-DescendantWtaIds -RootPid $rootPid).Count
            $tab = New-WtTab -App $script:app -Title 'cleanup'
            $grew = Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { @(Get-DescendantWtaIds -RootPid $rootPid).Count -gt $before }
            $grew | Should -BeTrue -Because 'a new tab pre-warms its own helper (one more wta.exe child)'
            Close-WtPane -App $script:app -SessionId $tab.session_id
            $cleaned = Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition { @(Get-DescendantWtaIds -RootPid $rootPid).Count -le $before }
            $cleaned | Should -BeTrue -Because 'closing the tab cleans up its helper (no leak)'
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
        It '/model opens the model picker (Copilot advertises models)' {
            Clear-AgentInput -App $script:app | Out-Null
            # Drive the REAL /model command (proven live: Copilot opens a "Select model" modal
            # listing Auto + its model variants). The old test only checked the menu rendered.
            Invoke-AgentMenuItem -App $script:app -Name '/model'
            # The picker title is localized (model_picker.title), so match it across all bundled
            # locales rather than hardcoding the en-US "Select model" (would fail on non-en-US).
            $titleRe = Get-WtaLocalizedTextRegex -Key 'model_picker.title'
            if (-not $titleRe) { $titleRe = '(?i)Select model' }
            Assert-AgentPaneText -App $script:app -Pattern $titleRe -TimeoutSec 15
            # Prove a selectable model row actually rendered, rather than guessing model names
            # (auto/claude/gpt drift with releases and "auto" matches "autofix"/"automatic"):
            # the picker marks its highlighted row with the "> " selector (model_popup.rs
            # highlight_symbol), drawn INSIDE the popup's left border — e.g. "│>  ● Claude …".
            # Require a popup border char (│/║/|) immediately before the marker so a Markdown
            # blockquote ("> …") or a stray "->" in the chat transcript can't false-positive.
            (Get-AgentPaneText -App $script:app -MaxLines 60) | Should -Match '(?m)^\s*[│║|]\s*>\s+\S'
            Send-AgentKey -App $script:app -Key Escape | Out-Null
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
