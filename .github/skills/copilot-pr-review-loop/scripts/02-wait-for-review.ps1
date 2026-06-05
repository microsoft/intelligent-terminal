<#
.SYNOPSIS
    Wait for a fresh Copilot review SUBMISSION (not just queue-pickup) to
    land against the PR's current HEAD.

.DESCRIPTION
    `01-request-review.ps1` verifies that Copilot *picked up* the request
    (`copilot_work_started` event lands within ~30s). That's the trigger
    check. This script does the orthogonal job: wait for Copilot to
    actually FINISH reviewing — i.e. a new `PullRequestReview` whose
    `commit.oid` equals the current HEAD and whose `submittedAt` is newer
    than the snapshot taken before this round.

    The two checks are deliberately separate:
      - Trigger check (01)  : seconds-scale. Did the bot accept the job?
      - Completion check (this script): minutes-scale. Did the review land?

    Why this matters: a PR review loop that only verifies the trigger but
    not the submission will declare "round done" on a stale review (Copilot
    reviewed an earlier commit but never re-reviewed the latest one). The
    user has been bitten by this exact false-done pattern; treat both
    checks as hard gates.

    Timing notes (verified empirically against Copilot reviewer bot):
      - Typical review: 3-6 min after `copilot_work_started`.
      - Suppression / batching can push small-diff reviews to 15-30+ min.
      - Default timeout is 35 min for this reason. Do NOT shorten without
        understanding the suppression window — premature timeouts will
        trigger blind retries that compound the suppression.

    Status values returned in JSON:
      - ReviewCompleted    : A new Copilot review at the expected HEAD landed.
      - TimedOut           : Deadline passed with no fresh review at HEAD.
                             Caller should NOT blindly retry — verify the
                             trigger event log and consider pushing a
                             substantive commit before re-triggering.
      - HeadAdvanced       : The PR HEAD changed during the wait — someone
                             pushed. Caller should re-snapshot and re-wait.
      - Error              : Unrecoverable API/auth error.

.PARAMETER Owner
    Repository owner. Defaults to current repo via `gh repo view`.

.PARAMETER Repo
    Repository name. Defaults to current repo via `gh repo view`.

.PARAMETER PrNumber
    PR number.

.PARAMETER ExpectedHeadOid
    The PR HEAD SHA the review must be against. If omitted, the script
    snapshots the current HEAD when it starts. Pass explicitly when you
    want to wait for the review of a specific commit you just pushed.

.PARAMETER SinceTimestamp
    ISO-8601 timestamp. Only reviews submitted strictly after this count
    as "fresh". If omitted, the script uses the latest existing Copilot
    review's submittedAt (or epoch if none exists).

.PARAMETER TimeoutMinutes
    Maximum wait. Default 35 — accounts for the 30-min suppression
    window on small diffs. Do not shorten casually.

.PARAMETER PollSeconds
    Seconds between polls. Default 60. Floor enforced at 30 — faster
    polling does not make the review arrive sooner and burns API budget.

.EXAMPLE
    pwsh 02-wait-for-review.ps1 -PrNumber 122

.EXAMPLE
    pwsh 02-wait-for-review.ps1 -PrNumber 122 -ExpectedHeadOid abc123 -TimeoutMinutes 40
#>
[CmdletBinding()]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [string]$ExpectedHeadOid,
    [string]$SinceTimestamp,

    [int]$TimeoutMinutes = 35,
    [int]$PollSeconds = 60
)

$ErrorActionPreference = 'Stop'

if ($PollSeconds -lt 30) {
    Write-Warning "PollSeconds=$PollSeconds is below the 30s floor; using 30."
    $PollSeconds = 30
}

function Invoke-GhGraphQL {
    param(
        [Parameter(Mandatory = $true)][string[]]$Args,
        [Parameter(Mandatory = $true)][string]$Context
    )
    $json = gh api graphql @Args 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "gh api graphql failed (exit $LASTEXITCODE) [$Context]: $json"
    }
    $data = $json | ConvertFrom-Json
    if ($data.errors) {
        $msgs = ($data.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL errors [$Context]: $msgs"
    }
    return $data
}

function Get-PrReviewSnapshot {
    param([string]$Owner, [string]$Repo, [int]$PrNumber)

    $query = @'
query($owner:String!,$repo:String!,$pr:Int!){
  repository(owner:$owner,name:$repo){
    pullRequest(number:$pr){
      headRefOid
      state
      latestReviews(first:50){
        nodes{
          author{login}
          state
          submittedAt
          body
          commit{oid}
        }
      }
    }
  }
}
'@
    $args = @('-f', "query=$query", '-f', "owner=$Owner", '-f', "repo=$Repo", '-F', "pr=$PrNumber")
    $data = Invoke-GhGraphQL -Args $args -Context "snapshot PR #$PrNumber for completion check"
    $pr = $data.data.repository.pullRequest
    if (-not $pr) { throw "PR #$PrNumber not found in $Owner/$Repo." }

    $copilotReviews = @($pr.latestReviews.nodes | Where-Object {
        $_.author.login -match '^(?i)copilot(-pull-request-reviewer)?$'
    })

    $latest = $null
    if ($copilotReviews.Count -gt 0) {
        $latest = $copilotReviews | Sort-Object submittedAt -Descending | Select-Object -First 1
    }

    [pscustomobject]@{
        HeadOid             = $pr.headRefOid
        State               = $pr.state
        LatestCopilotReview = $latest   # may be $null
    }
}

