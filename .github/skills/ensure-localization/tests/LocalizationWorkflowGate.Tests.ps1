#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Ensure localization repair workflow gate' -Tag 'Unit' {
    BeforeAll {
        $script:workflowPath = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localization.md')
        $script:guideWorkflowPath = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localizationguide-forkedrepo.md')
        $script:repairLockPath = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localization.lock.yml')

        function Get-RepairGateScript {
            param([switch]$Guide)

            $path = if ($Guide) { $script:guideWorkflowPath } else { $script:workflowPath }
            $workflow = Get-Content -LiteralPath $path -Raw
            $match = [regex]::Match(
                $workflow,
                "(?s)- name: Validate final localization checker report.*?node <<'NODE'\r?\n(?<script>.*?)\r?\n\s*NODE"
            )
            if (-not $match.Success) {
                throw 'Could not locate the repair gate Node.js script in ensure-localization.md.'
            }

            return $match.Groups['script'].Value
        }

        function New-PassReport {
            return [ordered]@{
                version = 1
                mode = 'repair'
                bundles = @(
                    [ordered]@{
                        check = 'Test-RequiredKeys'
                        status = 'PASS'
                        exitCode = 0
                        summary = 'pass'
                        results = @(
                            [ordered]@{
                                file = 'src/cascadia/TerminalApp/Resources/fr-FR/Resources.resw'
                                resource = 'sample'
                                status = 'PASS'
                                message = 'ok'
                            }
                        )
                    }
                )
            }
        }

        function New-AgentOutput {
            param([Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Types)

            return [ordered]@{
                items = @(
                    foreach ($type in $Types) {
                        [ordered]@{ type = $type }
                    }
                )
                errors = @()
            }
        }

        function Initialize-RepairGateRepo {
            param([Parameter(Mandatory)][string]$Path)

            [System.IO.Directory]::CreateDirectory($Path) | Out-Null
            $null = & git -C $Path init --quiet --initial-branch=main
            if ($LASTEXITCODE -ne 0) {
                throw 'Failed to initialize the gate test repository.'
            }

            $null = & git -C $Path config user.name 'Copilot Tests'
            if ($LASTEXITCODE -ne 0) {
                throw 'Failed to configure git user.name for the gate test repository.'
            }

            $null = & git -C $Path config user.email 'copilot-tests@example.test'
            if ($LASTEXITCODE -ne 0) {
                throw 'Failed to configure git user.email for the gate test repository.'
            }

            $resourcePath = Join-Path $Path 'src\cascadia\TerminalApp\Resources\fr-FR\Resources.resw'
            $resourceDirectory = Split-Path -Path $resourcePath -Parent
            [System.IO.Directory]::CreateDirectory($resourceDirectory) | Out-Null
            [System.IO.File]::WriteAllText($resourcePath, '<root />', [System.Text.UTF8Encoding]::new($false))

            $null = & git -C $Path add -- 'src/cascadia/TerminalApp/Resources/fr-FR/Resources.resw'
            if ($LASTEXITCODE -ne 0) {
                throw 'Failed to stage the gate test localization file.'
            }

            $null = & git -C $Path commit --quiet -m 'baseline'
            if ($LASTEXITCODE -ne 0) {
                throw 'Failed to create the gate test baseline commit.'
            }

            return (& git -C $Path rev-parse HEAD).Trim().ToLowerInvariant()
        }

        function Invoke-RepairGate {
            param(
                [Parameter(Mandatory)]$QueuedOutput,
                [bool]$CreateDirtyLocalizationChange = $false,
                [switch]$Guide,
                [switch]$Fixable
            )

            $caseRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
            $ghawRoot = Join-Path $caseRoot 'gh-aw'
            $agentRoot = Join-Path $ghawRoot 'agent'
            $repoRoot = Join-Path $caseRoot 'repo'
            [System.IO.Directory]::CreateDirectory($agentRoot) | Out-Null
            $head = Initialize-RepairGateRepo -Path $repoRoot

            $reportPath = Join-Path $agentRoot 'localization-final-checks.json'
            $queuedOutputPath = Join-Path $ghawRoot 'agent_output.json'
            $scriptPath = Join-Path $caseRoot 'repair-gate.js'

            $jsonEncoding = [System.Text.UTF8Encoding]::new($false)
            $report = New-PassReport
            if ($Guide) {
                $report.mode = 'guide'
            }
            if ($Fixable) {
                $report.bundles[0].status = 'FIXABLE'
                $report.bundles[0].exitCode = 20
                $report.bundles[0].results[0].status = 'FIXABLE'
            }
            if ($Guide) {
                $report = @($report.bundles)
            }
            [System.IO.File]::WriteAllText($reportPath, (ConvertTo-Json -InputObject $report -Compress -Depth 8), $jsonEncoding)
            [System.IO.File]::WriteAllText($queuedOutputPath, ($QueuedOutput | ConvertTo-Json -Compress -Depth 8), $jsonEncoding)

            if ($CreateDirtyLocalizationChange) {
                $resourcePath = Join-Path $repoRoot 'src\cascadia\TerminalApp\Resources\fr-FR\Resources.resw'
                [System.IO.File]::WriteAllText($resourcePath, '<root><data name="changed" /></root>', $jsonEncoding)
            }

            $scriptContent = Get-RepairGateScript -Guide:$Guide
            $scriptRootLiteral = (($ghawRoot -replace '\\', '/') | ConvertTo-Json -Compress)
            $scriptContent = $scriptContent -replace "const root = '/tmp/gh-aw';", "const root = $scriptRootLiteral;"
            [System.IO.File]::WriteAllText($scriptPath, $scriptContent, $jsonEncoding)

            Push-Location $repoRoot
            try {
                $env:LOCALIZATION_REPORT_MODE = if ($Guide) { 'guide' } else { 'repair' }
                $env:EXPECTED_HEAD_SHA = $head
                $output = & node $scriptPath 2>&1
                $exitCode = $LASTEXITCODE
            } finally {
                Pop-Location
                Remove-Item Env:LOCALIZATION_REPORT_MODE -ErrorAction SilentlyContinue
                Remove-Item Env:EXPECTED_HEAD_SHA -ErrorAction SilentlyContinue
            }

            return [pscustomobject]@{
                ExitCode = $exitCode
                Output = ($output | Out-String)
            }
        }

        function Invoke-GuideSnapshots {
            param(
                [Parameter(Mandatory)][string]$Repository,
                [Parameter(Mandatory)][string]$Base,
                [Parameter(Mandatory)][string]$Head,
                [Parameter(Mandatory)][string]$Destination
            )

            $workflow = Get-Content -LiteralPath $script:guideWorkflowPath -Raw
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
    }

    It 'accepts one queued branch push after a PASS rerun' {
        $result = Invoke-RepairGate -QueuedOutput (New-AgentOutput -Types @('push_to_pull_request_branch'))

        $result.ExitCode | Should -Be 0
        $result.Output.Trim() | Should -Be ''
    }

    It 'accepts one queued noop acknowledgement when no localization files changed' {
        $result = Invoke-RepairGate -QueuedOutput (New-AgentOutput -Types @('noop'))

        $result.ExitCode | Should -Be 0
        $result.Output.Trim() | Should -Be ''
    }

    It 'rejects add_comment, multiple outputs, or missing outputs after a PASS rerun' {
        foreach ($case in @(
            @{ Name = 'add-comment-only'; Types = @('add_comment') }
            @{ Name = 'push-and-comment'; Types = @('push_to_pull_request_branch', 'add_comment') }
            @{ Name = 'push-and-noop'; Types = @('push_to_pull_request_branch', 'noop') }
            @{ Name = 'no-output'; Types = @() }
        )) {
            $result = Invoke-RepairGate -QueuedOutput (New-AgentOutput -Types $case.Types)

            $result.ExitCode | Should -Be 1 -Because $case.Name
            $result.Output | Should -Match 'repair PASS (permits only a queued branch push or noop acknowledgement|requires exactly one queued branch push or noop acknowledgement)'
        }
    }

    It 'rejects a noop acknowledgement when localization repairs remain unpushed' {
        $result = Invoke-RepairGate -QueuedOutput (New-AgentOutput -Types @('noop')) -CreateDirtyLocalizationChange $true

        $result.ExitCode | Should -Be 1
        $result.Output | Should -Match 'noop acknowledgement cannot discard working localization repairs'
    }

    It 'accepts a guide report from the agent workspace independently of the safe-output queue' {
        foreach ($type in @('noop', 'add_comment')) {
            $result = Invoke-RepairGate -Guide -QueuedOutput (New-AgentOutput -Types @($type))
            $result.ExitCode | Should -Be 0
        }
    }

    It 'requires one guide comment for FIXABLE evidence and rejects multiple outputs' {
        $result = Invoke-RepairGate -Guide -Fixable -QueuedOutput (New-AgentOutput -Types @('add_comment'))
        $result.ExitCode | Should -Be 0

        foreach ($types in @(
            ,@('noop')
            ,@('add_comment', 'noop')
            ,@('noop', 'noop')
        )) {
            $result = Invoke-RepairGate -Guide -Fixable -QueuedOutput (New-AgentOutput -Types $types)
            $result.ExitCode | Should -Be 1
        }
    }

    It 'keeps the guide report upload and prompt on the same agent-workspace path' {
        $workflow = Get-Content -LiteralPath $script:guideWorkflowPath -Raw
        $workflow | Should -Match 'path: /tmp/gh-aw/agent/localization-final-checks\.json'
        $workflow | Should -Match "readJson\('localization-final-checks.json', path.join\(root, 'agent'\)\)"
        $workflow | Should -Not -Match '/tmp/gh-aw/localization-final-checks\.json'
        $workflow | Should -Match "'git ls-tree:\*'"
        $workflow | Should -Match 'report is a JSON array'
        $workflow | Should -Not -Match 'report\.mode'
    }

    It 'prepares exact immutable resource bytes without exporting fork scripts or attributes' {
        $repo = Join-Path $TestDrive 'snapshot-repo'
        $base = Initialize-RepairGateRepo -Path $repo
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
        $base = Initialize-RepairGateRepo -Path $repo
        {
            Invoke-GuideSnapshots -Repository $repo -Base 'main' -Head $base -Destination (Join-Path $TestDrive 'invalid-snapshots')
        } | Should -Throw '*Invalid base snapshot revision*'
    }

    It 'compiles pull-request read access for repair publication branch resolution' {
        $workflow = Get-Content -LiteralPath $script:workflowPath -Raw
        $lock = Get-Content -LiteralPath $script:repairLockPath -Raw

        $workflow | Should -Match '(?ms)safe_outputs:\s+if:\s+needs\.agent\.result == ''success''\s+permissions:\s+pull-requests:\s+read'
        $lock | Should -Match '(?ms)safe_outputs:.*?permissions:\s+contents:\s+write\s+issues:\s+write\s+pull-requests:\s+read'
    }

    It 'documents historical snapshot evidence for deletion-only and no-English-derived runs' {
        $repairWorkflow = Get-Content -LiteralPath $script:workflowPath -Raw
        $guideWorkflow = Get-Content -LiteralPath $script:guideWorkflowPath -Raw

        foreach ($workflow in @($repairWorkflow, $guideWorkflow)) {
            $workflow | Should -Match 'no English-derived scope'
            $workflow | Should -Match 'historical evidence only'
            $workflow | Should -Match 'pre-deletion snapshots'
            $workflow | Should -Match 'Localized-only edits or deletions do not independently create'
        }
    }
}
