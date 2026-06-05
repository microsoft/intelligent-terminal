<#
.SYNOPSIS
    List unresolved review threads on a pull request.

.DESCRIPTION
    Fetches review threads via the GraphQL API and prints every thread
    that is still `isResolved: false`. There is intentionally no body
    truncation and no secondary filter: the current unresolved thread
    state is the source of truth for convergence.

    Threads from all reviewers (Copilot, humans, other bots) are
    included; the loop's triage step decides what to do with each.

.PARAMETER Owner
    Repository owner (org or user). Defaults to the current repo's owner
    (resolved via `gh repo view`).

.PARAMETER Repo
    Repository name. Defaults to the current repo's name.

.PARAMETER PrNumber
    The pull request number.

.EXAMPLE
    pwsh 02-list-open-threads.ps1 -PrNumber 122

.EXAMPLE
    pwsh 02-list-open-threads.ps1 -Owner microsoft -Repo intelligent-terminal -PrNumber 122
#>
[CmdletBinding()]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'

function Invoke-GhGraphQL {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Args,

        [Parameter(Mandatory = $true)]
        [string]$Context
    )

    $json = gh api graphql @Args
    if ($LASTEXITCODE -ne 0) {
        throw "gh api graphql failed (exit $LASTEXITCODE) [$Context]."
    }

    $data = $json | ConvertFrom-Json
    if ($data.errors) {
        $msgs = ($data.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL errors [$Context]: $msgs"
    }

    return $data
}

if (-not $Owner -or -not $Repo) {
    $repoJson = gh repo view --json owner,name
    if ($LASTEXITCODE -ne 0) {
        throw "gh repo view failed (exit $LASTEXITCODE). Pass -Owner and -Repo explicitly or run from inside a gh-detected repo."
    }
    $repoInfo = $repoJson | ConvertFrom-Json
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

    $data = Invoke-GhGraphQL -Args $args -Context "list threads for $Owner/$Repo PR #$PrNumber"
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
