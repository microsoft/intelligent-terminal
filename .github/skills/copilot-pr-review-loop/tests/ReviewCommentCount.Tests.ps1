BeforeAll {
    $scriptPath = Join-Path $PSScriptRoot '..\scripts\02-check-review-status.ps1'
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
    if ($errors.Count) {
        throw 'Review checker has PowerShell parse errors.'
    }
    $reviewBlock = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.IfStatementAst] -and
        $node.Extent.Text.StartsWith('if ($latest) {')
    }, $true)
    if (-not $reviewBlock) {
        throw 'Review checker block was not found.'
    }
    $evaluateReview = [scriptblock]::Create($reviewBlock.Extent.Text + @'

[pscustomobject]@{ ReviewAtHead = $reviewAtHead; NoNewComments = $noNewComments }
'@)
    $head = 'a' * 40
    $pr = [pscustomobject]@{ headRefOid = $head }
}

Describe 'Authoritative latest-review comment count' {
    It 'queries the complete connection count rather than one page of comments' {
        (Get-Content -LiteralPath $scriptPath -Raw) | Should -Match 'comments\(first:1\)\{totalCount\}'
    }

    It 'accepts zero comments independently of review wording' {
        foreach ($body in @('0 open findings', 'Comments generated: 0 new', 'Changed summary format', '')) {
            $latest = [pscustomobject]@{
                body = $body
                commit = [pscustomobject]@{ oid = $head }
                comments = [pscustomobject]@{ totalCount = 0 }
            }
            $result = & $evaluateReview
            $result.ReviewAtHead | Should -BeTrue
            $result.NoNewComments | Should -BeTrue
        }
    }

    It 'rejects a nonzero review count even when the body claims zero open findings' {
        $latest = [pscustomobject]@{
            body = '0 open findings; generated no new comments'
            commit = [pscustomobject]@{ oid = $head }
            comments = [pscustomobject]@{ totalCount = 101 }
        }
        (& $evaluateReview).NoNewComments | Should -BeFalse
    }

    It 'does not make a zero-comment review at an old head current' {
        $latest = [pscustomobject]@{
            body = '0 open findings'
            commit = [pscustomobject]@{ oid = 'b' * 40 }
            comments = [pscustomobject]@{ totalCount = 0 }
        }
        (& $evaluateReview).ReviewAtHead | Should -BeFalse
    }

    It 'fails explicitly for missing or invalid count metadata' {
        foreach ($count in @($null, -1, '0', 0.5)) {
            $latest = [pscustomobject]@{
                body = '0 open findings'
                commit = [pscustomobject]@{ oid = $head }
                comments = [pscustomobject]@{ totalCount = $count }
            }
            { & $evaluateReview } | Should -Throw '*invalid or missing comment count*'
        }
    }
}
