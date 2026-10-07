# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

BeforeAll {
    $path = Join-Path $PSScriptRoot '..\scripts\02-check-review-status.ps1'
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $path, [ref]$null, [ref]$errors)
    if ($errors.Count) { throw 'Review status script does not parse.' }
    $function = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Test-CopilotReviewHasNoNewFindings'
    }, $true)
    . ([scriptblock]::Create($function.Extent.Text))
}

Describe 'Copilot review summary compatibility' {
    It 'accepts legacy no-comment summaries' {
        Test-CopilotReviewHasNoNewFindings 'Copilot generated no new comments.' | Should -BeTrue
        Test-CopilotReviewHasNoNewFindings 'Copilot generated 0 comments.' | Should -BeTrue
    }

    It 'accepts the current no-findings field' {
        Test-CopilotReviewHasNoNewFindings "## Copilot review overview`n**Findings:** None" | Should -BeTrue
    }

    It 'accepts zero new comments with markdown or plain labels' {
        Test-CopilotReviewHasNoNewFindings '- **Comments generated:** 0 new' | Should -BeTrue
        Test-CopilotReviewHasNoNewFindings 'Comments generated: 0 new' | Should -BeTrue
    }

    It 'does not mistake resolved findings for new findings' {
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`nResolved since last review (2)" | Should -BeTrue
    }

    It 'rejects nonzero previously missed findings despite a no-findings field' {
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`n<summary><strong>Previously missed (1)</strong></summary>" | Should -BeFalse
    }

    It 'rejects ordinary nonzero findings and comments' {
        Test-CopilotReviewHasNoNewFindings '**Findings:** 2' | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings '- **Comments generated:** 1 new' | Should -BeFalse
    }

    It 'does not accept quoted or example fields as the summary field' {
        Test-CopilotReviewHasNoNewFindings '> **Findings:** None' | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings 'Example: **Findings:** None' | Should -BeFalse
    }

    It 'rejects an absent summary' {
        Test-CopilotReviewHasNoNewFindings '' | Should -BeFalse
    }

    It 'ignores fenced example fields' {
        $body = '```' + "`n**Findings:** None`n" + '```'
        Test-CopilotReviewHasNoNewFindings $body | Should -BeFalse
    }

    It 'prefers the current explicit nonzero field over historical zero comments' {
        Test-CopilotReviewHasNoNewFindings "**Findings:** 1`nThe previous review generated no new comments." | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "Comments generated: 1 new`nThe previous review generated 0 comments." | Should -BeFalse
    }

    It 'rejects contradictory observed summaries in either direction' {
        # PR 985, thread PRRT_kwDOSgzUZs6psYs_: Findings alone hid a new comment.
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`n- **Comments generated:** 1 new" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "- **Comments generated:** 1 new`n**Findings:** None" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "**Findings:** 1`n- **Comments generated:** 0 new" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "- **Comments generated:** 0 new`n**Findings:** 1" | Should -BeFalse
    }

    It 'accepts multiple matching zero summaries' {
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`nFindings: 0`n- **Comments generated:** 0 new`nComments generated: 0 new`nPreviously missed (0)" | Should -BeTrue
    }

    It 'checks every repeated explicit field rather than only the first' {
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`n**Findings:** 2" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "**Findings:** 2`n**Findings:** None" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "Comments generated: 0 new`nComments generated: 3 new" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "Comments generated: 3 new`nComments generated: 0 new" | Should -BeFalse
    }

    It 'recognizes formatted comment counts without losing nonzero constraints' {
        Test-CopilotReviewHasNoNewFindings '- **Comments generated: 0 new**' | Should -BeTrue
        Test-CopilotReviewHasNoNewFindings '**Comments generated:** **0 new**' | Should -BeTrue
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`n- **Comments generated: 1 new**" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`n**Comments generated:** **1 new**" | Should -BeFalse
    }

    It 'ignores quoted constraints and legacy example text' {
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`n> Comments generated: 2 new`n> Previously missed (1)" | Should -BeTrue
        Test-CopilotReviewHasNoNewFindings "> Copilot generated no new comments.`n> Comments generated: 0 new" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "> **Findings:** None`nComments generated: 1 new" | Should -BeFalse
    }

    It 'ignores backtick and tilde fenced constraints including previously missed counts' {
        foreach ($fence in @('```', '~~~~')) {
            $example = $fence + "`n**Findings:** 1`nComments generated: 2 new`nPreviously missed (3)`n" + $fence
            Test-CopilotReviewHasNoNewFindings ("**Findings:** None`n" + $example) | Should -BeTrue
            $zeroExample = $fence + "`nCopilot generated no new comments.`nComments generated: 0 new`n" + $fence
            Test-CopilotReviewHasNoNewFindings $zeroExample | Should -BeFalse
            Test-CopilotReviewHasNoNewFindings ($zeroExample + "`n**Findings:** 1") | Should -BeFalse
        }
    }

    It 'does not end a longer fence on a shorter example fence' {
        $body = '````' + "`n" + '```' + "`n**Findings:** None`n" + '````'
        Test-CopilotReviewHasNoNewFindings $body | Should -BeFalse
    }

    It 'rejects previously missed findings with either structured or legacy zero summaries' {
        Test-CopilotReviewHasNoNewFindings "Comments generated: 0 new`nPreviously missed (1)" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "Copilot generated no new comments.`nPreviously missed (2)" | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`nPreviously missed (01)" | Should -BeFalse
    }

    It 'does not infer a zero or nonzero count from vague overview claims' {
        Test-CopilotReviewHasNoNewFindings 'The review found no actionable issues.' | Should -BeFalse
        Test-CopilotReviewHasNoNewFindings "**Findings:** None`nThe overview mentions potential concerns." | Should -BeTrue
    }
}
