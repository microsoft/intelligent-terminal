#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #447: a Settings-selected OpenAI-compatible provider reaches the real
# Copilot ACP process, and leaving BYOK restores its native cloud catalog.
#
#   Invoke-Pester test/e2e/tests/Feature.ByokProvider.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: BYOK provider lifecycle' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-OpenAIChatServer.ps1')).Path
    }
    BeforeEach {
        $portProbe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
        $portProbe.Start()
        $script:port = ([System.Net.IPEndPoint]$portProbe.LocalEndpoint).Port
        $portProbe.Stop()

        $script:requestLog = Join-Path $env:TEMP "ite2e-byok-$([guid]::NewGuid().ToString('N')).log"
        $script:fixtureProcess = Start-Process pwsh -ArgumentList @(
            '-NoProfile',
            '-File', "`"$script:fixture`"",
            '-Port', $script:port,
            '-LogPath', "`"$script:requestLog`""
        ) -WindowStyle Hidden -PassThru
        Wait-Until -TimeoutSec 10 -Because 'the local OpenAI-compatible fixture to listen' -Condition {
            (Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue) -match '^READY\|'
        } | Out-Null

        $provider = @{
            id = 'ite2e-provider'
            name = 'ItE2E Provider'
            baseUrl = "http://127.0.0.1:$script:port/v1"
            apiContract = 'openai-compatible'
            location = 'auto'
            apiKeyRequired = $false
            models = @(@{ id = 'ite2e-model'; name = 'ItE2E Model' })
        }
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            acpModel = ''
            customModelSelection = 'custom:ite2e-provider:ite2e-model'
            customModelProviders = @($provider)
        }
        Initialize-LogOffsets -App $script:app | Out-Null
        Open-AgentPane -App $script:app | Out-Null
        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' -Pattern 'ite2e-model \(BYOK\).*state.:.connected' -TimeoutSec 60
    }
    AfterEach {
        if ($script:app) {
            Stop-Terminal -App $script:app
            $script:app = $null
        }
        if ($script:fixtureProcess -and -not $script:fixtureProcess.HasExited) {
            Stop-Process -Id $script:fixtureProcess.Id -Force
            $script:fixtureProcess.WaitForExit()
        }
        $script:fixtureProcess = $null
        if ($script:requestLog -and (Test-Path -LiteralPath $script:requestLog)) {
            Remove-Item -LiteralPath $script:requestLog -Force
        }
    }

    It 'BYOK provider serves the selected model end to end' {
        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'
        $picker = Get-AgentPaneText -App $script:app -MaxLines 40
        $picker | Should -Match '(?m)^\s*[│║|]\s*>\s+ite2e-model \(BYOK\)\s*[│║|]\s*$'
        $picker | Should -Not -Match '(?m)^\s*[│║|]\s+(Auto|GPT-|Claude)'
        Send-AgentKey -App $script:app -Key Escape | Out-Null

        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text 'Return the fixture response.' | Out-Null
        $request = Wait-Until -TimeoutSec 30 -Because 'Copilot to call the selected local provider' -Condition {
            $line = Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -like 'REQUEST|*' } |
                Select-Object -Last 1
            if ($line) {
                $line.Substring(8) | ConvertFrom-Json
            }
        }
        $request.path | Should -Be '/v1/chat/completions'
        ($request.body | ConvertFrom-Json).model | Should -Be 'ite2e-model'
        Assert-AgentPaneText -App $script:app -Pattern 'BYOK fixture response' -TimeoutSec 30
    }

    It 'Leaving BYOK restarts and restores cloud models' {
        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'
        Assert-AgentPaneText -App $script:app -Pattern 'ite2e-model \(BYOK\)' -TimeoutSec 10
        Send-AgentKey -App $script:app -Key Escape | Out-Null

        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtSetting -App $script:app -Key 'customModelSelection' -Value '' | Out-Null
        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' -Pattern '_RebuildAgentStack: agent settings changed, rebuilding' -TimeoutSec 20
        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' -Pattern 'state.:.connected.*available_models.*id.:.auto' -TimeoutSec 60

        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'
        $picker = Get-AgentPaneText -App $script:app -MaxLines 50
        $picker | Should -Match '(?m)^\s*[│║|]\s*>?\s*Auto\s*[│║|]\s*$'
        $picker | Should -Not -Match 'BYOK'
    }
}
