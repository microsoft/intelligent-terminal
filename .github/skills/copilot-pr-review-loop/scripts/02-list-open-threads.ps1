<#
.SYNOPSIS
    List unresolved review threads on a pull request (all reviewers).

.DESCRIPTION
    Fetches review threads via GraphQL (paginated) and prints every
    thread that is still `isResolved: false`. Threads from all reviewers
    (Copilot, humans, other bots) are included; the triage step decides
    what to do with each.

    Each thread's `comments(first:1)` is the originating review comment
    — that's where `path`, `line`, and `body` come from. Reply chains
    on the same thread are intentionally not surfaced here; this script
    is the input to triage, not to reading conversation history.

.PARAMETER Owner / .PARAMETER Repo   Optional; auto-resolved from `gh repo view`.
.PARAMETER PrNumber                  The pull request number.

.EXAMPLE
    pwsh 02-list-open-threads.ps1 -PrNumber 122
#>
[CmdletBinding()]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/_lib.ps1"

$coords = Resolve-RepoCoords -Owner $Owner -Repo $Repo
$Owner = $coords.Owner
$Repo  = $coords.Repo

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
    $ghArgs = @('-f', "query=$query", '-f', "owner=$Owner", '-f', "repo=$Repo", '-F', "pr=$PrNumber")
    if ($after) { $ghArgs += @('-f', "after=$after") }

    $data = Invoke-GhGraphQL -GhArgs $ghArgs -Context "list threads for $Owner/$Repo PR #$PrNumber"
    $page = $data.data.repository.pullRequest.reviewThreads
    $all += $page.nodes
    $after = $page.pageInfo.endCursor
} while ($page.pageInfo.hasNextPage)

$threads = $all

$open = $threads | Where-Object { -not $_.isResolved }

if (-not $open) {
    Write-Output 'No open threads.'
    return
}

foreach ($t in $open) {
    $c = $t.comments.nodes[0]
    $body = $c.body
    $path = if ($null -ne $c.line) { "$($c.path):$($c.line)" } else { $c.path }
    [pscustomobject]@{
        ThreadId   = $t.id
        Author     = $c.author.login
        Path       = $path
        CreatedAt  = $c.createdAt
        Body       = $body -replace "`r?`n", ' '
    }
}
