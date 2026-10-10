#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:runner = Join-Path $PSScriptRoot '..\Invoke-ItE2EReport.ps1'

    function Invoke-ReportFixture {
        param(
            [Parameter(Mandatory)][string]$Body,
            [string]$Tag,
            [switch]$GenerateReport,
            [switch]$ObstructReport,
            [switch]$SeedReport,
            [switch]$SeedPriorArtifacts,
            [switch]$FailGenerator,
            [switch]$FailPester,
            [switch]$NoPesterResult,
            [switch]$IsolatedDevProof,
            [switch]$UpdateReport,
            [switch]$RequireNoSkips,
            [string[]]$AdditionalArguments
        )

        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $out = Join-Path $root 'results'
        New-Item -ItemType Directory -Path $out -Force | Out-Null
        $testFile = Join-Path $root 'Example.Tests.ps1'
        Set-Content -LiteralPath $testFile -Value $Body -Encoding utf8
        if ($ObstructReport) {
            New-Item -ItemType Directory -Path (Join-Path $out 'release-report.md') | Out-Null
        }
        if ($UpdateReport -or $SeedReport -or $SeedPriorArtifacts) {
            @(
                '# Release Report'
                '> **Automated: 1 passed, 0 failed. Manual: 0 item(s) left for you.** (total 1)'
                '- [x] `C349` **Sidebar startup snapshots preserve the consolidated launch contract:** prior result'
            ) | Set-Content -LiteralPath (Join-Path $out 'release-report.md')
        }

        $runner = $script:runner
        if ($FailGenerator -or $FailPester -or $NoPesterResult -or $IsolatedDevProof) {
            $runnerDir = Join-Path $root 'runner'
            New-Item -ItemType Directory -Path $runnerDir | Out-Null
            $runner = Join-Path $runnerDir 'Invoke-ItE2EReport.ps1'
            if ($FailPester -or $NoPesterResult) {
                $source = Get-Content -LiteralPath $script:runner -Raw
                $invocation = '$pesterOutput = @(Invoke-Pester -Configuration $cfg)'
                if (-not $source.Contains($invocation)) { throw 'Pester invocation not found in runner fixture.' }
                $stub = if ($FailPester) { "function Invoke-Pester { throw 'fixture Invoke-Pester failed' }" }
                    else { 'function Invoke-Pester { return $null }' }
                $source.Replace($invocation, "$stub`n$invocation") |
                    Set-Content -LiteralPath $runner -Encoding utf8
            }
            else { Copy-Item -LiteralPath $script:runner -Destination $runner }
            if ($FailGenerator) {
                foreach ($generator in @('New-ReleaseReport.ps1', 'Update-ReleaseReport.ps1')) {
                    "throw 'fixture report generator failed'" |
                        Set-Content -LiteralPath (Join-Path $runnerDir $generator)
                }
            }
            if ($IsolatedDevProof) {
                $pfn = 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr'
                $moduleDir = Join-Path $runnerDir 'ItE2E'
                New-Item -ItemType Directory -Path $moduleDir | Out-Null
                @(
                    '@{'
                    "RootModule = 'ItE2E.psm1'"
                    "ModuleVersion = '0.1.0'"
                    "GUID = '$([guid]::NewGuid())'"
                    "FunctionsToExport = @('Get-ItDevPackageFamilyName')"
                    '}'
                ) | Set-Content -LiteralPath (Join-Path $moduleDir 'ItE2E.psd1')
                "function Get-ItDevPackageFamilyName { '$pfn' }`nExport-ModuleMember -Function Get-ItDevPackageFamilyName" |
                    Set-Content -LiteralPath (Join-Path $moduleDir 'ItE2E.psm1')
                @'
param([string]$SourceRoot, [string]$ExpectedHead, [string]$RecipePath,
    [string]$MsixPath, [string]$PackageFamilyName)
if ($PackageFamilyName -ne '__PFN__') { throw 'wrong configured Dev family reached verifier' }
throw 'fixture verifier received configured isolated Dev family'
'@.Replace('__PFN__', $pfn) |
                    Set-Content -LiteralPath (Join-Path $runnerDir 'Verify-PackageProvenance.ps1')
            }
        }
        if ($FailGenerator -or $SeedPriorArtifacts -or $ObstructReport) {
            '<html><body>ALL PASSED</body></html>' |
                Set-Content -LiteralPath (Join-Path $out 'report.html')
            '# All tests passed' | Set-Content -LiteralPath (Join-Path $out 'summary.md')
        }
        if ($SeedPriorArtifacts) {
            '<test-run result="Passed" />' | Set-Content -LiteralPath (Join-Path $out 'results.xml')
        }

        $arguments = @('-NoProfile', '-File', $runner, '-Path', $testFile, '-OutDir', $out)
        if (-not $GenerateReport) { $arguments += '-SkipReleaseReport' }
        if ($UpdateReport) { $arguments += '-UpdateReport' }
        if ($Tag) { $arguments += @('-Tag', $Tag) }
        if ($RequireNoSkips) { $arguments += '-RequireNoSkips' }
        if ($AdditionalArguments) { $arguments += $AdditionalArguments }

        $log = Join-Path $root 'runner.log'
        & pwsh @arguments *> $log
        $exitCode = $LASTEXITCODE
        $html = Join-Path $out 'report.html'
        [pscustomobject]@{
            ExitCode = $exitCode
            Output = Get-Content -LiteralPath $log -Raw
            Html = if (Test-Path -LiteralPath $html) { Get-Content -LiteralPath $html -Raw } else { '' }
            Summary = if (Test-Path -LiteralPath (Join-Path $out 'summary.md') -PathType Leaf) {
                Get-Content -LiteralPath (Join-Path $out 'summary.md') -Raw
            } else { '' }
            ReleaseReport = if (Test-Path -LiteralPath (Join-Path $out 'release-report.md') -PathType Leaf) {
                Get-Content -LiteralPath (Join-Path $out 'release-report.md') -Raw
            } else { '' }
            ResultsXmlExists = Test-Path -LiteralPath (Join-Path $out 'results.xml') -PathType Leaf
        }
    }
}

