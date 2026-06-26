#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# FEATURE TEST: agent pane chat input.
# User story: a user opens the agent pane, types a question, and the agent answers
# inline in the pane. Verified from the user's perspective (rendered pane text + AI oracle).
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: agent pane chat' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'answers a question typed into the agent pane' {
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null

        # The user types a question and submits it.
        $sess = Send-AgentPrompt -App $script:app -Text 'What is 7 times 6? Reply with only the number.'
        $sess.PaneSessionId | Should -Match '[0-9A-Fa-f-]{36}'

        # The agent's answer appears in the pane within a reasonable time.
        $answered = Test-Until -TimeoutSec 40 -IntervalSec 2 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 80) -match '42'
        }
        $answered | Should -BeTrue -Because 'the agent should answer 42 in the pane'

        # And the AI oracle agrees the pane shows a correct answer to the question.
        $paneText = Get-AgentPaneText -App $script:app -MaxLines 80
        Assert-AI -Claim 'The agent answered the question "what is 7 times 6" with 42.' -Context $paneText
    }
}
