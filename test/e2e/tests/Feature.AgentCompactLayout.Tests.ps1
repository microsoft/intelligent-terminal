#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #580: the real WT splitter minimum and WTA compact planner must keep a
# recommendation and the input usable together at constrained pane heights.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentCompactLayout.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: compact agent pane layout' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpProposalAgent.ps1')).Path
        $script:fixtureLog = Join-Path $env:TEMP "ite2e-compact-layout-$([guid]::NewGuid().ToString('N')).log"
        $command = "pwsh -NoProfile -File $fixture -LogPath $script:fixtureLog"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:proposal-fixture'
            acpCustomCommand = $command
        }
        $script:shellPane = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $script:shellPane.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before creating the recommendation'
    }
    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
        if ($script:fixtureLog -and (Test-Path -LiteralPath $script:fixtureLog)) {
            Remove-Item -LiteralPath $script:fixtureLog -Force
        }
    }

    It 'Compact height keeps the recommendation and input usable' {
        $commandMarker = "COMPACT$([guid]::NewGuid().ToString('N').Substring(0, 12))"
        Clear-AgentInput -App $script:app -PaneSessionId $script:agentPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "Create the compact recommendation for $commandMarker." | Out-Null
        $fullCard = Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 80
            ($text -match (Get-RecommendationCardRegex)) -and
                ($text -match [regex]::Escape($commandMarker))
        }
        $fullCard | Should -BeTrue -Because 'the real proposal path must render a full recommendation before resizing'

        Set-AgentPaneFocus -App $script:app | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x26 -Alt -Shift -Repeat 10 -RequireForeground | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x28 -Alt -Shift -Repeat 10 -RequireForeground | Out-Null

        $compact = Wait-Until -TimeoutSec 10 -IntervalSec 0.25 -Because 'the recommendation to use the compact layout' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            $lineCount = @(($text -split "`r?`n")).Count
            if ($lineCount -le 9 -and
                $text -match [regex]::Escape($commandMarker) -and
                $text -match 'Run command' -and
                $text -match 'Insert in Terminal' -and
                $text -match 'Ask anything') {
                $text
            }
        }
        @(($compact -split "`r?`n")).Count | Should -BeLessOrEqual 9 -Because 'the assertion must run at the real compact pane height'

        $draftMarker = "DRAFT$([guid]::NewGuid().ToString('N').Substring(0, 12))"
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Down | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draftMarker -NoSubmit | Out-Null
        $withDraft = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 20
        $withDraft | Should -Match ([regex]::Escape($draftMarker)) -Because 'the compact input must remain editable beside the recommendation'
        $withDraft | Should -Match ([regex]::Escape($commandMarker)) -Because 'editing input must not hide the selected compact recommendation'

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Up | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Right | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null
        Assert-Pane -App $script:app -SessionId $script:shellPane.session_id -Match $commandMarker -TimeoutSec 12
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -Pattern ([regex]::Escape($draftMarker)) -TimeoutSec 5
        Send-WtKeys -App $script:app -SessionId $script:shellPane.session_id -Keys @('C-c') | Out-Null
    }
}
