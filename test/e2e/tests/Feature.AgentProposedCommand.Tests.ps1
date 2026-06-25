#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2 "Insert into pane works" / "Run in pane works" / "Command target is
# correct" via the NON-autofix chat path: ask Copilot to propose a specific command, which
# surfaces the same Run/Insert recommendation card the autofix flow uses, then Insert / Run it
# into the active shell pane. Distinct trigger from Feature.AutofixPane.Tests.ps1 (which arrives
# via a command failure); this is the agent proposing a command in normal chat.
#
# LLM nondeterminism is tamed by (a) a UNIQUE marker we fully control, so the card text and the
# pane assertion are exact, and (b) cross-retry + skip if Copilot explains instead of carding.
# IMPORTANT: Insert and Run each use their OWN fresh terminal (like Feature.AutofixPane). With a
# shared terminal a prior card's "Run command"/"Insert in Terminal" text lingers in the
# scrollback and could co-occur with the next case's marker (which appears in the prompt echo)
# to false-positive the card-readiness check before a fresh card actually renders.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

Describe 'Feature §2 agent-proposed command — Insert (chat path)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the copilot agent pane must reach a connected ACP session before driving the proposed-command card'
        # Recommendation card button labels ([ Run command ] / Insert in Terminal) are localized,
        # so match each across all bundled locales (Get-WtaLocalizedTextRegex; en-US fallback).
        $script:CardRunRegex = (Get-WtaLocalizedTextRegex -Key 'recommendations.button_run_command')
        if (-not $script:CardRunRegex) { $script:CardRunRegex = 'Run command' }
        $script:CardInsertRegex = (Get-WtaLocalizedTextRegex -Key 'recommendations.button_insert_in_terminal')
        if (-not $script:CardInsertRegex) { $script:CardInsertRegex = 'Insert in Terminal' }
        # Ask Copilot to propose `echo <marker>`; return $true once the recommendation card
        # renders BOTH the Run AND Insert actions AND our exact marker.
        $script:GetCommandCard = {
            param($marker)
            for ($try = 0; $try -lt 3; $try++) {
                Clear-AgentInput -App $script:app | Out-Null
                Send-AgentPrompt -App $script:app -Text "Propose the exact shell command: echo $marker -- as a runnable command for my terminal so I can Run or Insert it. Do not just explain it." | Out-Null
                $ok = Test-Until -TimeoutSec 35 -IntervalSec 2 -Condition {
                    $t = Get-AgentPaneText -App $script:app -MaxLines 60
                    ($t -match $script:CardRunRegex) -and ($t -match $script:CardInsertRegex) -and ($t -match [regex]::Escape($marker))
                }
                if ($ok) { return $true }
            }
            return $false
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Insert: an agent-proposed command is inserted into the active shell pane (not run)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $marker = "INS$(Get-Random)"
        if (-not (& $script:GetCommandCard $marker)) { Set-ItResult -Skipped -Because 'Copilot returned an explanation, not a runnable-command card (LLM variance)'; return }
        # Insert action = navigate Right (Run is the default-left action) then Enter.
        Send-AgentKey -App $script:app -Key Right | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null
        # The proposed command text lands in the source shell pane.
        Assert-Pane -App $script:app -SessionId $sid -Match $marker -TimeoutSec 12
        Send-WtKeys -App $script:app -SessionId $sid -Keys @('C-c')   # clear the inserted (unexecuted) line
    }
}

Describe 'Feature §2 agent-proposed command — Run (chat path)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the copilot agent pane must reach a connected ACP session before driving the proposed-command card'
        $script:CardRunRegex = (Get-WtaLocalizedTextRegex -Key 'recommendations.button_run_command')
        if (-not $script:CardRunRegex) { $script:CardRunRegex = 'Run command' }
        $script:CardInsertRegex = (Get-WtaLocalizedTextRegex -Key 'recommendations.button_insert_in_terminal')
        if (-not $script:CardInsertRegex) { $script:CardInsertRegex = 'Insert in Terminal' }
        $script:GetCommandCard = {
            param($marker)
            for ($try = 0; $try -lt 3; $try++) {
                Clear-AgentInput -App $script:app | Out-Null
                Send-AgentPrompt -App $script:app -Text "Propose the exact shell command: echo $marker -- as a runnable command for my terminal so I can Run or Insert it. Do not just explain it." | Out-Null
                $ok = Test-Until -TimeoutSec 35 -IntervalSec 2 -Condition {
                    $t = Get-AgentPaneText -App $script:app -MaxLines 60
                    ($t -match $script:CardRunRegex) -and ($t -match $script:CardInsertRegex) -and ($t -match [regex]::Escape($marker))
                }
                if ($ok) { return $true }
            }
            return $false
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Run: an agent-proposed command runs in the active shell pane' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $marker = "RUN$(Get-Random)"
        if (-not (& $script:GetCommandCard $marker)) { Set-ItResult -Skipped -Because 'Copilot returned an explanation, not a runnable-command card (LLM variance)'; return }
        # Run action is the default (left) selection -> Enter.
        Send-AgentKey -App $script:app -Key Left | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null
        # Executed in the source shell pane: `echo <marker>` prints the marker.
        Assert-Pane -App $script:app -SessionId $sid -Match $marker -TimeoutSec 15
        Send-WtKeys -App $script:app -SessionId $sid -Keys @('C-c')
    }
}
