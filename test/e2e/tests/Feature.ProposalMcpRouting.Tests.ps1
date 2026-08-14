#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #560: proposal MCP capabilities and server names are isolated per ACP
# session, so a later tab cannot redirect an earlier tab's tool call.
#
#   Invoke-Pester test/e2e/tests/Feature.ProposalMcpRouting.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: proposal MCP session routing' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }

        $tabAShell = (Get-ActivePane -App $script:app).session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:tabAPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $tabAShell -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:tabAPane -TimeoutSec 60 |
            Should -BeTrue -Because 'tab A must have a connected ACP session'

        $tabB = New-WtTab -App $script:app -Title 'proposal-mcp-tab-b'
        Set-WtPaneFocus -App $script:app -SessionId $tabB.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:tabBPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $tabB.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:tabBPane -TimeoutSec 60 |
            Should -BeTrue -Because 'tab B must have a connected ACP session'
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Proposal MCP routing is isolated per tab' {
        $masterLog = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $serverNames = [regex]::Matches($masterLog, 'intellterm_[0-9a-f]{20}') |
            ForEach-Object Value
        $uniqueServerNames = @($serverNames | Select-Object -Unique)
        @($serverNames).Count | Should -BeGreaterOrEqual 2 -Because 'each tab session/new must receive a proposal MCP server'
        $uniqueServerNames.Count | Should -Be @($serverNames).Count -Because 'every ACP session must receive an independently named MCP server'

        $cardRegex = Get-RecommendationCardRegex
        $markerA = "MCPA$(Get-Random)"
        Initialize-LogOffsets -App $script:app | Out-Null
        Clear-AgentInput -App $script:app -PaneSessionId $script:tabAPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabAPane -Text "Submit a Direct Helper Proposal for exactly this shell command: echo $markerA. Present the Run and Insert card now." | Out-Null

        $cardA = Test-Until -TimeoutSec 60 -IntervalSec 1 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 80
            ($text -match $cardRegex) -and ($text -match [regex]::Escape($markerA))
        }
        $cardA | Should -BeTrue -Because 'tab A proposal must render in tab A after tab B has registered its MCP server'
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:tabBPane -MaxLines 80) |
            Should -Not -Match $cardRegex -Because 'tab A capability must not route its card into tab B'

        $routeLogA = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $routeA = [regex]::Matches(
            $routeLogA,
            'routing terminal action request to owning Helper.*helper_id=HelperId\((?<helper>\d+)\)\s+session_id=(?<session>[0-9a-f-]{36})'
        ) | Select-Object -Last 1
        $routeA | Should -Not -BeNullOrEmpty -Because 'the master must log tab A session-to-helper routing'

        for ($i = 0; $i -lt 5 -and ((Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 60) -match $cardRegex); $i++) {
            Send-AgentKey -App $script:app -PaneSessionId $script:tabAPane -Key Escape | Out-Null
        }
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 60) |
            Should -Not -Match $cardRegex -Because 'tab A card must be dismissed before the reverse-routing control'

        $markerB = "MCPB$(Get-Random)"
        Initialize-LogOffsets -App $script:app | Out-Null
        Clear-AgentInput -App $script:app -PaneSessionId $script:tabBPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabBPane -Text "Submit a Direct Helper Proposal for exactly this shell command: echo $markerB. Present the Run and Insert card now." | Out-Null

        $cardB = Test-Until -TimeoutSec 60 -IntervalSec 1 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:tabBPane -MaxLines 80
            ($text -match $cardRegex) -and ($text -match [regex]::Escape($markerB))
        }
        $cardB | Should -BeTrue -Because 'tab B proposal must render in tab B'
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 80) |
            Should -Not -Match $cardRegex -Because 'tab B capability must not route its card into tab A'

        $routeLogB = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $routeB = [regex]::Matches(
            $routeLogB,
            'routing terminal action request to owning Helper.*helper_id=HelperId\((?<helper>\d+)\)\s+session_id=(?<session>[0-9a-f-]{36})'
        ) | Select-Object -Last 1
        $routeB | Should -Not -BeNullOrEmpty -Because 'the master must log tab B session-to-helper routing'
        $routeB.Groups['session'].Value | Should -Not -Be $routeA.Groups['session'].Value
        $routeB.Groups['helper'].Value | Should -Not -Be $routeA.Groups['helper'].Value
    }
}
