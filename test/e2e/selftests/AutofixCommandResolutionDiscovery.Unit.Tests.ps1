#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Autofix command-resolution suite package discovery' -Tag 'Unit' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:suite = (Resolve-Path (Join-Path $PSScriptRoot '..\tests\Feature.AutofixCommandResolution.Tests.ps1')).Path
        $script:pwsh = (Get-Process -Id $PID).Path

        function Invoke-DiscoveryProbe([string]$Selector, [switch]$DiscoveryOnly) {
            $artifactRoot = Join-Path $TestDrive 'must-not-create-artifacts'
            $probe = @"
`$env:ITE2E_PACKAGE = '$($Selector.Replace("'", "''"))'
`$env:ITE2E_ARTIFACT_ROOT = '$($artifactRoot.Replace("'", "''"))'
`$env:ITE2E_EXPECTED_WTA_SHA256 = 'discovery-only-do-not-launch'
`$cfg = New-PesterConfiguration
`$cfg.Run.Path = '$($script:suite.Replace("'", "''"))'
`$cfg.Run.PassThru = `$true
`$cfg.Run.SkipRun = `$$($DiscoveryOnly.IsPresent.ToString().ToLowerInvariant())
`$cfg.Output.Verbosity = 'None'
`$result = Invoke-Pester -Configuration `$cfg
@{
    total = `$result.TotalCount
    failed = `$result.FailedCount
    skipped = `$result.SkippedCount
    suiteSkipped = `$result.Containers[0].Blocks[0].Skip
    errors = @(`$result.Containers | ForEach-Object { `$_.Blocks.ErrorRecord.Exception.Message })
} | ConvertTo-Json -Compress
"@
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
            $result = Invoke-Native -FilePath $script:pwsh -Arguments @('-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) -TimeoutSec 30
            $result.TimedOut | Should -BeFalse
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            Test-Path -LiteralPath $artifactRoot | Should -BeFalse -Because 'package discovery must not reach fixture setup or app launch'
            $result.StdOut | ConvertFrom-Json
        }
    }

    It 'skips all diagnostics cases for <Selector> without setup' -TestCases @(
        @{ Selector = 'Store' },
        @{ Selector = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe' }
    ) {
        $result = Invoke-DiscoveryProbe -Selector $Selector
        $result.total | Should -BeGreaterThan 0
        $result.failed | Should -Be 0
        $result.skipped | Should -Be $result.total
        $result.suiteSkipped | Should -BeTrue
    }

    It 'keeps Dev cases enabled without launching them during discovery' {
        $result = Invoke-DiscoveryProbe -Selector Dev -DiscoveryOnly
        $result.total | Should -BeGreaterThan 0
        $result.failed | Should -Be 0
        $result.skipped | Should -Be 0
        $result.suiteSkipped | Should -BeFalse
    }

    It 'fails rather than skips the <Label> package selection' -TestCases @(
        @{ Label = 'missing'; Selector = ''; ErrorPattern = '*Choose the live integration-test package explicitly*' },
        @{ Label = 'Auto'; Selector = 'Auto'; ErrorPattern = "*'Auto' is not allowed*" }
    ) {
        $result = Invoke-DiscoveryProbe -Selector $Selector
        $result.failed | Should -BeGreaterThan 0
        $result.skipped | Should -Be 0
        ($result.errors -join "`n") | Should -BeLike $ErrorPattern
    }
}
