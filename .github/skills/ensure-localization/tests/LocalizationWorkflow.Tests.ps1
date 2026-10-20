#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Localization workflow contracts' -Tag 'Unit' {
    BeforeAll {
        $script:workflowRoot = Join-Path $PSScriptRoot '..\..\..\workflows'
        $script:repairPath = Join-Path $script:workflowRoot 'ensure-localization.md'

        function Invoke-WorkerFreshnessCheck {
            param(
                [Parameter(Mandatory)][string]$CurrentHead,
                [int]$ApiExitCode = 0
            )

            $workflow = Get-Content -LiteralPath $script:repairPath -Raw
            $match = [regex]::Match(
                $workflow,
                '(?s)- name: Reject stale worker output before publication.*?run: \|\r?\n(?<script>.*?)\r?\ntimeout-minutes:'
            )
            if (-not $match.Success) { throw 'Cannot locate the native freshness check.' }
            $step = [scriptblock]::Create(($match.Groups['script'].Value -replace '(?m)^      ', ''))
            function gh {
                $global:LASTEXITCODE = $ApiExitCode
                return $CurrentHead
            }
            $names = @('PR_NUMBER', 'EXPECTED_HEAD_SHA', 'REPOSITORY')
            $original = @{}
            foreach ($name in $names) { $original[$name] = [Environment]::GetEnvironmentVariable($name) }
            $previousExitCode = $global:LASTEXITCODE
            try {
                $env:PR_NUMBER = '1'
                $env:EXPECTED_HEAD_SHA = 'a' * 40
                $env:REPOSITORY = 'test/repo'
                & $step
            } finally {
                foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $original[$name]) }
                $global:LASTEXITCODE = $previousExitCode
            }
        }
    }

    It 'does not require an aggregate checker report in the source or compiled worker' {
        foreach ($path in @(
            $script:repairPath
            (Join-Path $script:workflowRoot 'ensure-localization.lock.yml')
            (Join-Path $script:workflowRoot 'shared\localization-summary.md')
            (Join-Path $PSScriptRoot '..\SKILL.md')
        )) {
            Get-Content -LiteralPath $path -Raw |
                Should -Not -Match 'localization-final-checks|Validate final localization checker report|Upload final localization checker report'
        }
    }

    It 'keeps actual checks, independent review, and native publication restrictions' {
        $repair = Get-Content -LiteralPath $script:repairPath -Raw
        $lock = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'ensure-localization.lock.yml') -Raw
        $repair | Should -Match '\.github/skills/ensure-localization/SKILL\.md'
        $repair | Should -Match 'Do not create an aggregate checker-report file'
        $repair | Should -Match 'appropriate native incomplete outcome'
        $repair | Should -Match 'shared/localization-summary\.md'
        $repair | Should -Match 'independent review'
        $repair | Should -Match 'explicit `PASS` before requesting any branch write'
        $repair | Should -Match '(?s)allowed-files:.*src/cascadia/\*\*/Resources/\*\.resw.*tools/wta/locales/\*\.yml'
        $repair | Should -Match 'protected-files: blocked'
        $repair | Should -Match 'fallback-as-pull-request: false'
        $repair | Should -Match 'needs\.agent\.result == ''success'''
        $lock | Should -Match '(?ms)safe_outputs:.*?permissions:\s+contents:\s+write\s+issues:\s+write\s+pull-requests:\s+read'
    }

    It 'retains same-repository dispatch and does not restore fork workers' {
        $controller = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'ensure-localization-controller.yml') -Raw
        $controller | Should -Match '(?m)^  pull_request:'
        $controller | Should -Not -Match 'pull_request_target:|ensure-localizationguide-forkedrepo|SAME_REPO|sameRepo'
        $controller | Should -Match 'if: github\.event\.pull_request\.head\.repo\.id == github\.repository_id'
        $controller | Should -Match 'const worker = "ensure-localization\.lock\.yml";'
        $repair = Get-Content -LiteralPath $script:repairPath -Raw
        $lock = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'ensure-localization.lock.yml') -Raw
        $repair | Should -Not -Match 'same_repo'
        $lock | Should -Not -Match 'same_repo'
        Test-Path -LiteralPath (Join-Path $script:workflowRoot 'ensure-localizationguide-forkedrepo.md') | Should -BeFalse
        Test-Path -LiteralPath (Join-Path $script:workflowRoot 'ensure-localizationguide-forkedrepo.lock.yml') | Should -BeFalse
    }

    It 'still rejects stale or unreadable PR heads before publication' {
        { Invoke-WorkerFreshnessCheck -CurrentHead ('a' * 40) } | Should -Not -Throw
        { Invoke-WorkerFreshnessCheck -CurrentHead ('b' * 40) } | Should -Throw '*Stale localization output rejected*'
        { Invoke-WorkerFreshnessCheck -CurrentHead 'not-a-sha' } | Should -Throw '*invalid current head SHA*'
        { Invoke-WorkerFreshnessCheck -CurrentHead ('a' * 40) -ApiExitCode 1 } | Should -Throw '*Failed to read the current head SHA*'
    }

    It 'retains deletion-only historical evidence without expanding localized-only scope' {
        $repair = Get-Content -LiteralPath $script:repairPath -Raw
        $repair | Should -Match 'no English-derived scope'
        $repair | Should -Match 'historical evidence only'
        $repair | Should -Match 'pre-deletion snapshots'
        $repair | Should -Match 'Localized-only edits or deletions do not independently create'
    }
}
