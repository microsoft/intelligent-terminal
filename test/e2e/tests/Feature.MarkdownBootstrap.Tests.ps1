#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature: Markdown rendering helper bootstrap' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
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
}