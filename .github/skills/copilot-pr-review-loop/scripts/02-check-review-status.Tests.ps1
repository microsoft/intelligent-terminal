#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Copilot review status summary parsing' -Tag 'Unit' {
    BeforeAll {
        $path = Join-Path $PSScriptRoot '02-check-review-status.ps1'
        $code = (Get-Content -LiteralPath $path -Raw).Replace('. "$PSScriptRoot/_lib.ps1"', '')
        $script:statusScript = [scriptblock]::Create($code)

        function Resolve-RepoCoords {
            param([string]$Owner, [string]$Repo)
            @{ Owner = $Owner; Repo = $Repo }
        }

        function Invoke-Gh {
            param([string[]]$GhArgs)
            if ($GhArgs[1] -eq 'user') {
                return [pscustomobject]@{ ExitCode = 0; Stdout = 'merge-owner'; Stderr = '' }
            }
            if (($GhArgs -join ' ') -match 'reviewThreads') {
                $threads = @()
                if ($script:lastAuthor) {
                    $threads = @(@{ isResolved = $false; comments = @{ nodes = @(@{ author = @{ login = $script:lastAuthor } }) } })
                }
                $pr = @{ reviewThreads = @{ nodes = $threads; pageInfo = @{ hasNextPage = $false; endCursor = $null } } }
            } else {
                $pr = @{
                    headRefOid = 'current-head'
                    state = 'OPEN'
                    reviews = @{ nodes = @(@{
                        author = @{ login = 'copilot-pull-request-reviewer' }
                        state = 'COMMENTED'
                        submittedAt = '2026-10-07T12:07:41Z'
                        body = $script:reviewBody
                        commit = @{ oid = $script:reviewSha }
                    }) }
                    reviewRequests = @{ nodes = @(if ($script:pending) {
                        @{ requestedReviewer = @{ login = 'copilot-pull-request-reviewer' } }
                    }) }
                }
            }
            $json = @{ data = @{ repository = @{ pullRequest = $pr } } } | ConvertTo-Json -Depth 12
            [pscustomobject]@{ ExitCode = 0; Stdout = $json; Stderr = '' }
        }

        function Invoke-StatusFixture {
            param([AllowEmptyString()][string]$Body, [string]$ReviewSha = 'current-head', [string]$LastAuthor = '', [switch]$Pending)
            $script:reviewBody = $Body
            $script:reviewSha = $ReviewSha
            $script:lastAuthor = $LastAuthor
            $script:pending = $Pending.IsPresent
            & $script:statusScript -Owner microsoft -Repo intelligent-terminal -PrNumber 1070 | ConvertFrom-Json
        }

        $script:resolvedBody = @'
<!-- ccr-overview-v2 -->

### Needs a closer look

The latest XAML fix and related live Sidebar scenarios still lack successful runtime validation.

**0 open findings**

<details>
<summary><strong>2 resolved since last review</strong></summary>

- <picture><img alt="Medium severity" /></picture> [UTF-8 title fallback](#discussion_r4210592199)
- <picture><img alt="Medium severity" /></picture> [Horizontal transaction](#discussion_r4210592121)
</details>

Review effort: Balanced
'@
    }

    It 'Recognizes only supported zero-comment summaries: <Body>' -TestCases @(
        @{ Body = 'generated no new comments'; Expected = $true }
        @{ Body = 'generated 0 comments'; Expected = $true }
        @{ Body = 'Findings: None'; Expected = $true }
        @{ Body = "<!-- ccr-overview-v2 -->`r`n**Review effort:** Balanced  `r`n**Findings:** None"; Expected = $true }
        @{ Body = '**Comments generated:** 0 new'; Expected = $true }
        @{ Body = 'Comments generated: 0 new'; Expected = $true }
        @{ Body = "- **Comments generated:** 0 new"; Expected = $true }
        @{ Body = "- **Comments generated:** 1 new"; Expected = $false }
        @{ Body = "- **Comments generated:** 10 new"; Expected = $false }
        @{ Body = 'generated 1 comments'; Expected = $false }
        @{ Body = '**Findings:** 1'; Expected = $false }
        @{ Body = '**Findings:** None pending; 1 new'; Expected = $false }
        @{ Body = '**Comments generated:** 1 new'; Expected = $false }
        @{ Body = '**Comments generated:** 10 new'; Expected = $false }
        @{ Body = '**Comments generated:** 0 new and 1 existing'; Expected = $false }
        @{ Body = 'Quoted text: Findings: None'; Expected = $false }
        @{ Body = 'Findings: None found yet'; Expected = $false }
        @{ Body = 'Needs a closer look'; Expected = $false }
        @{ Body = ''; Expected = $false }
    ) {
        param($Body, $Expected)
        $status = Invoke-StatusFixture -Body $Body
        $status.NoNewComments | Should -Be $Expected
        $status.Converged | Should -Be $Expected
    }

    It 'Does not converge for a zero-findings review on a stale head' {
        $status = Invoke-StatusFixture -Body '**Findings:** None' -ReviewSha 'stale-head'
        $status.NoNewComments | Should -BeTrue
        $status.ReviewAtHead | Should -BeFalse
        $status.Converged | Should -BeFalse
    }

    It 'Does not converge while an open thread awaits our reply' {
        $status = Invoke-StatusFixture -Body '**Comments generated:** 0 new' -LastAuthor 'copilot-pull-request-reviewer'
        $status.ReviewAtHead | Should -BeTrue
        $status.NoNewComments | Should -BeTrue
        $status.OpenThreadsAwaitingReply | Should -Be 1
        $status.Converged | Should -BeFalse
    }

    It 'Preserves convergence with an explicitly handed-off open thread' {
        $status = Invoke-StatusFixture -Body '**Findings:** None' -LastAuthor 'merge-owner'
        $status.OpenThreadCount | Should -Be 1
        $status.OpenThreadsAwaitingReply | Should -Be 0
        $status.Converged | Should -BeTrue
    }

    It 'Handles bounded structured summaries: <Name>' -Tag 'Structured' -TestCases @(
        @{ Name = 'zero open only'; Body = '**0 open findings**'; Expected = $true }
        @{ Name = 'zero open and resolved zero'; Body = "**0 open findings**`n<details><summary><strong>0 resolved since last review</strong></summary></details>"; Expected = $true }
        @{ Name = 'zero open and resolved three'; Body = "**0 open findings**`n<details><summary><strong>3 resolved since last review</strong></summary><img alt='Low severity' />#discussion_r1</details>"; Expected = $true }
        @{ Name = 'nonzero open'; Body = '**1 open finding**'; Expected = $false }
        @{ Name = 'ten open'; Body = '**10 open findings**'; Expected = $false }
        @{ Name = 'resolved without open zero'; Body = '<details><summary><strong>2 resolved since last review</strong></summary></details>'; Expected = $false }
        @{ Name = 'previously missed medium'; Body = "**0 open findings**`n<details><summary><strong>Previously missed (1)</strong></summary><img alt='Medium severity' />bug</details>"; Expected = $false }
        @{ Name = 'previously missed low'; Body = "**0 open findings**`n<details><summary><strong>Previously missed (1)</strong></summary><img alt='Low severity' />bug</details>"; Expected = $false }
        @{ Name = 'medium unresolved text'; Body = "**0 open findings**`n**Medium:** unresolved finding: bug"; Expected = $false }
        @{ Name = 'low unresolved text'; Body = "**0 open findings**`n**Low:**unresolved finding: bug"; Expected = $false }
        @{ Name = 'unresolved section after resolved'; Body = "**0 open findings**`n<details><summary><strong>2 resolved since last review</strong></summary></details><details><summary>Medium unresolved</summary>bug</details>"; Expected = $false }
        @{ Name = 'nested finding in resolved section'; Body = "**0 open findings**`n<details><summary><strong>2 resolved since last review</strong></summary><details><summary>Medium unresolved</summary>bug</details></details>"; Expected = $false }
        @{ Name = 'unresolved text contradicts resolved section'; Body = "**0 open findings**`n<details><summary><strong>2 resolved since last review</strong></summary>**Medium:** unresolved finding: bug</details>"; Expected = $false }
        @{ Name = 'unknown section'; Body = "**0 open findings**`n<details><summary>Other findings</summary>bug</details>"; Expected = $false }
        @{ Name = 'contradictory open count'; Body = "**0 open findings**`n**1 open finding**"; Expected = $false }
        @{ Name = 'dangling finding link'; Body = "**0 open findings**`n[Bug](#discussion_r123)"; Expected = $false }
        @{ Name = 'legacy none with unresolved body'; Body = "**Findings:** None`n<details><summary>Medium unresolved</summary>bug</details>"; Expected = $false }
    ) {
        param($Body, $Expected)
        $status = Invoke-StatusFixture -Body $Body
        $status.NoNewComments | Should -Be $Expected
        $status.Converged | Should -Be $Expected
    }

    It 'Accepts the observed zero-open/resolved-body shape with a runtime warning' -Tag 'Structured' {
        $status = Invoke-StatusFixture -Body $script:resolvedBody
        $status.NoNewComments | Should -BeTrue
        $status.Converged | Should -BeTrue
    }

    It 'Accepts the actual bulleted review-details shape from PR758 review5085519475' {
        $body = @'
### Approval recommended

The change is narrowly scoped, reuses the existing cached ACP listing, and is backed by targeted unit tests that cover the reported regression and key safety/authority edge cases.

<details>
<summary>Review details</summary>

- **Files reviewed:** 5/5 changed files
- **Comments generated:** 0 new
- **Review effort level:** Lite
</details>
'@
        $status = Invoke-StatusFixture -Body $body
        $status.NoNewComments | Should -BeTrue
        $status.Converged | Should -BeTrue
    }

    It 'Rejects structured convergence at a stale head' -Tag 'Structured' {
        $status = Invoke-StatusFixture -Body $script:resolvedBody -ReviewSha 'stale-head'
        $status.NoNewComments | Should -BeTrue
        $status.ReviewAtHead | Should -BeFalse
        $status.Converged | Should -BeFalse
    }

    It 'Rejects structured convergence with an unanswered thread' -Tag 'Structured' {
        $status = Invoke-StatusFixture -Body $script:resolvedBody -LastAuthor 'copilot-pull-request-reviewer'
        $status.OpenThreadsAwaitingReply | Should -Be 1
        $status.Converged | Should -BeFalse
    }

    It 'Rejects structured convergence while a fresh review is pending' -Tag 'Structured' {
        $status = Invoke-StatusFixture -Body $script:resolvedBody -Pending
        $status.NoNewComments | Should -BeTrue
        $status.CopilotPending | Should -BeTrue
        $status.Converged | Should -BeFalse
    }
}
