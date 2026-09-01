#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #560: proposal MCP capabilities and server names are isolated per ACP
# session, so a later tab cannot redirect an earlier tab's tool call.
#
#   Invoke-Pester test/e2e/tests/Feature.ProposalMcpRouting.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $fixturePath = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue) -and
        ($fixturePath -notmatch '\s') -and
        ($env:TEMP -notmatch '\s')
    )
}

Describe 'Feature: proposal MCP session routing' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $script:requestLog = Join-Path $env:TEMP "ite2e-proposal-routing-$([guid]::NewGuid().ToString('N')).log"
        $command = "pwsh -NoProfile -File $script:fixture -LogPath $script:requestLog"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:interaction-fixture'
            acpCustomCommand = $command
            acpModel = ''
        }
        $tabA = Get-ActivePane -App $script:app
        $tabAShell = $tabA.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:tabAPane = (Wait-NewAgentPaneSession -App $script:app -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:tabAPane -TimeoutSec 60 |
            Should -BeTrue -Because 'tab A must have a connected ACP session'

        $tabB = New-WtTab -App $script:app -Title 'proposal-mcp-tab-b'
        Set-WtPaneFocus -App $script:app -SessionId $tabB.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:tabBPane = (Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId @($script:tabAPane) -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:tabBPane -TimeoutSec 60 |
            Should -BeTrue -Because 'tab B must have a connected ACP session'
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($script:requestLog -and (Test-Path -LiteralPath $script:requestLog)) {
            Remove-Item -LiteralPath $script:requestLog -Force
        }
    }

    It 'Proposal MCP routing is isolated per tab' {
        $sessionLines = Wait-Until -TimeoutSec 15 -Because 'both ACP sessions to record their public Session MCP server' -Condition {
            $lines = @(
                Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                    Where-Object { $_ -match '\|session/new\|' }
            )
            if ($lines.Count -ge 2) { $lines }
        }
        foreach ($line in $sessionLines) {
            $line | Should -Match '\|session/new\|[^|]+\|mcp_server=intellterm_[0-9a-f]{16}$'
        }
        $serverNames = @(
            $sessionLines | ForEach-Object {
                [regex]::Match($_, 'mcp_server=(intellterm_[0-9a-f]{16})$').Groups[1].Value
            }
        )
        $uniqueServerNames = @($serverNames | Select-Object -Unique)
        $serverNames.Count | Should -BeGreaterOrEqual 2 -Because 'each tab session/new must receive a Session MCP server with the public 16-hex name'
        $uniqueServerNames.Count | Should -Be @($serverNames).Count -Because 'every ACP session must receive an independently named MCP server'
        $cardRegex = Get-WtaLocalizedTextRegex -Key 'recommendations.button_open_in_new_tab'
        if (-not $cardRegex) { $cardRegex = '(?i)Open in New Tab' }
        $markerA = "A$([guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant())"
        Initialize-LogOffsets -App $script:app | Out-Null
        Clear-AgentInput -App $script:app -PaneSessionId $script:tabAPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabAPane -Text "TAB_DIRECTION_$markerA" | Out-Null

        $cardA = Wait-Until -TimeoutSec 30 -IntervalSec 0.5 -Because 'tab A Session MCP workspace card to render in tab A' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 80
            if (($text -match $cardRegex) -and ($text -match [regex]::Escape($markerA))) { $text }
        }
        $cardA | Should -Not -BeNullOrEmpty -Because 'tab A proposal must render in tab A after tab B has registered its MCP server'
        $tabBText = Get-AgentPaneText -App $script:app -PaneSessionId $script:tabBPane -MaxLines 80
        $tabBText | Should -Not -Match $cardRegex -Because 'tab A capability must not render any workspace card in tab B'
        $tabBText | Should -Not -Match ([regex]::Escape($markerA)) -Because 'tab A capability must not route its marker into tab B'

        $routeLogA = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $routeA = [regex]::Matches(
            $routeLogA,
            'routing MCP request to owning Helper.*helper_id=HelperId\((?<helper>\d+)\)\s+session_id=(?<session>interaction-\d+-\d+)'
        ) | Select-Object -Last 1
        $routeA | Should -Not -BeNullOrEmpty -Because 'the master must log tab A session-to-helper routing'

        for ($i = 0; $i -lt 5 -and ((Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 60) -match $cardRegex); $i++) {
            Send-AgentKey -App $script:app -PaneSessionId $script:tabAPane -Key Escape | Out-Null
        }
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 60) |
            Should -Not -Match $cardRegex -Because 'tab A card must be dismissed before the reverse-routing control'

        $markerB = "B$([guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant())"
        Initialize-LogOffsets -App $script:app | Out-Null
        Clear-AgentInput -App $script:app -PaneSessionId $script:tabBPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabBPane -Text "TAB_DIRECTION_$markerB" | Out-Null

        $cardB = Wait-Until -TimeoutSec 30 -IntervalSec 0.5 -Because 'tab B Session MCP workspace card to render in tab B' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:tabBPane -MaxLines 80
            if (($text -match $cardRegex) -and ($text -match [regex]::Escape($markerB))) { $text }
        }
        $cardB | Should -Not -BeNullOrEmpty -Because 'tab B proposal must render in tab B'
        $tabAText = Get-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -MaxLines 80
        $tabAText | Should -Not -Match $cardRegex -Because 'tab B capability must not render any workspace card in tab A'
        $tabAText | Should -Not -Match ([regex]::Escape($markerB)) -Because 'tab B capability must not route its marker into tab A'

        $routeLogB = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $routeB = [regex]::Matches(
            $routeLogB,
            'routing MCP request to owning Helper.*helper_id=HelperId\((?<helper>\d+)\)\s+session_id=(?<session>interaction-\d+-\d+)'
        ) | Select-Object -Last 1
        $routeB | Should -Not -BeNullOrEmpty -Because 'the master must log tab B session-to-helper routing'
        $routeB.Groups['session'].Value | Should -Not -Be $routeA.Groups['session'].Value
        $routeB.Groups['helper'].Value | Should -Not -Be $routeA.Groups['helper'].Value
    }
}
