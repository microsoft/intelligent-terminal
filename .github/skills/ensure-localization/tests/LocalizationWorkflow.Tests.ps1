#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Localization workflow contracts' -Tag 'Unit' {
    BeforeAll {
        $script:workflowRoot = Join-Path $PSScriptRoot '..\..\..\workflows'
        $script:repairPath = Join-Path $script:workflowRoot 'ensure-localization.md'
        $script:guidePath = Join-Path $script:workflowRoot 'ensure-localizationguide-forkedrepo.md'

        function Initialize-SnapshotRepo {
            param([Parameter(Mandatory)][string]$Path)

            [IO.Directory]::CreateDirectory($Path) | Out-Null
            & git -C $Path init --quiet --initial-branch=main
            if ($LASTEXITCODE -ne 0) { throw 'Cannot initialize snapshot fixture.' }
            & git -C $Path config user.name 'Copilot Tests'
            if ($LASTEXITCODE -ne 0) { throw 'Cannot configure fixture author.' }
            & git -C $Path config user.email 'copilot-tests@example.test'
            if ($LASTEXITCODE -ne 0) { throw 'Cannot configure fixture email.' }
            $resource = Join-Path $Path 'src\cascadia\TerminalApp\Resources\fr-FR\Resources.resw'
            [IO.Directory]::CreateDirectory((Split-Path $resource -Parent)) | Out-Null
            [IO.File]::WriteAllText($resource, '<root />', [Text.UTF8Encoding]::new($false))
            & git -C $Path add .
            if ($LASTEXITCODE -ne 0) { throw 'Cannot stage snapshot fixture.' }
            & git -C $Path commit --quiet -m baseline
            if ($LASTEXITCODE -ne 0) { throw 'Cannot commit snapshot fixture.' }
            return (& git -C $Path rev-parse HEAD).Trim()
        }

        function Invoke-GuideSnapshots {
            param(
                [Parameter(Mandatory)][string]$Repository,
                [Parameter(Mandatory)][string]$Base,
                [Parameter(Mandatory)][string]$Head,
                [Parameter(Mandatory)][string]$Destination
            )

            $workflow = Get-Content -LiteralPath $script:guidePath -Raw
            $match = [regex]::Match(
                $workflow,
                '(?s)- name: Prepare immutable localization snapshots.*?run: \|\r?\n(?<script>.*?)\r?\n\s*post-steps:'
            )
            if (-not $match.Success) { throw 'Cannot locate the native snapshot step.' }
            $step = [scriptblock]::Create(($match.Groups['script'].Value -replace '(?m)^      ', ''))
            $names = @('COMPARISON_BASE_SHA', 'EXPECTED_HEAD_SHA', 'LOCALIZATION_SNAPSHOT_ROOT')
            $original = @{}
            foreach ($name in $names) { $original[$name] = [Environment]::GetEnvironmentVariable($name) }
            Push-Location $Repository
            try {
                $env:COMPARISON_BASE_SHA = $Base
                $env:EXPECTED_HEAD_SHA = $Head
                $env:LOCALIZATION_SNAPSHOT_ROOT = $Destination
                & $step
            } finally {
                Pop-Location
                foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $original[$name]) }
            }
        }

        function Invoke-WorkerFreshnessCheck {
            param(
                [Parameter(Mandatory)][string]$WorkflowPath,
                [Parameter(Mandatory)][string]$CurrentHead,
                [int]$ApiExitCode = 0
            )

            $workflow = Get-Content -LiteralPath $WorkflowPath -Raw
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

    It 'does not require an aggregate checker report in either source or compiled worker' {
        $paths = @(
            $script:repairPath
            $script:guidePath
            (Join-Path $script:workflowRoot 'ensure-localization.lock.yml')
            (Join-Path $script:workflowRoot 'ensure-localizationguide-forkedrepo.lock.yml')
            (Join-Path $script:workflowRoot 'shared\localization-summary.md')
            (Join-Path $PSScriptRoot '..\SKILL.md')
        )
        foreach ($path in $paths) {
            $content = Get-Content -LiteralPath $path -Raw
            $content | Should -Not -Match 'localization-final-checks|Validate final localization checker report|Upload final localization checker report'
        }
    }

    It 'keeps actual checks, agent review, and native publication restrictions' {
        $repair = Get-Content -LiteralPath $script:repairPath -Raw
        $guide = Get-Content -LiteralPath $script:guidePath -Raw
        $lock = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'ensure-localization.lock.yml') -Raw
        foreach ($workflow in @($repair, $guide)) {
            $workflow | Should -Match '\.github/skills/ensure-localization/SKILL\.md'
            $workflow | Should -Match 'Do not create an aggregate checker-report file'
            $workflow | Should -Match 'appropriate native incomplete outcome'
            $workflow | Should -Match 'shared/localization-summary\.md'
            $workflow | Should -Match 'needs\.agent\.result == ''success'''
        }
        $repair | Should -Match 'independent review'
        $repair | Should -Match 'explicit `PASS` before requesting any branch write'
        $repair | Should -Match '(?s)allowed-files:.*src/cascadia/\*\*/Resources/\*\.resw.*tools/wta/locales/\*\.yml'
        $repair | Should -Match 'protected-files: blocked'
        $repair | Should -Match 'fallback-as-pull-request: false'
        $lock | Should -Match '(?ms)safe_outputs:.*?permissions:\s+contents:\s+write\s+issues:\s+write\s+pull-requests:\s+read'
        $guide | Should -Match 'edit: false'
        $guide | Should -Match 'never check out or execute fork'
        $guide | Should -Match 'Test-PseudoLocale'
        $guide | Should -Match '(?s)add-comment:.*max: 1'
        $guide | Should -Not -Match 'push-to-pull-request-branch:'
    }

    It 'still rejects stale or unreadable PR heads before either worker publishes' {
        foreach ($path in @($script:repairPath, $script:guidePath)) {
            { Invoke-WorkerFreshnessCheck -WorkflowPath $path -CurrentHead ('a' * 40) } | Should -Not -Throw
            { Invoke-WorkerFreshnessCheck -WorkflowPath $path -CurrentHead ('b' * 40) } | Should -Throw '*Stale localization output rejected*'
            { Invoke-WorkerFreshnessCheck -WorkflowPath $path -CurrentHead 'not-a-sha' } | Should -Throw '*invalid current head SHA*'
            { Invoke-WorkerFreshnessCheck -WorkflowPath $path -CurrentHead ('a' * 40) -ApiExitCode 1 } | Should -Throw '*Failed to read the current head SHA*'
        }
    }

    It 'prepares exact immutable resource bytes without exporting fork scripts or attributes' {
        $repo = Join-Path $TestDrive 'snapshot-repo'
        $base = Initialize-SnapshotRepo -Path $repo
        $relativePath = 'src\cascadia\TerminalApp\Resources\fr-FR\Resources.resw'
        $headBytes = [byte[]](@(0xEF, 0xBB, 0xBF) + [Text.Encoding]::UTF8.GetBytes('<root><data name="new"><value>value</value></data></root>'))
        [IO.File]::WriteAllBytes((Join-Path $repo $relativePath), $headBytes)
        [IO.File]::WriteAllText((Join-Path $repo '.gitattributes'), '*.resw export-ignore')
        [IO.File]::WriteAllText((Join-Path $repo 'src\cascadia\TerminalApp\Resources\do-not-run.ps1'), 'throw "fork code must not run"')
        & git -C $repo add .
        & git -C $repo commit --quiet -m head
        if ($LASTEXITCODE -ne 0) { throw 'Cannot create snapshot fixture head.' }
        $head = (& git -C $repo rev-parse HEAD).Trim()
        & git -C $repo checkout --quiet --detach $base
        if ($LASTEXITCODE -ne 0) { throw 'Cannot restore trusted fixture checkout.' }

        $destination = Join-Path $TestDrive 'snapshots'
        Invoke-GuideSnapshots -Repository $repo -Base $base -Head $head -Destination $destination

        [IO.File]::ReadAllText((Join-Path $destination "base\$relativePath")) | Should -Be '<root />'
        [Convert]::ToBase64String([IO.File]::ReadAllBytes((Join-Path $destination "head\$relativePath"))) |
            Should -Be ([Convert]::ToBase64String($headBytes))
        Test-Path -LiteralPath (Join-Path $destination 'head\src\cascadia\TerminalApp\Resources\do-not-run.ps1') | Should -BeFalse
        @(Get-ChildItem -LiteralPath $destination -Filter '*.zip').Count | Should -Be 0
    }

    It 'rejects non-immutable snapshot revisions before exporting files' {
        $repo = Join-Path $TestDrive 'invalid-snapshot-repo'
        $base = Initialize-SnapshotRepo -Path $repo
        {
            Invoke-GuideSnapshots -Repository $repo -Base 'main' -Head $base -Destination (Join-Path $TestDrive 'invalid-snapshots')
        } | Should -Throw '*Invalid base snapshot revision*'
    }
}