# ---------- resolve repo ----------

if (-not $Owner -or -not $Repo) {
    $repoJson = gh repo view --json owner,name
    if ($LASTEXITCODE -ne 0) {
        throw "gh repo view failed (exit $LASTEXITCODE). Pass -Owner and -Repo explicitly."
    }
    $repoInfo = $repoJson | ConvertFrom-Json
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

# ---------- baseline ----------

$start = Get-Date
$initial = Get-PrReviewSnapshot -Owner $Owner -Repo $Repo -PrNumber $PrNumber

if ($initial.State -ne 'OPEN') {
    @{
        Status   = 'Error'
        Detail   = "PR is not OPEN (state=$($initial.State))."
        PrNumber = $PrNumber
    } | ConvertTo-Json -Depth 5
    return
}

if (-not $ExpectedHeadOid) { $ExpectedHeadOid = $initial.HeadOid }
if (-not $SinceTimestamp) {
    if ($initial.LatestCopilotReview) {
        $SinceTimestamp = $initial.LatestCopilotReview.submittedAt
    } else {
        $SinceTimestamp = '1970-01-01T00:00:00Z'
    }
}

$sinceDt = [datetime]$SinceTimestamp
Write-Host "[baseline] expectedHead=$($ExpectedHeadOid.Substring(0,7)) since=$SinceTimestamp timeout=${TimeoutMinutes}min poll=${PollSeconds}s"

# ---------- poll loop ----------

$deadline = $start.AddMinutes($TimeoutMinutes)
$result = [ordered]@{
    Owner          = $Owner
    Repo           = $Repo
    PrNumber       = $PrNumber
    Status         = 'Unknown'
    ExpectedHead   = $ExpectedHeadOid
    Since          = $SinceTimestamp
    LatestReview   = $null
    NoNewComments  = $false   # convergence condition (b); set on ReviewCompleted
    BodyHead       = $null    # first 300 chars of review body for visibility
    ElapsedSec     = 0
    Detail         = $null
}

while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds $PollSeconds
    $current = Get-PrReviewSnapshot -Owner $Owner -Repo $Repo -PrNumber $PrNumber

    if ($current.HeadOid -ne $ExpectedHeadOid) {
        $result.Status       = 'HeadAdvanced'
        $result.LatestReview = $current.LatestCopilotReview
        $result.Detail       = "PR head advanced from $($ExpectedHeadOid.Substring(0,7)) to $($current.HeadOid.Substring(0,7)) during wait. Re-snapshot and re-wait (typically: re-run 01-request-review.ps1 then 02-wait-for-review.ps1)."
        $result.ElapsedSec   = [int]((Get-Date) - $start).TotalSeconds
        Write-Host "[stop] $($result.Detail)"
        $result | ConvertTo-Json -Depth 5
        return
    }

    $latest = $current.LatestCopilotReview
    if ($latest -and [datetime]$latest.submittedAt -gt $sinceDt -and $latest.commit.oid -eq $ExpectedHeadOid) {
        $result.Status         = 'ReviewCompleted'
        $result.LatestReview   = $latest
        # Convergence condition (b) requires checking whether the review body
        # is the "generated no new comments" form. Expose this as a boolean
        # so callers can mechanically verify all three conditions from the
        # returned JSON without re-querying GitHub.
        $bodyText              = if ($latest.body) { $latest.body } else { '' }
        $result.NoNewComments  = ($bodyText -match '(?i)generated no new comments|generated\s+0\s+comments|reviewed\s+\d+\s+out\s+of\s+\d+\s+changed\s+files\s+in\s+this\s+pull\s+request\s+and\s+generated\s+no\s+new\s+comments')
        $result.BodyHead       = if ($bodyText.Length -gt 300) { $bodyText.Substring(0, 300) } else { $bodyText }
        $result.Detail         = "Copilot submitted review at $($latest.submittedAt) (state=$($latest.state)) against head $($ExpectedHeadOid.Substring(0,7)). NoNewComments=$($result.NoNewComments)."
        $result.ElapsedSec     = [int]((Get-Date) - $start).TotalSeconds
        Write-Host "[done] $($result.Detail)"
        $result | ConvertTo-Json -Depth 5
        return
    }

    $remaining = [int]($deadline - (Get-Date)).TotalSeconds
    $latestAt  = if ($latest) { $latest.submittedAt } else { '(none)' }
    $latestOid = if ($latest -and $latest.commit) { $latest.commit.oid.Substring(0,7) } else { '(none)' }
    Write-Host "[poll] no fresh review at HEAD yet (latestAt=$latestAt latestHead=$latestOid); ${remaining}s left"
}

$result.Status     = 'TimedOut'
$result.Detail     = "No Copilot review submission at HEAD $($ExpectedHeadOid.Substring(0,7)) within $TimeoutMinutes min. Do NOT blindly retry — first verify the copilot_work_started event for the trigger that should have produced this review; if it landed, the bot is suppressing the submission (small / trivial diff). Remedy: push a substantive new commit (not whitespace), then re-run 01-request-review.ps1 + 02-wait-for-review.ps1 against the new HEAD."
$result.ElapsedSec = [int]((Get-Date) - $start).TotalSeconds
$result | ConvertTo-Json -Depth 5
