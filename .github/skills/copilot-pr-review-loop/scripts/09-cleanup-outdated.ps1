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
    Repository owner (org or user).

.PARAMETER Repo
    Repository name.

.PARAMETER PrNumber
    The pull request number.

.EXAMPLE
    pwsh 09-cleanup-outdated.ps1 -Owner microsoft -Repo intelligent-terminal -PrNumber 122

.EXAMPLE
    pwsh 09-cleanup-outdated.ps1 -Owner microsoft -Repo intelligent-terminal -PrNumber 122 -WhatIf
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [Parameter(Mandatory = $true)]
    [string]$Owner,

    [Parameter(Mandatory = $true)]
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'

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
    $data = $json | ConvertFrom-Json
    $page = $data.data.repository.pullRequest.reviewThreads
    $all += $page.nodes
    $after = $page.pageInfo.endCursor
} while ($page.pageInfo.hasNextPage)

$threads = $all

$targets = $threads | Where-Object {
    $_.isOutdated -and
    -not $_.isResolved -and
    $_.comments.nodes[0].author.login -eq 'copilot-pull-request-reviewer'
}

if (-not $targets) {
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
        Write-Output "Resolved $($t.id)"
    }
}
