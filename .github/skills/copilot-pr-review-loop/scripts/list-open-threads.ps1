<#
.SYNOPSIS
    List open, non-outdated Copilot review threads on a pull request.

.DESCRIPTION
    Fetches review threads via the GraphQL API and prints the ones that are
    both unresolved AND non-outdated — i.e. the ones that still need a
    decision in the current loop iteration.

    Outdated threads (Copilot's earlier comments on lines that have since
    been rewritten) are not actionable in the current round and should be
    cleaned up at convergence via cleanup-outdated.ps1.

.PARAMETER Owner
    Repository owner (org or user).

.PARAMETER Repo
    Repository name.

.PARAMETER PrNumber
    The pull request number.

.PARAMETER MaxBodyLength
    Truncate each comment body to this many characters when printing.
    Defaults to 400.

.EXAMPLE
    pwsh list-open-threads.ps1 -Owner microsoft -Repo intelligent-terminal -PrNumber 122
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Owner,

    [Parameter(Mandatory = $true)]
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [int]$MaxBodyLength = 400
)

$ErrorActionPreference = 'Stop'

$query = @'
query($owner: String!, $repo: String!, $pr: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $pr) {
      reviewThreads(last: 50) {
        nodes {
          id
          isResolved
          isOutdated
          comments(first: 1) {
            nodes {
              author { login }
              body
              path
              line
              createdAt
            }
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

$open = $threads | Where-Object {
    -not $_.isResolved -and
    -not $_.isOutdated -and
    $_.comments.nodes[0].author.login -eq 'copilot-pull-request-reviewer'
}

if (-not $open) {
    Write-Output 'No open Copilot threads.'
    return
}

foreach ($t in $open) {
    $c = $t.comments.nodes[0]
    $body = $c.body
    if ($body.Length -gt $MaxBodyLength) {
        $body = $body.Substring(0, $MaxBodyLength) + '...'
    }
    [pscustomobject]@{
        ThreadId  = $t.id
        Path      = "$($c.path):$($c.line)"
        CreatedAt = $c.createdAt
        Body      = $body -replace "`r?`n", ' '
    }
}
