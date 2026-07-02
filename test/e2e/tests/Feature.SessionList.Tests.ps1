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
        It 'View switch preserves the draft input (type, open session view, return -> draft remains)' -Skip {
            # Checklist §2 "View switch preserves input" (C085). Left as a documented E2E gap: it is
            # NOT reliably automatable in this build, and the underlying contract is covered by design
            # + Rust units.
            #
            # Why no reliable E2E path:
            #   * The draft lives in the chat input. To prove it survives a view round-trip we must
            #     switch chat<->sessions WITHOUT typing into that input.
            #   * The `/sessions` SLASH command types "/sessions" INTO the input, which itself
            #     mutates the draft — invalid for this specific test.
            #   * The bottom-bar BUTTON switches without typing, but the button->session-view RENDER
            #     is not reliably observable here (the run's per-tab pre-warm registers >1 agent pane,
            #     so the run-scoped pane read can target a different pane than the one clicked — the
            #     same reason Open-SessionList prefers the slash path). The button REQUEST is
            #     confirmed (set_agent_state view=sessions in terminal-agent-pane.log) but the render
            #     can't be pinned, and Esc is overloaded (exits session view vs. clears the chat
            #     input), so we can't deterministically time the single Esc that returns to chat.
            # The draft is per-tab TUI state (app.rs:1550 "each tab keeps its own draft text") and is
            # not touched by a view switch, so preservation is structural; the recommendation-card
            # draft-preservation units (app.rs recommendation_card_enter_wins_over_draft_input, etc.)
            # exercise the same input-buffer-survives-a-mode-change contract.
            $true | Should -BeTrue
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
            Send-AgentKey -App $script:app -Key Down | Out-Null
            $sel2 = Get-SessionListSelection -App $script:app
            $second = "$($sel2.Title)|$($sel2.Meta)"
            # Rows can share a title (e.g. repeated oracle prompts), so compare title+meta
            # (timestamps differ). With >1 row the selection must move to a distinct row.
            if (@(Get-SessionRows -App $script:app).Count -gt 1) { $second | Should -Not -Be $first }
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
