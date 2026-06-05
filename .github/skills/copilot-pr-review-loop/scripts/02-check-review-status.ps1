<#
.SYNOPSIS
    Snapshot the current Copilot review state of a PR. Single-shot, no waiting.

.DESCRIPTION
    ONE job: return a JSON snapshot of the PR's current Copilot
    review state. The agent (caller) decides what to do with it —
    including how long to wait between snapshots when polling for a
    new review to land. THIS SCRIPT DOES NOT WAIT.

    Output JSON fields:
      - HeadOid           : current PR HEAD SHA
      - State             : PR state (OPEN/CLOSED/MERGED)
      - LatestCopilotReview: {state, submittedAt, commitOid, bodyHead}
                            or null if Copilot has never reviewed
      - ReviewAtHead       : true iff latest Copilot review's commit.oid == HeadOid
      - NoNewComments      : true iff the latest review body matches
                             "generated no new comments" / "generated 0 comments"
      - OpenThreadCount    : number of unresolved review threads (from all reviewers)

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

if (-not $Owner -or -not $Repo) {
    $repoJson = gh repo view --json owner,name
    if ($LASTEXITCODE -ne 0) {
        $repoErr = gh repo view --json owner,name 2>&1
        throw "gh repo view failed. Pass -Owner and -Repo explicitly. Error: $repoErr"
    }
    $repoInfo = $repoJson | ConvertFrom-Json
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

# GraphQL call: head SHA + state + reviews + paginated thread counts.
# Uses `reviews(last:100)` not `latestReviews` (stale-cache behavior).
$q = @'
query($o:String!,$r:String!,$n:Int!,$after:String){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      headRefOid
      state
      reviews(last:100){nodes{author{login} state submittedAt body commit{oid}}}
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
$d = $null
do {
    $ghArgs = @('api', 'graphql', '-f', "query=$q", '-f', "o=$Owner", '-f', "r=$Repo", '-F', "n=$PrNumber")
    if ($after) { $ghArgs += @('-f', "after=$after") }
    $j = gh @ghArgs
    if ($LASTEXITCODE -ne 0) {
        $err = gh @ghArgs 2>&1
        throw "GraphQL snapshot failed (exit $LASTEXITCODE): $err"
    }
    $d = $j | ConvertFrom-Json
    if ($d.errors) {
        $msgs = ($d.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL snapshot returned errors: $msgs"
    }
    $pagePr = $d.data.repository.pullRequest
    if (-not $pagePr) { throw "PR #$PrNumber not found in $Owner/$Repo." }
    $allThreads += $pagePr.reviewThreads.nodes
    $after = $pagePr.reviewThreads.pageInfo.endCursor
} while ($pagePr.reviewThreads.pageInfo.hasNextPage)

if (-not $d) {
    throw "GraphQL snapshot returned no data for $Owner/$Repo PR #$PrNumber."
}
$pr = $d.data.repository.pullRequest
if (-not $pr) { throw "PR #$PrNumber not found in $Owner/$Repo." }

$copilotReviews = @($pr.reviews.nodes | Where-Object { $_.author.login -match '(?i)^(copilot-pull-request-reviewer(\[bot\])?|copilot(\[bot\])?)$' })
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

$openCount = ($allThreads | Where-Object { -not $_.isResolved }).Count

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
    ReviewThreadsComplete = $true
    Converged       = ($reviewAtHead -and $noNewComments -and $openCount -eq 0)
}
$result | ConvertTo-Json -Depth 5
