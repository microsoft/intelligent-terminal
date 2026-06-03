<#
.SYNOPSIS
    List open, non-outdated review threads on a pull request.

.DESCRIPTION
    Fetches review threads via the GraphQL API and prints the ones that are
    both unresolved AND non-outdated — i.e. the ones that still need a
    decision in the current loop iteration. Threads from all reviewers
    (Copilot, humans, other bots) are included; the loop's triage step
    decides what to do with each.

    Outdated threads (earlier comments on lines that have since been
    rewritten) are not actionable in the current round and should be
    cleaned up at convergence via 09-cleanup-outdated.ps1.

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
    pwsh 02-list-open-threads.ps1 -Owner microsoft -Repo intelligent-terminal -PrNumber 122
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

$open = $threads | Where-Object {
    -not $_.isResolved -and
    -not $_.isOutdated
}

if (-not $open) {
    Write-Output 'No open threads.'
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
        Author    = $c.author.login
        Path      = "$($c.path):$($c.line)"
        CreatedAt = $c.createdAt
        Body      = $body -replace "`r?`n", ' '
    }
}
