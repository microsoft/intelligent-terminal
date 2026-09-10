#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Issue #793: adjacent Agent Pane regions must use the same top-level horizontal
# lane. The deterministic proposal fixture crosses ACP, master, Helper, ConPTY,
# and the packaged WTA renderer without consuming model tokens.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent pane padding' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpProposalAgent.ps1')).Path
        $script:fixtureLog = Join-Path $env:TEMP "ite2e-agent-pane-padding-$([guid]::NewGuid().ToString('N')).log"
        $command = "pwsh -NoProfile -File $fixture -LogPath $script:fixtureLog"
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) {
            $env:ITE2E_ARTIFACT_ROOT
        }
        else {
            Join-Path $PSScriptRoot '..\artifacts\agent-pane-padding\current'
        }
        $script:evidenceDir = Join-Path $artifactRoot 'agent-pane-padding'
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null

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
        try {
            if ($script:app) {
                Stop-Terminal -App $script:app
            }
        }
        finally {
            if ($script:fixtureLog -and (Test-Path -LiteralPath $script:fixtureLog)) {
                Copy-Item -LiteralPath $script:fixtureLog -Destination (Join-Path $script:evidenceDir 'fixture.log') -Force
                Remove-Item -LiteralPath $script:fixtureLog -Force
            }
        }
    }

    It 'Agent pane regions share consistent horizontal padding' {
        $marker = "COMPACT$([guid]::NewGuid().ToString('N').Substring(0, 12))"
        Clear-AgentInput -App $script:app -PaneSessionId $script:agentPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "Create the recommendation for $marker." | Out-Null

        $capture = Wait-Until -TimeoutSec 30 -IntervalSec 0.5 -Because 'the full recommendation and navigation hint to render' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            if ($text -match [regex]::Escape("echo $marker") -and
                $text -match (Get-RecommendationCardRegex) -and
                $text -match '↑.*↓.*←.*→') {
                $text
            }
        }
        $capture | Should -Not -BeNullOrEmpty

        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'rendered-pane.txt') -Value $capture -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'rendered-pane.png') | Out-Null

        $lines = @($capture -split "`r?`n")
        $commandLine = $lines | Where-Object { $_ -match [regex]::Escape("echo $marker") } | Select-Object -First 1
        $buttonLine = $lines | Where-Object { $_ -match (Get-WtaLocalizedTextRegex -Key 'recommendations.button_run_command') } | Select-Object -First 1
        $hintLine = $lines | Where-Object { $_ -match '↑.*↓.*←.*→' } | Select-Object -First 1

        $commandLine | Should -Not -BeNullOrEmpty
        $buttonLine | Should -Not -BeNullOrEmpty
        $hintLine | Should -Not -BeNullOrEmpty

        $cardLeft = $commandLine.IndexOf('│')
        $cardRight = $commandLine.LastIndexOf('│')
        $hintLeft = $hintLine.Length - $hintLine.TrimStart().Length
        $hintRight = $hintLine.TrimEnd().Length - 1
        $commandColumn = $commandLine.IndexOf("echo $marker", [StringComparison]::Ordinal)
        $buttonColumn = $buttonLine.IndexOf('[')

        $cardLeft | Should -Be 1 -Because 'the recommendation card uses the established one-cell top-level lane'
        $hintLeft | Should -BeGreaterOrEqual $cardLeft -Because 'the navigation hint must stay inside the card lane on the left'
        $hintRight | Should -BeLessOrEqual $cardRight -Because 'the navigation hint must stay inside the card lane on the right'
        (($hintLeft -eq $cardLeft) -or ($hintRight -eq $cardRight)) |
            Should -BeTrue -Because 'the localized navigation hint must touch the card lane on its locale-aligned edge'
        $buttonColumn | Should -Be $commandColumn -Because 'the existing card-internal command and action alignment must be preserved'
    }
}
