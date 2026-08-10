#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #533: Insert hands keyboard control back to the target shell pane after
# delivering the command without running it.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.PackageFamilyName -like 'IntelligentTerminal_*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent proposal target focus' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'copilot' }
        $script:otherShell = Get-ActivePane -App $script:app
        $script:targetShell = Split-WtPane -App $script:app -SessionId $script:otherShell.session_id -Direction right
        Set-WtPaneFocus -App $script:app -SessionId $script:targetShell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:agent = Get-AgentPaneSession -App $script:app
        Wait-AgentReady -App $script:app -TimeoutSec 90 |
            Should -BeTrue
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
    }

    It 'Insert returns keyboard focus to the target shell pane' {
        $marker = "ITE2E_FOCUS_$(Get-Random -Maximum 999999)"

        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text "Create a recommendation card for exactly this shell command: echo $marker. Present the Run and Insert actions now." | Out-Null
        Test-Until -TimeoutSec 45 -IntervalSec 1 -Condition {
            $text = Get-AgentPaneText -App $script:app -MaxLines 60
            ($text -match (Get-RecommendationCardRegex)) -and
                ($text -match [regex]::Escape($marker))
        } | Should -BeTrue

        Set-WtPaneFocus -App $script:app -SessionId $script:otherShell.session_id
        (Get-ActivePane -App $script:app).session_id |
            Should -Be $script:otherShell.session_id
        Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Right | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Enter | Out-Null

        Assert-Pane -App $script:app -SessionId $script:targetShell.session_id -Match $marker -TimeoutSec 12
        $insertedText = Get-WtCapture -App $script:app -SessionId $script:targetShell.session_id -MaxLines 30
        ([regex]::Matches($insertedText, [regex]::Escape($marker))).Count |
            Should -Be 1 -Because 'Insert must place the command at the prompt without running it'
        Test-Until -TimeoutSec 15 -IntervalSec 0.25 -Condition {
            (Get-ActivePane -App $script:app).session_id -eq $script:targetShell.session_id
        } | Should -BeTrue -Because 'Insert must focus the shell pane bound to the recommendation'
    }
}
