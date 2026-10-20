#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Same-repository PR automation entry points' -Tag 'Unit' {
    BeforeAll {
        $workflowRoot = Join-Path $PSScriptRoot '..\..\..\workflows'
        $script:localization = Get-Content (Join-Path $workflowRoot 'ensure-localization-controller.yml') -Raw
        $script:spelling = Get-Content (Join-Path $workflowRoot 'spelling2.yml') -Raw

        function Get-JobCondition {
            param([string]$Workflow, [string]$Job)

            $jobBlock = [regex]::Match($Workflow, "(?ms)^  ${Job}:\r?\n.*?(?=^  [\w-]+:|\z)").Value
            $condition = [regex]::Match($jobBlock, '(?m)^    if: (.+)$').Groups[1].Value.Trim()
            if (-not $condition) { throw "Missing condition for $Job" }
            return $condition -replace '^\$\{\{\s*|\s*\}\}$', ''
        }

        function Test-JobCondition {
            param([string]$Condition, [string]$Event, [int]$HeadRepo = 42,
                [string]$Owner = 'microsoft', [string]$State = 'open',
                [bool]$Followup = $true, [bool]$Success = $true, [bool]$Failure = $false)

            # Evaluate the actual workflow expressions, not a separately copied policy.
            # This intentionally supports only the operators/context used by these jobs.
            $expression = $Condition.Replace('github.event.pull_request.head.repo.id', '$HeadRepo').
                Replace('github.repository_id', '42').
                Replace('github.event.pull_request.state', '$State').
                Replace('github.repository_owner', '$Owner').
                Replace('github.event_name', '$Event').
                Replace('needs.spelling.outputs.followup', '$Followup').
                Replace('success()', '$Success').Replace('failure()', '$Failure').
                Replace('==', '-eq').Replace('&&', '-and').Replace('||', '-or')
            return [bool](& ([scriptblock]::Create($expression)))
        }
    }

    It 'uses ordinary PR events and retains localization event and path filters' {
        foreach ($workflow in @($script:localization, $script:spelling)) {
            $workflow | Should -Match '(?m)^  pull_request:'
            $workflow | Should -Not -Match 'pull_request_target'
        }
        $script:localization | Should -Match 'types: \[opened, reopened, synchronize, ready_for_review\]'
        foreach ($path in @('src/cascadia/**/Resources/*.resw', 'src/cascadia/**/Resources/**/*.resw', 'tools/wta/locales/*.yml')) {
            $script:localization | Should -Match ([regex]::Escape("'$path'"))
        }
        $script:spelling | Should -Match '(?s)  push:\s+branches:\s+- "\*\*"\s+tags-ignore:\s+- "\*\*"'
        $script:spelling | Should -Match '(?s)  issue_comment:\s+types:\s+- "created"'
    }

    It 'gates PR work without blocking spelling pushes: <Name>' -ForEach @(
        @{ Name = 'internal PR'; Event = 'pull_request'; Head = 42; Owner = 'microsoft'; State = 'open'; Localization = $true; Spelling = $true }
        @{ Name = 'external fork PR'; Event = 'pull_request'; Head = 99; Owner = 'microsoft'; State = 'open'; Localization = $false; Spelling = $false }
        @{ Name = 'push without PR payload'; Event = 'push'; Head = 0; Owner = 'microsoft'; State = ''; Localization = $false; Spelling = $true }
        @{ Name = 'comment'; Event = 'issue_comment'; Head = 0; Owner = 'microsoft'; State = ''; Localization = $false; Spelling = $false }
        @{ Name = 'fork repository push'; Event = 'push'; Head = 0; Owner = 'contributor'; State = ''; Localization = $false; Spelling = $false }
        @{ Name = 'closed internal PR'; Event = 'pull_request'; Head = 42; Owner = 'microsoft'; State = 'closed'; Localization = $true; Spelling = $false }
    ) {
        Test-JobCondition (Get-JobCondition $script:localization 'localization-review') -Event $Event -HeadRepo $Head -Owner $Owner -State $State |
            Should -Be $Localization
        Test-JobCondition (Get-JobCondition $script:spelling 'spelling') -Event $Event -HeadRepo $Head -Owner $Owner -State $State |
            Should -Be $Spelling
    }

    It 'reports spelling followups only for the matching event and repository: <Name>' -ForEach @(
        @{ Name = 'PR success'; Event = 'pull_request'; Head = 42; Followup = $true; Success = $true; Failure = $false; PR = $true; Push = $false }
        @{ Name = 'PR failure'; Event = 'pull_request'; Head = 42; Followup = $true; Success = $false; Failure = $true; PR = $true; Push = $false }
        @{ Name = 'fork PR'; Event = 'pull_request'; Head = 99; Followup = $true; Success = $true; Failure = $false; PR = $false; Push = $false }
        @{ Name = 'push'; Event = 'push'; Head = 0; Followup = $true; Success = $true; Failure = $false; PR = $false; Push = $true }
        @{ Name = 'no followup'; Event = 'pull_request'; Head = 42; Followup = $false; Success = $true; Failure = $false; PR = $false; Push = $false }
        @{ Name = 'skipped dependency'; Event = 'pull_request'; Head = 42; Followup = $false; Success = $false; Failure = $false; PR = $false; Push = $false }
    ) {
        $args = @{ Event = $Event; HeadRepo = $Head; Followup = $Followup; Success = $Success; Failure = $Failure }
        Test-JobCondition (Get-JobCondition $script:spelling 'comment-pr') @args | Should -Be $PR
        Test-JobCondition (Get-JobCondition $script:spelling 'comment-push') @args | Should -Be $Push
    }

}
