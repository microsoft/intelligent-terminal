#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature: Markdown rendering helper bootstrap' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            defaultProfile = '{574e775e-4f2a-5b96-ac1e-a2962a402336}'
            renderAgentMarkdown = $false
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'passes the disabled Markdown mode only to helpers' {
        $descendantIds = @(Get-DescendantWtaIds -RootPid $script:app.Pid)
        (Wait-Until -TimeoutSec 20 -Because 'the pre-warmed helper and shared master to start' -Condition {
            $descendantIds = @(Get-DescendantWtaIds -RootPid $script:app.Pid)
            $descendantIds.Count -ge 2
        }) | Should -BeTrue

        $processes = @(Get-CimInstance Win32_Process -ErrorAction Stop |
                Where-Object { $descendantIds -contains [int]$_.ProcessId })
        $helpers = @($processes | Where-Object { $_.CommandLine -match '(?:^|\s)--connect-master(?:\s|$)' })
        $masters = @($processes | Where-Object { $_.CommandLine -match '(?:^|\s)--master(?:\s|$)' })

        $helpers.Count | Should -BeGreaterThan 0 -Because 'each eligible tab pre-warms a helper'
        $masters.Count | Should -Be 1 -Because 'the window shares one wta-master'
        foreach ($helper in $helpers) {
            $helper.CommandLine | Should -Match '(?:^|\s)--no-agent-markdown(?:\s|$)'
        }
        $masters[0].CommandLine | Should -Not -Match '(?:^|\s)--no-agent-markdown(?:\s|$)'
    }

    It 'hot-updates Markdown mode without restarting the helper or master' {
        (Get-ActivePane -App $script:app).shell |
            Should -Be 'pwsh' -Because 'Markdown integration tests require PowerShell 7'

        $beforeIds = @(Get-DescendantWtaIds -RootPid $script:app.Pid | Sort-Object)
        $beforeProcesses = @(Get-CimInstance Win32_Process -ErrorAction Stop |
                Where-Object { $beforeIds -contains [int]$_.ProcessId })
        $beforeHelper = $beforeProcesses |
            Where-Object { $_.CommandLine -match '(?:^|\s)--connect-master(?:\s|$)' } |
            Select-Object -First 1
        $beforeMaster = $beforeProcesses |
            Where-Object { $_.CommandLine -match '(?:^|\s)--master(?:\s|$)' } |
            Select-Object -First 1
        $beforeHelper | Should -Not -BeNullOrEmpty
        $beforeMaster | Should -Not -BeNullOrEmpty

        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtSetting -App $script:app -Key 'renderAgentMarkdown' -Value $true | Out-Null
        Assert-Log -App $script:app -Name 'wta-main_helper-*.log' `
            -Pattern 'agent Markdown rendering hot-reloaded from settings change' -TimeoutSec 15
        @(Get-DescendantWtaIds -RootPid $script:app.Pid | Sort-Object) |
            Should -Be $beforeIds -Because 'enabling Markdown must preserve helper and master identity'

        $currentHelper = Get-CimInstance Win32_Process -Filter "ProcessId = $($beforeHelper.ProcessId)" -ErrorAction Stop
        $currentHelper.CommandLine | Should -Be $beforeHelper.CommandLine
        $currentHelper.CommandLine | Should -Match '(?:^|\s)--no-agent-markdown(?:\s|$)'

        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtSetting -App $script:app -Key 'renderAgentMarkdown' -Value $false | Out-Null
        Assert-Log -App $script:app -Name 'wta-main_helper-*.log' `
            -Pattern 'agent Markdown rendering hot-reloaded from settings change' -TimeoutSec 15
        @(Get-DescendantWtaIds -RootPid $script:app.Pid | Sort-Object) |
            Should -Be $beforeIds -Because 'disabling Markdown must preserve helper and master identity'
    }
}
