#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Ensure localization repair workflow gate' -Tag 'Unit' {
    BeforeAll {
        $script:workflowPath = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localization.md')
        $script:guideWorkflowPath = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localizationguide-forkedrepo.md')
        $script:repairLockPath = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localization.lock.yml')

        function Get-RepairGateScript {
            $workflow = Get-Content -LiteralPath $script:workflowPath -Raw
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
                [bool]$CreateDirtyLocalizationChange = $false
            )

            $caseRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
            $ghawRoot = Join-Path $caseRoot 'gh-aw'
            $repoRoot = Join-Path $caseRoot 'repo'
            [System.IO.Directory]::CreateDirectory($ghawRoot) | Out-Null
            $head = Initialize-RepairGateRepo -Path $repoRoot

            $reportPath = Join-Path $ghawRoot 'localization-final-checks.json'
            $queuedOutputPath = Join-Path $ghawRoot 'agent_output.json'
            $scriptPath = Join-Path $caseRoot 'repair-gate.js'

            $jsonEncoding = [System.Text.UTF8Encoding]::new($false)
            [System.IO.File]::WriteAllText($reportPath, ((New-PassReport) | ConvertTo-Json -Compress -Depth 8), $jsonEncoding)
            [System.IO.File]::WriteAllText($queuedOutputPath, ($QueuedOutput | ConvertTo-Json -Compress -Depth 8), $jsonEncoding)

            if ($CreateDirtyLocalizationChange) {
                $resourcePath = Join-Path $repoRoot 'src\cascadia\TerminalApp\Resources\fr-FR\Resources.resw'
                [System.IO.File]::WriteAllText($resourcePath, '<root><data name="changed" /></root>', $jsonEncoding)
            }

            $scriptContent = Get-RepairGateScript
            $scriptRootLiteral = (($ghawRoot -replace '\\', '/') | ConvertTo-Json -Compress)
            $scriptContent = $scriptContent -replace "const root = '/tmp/gh-aw';", "const root = $scriptRootLiteral;"
            [System.IO.File]::WriteAllText($scriptPath, $scriptContent, $jsonEncoding)

            Push-Location $repoRoot
            try {
                $env:LOCALIZATION_REPORT_MODE = 'repair'
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
