<#
.SYNOPSIS
    List unresolved review threads on a pull request.

.DESCRIPTION
    Fetches review threads via the GraphQL API and prints every thread
    that is still `isResolved: false` — including threads marked
    `isOutdated: true` (e.g. because a prior round's fix shifted the
    cited lines). Each entry carries an `IsOutdated` column so triage
    can see the distinction at a glance.

    Why outdated threads are NOT filtered out: a thread that was
    actionable when Copilot posted it can BECOME outdated mid-round
    when your own fix shifts its cited lines. Filtering on
    `!isOutdated` would silently drop those threads from the per-round
    list, leaving them unresolved in the PR UI even after the
    underlying code is fixed. Reply + resolve every unresolved thread,
    outdated or not.

    Threads from all reviewers (Copilot, humans, other bots) are
    included; the loop's triage step decides what to do with each.

.PARAMETER Owner
    Repository owner (org or user). Defaults to the current repo's owner
    (resolved via `gh repo view`).

.PARAMETER Repo
    Repository name. Defaults to the current repo's name.

.PARAMETER PrNumber
    The pull request number.

.PARAMETER MaxBodyLength
    Truncate each comment body to this many characters when printing.
    Defaults to 400.

.PARAMETER ExcludeOutdated
    Suppress threads whose code has since been rewritten
    (`isOutdated: true`). Off by default — outdated-but-unresolved
    threads still need a reply + resolve in the current round. Pass
    this flag only when you specifically want the legacy filter
    (e.g. for a quick scan of "what's actionable on current lines").

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
    [int]$PrNumber,

    [int]$MaxBodyLength = 400,

    [switch]$ExcludeOutdated
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

    $data = Invoke-GhGraphQL -Args $args -Context "list threads for $Owner/$Repo PR #$PrNumber"
    $page = $data.data.repository.pullRequest.reviewThreads
    $all += $page.nodes
    $after = $page.pageInfo.endCursor
} while ($page.pageInfo.hasNextPage)

$threads = $all

$open = $threads | Where-Object {
    -not $_.isResolved -and
    (-not $ExcludeOutdated -or -not $_.isOutdated)
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
        ThreadId   = $t.id
        Author     = $c.author.login
        Path       = "$($c.path):$($c.line)"
        IsOutdated = $t.isOutdated
        CreatedAt  = $c.createdAt
        Body       = $body -replace "`r?`n", ' '
    }
}
