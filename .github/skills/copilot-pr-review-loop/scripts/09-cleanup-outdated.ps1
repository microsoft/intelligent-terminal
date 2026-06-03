<#
.SYNOPSIS
    Batch-resolve outdated Copilot review threads on a PR.

.DESCRIPTION
    After a review loop converges, the PR may still show old `isOutdated`
    Copilot threads listed as open. They were addressed by later commits
    but never explicitly resolved. This script finds them and resolves them
    in bulk.

    Only acts on threads where:
      - isOutdated: true
      - isResolved: false
      - the first comment's author is copilot-pull-request-reviewer

    Threads from human reviewers are never touched.

.PARAMETER Owner
    Repository owner (org or user). Defaults to the current repo's owner
    (resolved via `gh repo view`).

.PARAMETER Repo
    Repository name. Defaults to the current repo's name.

.PARAMETER PrNumber
    The pull request number.

.EXAMPLE
    pwsh 09-cleanup-outdated.ps1 -PrNumber 122

.EXAMPLE
    pwsh 09-cleanup-outdated.ps1 -PrNumber 122 -WhatIf
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'

if (-not $Owner -or -not $Repo) {
    $repoInfo = gh repo view --json owner,name | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) {
        throw "gh repo view failed (exit $LASTEXITCODE). Pass -Owner and -Repo explicitly or run from inside a gh-detected repo."
    }
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

$query = @'
query($owner: String!, $repo: String!, $pr: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $pr) {
      reviewThreads(first: 100, after: $after) {
        pageInfo {
          endCursor
          hasNextPage
        }
        nodes {
          id
          isResolved
          isOutdated
          comments(first: 1) {
            nodes { author { login } }
          }
        }
      }
    }
  }
}
'@

$all = @()
$after = $null
do {
    $args = @('-f', "query=$query", '-f', "owner=$Owner", '-f', "repo=$Repo", '-F', "pr=$PrNumber")
    if ($after) { $args += @('-f', "after=$after") }

    $json = gh api graphql @args
    if ($LASTEXITCODE -ne 0) {
        throw "gh api graphql (list) failed (exit $LASTEXITCODE) for $Owner/$Repo PR #$PrNumber."
    }
    $data = $json | ConvertFrom-Json
    $page = $data.data.repository.pullRequest.reviewThreads
    $all += $page.nodes
    $after = $page.pageInfo.endCursor
} while ($page.pageInfo.hasNextPage)

$threads = $all

$targets = @($threads | Where-Object {
    $_.isOutdated -and
    -not $_.isResolved -and
    $_.comments.nodes[0].author.login -eq 'copilot-pull-request-reviewer'
})

if ($targets.Count -eq 0) {
    Write-Output 'No outdated Copilot threads to clean up.'
    return
}

Write-Output "Found $($targets.Count) outdated Copilot thread(s) to resolve."

$resolveMutation = @'
mutation($tid: ID!) {
  resolveReviewThread(input: { threadId: $tid }) {
    thread { isResolved }
  }
}
'@

foreach ($t in $targets) {
    if ($PSCmdlet.ShouldProcess($t.id, 'Resolve outdated Copilot thread')) {
        gh api graphql -f query=$resolveMutation -f "tid=$($t.id)" | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "gh api graphql (resolve) failed (exit $LASTEXITCODE) for thread $($t.id)."
        }
        Write-Output "Resolved $($t.id)"
    }
}
