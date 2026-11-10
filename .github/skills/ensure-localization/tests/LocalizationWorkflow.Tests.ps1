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

            function Invoke-CompletionGate {
                param(
                    [object[]]$Items = @(@{ type = 'noop' }),
                    [string]$Checks = 'PASS',
                    [string]$Review = 'PASS',
                    [object[]]$Errors = @(),
                    [switch]$MissingSummary
                )
                $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
                [IO.Directory]::CreateDirectory((Join-Path $root 'agent')) | Out-Null
                @{ items = $Items; errors = $Errors } | ConvertTo-Json -Depth 10 |
                    Set-Content -LiteralPath (Join-Path $root 'agent_output.json')
                if (-not $MissingSummary) {
                    "**Final checks:** $Checks`n**Independent review:** $Review" |
                        Set-Content -LiteralPath (Join-Path $root 'agent/localization-summary.md')
                }
                $template = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'shared/localization-summary.md') -Raw
                $match = [regex]::Match($template, '(?s)- name: Require complete localization outcome before publication.*?run: \|\r?\n(?<script>.*?)  - name: Upload agent-authored')
                if (-not $match.Success) { throw 'Cannot locate the native completion gate.' }
                $body = ($match.Groups['script'].Value -replace '(?m)^      ', '').Replace('/tmp/gh-aw', $root.Replace('\', '/'))
                & ([scriptblock]::Create($body))
            }

            function Invoke-WorkerCancellation {
                param(
                    [switch]$KnownRun,
                    [switch]$LateDispatch,
                    [switch]$Completed,
                    [int]$CancelError = 0,
                    [switch]$CompletesDuringCancel
                )
                $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
                [IO.Directory]::CreateDirectory($root) | Out-Null
                $controller = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'ensure-localization-controller.yml') -Raw
                $match = [regex]::Match($controller, '(?ms)^      - name: Cancel unfinished detached worker\r?\n.*?^          script: \|\r?\n(?<script>.*?)(?=^      - name:)')
                if (-not $match.Success) { throw 'Cannot locate detached worker cancellation.' }
                $bodyLiteral = ConvertTo-Json -InputObject ($match.Groups['script'].Value -replace '(?m)^            ', '') -Compress
                $nodeScript = @"
const assert = require('node:assert/strict');
const run = new (Object.getPrototypeOf(async function() {}).constructor)('github', 'context', 'core', $bodyLiteral);
const expected = 'Ensure Localization ensure-localization-1-' + 'a'.repeat(40) + '-10-2';
const context = { repo: { owner: 'test', repo: 'repo' }, runId: 10, runAttempt: 2,
  payload: { pull_request: { number: 1, head: { sha: 'a'.repeat(40) } } } };
let listed = 0, cancelled = [], reads = 0;
process.env.WORKER_RUN_ID = $(if ($KnownRun) { "'20'" } else { "''" });
global.setTimeout = cb => cb();
const github = { rest: { actions: {
  listWorkflowRuns: () => {},
  getWorkflowRun: async args => {
    if (args.run_id === 10) return { data: { created_at: '2026-11-10T00:00:00Z' } };
    reads++;
    return { data: { status: $(
            if ($Completed) { "'completed'" }
            elseif ($CompletesDuringCancel) { "reads > 1 ? 'completed' : 'in_progress'" }
            else { "'in_progress'" }) } };
  },
  cancelWorkflowRun: async args => {
    cancelled.push(args.run_id);
    if ($CancelError) throw Object.assign(new Error('cancel failed'), { status: $CancelError });
  }
} }, paginate: async (_, args) => {
  assert.equal(args.workflow_id, 'ensure-localization.lock.yml');
  assert.equal(args.created, '>=2026-11-10T00:00:00Z');
  if ($(if ($LateDispatch) { 'true' } else { 'false' }) && listed++ === 0) return [];
  return [
    { id: 21, display_title: expected + '-newer' },
    { id: 22, display_title: expected.replace('-10-2', '-10-1') },
    { id: 20, display_title: expected }
  ];
} };
run(github, context, { warning: () => {} }).then(() => {
  assert.deepEqual(cancelled, $(if ($Completed) { '[]' } else { '[20]' }));
}).catch(error => { console.error(error.message); process.exitCode = 1; });
"@
                $path = Join-Path $root 'cancel.js'
                [IO.File]::WriteAllText($path, $nodeScript)
                $output = & node $path 2>&1
                [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = ($output | Out-String) }
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
        $repair | Should -Match 'actual returned `PASS` before requesting any branch write'
        $repair | Should -Match 'agent_type: "Localization Reviewer"'
        $repair | Should -Match '## agent: `localization-reviewer`'
        $repair | Should -Not -Match 'localization-review-gate'
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

    It 'accepts only unambiguous successful native outcomes with both required PASS attestations' {
        { Invoke-CompletionGate } | Should -Not -Throw
        { Invoke-CompletionGate -Items @(@{ type = 'push_to_pull_request_branch' }) } | Should -Not -Throw
        foreach ($type in @('report_incomplete', 'missing_tool', 'missing_data', 'unknown')) {
            { Invoke-CompletionGate -Items @(@{ type = $type }) } | Should -Throw '*Localization did not complete*'
        }
        { Invoke-CompletionGate -Items @() } | Should -Throw '*Localization did not complete*'
        { Invoke-CompletionGate -Items @(@{ type = 'noop' }, @{ type = 'report_incomplete' }) } | Should -Throw '*Localization did not complete*'
        { Invoke-CompletionGate -Items @(@{ type = 'noop' }, @{ type = 'noop' }) } | Should -Throw '*Localization did not complete*'
        { Invoke-CompletionGate -Errors @('tool failed') } | Should -Throw '*Localization did not complete*'
    }

    It 'rejects denied or failed checks, unrun reviews, and missing summaries rather than promoting prose to PASS' {
        foreach ($status in @('BLOCKED', 'FIXABLE', 'INVALID_INPUT', '', 'PASS but not executed')) {
            { Invoke-CompletionGate -Checks $status } | Should -Throw '*Final checks must explicitly be PASS*'
            { Invoke-CompletionGate -Review $status } | Should -Throw '*Independent review must explicitly be PASS*'
        }
        { Invoke-CompletionGate -MissingSummary } | Should -Throw
    }

    It 'executes the documented preflight and fails on unavailable PowerShell cmdlets' {
        $repair = Get-Content -LiteralPath $script:repairPath -Raw
        $match = [regex]::Match($repair, '(?m)^pwsh -NoProfile -NonInteractive -Command ''(?<command>.*)''$')
        $match.Success | Should -BeTrue
        $repoRoot = (Resolve-Path (Join-Path $script:workflowRoot '../..')).Path
        Push-Location $repoRoot
        try {
            $output = & pwsh -NoProfile -NonInteractive -Command $match.Groups['command'].Value 2>&1
            $LASTEXITCODE | Should -Be 0
            ($output | Out-String) | Should -Match '"check":"Test-ResourceSyntax".*"status":"PASS"'
            $output = & pwsh -NoProfile -NonInteractive -Command ('$PSModuleAutoLoadingPreference = "None"; ' + $match.Groups['command'].Value) 2>&1
            $LASTEXITCODE | Should -Not -Be 0
            ($output | Out-String) | Should -Match 'Get-ChildItem'
        } finally { Pop-Location }
    }

    It 'cancels only the correlated detached run, including delayed dispatch and completion races' {
        (Invoke-WorkerCancellation -KnownRun).ExitCode | Should -Be 0
        (Invoke-WorkerCancellation -LateDispatch).ExitCode | Should -Be 0
        (Invoke-WorkerCancellation -Completed).ExitCode | Should -Be 0
        (Invoke-WorkerCancellation -CancelError 409 -CompletesDuringCancel).ExitCode | Should -Be 0
        (Invoke-WorkerCancellation -CancelError 409).ExitCode | Should -Be 1
        (Invoke-WorkerCancellation -CancelError 403).ExitCode | Should -Be 1
    }

    It 'does not promote successful jobs or skipped inference without the native completion gate to controller success' {
        $controller = Get-Content -LiteralPath (Join-Path $script:workflowRoot 'ensure-localization-controller.yml') -Raw
        $match = [regex]::Match($controller, '(?ms)^      - name: Dispatch and wait for internal GHAW\r?\n.*?^          script: \|\r?\n(?<script>.*?)(?=^      - name:)')
        $match.Success | Should -BeTrue
        $bodyLiteral = ConvertTo-Json -InputObject ($match.Groups['script'].Value -replace '(?m)^            ', '') -Compress
        foreach ($case in @(
            @{ Agent = 'success'; Gate = 'success'; Run = 'success'; Expected = 'success' }
            @{ Agent = 'success'; Gate = ''; Run = 'success'; Expected = 'failure' }
            @{ Agent = 'success'; Gate = 'failure'; Run = 'success'; Expected = 'failure' }
            @{ Agent = 'skipped'; Gate = ''; Run = 'success'; Expected = 'failure' }
            @{ Agent = 'failure'; Gate = 'success'; Run = 'failure'; Expected = 'failure' }
        )) {
            $caseLiteral = ConvertTo-Json -InputObject $case -Compress
            $nodeScript = @"
const assert = require('node:assert/strict');
const fixture = $caseLiteral;
const run = new (Object.getPrototypeOf(async function() {}).constructor)('github', 'context', 'core', $bodyLiteral);
const context = { repo: { owner: 'test', repo: 'repo' }, runId: 10, runAttempt: 1,
  payload: { pull_request: { number: 1 } } };
const github = {
  request: async () => ({ data: { workflow_run_id: 20 } }),
  rest: { actions: {
    getWorkflowRun: async () => ({ data: { status: 'completed', conclusion: fixture.Run, html_url: 'https://example.test/20' } }),
    listJobsForWorkflowRun: () => {}
  } },
  paginate: async () => [
    { name: 'agent', status: 'completed', conclusion: fixture.Agent,
      steps: fixture.Gate ? [{ name: 'Require complete localization outcome before publication', status: 'completed', conclusion: fixture.Gate }] : [] },
    { name: 'detection', status: 'completed', conclusion: fixture.Agent === 'skipped' ? 'skipped' : 'success' }
  ]
};
const outputs = {};
run(github, context, { setOutput: (key, value) => outputs[key] = value, warning: () => {} })
  .then(() => assert.equal(outputs.conclusion, fixture.Expected))
  .catch(error => { console.error(error.message); process.exitCode = 1; });
"@
            $path = Join-Path $TestDrive 'worker-outcome.js'
            [IO.File]::WriteAllText($path, $nodeScript)
            $output = & node $path 2>&1
            $LASTEXITCODE | Should -Be 0 -Because ($output | Out-String)
        }
    }
}
