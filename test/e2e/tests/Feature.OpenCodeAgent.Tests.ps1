#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #458: OpenCode is a first-class built-in ACP agent. Exercise its native
# `opencode acp` path through the deployed agent pane rather than only checking
# registry/command construction.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = [bool]((Get-Command opencode -ErrorAction SilentlyContinue) -and (Test-WinAppAvailable))
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature: OpenCode built-in ACP agent' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:openCodeStatus = Get-AgentCliStatus -Agent opencode -TimeoutSec 60
    }

    It 'OpenCode built-in agent connects through ACP and answers' {
        if ($script:openCodeStatus -ne 'authed') {
            Set-ItResult -Skipped -Because "OpenCode cannot answer non-interactively ($script:openCodeStatus)"
            return
        }

        $app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'opencode' }
        try {
            $shellPane = Get-ActivePane -App $app
            Open-AgentPane -App $app | Out-Null
            $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId

            Wait-AgentReady -App $app -PaneSessionId $agentPane -TimeoutSec 90 |
                Should -BeTrue -Because 'the built-in OpenCode entry must launch its native ACP server'
            Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text `
                'What is 3 plus 4? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $app -PaneSessionId $agentPane -Pattern '\b7\b' -TimeoutSec 120
        }
        finally {
            if ($app) {
                Stop-Terminal -App $app
            }
        }
    }
}
