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

.PARAMETER WhatIf
    Standard PowerShell switch. When set, lists threads that would be
    resolved without actually resolving them.

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
query($owner: String!, $repo: String!, $pr: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $pr) {
      reviewThreads(last: 100) {
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

$json = gh api graphql `
    -f query=$query `
    -F owner=$Owner `
    -F repo=$Repo `
    -F pr=$PrNumber

$data = $json | ConvertFrom-Json
$threads = $data.data.repository.pullRequest.reviewThreads.nodes

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
        gh api graphql -f query=$resolveMutation -F tid=$t.id | Out-Null
        Write-Output "Resolved $($t.id)"
    }
}
