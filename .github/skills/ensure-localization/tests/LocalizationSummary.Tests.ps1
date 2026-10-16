#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Agent-authored localization summary publication' -Tag 'Unit' {
    BeforeAll {
        $script:controllerPath = Join-Path $PSScriptRoot '..\..\..\workflows\ensure-localization-controller.yml'
        $script:summaryTemplatePath = Join-Path $PSScriptRoot '..\..\..\workflows\shared\localization-summary.md'

        function Invoke-SummaryPublication {
            param(
                [Parameter(Mandatory)][AllowEmptyString()][string]$Content,
                [switch]$CompletionCheck,
                [switch]$Missing
            )

            $caseRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
            $summaryRoot = Join-Path $caseRoot 'localization-summary'
            [IO.Directory]::CreateDirectory($summaryRoot) | Out-Null
            $encoding = [Text.UTF8Encoding]::new($false)
            if (-not $Missing) {
                [IO.File]::WriteAllText((Join-Path $summaryRoot 'localization-summary.md'), $Content, $encoding)
            }
            $publishedPath = Join-Path $caseRoot 'published.md'
            $capturedCheck = Join-Path $caseRoot 'check.json'
            $stepName = if ($CompletionCheck) { 'Create completion check on pushed head' } else { 'Publish agent-authored localization summary' }
            $controller = Get-Content -LiteralPath $script:controllerPath -Raw
            $match = [regex]::Match(
                $controller,
                "(?ms)^      - name: $stepName\r?\n.*?^          script: \|\r?\n(?<script>.*?)(?=^      - name:|\z)"
            )
            if (-not $match.Success) { throw "Cannot find summary publication step: $stepName" }
            $body = $match.Groups['script'].Value -replace '(?m)^            ', ''
            $bodyLiteral = ConvertTo-Json -InputObject $body -Compress
            $captureLiteral = ConvertTo-Json -InputObject $capturedCheck -Compress
            $nodeScript = @"
const fs = require('fs');
const run = new (Object.getPrototypeOf(async function() {}).constructor)('require', 'github', 'context', $bodyLiteral);
const github = { rest: {
  pulls: { get: async () => ({ data: { head: { sha: 'a'.repeat(40) } } }) },
  checks: { create: async check => fs.writeFileSync($captureLiteral, JSON.stringify(check)) }
} };
const context = { repo: { owner: 'test', repo: 'repo' }, payload: { pull_request: { number: 1 } }, runId: 1 };
run(require, github, context).catch(error => { console.error(error.message); process.exitCode = 1; });
"@
            $nodePath = Join-Path $caseRoot 'publish.js'
            [IO.File]::WriteAllText($nodePath, $nodeScript, $encoding)
            $names = @('RUNNER_TEMP', 'GITHUB_STEP_SUMMARY', 'FINAL_HEAD_SHA', 'CONTROLLER_RUN_URL')
            $original = @{}
            foreach ($name in $names) { $original[$name] = [Environment]::GetEnvironmentVariable($name) }
            try {
                $env:RUNNER_TEMP = $caseRoot
                $env:GITHUB_STEP_SUMMARY = $publishedPath
                $env:FINAL_HEAD_SHA = 'a' * 40
                $env:CONTROLLER_RUN_URL = 'https://github.com/test/repo/actions/runs/1'
                $output = & node $nodePath 2>&1
                $exitCode = $LASTEXITCODE
            } finally {
                foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $original[$name]) }
            }
            $published = if (Test-Path -LiteralPath $publishedPath) { [IO.File]::ReadAllText($publishedPath) } else { $null }
            $check = if (Test-Path -LiteralPath $capturedCheck) { Get-Content -LiteralPath $capturedCheck -Raw | ConvertFrom-Json } else { $null }
            return [pscustomobject]@{ ExitCode = $exitCode; Output = ($output | Out-String); Published = $published; Check = $check }
        }
    }

    It 'copies Copilot Markdown unchanged into the Actions summary and completion check' {
        $content = @'
## Localization review

**Result:** PASS

`${literal}` & <not a script>
'@
        $published = Invoke-SummaryPublication -Content $content
        $published.ExitCode | Should -Be 0
        $published.Published | Should -BeExactly $content

        $completion = Invoke-SummaryPublication -Content $content -CompletionCheck
        $completion.ExitCode | Should -Be 0
        $completion.Check.output.summary | Should -BeExactly $content
        $completion.Check.conclusion | Should -Be 'success'
    }

    It 'fails explicitly when the Markdown artifact is missing, empty, or oversized' {
        $missing = Invoke-SummaryPublication -Content '' -Missing
        $missing.ExitCode | Should -Be 1
        $missing.Published | Should -BeNullOrEmpty
        foreach ($content in @('', ('x' * (64 * 1024 + 1)))) {
            $result = Invoke-SummaryPublication -Content $content
            $result.ExitCode | Should -Be 1
            $result.Output | Should -Match 'non-empty regular Markdown file'
            $result.Published | Should -BeNullOrEmpty
        }
    }

    It 'uses one shared Copilot template instead of composing summaries in scripts' {
        $controller = Get-Content -LiteralPath $script:controllerPath -Raw
        $template = Get-Content -LiteralPath $script:summaryTemplatePath -Raw
        $controller | Should -Not -Match 'const summary = \[|Add-Content.*GITHUB_STEP_SUMMARY|echo.*>>.*GITHUB_STEP_SUMMARY|Report AI usage'
        $controller | Should -Match 'name: localization-summary'
        $template | Should -Match 'author[\s\S]*localization-summary\.md'
        $template | Should -Match 'Do not generate the\s+prose with a script'
        $template | Should -Match 'if-no-files-found: error'
        $template | Should -Match '### Scope and findings'
        $template | Should -Match '### Verification'
        $template | Should -Match '### Changes and limitations'
    }
}
