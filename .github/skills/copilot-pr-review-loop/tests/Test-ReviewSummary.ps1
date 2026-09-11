$ErrorActionPreference = 'Stop'
$scriptPath = Join-Path $PSScriptRoot '..\scripts\02-check-review-status.ps1'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $scriptPath, [ref]$tokens, [ref]$errors)
if ($errors.Count -gt 0) {
    throw ($errors | Out-String)
}

# Load only the pure classifier, without authentication or GitHub requests.
$function = $ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $node.Name -eq 'Test-CopilotZeroCommentReview'
}, $true)
if (-not $function) {
    throw 'Review-summary classifier was not found.'
}
. ([scriptblock]::Create($function.Extent.Text))

$cases = @(
    @{ Body = 'Copilot generated no new comments.'; Count = 0; Expected = $true },
    @{ Body = 'Copilot generated 0 comments.'; Count = 0; Expected = $true },
    @{ Body = "- **Comments generated:** 0 new`n- **Review effort level:** Lite"; Count = 0; Expected = $true },
    @{ Body = 'Comments generated: 0 new'; Count = 0; Expected = $true },
    @{ Body = 'Copilot generated 10 comments.'; Count = 10; Expected = $false },
    @{ Body = '- **Comments generated:** 10 new'; Count = 10; Expected = $false },
    @{ Body = '- **Comments generated:** 0 new'; Count = 1; Expected = $false },
    @{ Body = 'Copilot generated no new comments.'; Count = 2; Expected = $false },
    @{ Body = '- **Comments generated:** 0 new'; Count = $null; Expected = $false },
    @{ Body = 'Needs a closer look.'; Count = 0; Expected = $false },
    @{ Body = ''; Count = 0; Expected = $false }
)

foreach ($case in $cases) {
    $actual = Test-CopilotZeroCommentReview -Body $case.Body -CommentCount $case['Count']
    if ($actual -ne $case.Expected) {
        throw "Unexpected zero-comment classification: body='$($case.Body)', count='$($case['Count'])'"
    }
}
"Passed $($cases.Count) review-summary cases."