Describe 'Report runner acceptance' -Tag 'Unit' {
    It 'does not claim success when every selected test skips' {
        $run = Invoke-ReportFixture -Body @"
Describe 'external prerequisite' {
    It 'unavailable' { Set-ItResult -Skipped -Because 'not installed' }
}
"@

        $run.Output | Should -Match 'Passed=0 Failed=0 Skipped=1'
        $run.ExitCode | Should -Not -Be 0
        $run.Html | Should -Not -Match 'ALL PASSED'
    }

    It 'does not claim success when no test is selected' {
        $run = Invoke-ReportFixture -Tag NoSuchTag -Body @"
Describe 'ordinary suite' -Tag Unit {
    It 'passes' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Html | Should -Not -Match 'ALL PASSED'
    }

    It 'does not swallow a failed release-report generation' {
        $run = Invoke-ReportFixture -GenerateReport -ObstructReport -Body @"
Describe 'ordinary suite' {
    It 'passes' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Not -Match 'release-report.md : SKIPPED'
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
    }

    It 'removes stale summaries even when a blocked release checklist cannot be written' {
        $run = Invoke-ReportFixture -GenerateReport -ObstructReport -Body @"
Describe 'cleanup failure' {
    It 'runs' { `$true | Should -BeTrue }
    AfterAll { throw 'fixture AfterAll failed' }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
    }

    It 'invalidates previous green artifacts when Pester <Mode>' -ForEach @(
        @{ Mode = 'throws'; FailPester = $true; NoPesterResult = $false }
        @{ Mode = 'returns no result'; FailPester = $false; NoPesterResult = $true }
    ) {
        $run = Invoke-ReportFixture -SeedPriorArtifacts -FailPester:$FailPester `
            -NoPesterResult:$NoPesterResult -Body @"
Describe 'never reached' {
    It 'passes' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match $(if ($FailPester) { 'fixture Invoke-Pester failed' }
            else { 'Pester did not return a test result object' })
        $run.ReleaseReport | Should -BeNullOrEmpty
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
        $run.ResultsXmlExists | Should -BeFalse
    }

    It 'invalidates an earlier green checklist when a <Mode> generator fails' -ForEach @(
        @{ Mode = 'full'; UpdateReport = $false }
        @{ Mode = 'incremental'; UpdateReport = $true }
    ) {
        $run = Invoke-ReportFixture -GenerateReport -SeedReport -FailGenerator `
            -UpdateReport:$UpdateReport -Body @"
Describe 'ordinary suite' {
    It 'passes' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'fixture report generator failed'
        $run.ReleaseReport | Should -Match 'AUTOMATION FAILED.*report generation failed'
        $run.ReleaseReport | Should -Not -Match '(?m)^- \[x\]'
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
    }

    It 'withholds all checklist credit after a structural cleanup failure in <Mode> mode' -ForEach @(
        @{ Mode = 'full'; UpdateReport = $false }
        @{ Mode = 'incremental'; UpdateReport = $true }
    ) {
        $run = Invoke-ReportFixture -GenerateReport -UpdateReport:$UpdateReport -Body @"
Describe 'Sidebar startup snapshots preserve the consolidated launch contract' {
    It 'passes before cleanup fails' { `$true | Should -BeTrue }
    AfterAll { throw 'fixture AfterAll failed' }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'Passed=1 Failed=0'
        $run.ReleaseReport | Should -Match 'AUTOMATION FAILED.*setup or cleanup failed'
        $run.ReleaseReport | Should -Not -Match '(?m)^- \[x\]'
        $run.Output | Should -Match 'PRECISE FAILURES:'
        $run.Output | Should -Match 'fixture AfterAll failed'
        $run.Summary | Should -Match 'fixture AfterAll failed'
        $run.Html | Should -Match 'fixture AfterAll failed'
    }

    It 'does not preserve checklist credit when all tests skip in <Mode> mode' -ForEach @(
        @{ Mode = 'full'; UpdateReport = $false }
        @{ Mode = 'incremental'; UpdateReport = $true }
    ) {
        $run = Invoke-ReportFixture -GenerateReport -UpdateReport:$UpdateReport -Body @"
Describe 'skipped suite' {
    It 'has an unavailable prerequisite' { Set-ItResult -Skipped -Because 'not installed' }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'Passed=0 Failed=0 Skipped=1'
        $run.ReleaseReport | Should -Match 'AUTOMATION FAILED.*No tests passed'
        $run.ReleaseReport | Should -Not -Match '(?m)^- \[x\]'
    }

    It 'does not preserve earlier checklist ticks when an incremental run selects no tests' {
        $run = Invoke-ReportFixture -GenerateReport -UpdateReport -Tag NoSuchTag -Body @"
Describe 'ordinary suite' -Tag Unit {
    It 'passes if selected' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.ReleaseReport | Should -Match 'AUTOMATION FAILED.*No tests were selected'
        $run.ReleaseReport | Should -Not -Match '(?m)^- \[x\]'
    }

    It 'shows a failed Pester container in the summary, HTML and console' {
        $run = Invoke-ReportFixture -GenerateReport -Body @"
BeforeDiscovery { throw 'fixture discovery failed' }
Describe 'unreachable suite' {
    It 'never executes' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'PRECISE FAILURES:'
        $run.Output | Should -Match 'fixture discovery failed'
        $run.Summary | Should -Match 'fixture discovery failed'
        $run.Html | Should -Match 'fixture discovery failed'
        $run.ReleaseReport | Should -Not -Match '(?m)^- \[x\]'
    }

    It 'distinguishes an allowed mixed skip from all tests passed' {
        $run = Invoke-ReportFixture -Body @"
Describe 'conditional suite' {
    It 'passes' { `$true | Should -BeTrue }
    It 'skips' { Set-ItResult -Skipped -Because 'external dependency' }
}
"@

        $run.ExitCode | Should -Be 0
        $run.Html | Should -Match 'PASSED WITH SKIPS'
        $run.Html | Should -Not -Match 'ALL PASSED'
    }

    It 'can require zero skips for a strict PR run' {
        $run = Invoke-ReportFixture -RequireNoSkips -Body @"
Describe 'strict suite' {
    It 'passes' { `$true | Should -BeTrue }
    It 'skips' { Set-ItResult -Skipped -Because 'external dependency' }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Html | Should -Match 'UNEXPECTED SKIPS'
    }

    It 'still passes an ordinary fully executed test' {
        $run = Invoke-ReportFixture -Body @"
Describe 'ordinary suite' {
    It 'passes' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Be 0
        $run.Html | Should -Match 'ALL PASSED'
    }

    It 'refuses an incomplete package proof before executing any tests' {
        $run = Invoke-ReportFixture -AdditionalArguments @('-SourceRoot', $TestDrive) -Body @"
Describe 'must not execute' {
    It 'fails if reached' { `$true | Should -BeFalse }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'Provide -SourceRoot, -ExpectedHead, -RecipePath and -MsixPath together'
        $run.Html | Should -BeNullOrEmpty
    }

    It 'does not run tests or retain old reports for an explicitly empty proof input' {
        $run = Invoke-ReportFixture -SeedPriorArtifacts -AdditionalArguments @('-SourceRoot', '') -Body @"
Describe 'must not execute' {
    It 'would pass' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'Provide -SourceRoot, -ExpectedHead, -RecipePath and -MsixPath together'
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
        $run.ReleaseReport | Should -BeNullOrEmpty
        $run.ResultsXmlExists | Should -BeFalse
    }

    It 'clears old reports when the expected source revision is malformed' {
        $run = Invoke-ReportFixture -SeedPriorArtifacts -AdditionalArguments @(
            '-SourceRoot', $TestDrive,
            '-ExpectedHead', 'bad',
            '-RecipePath', (Join-Path $TestDrive 'unused.appxrecipe'),
            '-MsixPath', (Join-Path $TestDrive 'unused.msix')
        ) -Body @"
Describe 'must not execute' {
    It 'would pass' { `$true | Should -BeTrue }
}
"@

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'ExpectedHead'
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
        $run.ReleaseReport | Should -BeNullOrEmpty
        $run.ResultsXmlExists | Should -BeFalse
    }

    It 'runs package proof before Pester and rejects a different source head' {
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $root | Out-Null
        'recipe' | Set-Content -LiteralPath (Join-Path $root 'fixture.build.appxrecipe')
        'archive' | Set-Content -LiteralPath (Join-Path $root 'fixture.msix')
        & git -C $root init --quiet
        & git -C $root config user.name 'Offline report test'
        & git -C $root config user.email 'report-test@example.invalid'
        & git -C $root add .
        & git -C $root commit --quiet -m 'record fixture'
        $previousPackage = $env:ITE2E_PACKAGE
        try {
            $env:ITE2E_PACKAGE = 'Dev'
            $run = Invoke-ReportFixture -SeedPriorArtifacts -AdditionalArguments @(
                '-SourceRoot', $root,
                '-ExpectedHead', ('f' * 40),
                '-RecipePath', (Join-Path $root 'fixture.build.appxrecipe'),
                '-MsixPath', (Join-Path $root 'fixture.msix')
            ) -Body @"
Describe 'must not execute' {
    It 'fails if reached' { `$true | Should -BeFalse }
}
"@
            $run.ExitCode | Should -Not -Be 0
            $run.Output | Should -Match 'source HEAD'
            $run.Html | Should -BeNullOrEmpty
            $run.Summary | Should -BeNullOrEmpty
            $run.ReleaseReport | Should -BeNullOrEmpty
            $run.ResultsXmlExists | Should -BeFalse
        }
        finally {
            if ($null -eq $previousPackage) { Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_PACKAGE = $previousPackage }
        }
    }

    It 'routes <Selector> proof through the configured isolated Dev family' -ForEach @(
        @{ Selector = 'Dev' }
        @{ Selector = 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr' }
    ) {
        $previous = $env:ITE2E_PACKAGE
        try {
            $env:ITE2E_PACKAGE = $Selector
            $run = Invoke-ReportFixture -SeedPriorArtifacts -IsolatedDevProof -AdditionalArguments @(
                '-SourceRoot', $TestDrive, '-ExpectedHead', ('f' * 40),
                '-RecipePath', (Join-Path $TestDrive 'unused.appxrecipe'),
                '-MsixPath', (Join-Path $TestDrive 'unused.msix')
            ) -Body @"
Describe 'unreachable test' {
    It 'must not run' { `$true | Should -BeFalse }
}
"@
        }
        finally {
            if ($null -eq $previous) { Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_PACKAGE = $previous }
        }

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'fixture verifier received configured isolated Dev family'
        $run.Html | Should -BeNullOrEmpty
        $run.Summary | Should -BeNullOrEmpty
        $run.ReleaseReport | Should -BeNullOrEmpty
        $run.ResultsXmlExists | Should -BeFalse
    }

    It 'refuses Store proof even when its package is explicitly selected' {
        $previous = $env:ITE2E_PACKAGE
        try {
            $env:ITE2E_PACKAGE = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $run = Invoke-ReportFixture -SeedPriorArtifacts -AdditionalArguments @(
                '-SourceRoot', $TestDrive, '-ExpectedHead', ('f' * 40),
                '-RecipePath', (Join-Path $TestDrive 'unused.appxrecipe'),
                '-MsixPath', (Join-Path $TestDrive 'unused.msix')
            ) -Body @"
Describe 'unreachable test' {
    It 'must not run' { `$true | Should -BeFalse }
}
"@
        }
        finally {
            if ($null -eq $previous) { Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_PACKAGE = $previous }
        }

        $run.ExitCode | Should -Not -Be 0
        $run.Output | Should -Match 'configured Dev package'
        $run.Html | Should -BeNullOrEmpty
        $run.ReleaseReport | Should -BeNullOrEmpty
        $run.ResultsXmlExists | Should -BeFalse
    }
}
