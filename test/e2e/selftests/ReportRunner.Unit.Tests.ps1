#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:runner = Join-Path $PSScriptRoot '..\Invoke-ItE2EReport.ps1'

    function Invoke-ReportFixture {
        param(
            [Parameter(Mandatory)][string]$Body,
            [string]$Tag,
            [switch]$GenerateReport,
            [switch]$ObstructReport,
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
        if ($UpdateReport) {
            @(
                '# Release Report'
                '> **Automated: 1 passed, 0 failed. Manual: 0 item(s) left for you.** (total 1)'
                '- [x] `C349` **Sidebar startup snapshots preserve the consolidated launch contract:** prior result'
            ) | Set-Content -LiteralPath (Join-Path $out 'release-report.md')
        }

        $arguments = @('-NoProfile', '-File', $script:runner, '-Path', $testFile, '-OutDir', $out)
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
            $run = Invoke-ReportFixture -AdditionalArguments @(
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
        }
        finally {
            if ($null -eq $previousPackage) { Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_PACKAGE = $previousPackage }
        }
    }
}
