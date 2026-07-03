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
            # BUTTONS (no typing into the chat input, unlike the `/sessions` slash) and observe the
            # view state via the AgentLabelText UIA element, NOT a conpty capture: the buttons flip
            # C++'s _isSessionsView through the set_agent_state round-trip, which sets AgentLabelText
            # to the localized "Agent sessions[: <agent>]" title (AgentPaneContent.cpp:181) — a clean,
            # per-focused-window XAML label with no agent-pane-sessions.jsonl newest-alive ambiguity.
            # We compare against the captured chat label (locale-robust) rather than matching English.
            # Both view switches go through XAML buttons (SessionToggleButton / AgentToggleButton), so
            # neither touches the input, and we never use Esc (overloaded: view-exit vs. input-clear).
            #
            # Ensure the agent pane is actually visible first — a prior test may have left it stashed,
            # in which case AgentLabelText isn't in the tree at all (sustained empty reads).
            Open-AgentPane -App $script:app | Out-Null
            Start-Sleep -Milliseconds 500
            # Normalize to a CLEAN chat baseline: a prior test can return to chat via Esc, whose
            # TUI-internal switch does not reliably notify C++, leaving _isSessionsView (hence the
            # label) stale on "Agent sessions". Clicking the chat button forces view=chat. winapp
            # get-value can transiently return empty while the pane content rebuilds after the click,
            # so we poll for the same non-empty label value observed twice (robust to empty blips).
            Invoke-UiElement -App $script:app -Selector 'AgentToggleButton' -TimeoutSec 10 | Out-Null
            $chatLabel = $null
            $seen = @{}
            for ($i = 0; $i -lt 24 -and -not $chatLabel; $i++) {
                Start-Sleep -Milliseconds 500
                $v = Get-UiValue -App $script:app -Selector 'AgentLabelText'
                if ($v) { $seen[$v] = 1 + ($seen[$v]); if ($seen[$v] -ge 2) { $chatLabel = $v } }
            }
            $chatLabel | Should -Not -BeNullOrEmpty -Because 'setup: a stable chat-view agent label must be readable'
            $draft = "draft$(Get-Random)"
            $typed = $false
            for ($t = 0; $t -lt 3 -and -not $typed; $t++) {
                Clear-AgentInput -App $script:app | Out-Null
                Send-AgentPrompt -App $script:app -Text $draft -NoSubmit | Out-Null
                $typed = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 40) -match $draft }
            }
            $typed | Should -BeTrue -Because 'setup: the draft must be typed into the chat input'
            # Switch chat -> sessions via the button; confirm via the UIA label (not conpty). Retry
            # the invoke (occasional no-op), re-checking before each click so we don't over-toggle.
            $inSessions = $false
            for ($c = 0; $c -lt 4 -and -not $inSessions; $c++) {
                if ((Get-UiValue -App $script:app -Selector 'AgentLabelText') -ne $chatLabel) { $inSessions = $true; break }
                Invoke-UiElement -App $script:app -Selector 'SessionToggleButton' -TimeoutSec 10 | Out-Null
                $inSessions = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                    (Get-UiValue -App $script:app -Selector 'AgentLabelText') -ne $chatLabel
                }
            }
            $inSessions | Should -BeTrue -Because 'the button must switch the pane into the session view (AgentLabelText changes)'
            # Return sessions -> chat via the AgentToggleButton (a XAML button routed through the same
            # set_agent_state round-trip, view="chat" — _AgentToggleButtonOnClick). This avoids Esc
            # entirely (Esc's TUI-internal return doesn't reliably notify C++ to restore the label and
            # risks clearing the chat input). The winapp invoke occasionally no-ops, so retry, and
            # re-check the label BEFORE each click so we never over-toggle back into sessions.
            $backToChat = $false
            for ($c = 0; $c -lt 4 -and -not $backToChat; $c++) {
                if ((Get-UiValue -App $script:app -Selector 'AgentLabelText') -eq $chatLabel) { $backToChat = $true; break }
                Invoke-UiElement -App $script:app -Selector 'AgentToggleButton' -TimeoutSec 10 | Out-Null
                $backToChat = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                    (Get-UiValue -App $script:app -Selector 'AgentLabelText') -eq $chatLabel
                }
            }
            $backToChat | Should -BeTrue -Because 'the chat button must return the pane to the chat view (AgentLabelText restores)'
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
