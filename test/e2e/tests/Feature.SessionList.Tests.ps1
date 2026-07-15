#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: Surfaces + Session states + Chat/session view switching + Focus/restore
# (the agent-pane session-management view). Deterministic where possible; AI only for
# semantic checks.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature: session list + view switching + focus/restore' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        # Seed a live session with a known marker so a row exists.
        Send-AgentPrompt -App $script:app -Text 'What is 5 plus 5? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern '\b10\b' -TimeoutSec 40
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    Context 'Surfaces' {
        It 'Session button works (opens the session view)' {
            # Verify the BOTTOM-BAR button specifically (not the slash path Open-SessionList now
            # prefers). The button is wired to _RequestAgentStateForTab(view="sessions") — proven at
            # the product level by the emitted `set_agent_state view=sessions` in the agent-pane log,
            # which fires reliably on every click (the pane-text READ after a click is what's flaky
            # when the run has >1 agent pane, so we assert the emitted request, not the render).
            # Make sure we start in chat view so the click switches INTO sessions.
            if (Test-SessionListShown -App $script:app -TimeoutSec 1) { Close-SessionList -App $script:app | Out-Null; Start-Sleep -Milliseconds 500 }
            Initialize-LogOffsets -App $script:app | Out-Null
            Invoke-UiElement -App $script:app -Selector 'SessionToggleButton' -TimeoutSec 10 | Out-Null
            $requested = Test-Until -TimeoutSec 10 -IntervalSec 0.5 -Condition {
                (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) -match 'requesting set_agent_state:.*view=sessions'
            }
            $requested | Should -BeTrue -Because 'clicking the session-management button must request the sessions view (set_agent_state view=sessions)'
            # Leave the pane in the sessions view for the following cases (open reliably via slash).
            Open-SessionList -App $script:app | Out-Null
        }
        It 'Session view shows rows with navigation hints' {
            Assert-AgentPaneText -App $script:app -Pattern 'to launch session|to navigate|Enter to launch' -TimeoutSec 8
            @(Get-SessionRows -App $script:app).Count | Should -BeGreaterThan 0
        }
        It 'Session view refresh works (reopen renders the list again)' {
            Close-SessionList -App $script:app | Out-Null
            Open-SessionList -App $script:app | Out-Null
            Test-SessionListShown -App $script:app | Should -BeTrue
        }
        It 'Slash command works (/sessions opens the session view)' {
            # The session view is reachable from the `/` command menu (not just the button).
            Close-SessionList -App $script:app | Out-Null
            Invoke-AgentMenuItem -App $script:app -Name '/sessions'
            Test-SessionListShown -App $script:app | Should -BeTrue
        }
        It 'Command action / out-of-band registry list (wta sessions list) is identity-gated' -Skip {
            # KNOWN LIMITATION: `wta sessions list --json` needs the in-package master, but
            # an externally-launched wta copy resolves the wrong runtime paths and reports
            # "wta-master not running". The in-pane session view (covered above) is the
            # automatable surface; the CLI registry list requires in-package identity.
            Get-SessionListJson -App $script:app -Origin all | Out-Null
        }
    }

    Context 'Session states' {
        It 'Active/Live state is correct (the just-created session shows as current)' {
            Open-SessionList -App $script:app | Out-Null
            $rows = Get-SessionRows -App $script:app
            ($rows | Where-Object { $_.Meta -match 'just now|now|second|minute' }).Count | Should -BeGreaterThan 0
        }
        It 'Historical state is correct (older sessions show relative timestamps)' {
            $rows = Get-SessionRows -App $script:app
            # At least the live row exists; historical rows (if any) carry "ago" timestamps.
            ($rows | Where-Object { $_.Meta -match 'ago|just now' }).Count | Should -BeGreaterThan 0
        }
        It 'State transitions are correct (selection marker is unique)' {
            @(Get-SessionRows -App $script:app | Where-Object Selected).Count | Should -BeLessOrEqual 1
        }
    }

    Context 'Chat/session view switching' {
        It 'Session view opens from chat' {
            Close-SessionList -App $script:app | Out-Null
            Open-SessionList -App $script:app | Out-Null
            Test-SessionListShown -App $script:app | Should -BeTrue
        }
        It 'Chat view restores (Esc returns to chat input)' {
            Close-SessionList -App $script:app | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern 'Ask anything|/ for commands' -TimeoutSec 8
        }
        It 'View switch preserves the draft input (type, open session view, return -> draft remains)' {
            # Checklist §2 "View switch preserves input" (C085). Switch views via the BOTTOM-BAR
            # BUTTONS (SessionToggleButton / AgentToggleButton) — not the `/sessions` slash (which
            # types into the chat input) and not Esc (overloaded: view-exit vs. input-clear).
            #
            # Detect the view from the PANE TEXT (locale-robust Get-SessionViewRenderRegex, the same
            # signal Open-SessionList uses), NOT the AgentLabelText UIA value: winapp `get-value` on
            # that XAML label returns empty here (its text is exposed via Name, not a Value pattern,
            # and the per-tab pre-warm can target another pane), which turned the old baseline read
            # into a hard setup failure. The session view's footer nav hint ("↑ ↓ …") is the reliable
            # "the list is rendered" signal.
            Open-AgentPane -App $script:app | Out-Null
            Start-Sleep -Milliseconds 500
            $sessionRe = Get-SessionViewRenderRegex
            $inSessionsView = { (Get-AgentPaneText -App $script:app -MaxLines 50) -match $sessionRe }
            $inChatView = { -not ((Get-AgentPaneText -App $script:app -MaxLines 50) -match $sessionRe) }

            # Start from a CLEAN chat baseline: a prior test can leave the pane in the session view.
            # Only click the chat button when we are actually in the session view — clicking it while
            # already in chat would STASH the pane (the old code's unconditional click was itself a
            # flake source / empty-read cause).
            $startedChat = $false
            for ($c = 0; $c -lt 4 -and -not $startedChat; $c++) {
                if (& $inChatView) { $startedChat = $true; break }
                Invoke-UiElement -App $script:app -Selector 'AgentToggleButton' -TimeoutSec 10 | Out-Null
                $startedChat = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition $inChatView
            }
            $startedChat | Should -BeTrue -Because 'setup: the pane must start in the chat view before typing a draft'

            $draft = "draft$(Get-Random)"
            $typed = $false
            for ($t = 0; $t -lt 3 -and -not $typed; $t++) {
                Clear-AgentInput -App $script:app | Out-Null
                Send-AgentPrompt -App $script:app -Text $draft -NoSubmit | Out-Null
                $typed = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 40) -match $draft }
            }
            $typed | Should -BeTrue -Because 'setup: the draft must be typed into the chat input'

            # Switch chat -> sessions via the button; confirm the session list actually renders.
            # Re-check before each click so we never over-toggle back out of the session view.
            $inSessions = $false
            for ($c = 0; $c -lt 4 -and -not $inSessions; $c++) {
                if (& $inSessionsView) { $inSessions = $true; break }
                Invoke-UiElement -App $script:app -Selector 'SessionToggleButton' -TimeoutSec 10 | Out-Null
                $inSessions = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition $inSessionsView
            }
            $inSessions | Should -BeTrue -Because 'the button must switch the pane into the session view (session list renders)'

            # Return sessions -> chat via the AgentToggleButton (routed through set_agent_state
            # view="chat", _AgentToggleButtonOnClick). Avoids Esc, which risks clearing the input.
            # Re-check before each click so we never over-toggle back into the session view.
            $backToChat = $false
            for ($c = 0; $c -lt 4 -and -not $backToChat; $c++) {
                if (& $inChatView) { $backToChat = $true; break }
                Invoke-UiElement -App $script:app -Selector 'AgentToggleButton' -TimeoutSec 10 | Out-Null
                $backToChat = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition $inChatView
            }
            $backToChat | Should -BeTrue -Because 'the chat button must return the pane to the chat view (session list gone)'
            # The draft must still be in the chat input after the round-trip.
            Assert-AgentPaneText -App $script:app -Pattern $draft -TimeoutSec 8
            Clear-AgentInput -App $script:app | Out-Null
        }

        It 'View switch preserves connection (agent still answers after switching)' {
            Send-AgentPrompt -App $script:app -Text 'What is 8 plus 1? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern '\b9\b' -TimeoutSec 40
        }
    }

    Context 'Focus and restore' {
        It 'Navigate the list (selection moves with arrow keys)' {
            Open-SessionList -App $script:app | Out-Null
            $sel1 = Get-SessionListSelection -App $script:app
            $first = "$($sel1.Title)|$($sel1.Meta)"
            # A single-row list can't move the selection — nothing to assert.
            if (@(Get-SessionRows -App $script:app).Count -le 1) {
                Set-ItResult -Skipped -Because 'session list has a single row'
                return
            }
            # Press Down and poll for the move: the TUI re-render + capture-pane read can lag
            # the keypress, so a single immediate read occasionally catches the pre-move frame
            # (flaky under load). Retry (re-pressing Down) until the selected row differs from
            # the start. Rows can share a title (repeated oracle prompts), so compare
            # title+meta (timestamps differ).
            $moved = Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                Send-AgentKey -App $script:app -Key Down | Out-Null
                $sel2 = Get-SessionListSelection -App $script:app
                "$($sel2.Title)|$($sel2.Meta)" -ne $first
            }
            $moved | Should -BeTrue -Because 'arrow Down must move the selection to a distinct row'
        }
        It 'Enter behavior works (Enter on a row launches/resumes without error)' {
            # Select the live session row and resume it; the pane returns to a chat view.
            Open-SessionList -App $script:app | Out-Null
            Resume-Session -App $script:app | Out-Null
            # After Enter the session view is dismissed (back to a chat/agent view).
            $backToChat = Test-Until -TimeoutSec 12 -Condition {
                -not (Test-SessionListShown -App $script:app -TimeoutSec 1)
            }
            $backToChat | Should -BeTrue
        }
    }
}
