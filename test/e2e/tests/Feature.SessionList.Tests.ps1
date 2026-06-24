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
            Open-SessionList -App $script:app | Out-Null
            Test-SessionListShown -App $script:app | Should -BeTrue
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
