#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2 "Insert into pane works" / "Run in pane works" / "Command target is
# correct" via the Direct Helper Proposal path: ask Copilot to submit a specific command through
# the canonical WTA CLI, then Insert / Run it into the active shell pane. Distinct trigger from
# Feature.AutofixPane.Tests.ps1 (which arrives via a command failure).
#
# A UNIQUE marker makes the card text and pane assertion exact. Missing cards fail the test so
# canonical command, permission arming, and direct-pipe regressions remain visible.
# IMPORTANT: Insert and Run each use their OWN fresh terminal (like Feature.AutofixPane). With a
# shared terminal a prior card's "Run command"/"Insert in Terminal" text lingers in the
# scrollback and could co-occur with the next case's marker (which appears in the prompt echo)
# to false-positive the card-readiness check before a fresh card actually renders.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

Describe 'Feature §2 Direct Helper Proposal — Insert' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the copilot agent pane must reach a connected ACP session before driving the proposed-command card'
        $script:GetDirectProposalCard = {
            param($marker)
            Clear-AgentInput -App $script:app | Out-Null
            Send-AgentPrompt -App $script:app -Text "Submit a Direct Helper Proposal for exactly this shell command: echo $marker. Present the Run and Insert card now." | Out-Null
            $proposalGate = Wait-TerminalActionProposal -App $script:app -TimeoutSec 45 -ReturnOnPermission
            if ($proposalGate.Mode -eq 'Permission') {
                # Explicit test-user selection of the provider's allow option.
                Send-AgentKey -App $script:app -Key Y | Out-Null
            }
            Test-Until -TimeoutSec 10 -IntervalSec 1 -Condition {
                $t = Get-AgentPaneText -App $script:app -MaxLines 60
                ($t -match (Get-RecommendationCardRegex)) -and ($t -match [regex]::Escape($marker))
            }
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Insert: an agent-proposed command is inserted into the active shell pane (not run)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $marker = "INS$(Get-Random)"
        (& $script:GetDirectProposalCard $marker) | Should -BeTrue -Because 'the canonical WTA proposal command must produce a Direct Helper Proposal card'
        # Insert action = navigate Right (Run is the default-left action) then Enter.
        Send-AgentKey -App $script:app -Key Right | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null
        # The proposed command text lands in the source shell pane.
        Assert-Pane -App $script:app -SessionId $sid -Match $marker -TimeoutSec 12
        Send-WtKeys -App $script:app -SessionId $sid -Keys @('C-c')   # clear the inserted (unexecuted) line
    }
}

Describe 'Feature §2 Direct Helper Proposal — Run' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the copilot agent pane must reach a connected ACP session before driving the proposed-command card'
        $script:GetDirectProposalCard = {
            param($marker)
            Clear-AgentInput -App $script:app | Out-Null
            Send-AgentPrompt -App $script:app -Text "Submit a Direct Helper Proposal for exactly this shell command: echo $marker. Present the Run and Insert card now." | Out-Null
            $proposalGate = Wait-TerminalActionProposal -App $script:app -TimeoutSec 45 -ReturnOnPermission
            if ($proposalGate.Mode -eq 'Permission') {
                # Explicit test-user selection of the provider's allow option.
                Send-AgentKey -App $script:app -Key Y | Out-Null
            }
            Test-Until -TimeoutSec 10 -IntervalSec 1 -Condition {
                $t = Get-AgentPaneText -App $script:app -MaxLines 60
                ($t -match (Get-RecommendationCardRegex)) -and ($t -match [regex]::Escape($marker))
            }
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Run: an agent-proposed command runs in the active shell pane' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $marker = "RUN$(Get-Random)"
        (& $script:GetDirectProposalCard $marker) | Should -BeTrue -Because 'the canonical WTA proposal command must produce a Direct Helper Proposal card'
        # Run action is the default (left) selection -> Enter.
        Send-AgentKey -App $script:app -Key Left | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null
        # Executed in the source shell pane: `echo <marker>` prints the marker.
        Assert-Pane -App $script:app -SessionId $sid -Match $marker -TimeoutSec 15
        Send-WtKeys -App $script:app -SessionId $sid -Keys @('C-c')
    }
}
