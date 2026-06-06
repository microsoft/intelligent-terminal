<#
.SYNOPSIS
    Snapshot the current Copilot review state of a PR. Single-shot, no waiting.

.DESCRIPTION
    ONE job: return a JSON snapshot of the PR's current Copilot
    review state. The agent (caller) decides what to do with it —
    including how long to wait between snapshots when polling for a
    new review to land. THIS SCRIPT DOES NOT WAIT.

    Output JSON fields:
      - PrNumber, Owner, Repo
      - HeadOid           : current PR HEAD SHA
      - State             : PR state (OPEN/CLOSED/MERGED)
      - LatestCopilotReview: {state, submittedAt, commitOid, bodyHead}
                            or null if Copilot has never reviewed
      - ReviewAtHead       : true iff latest Copilot review's commit.oid == HeadOid
      - NoNewComments      : true iff the latest review body matches
                             "generated no new comments" / "generated 0 comments"
      - OpenThreadCount    : number of unresolved review threads (from all reviewers)
      - Converged          : true iff ReviewAtHead && NoNewComments && OpenThreadCount==0

    Typical agent loop:
      1. Call 01-request-review.ps1 → get TriggerLanded
      2. Schedule a check N minutes later
      3. Call this script (02-check-review-status.ps1)
      4. If ReviewAtHead && NoNewComments && OpenThreadCount==0 → converged
      5. Otherwise, fetch threads via 02-list-open-threads.ps1, triage, fix, repeat

.PARAMETER PrNumber
    The pull request number. The only required parameter.

.PARAMETER Owner
    Repository owner. OPTIONAL — auto-resolved from `gh repo view`.

.PARAMETER Repo
    Repository name. OPTIONAL — auto-resolved from `gh repo view`.

.EXAMPLE
    pwsh 02-check-review-status.ps1 -PrNumber 236

    # Output (converged):
    # {"HeadOid":"abc...","State":"OPEN","LatestCopilotReview":{...},"ReviewAtHead":true,"NoNewComments":true,"OpenThreadCount":0}

    # Output (not converged — new findings):
    # {"HeadOid":"abc...","ReviewAtHead":true,"NoNewComments":false,"OpenThreadCount":3,...}
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [string]$Owner,
    [string]$Repo
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/_lib.ps1"

$coords = Resolve-RepoCoords -Owner $Owner -Repo $Repo
$Owner = $coords.Owner
$Repo  = $coords.Repo

# Query A (once): PR head/state/reviews. Reviews are not paginated
# here — `reviews(last:100)` is the most recent 100 reviews, sufficient
# for finding the latest Copilot review.
$qHead = @'
query($o:String!,$r:String!,$n:Int!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      headRefOid
      state
      reviews(last:100){nodes{author{login} state submittedAt body commit{oid}}}
    }
  }
}
'@

$r = Invoke-Gh -GhArgs @('api','graphql','-f',"query=$qHead",'-f',"o=$Owner",'-f',"r=$Repo",'-F',"n=$PrNumber")
if ($r.ExitCode -ne 0) {
    throw "GraphQL head query failed (exit $($r.ExitCode)): $($r.Stderr) $($r.Stdout)"
}
$d = $r.Stdout | ConvertFrom-Json
if ($d.errors) {
    $msgs = ($d.errors | ForEach-Object { $_.message }) -join '; '
    throw "GraphQL head query returned errors: $msgs"
}
$pr = $d.data.repository.pullRequest
if (-not $pr) { throw "PR #$PrNumber not found in $Owner/$Repo." }

# Query B (paginated): reviewThreads only — separated so we don't
# re-fetch the full review bodies on every page.
$qThreads = @'
query($o:String!,$r:String!,$n:Int!,$after:String){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      reviewThreads(first:100, after:$after){
        pageInfo{endCursor hasNextPage}
        nodes{isResolved}
      }
    }
  }
}
'@

$after = $null
$allThreads = @()
do {
    $ghArgs = @('api', 'graphql', '-f', "query=$qThreads", '-f', "o=$Owner", '-f', "r=$Repo", '-F', "n=$PrNumber")
    if ($after) { $ghArgs += @('-f', "after=$after") }
    $r = Invoke-Gh -GhArgs $ghArgs
    if ($r.ExitCode -ne 0) {
        throw "GraphQL threads query failed (exit $($r.ExitCode)): $($r.Stderr) $($r.Stdout)"
    }
    $threadResp = $r.Stdout | ConvertFrom-Json
    if ($threadResp.errors) {
        $msgs = ($threadResp.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL threads query returned errors: $msgs"
    }
    $payload = $threadResp | Select-Object -ExpandProperty data
    $pagePr = $payload.repository.pullRequest
    if (-not $pagePr) { throw "PR #$PrNumber not found in $Owner/$Repo (threads page)." }
    $allThreads += $pagePr.reviewThreads.nodes
    $after = $pagePr.reviewThreads.pageInfo.endCursor
} while ($pagePr.reviewThreads.pageInfo.hasNextPage)

$copilotReviews = @($pr.reviews.nodes | Where-Object { $_.author.login -match '(?i)^copilot-pull-request-reviewer(\[bot\])?$' })
$latest = if ($copilotReviews.Count -gt 0) { $copilotReviews | Sort-Object submittedAt -Descending | Select-Object -First 1 } else { $null }

$reviewAtHead = $false
$noNewComments = $false
$bodyHead = $null
if ($latest) {
    $reviewAtHead = ($latest.commit.oid -eq $pr.headRefOid)
    $bodyText = if ($latest.body) { $latest.body } else { '' }
    $noNewComments = ($bodyText -match '(?i)generated no new comments|generated\s+0\s+comments|reviewed\s+\d+\s+out\s+of\s+\d+\s+changed\s+files\s+in\s+this\s+pull\s+request\s+and\s+generated\s+no\s+new\s+comments')
    $bodyHead = if ($bodyText.Length -gt 300) { $bodyText.Substring(0, 300) } else { $bodyText }
}

$openThreads = @($allThreads | Where-Object { -not $_.isResolved })
$openCount = $openThreads.Count

$result = [ordered]@{
    PrNumber            = $PrNumber
    Owner               = $Owner
    Repo                = $Repo
    HeadOid             = $pr.headRefOid
    State               = $pr.state
    LatestCopilotReview = if ($latest) {
        [ordered]@{
            state       = $latest.state
            submittedAt = $latest.submittedAt
            commitOid   = $latest.commit.oid
            bodyHead    = $bodyHead
        }
    } else { $null }
    ReviewAtHead    = $reviewAtHead
    NoNewComments   = $noNewComments
    OpenThreadCount = $openCount
    Converged       = ($reviewAtHead -and $noNewComments -and $openCount -eq 0)
}
$result | ConvertTo-Json -Depth 5 -Compress
