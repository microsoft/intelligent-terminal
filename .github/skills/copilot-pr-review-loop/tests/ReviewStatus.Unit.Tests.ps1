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
}
