#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #554: slash-command model picks hot-apply, while Settings model changes
# rebuild the shared agent stack.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentModelLifecycle.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent model switching lifecycle' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpModelSwitchAgent.ps1')).Path
    }
    BeforeEach {
        $script:requestLog = Join-Path $env:TEMP "ite2e-model-switch-$([guid]::NewGuid().ToString('N')).log"
        $command = "pwsh -NoProfile -File $script:fixture -LogPath $script:requestLog"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:model-switch-fixture'
            acpCustomCommand = $command
            acpModel = ''
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 30 | Should -BeTrue -Because 'the ACP model-switch fixture must connect'
    }
    AfterEach {
        if ($script:app) {
            Stop-Terminal -App $script:app
            $script:app = $null
        }
        if ($script:requestLog -and (Test-Path -LiteralPath $script:requestLog)) {
            Remove-Item -LiteralPath $script:requestLog -Force
        }
    }

    It '/model hot-applies without restarting the agent' {
        $beforeSession = (Wait-Until -TimeoutSec 10 -Because 'initial agent pane session' -Condition {
            Get-AgentPaneSession -App $script:app
        }).PaneSessionId
        $beforePid = ((Get-Content -LiteralPath $script:requestLog) |
            Where-Object { $_ -match '\|initialize\|' } |
            Select-Object -First 1).Split('|')[0]

        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'
        Assert-AgentPaneText -App $script:app -Pattern 'Initial Model' -TimeoutSec 10
        Send-AgentKey -App $script:app -Key Down | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null

        $applied = Test-Until -TimeoutSec 15 -Condition {
            (Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue) -match
                '\|session/set_config_option\|.*effective-model'
        }
        $applied | Should -BeTrue -Because '/model must hot-apply the selected model through ACP'

        $afterSession = (Get-AgentPaneSession -App $script:app).PaneSessionId
        $afterPids = (Get-Content -LiteralPath $script:requestLog) |
            Where-Object { $_ -match '\|initialize\|' } |
            ForEach-Object { $_.Split('|')[0] } |
            Select-Object -Unique
        $afterSession | Should -Be $beforeSession -Because '/model must preserve the existing helper session'
        @($afterPids) | Should -HaveCount 1
        @($afterPids)[0] | Should -Be $beforePid
    }

    It 'Settings model changes restart and reconnect the agent' {
        Stop-Terminal -App $script:app
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            acpModel = ''
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'Copilot must connect before changing its configured model'

        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'
        $picker = Get-AgentPaneText -App $script:app -MaxLines 50
        $targetRows = [regex]::Matches(
            $picker,
            '(?m)^\s*[│║|]\s{2}(?<name>\S.*?)\s*[│║|]\s*$'
        )
        $targetRow = $targetRows |
            Where-Object { $_.Groups['name'].Value.Trim() -ne 'Auto' } |
            Select-Object -First 1
        if (-not $targetRow) {
            Set-ItResult -Skipped -Because 'the installed Copilot agent did not advertise multiple selectable cloud models'
            return
        }
        $targetName = $targetRow.Groups['name'].Value.Trim()
        $targetId = (($targetName.ToLowerInvariant() -replace '[^a-z0-9.]+', '-').Trim('-'))
        Send-AgentKey -App $script:app -Key Escape | Out-Null
        Clear-AgentInput -App $script:app | Out-Null

        $beforeSession = (Get-AgentPaneSession -App $script:app).PaneSessionId
        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtSetting -App $script:app -Key 'acpModel' -Value $targetId | Out-Null

        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' -Pattern '_RebuildAgentStack: agent settings changed, rebuilding' -TimeoutSec 20
        $sessionChanged = Test-Until -TimeoutSec 30 -IntervalSec 1 -Condition {
            $session = Get-AgentPaneSession -App $script:app
            $session -and $session.PaneSessionId -ne $beforeSession
        }
        $sessionChanged | Should -BeTrue -Because 'the Settings model change must replace the helper session'
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the rebuilt agent stack must reconnect'

        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'
        $selectedModel = [regex]::Escape($targetName)
        $newModelApplied = Test-Until -TimeoutSec 15 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 40) -match
                "(?m)^\s*[│║|]\s*>\s+$selectedModel\s*[│║|]\s*$"
        }
        $newModelApplied | Should -BeTrue -Because 'the rebuilt stack must select the newly configured model'
    }
}
