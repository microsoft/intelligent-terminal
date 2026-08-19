#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #538: ACP config-option updates replace stale session/new model state.
#
# The fixture is a real ACP stdio child process. It returns Initial Model from
# session/new, then reports Effective Model through session/update. The test
# observes the final state through the deployed agent pane's /model picker.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentModelSync.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: ACP model synchronization' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpModelUpdateAgent.ps1')).Path
        $command = "pwsh -NoProfile -File $fixture"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:model-update-fixture'
            acpCustomCommand = $command
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 30 | Should -BeTrue -Because 'the ACP model-update fixture must connect'
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'ACP model updates refresh the active picker' {
        Start-Sleep -Seconds 1
        Clear-AgentInput -App $script:app | Out-Null
        Invoke-AgentMenuItem -App $script:app -Name '/model'

        $titleRe = Get-WtaLocalizedTextRegex -Key 'model_picker.title'
        if (-not $titleRe) { $titleRe = '(?i)Select model' }
        Assert-AgentPaneText -App $script:app -Pattern $titleRe -TimeoutSec 10

        $picker = Get-AgentPaneText -App $script:app -MaxLines 40
        $picker | Should -Match '(?m)^\s*[│║|]\s*>\s+Effective Model\s*[│║|]\s*$' -Because 'the config_option_update model must replace the stale session/new selection'
        $picker | Should -Match '(?m)^\s*[│║|]\s+Initial Model\s*[│║|]\s*$' -Because 'the initial model must remain available without staying selected'
        $picker | Should -Not -Match '(?m)^\s*[│║|]\s*>\s+Initial Model\s*[│║|]\s*$'
    }
}
