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

    This script does not decide whether review comments are still open or
    actionable. It reports whether the Copilot review has not started,
    started but not finished, or finished after the expected commit. Use
    02-list-open-threads.ps1 as the source of truth for comments from any
    reviewer.

    Timing notes (verified empirically against Copilot reviewer bot):
      - Typical review: 3-6 min after `copilot_work_started`.
      - Suppression / batching can push small-diff reviews to 15-30+ min.
      - Default timeout is 35 min for this reason. Do NOT shorten without
        understanding the suppression window — premature timeouts will
        trigger blind retries that compound the suppression.

    Status values returned in JSON:
      - ReviewCompleted    : A new Copilot review at the expected HEAD landed.
      - TimedOut           : Deadline passed with no fresh review at HEAD.
                             The ReviewProgress field distinguishes
                             NoReviewStarted from ReviewStartedNotCompleted.
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
    # Wait against a specific commit (full 40-char SHA required — the
    # comparison is exact-equality against the GraphQL commit.oid).
    pwsh 02-wait-for-review.ps1 -PrNumber 122 -ExpectedHeadOid 7b88ffc4cc5d40ca5b307be50056adc54e7e20e0 -TimeoutMinutes 40
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

# Safe SHA truncation for log lines. Strings shorter than 7 chars
# (short SHAs, partial inputs) are returned as-is rather than throwing,
# so a display helper can never prevent emission of a status JSON.
function Short {
    param([string]$Sha)
    if (-not $Sha) { return '(none)' }
    if ($Sha.Length -le 7) { return $Sha }
    return $Sha.Substring(0, 7)
}

function Get-PrReviewSnapshot {
    param([string]$Owner, [string]$Repo, [int]$PrNumber)

    # IMPORTANT: We use `reviews(last: 50)` instead of `latestReviews`.
    # `latestReviews` is documented as "latest per user" but empirically
    # exhibits stale-cache behavior: a fresh Copilot review can be
    # absent from `latestReviews` for several minutes after submission
    # while the standard `reviews` connection (and REST /reviews) shows
    # it immediately. Using the stale view causes the wait/convergence
    # logic to see an outdated commit OID and either falsely declare
    # convergence on the wrong commit or TimeOut waiting for a review
    # that already exists. The `reviews(last: N)` form is the
    # authoritative source.
    $query = @'
query($owner:String!,$repo:String!,$pr:Int!){
  repository(owner:$owner,name:$repo){
    pullRequest(number:$pr){
      headRefOid
      state
      reviews(last:50){
        nodes{
          author{login}
          state
          submittedAt
          body
          commit{oid}
        }

        function Get-LatestCopilotWorkStarted {
            $json = gh api --paginate "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
                --jq '[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""' 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw "gh api events failed (exit $LASTEXITCODE) while fetching copilot_work_started events: $json"
            }
            $lines = $json -split "`n" | Where-Object { $_.Trim() } | ForEach-Object { $_.Trim() }
            if (-not $lines -or $lines.Count -eq 0) { return '' }
            ($lines | Sort-Object | Select-Object -Last 1)
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

    $copilotReviews = @($pr.reviews.nodes | Where-Object {
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

# UTC-normalizing parser. PowerShell's ConvertFrom-Json strips the Z
# from ISO-8601 timestamps and returns DateTime with Kind=Unspecified,
# treating the literal hours as local time. Comparing a JSON-parsed
# DateTime to one created via [datetime]"...Z" silently gives wrong
# answers (off by the local UTC offset). All timestamp comparisons MUST
# go through ToUtcDt.
function ToUtcDt {
    param($Value)
    if ($null -eq $Value -or $Value -eq '') { return $null }
    $s = if ($Value -is [datetime]) {
        if ($Value.Kind -eq [System.DateTimeKind]::Unspecified) {
            [System.DateTime]::SpecifyKind($Value, [System.DateTimeKind]::Utc).ToString('o')
        } else {
            $Value.ToUniversalTime().ToString('o')
        }
    } else {
        [string]$Value
    }
    return [datetime]::Parse(
        $s,
        [System.Globalization.CultureInfo]::InvariantCulture,
        [System.Globalization.DateTimeStyles]::AdjustToUniversal -bor [System.Globalization.DateTimeStyles]::AssumeUniversal
    )
}

$sinceDt = ToUtcDt $SinceTimestamp
Write-Host "[baseline] expectedHead=$(Short $ExpectedHeadOid) since=$SinceTimestamp timeout=${TimeoutMinutes}min poll=${PollSeconds}s"

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
    LatestWorkStarted = $null
    ReviewProgress = 'Unknown'
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
        $result.Detail       = "PR head advanced from $(Short $ExpectedHeadOid) to $(Short $current.HeadOid) during wait. Re-snapshot and re-wait (typically: re-run 01-request-review.ps1 then 02-wait-for-review.ps1)."
        $result.ElapsedSec   = [int]((Get-Date) - $start).TotalSeconds
        Write-Host "[stop] $($result.Detail)"
        $result | ConvertTo-Json -Depth 5
        return
    }

    $latest = $current.LatestCopilotReview
    $latestDt = if ($latest) { ToUtcDt $latest.submittedAt } else { $null }
    if ($latest -and $latestDt -gt $sinceDt -and $latest.commit.oid -eq $ExpectedHeadOid) {
        $result.Status         = 'ReviewCompleted'
        $result.LatestReview   = $latest
        $result.LatestWorkStarted = Get-LatestCopilotWorkStarted
        $result.ReviewProgress = 'ReviewStartedAndCompleted'
        # Convergence condition (b) requires checking whether the review body
        # is the "generated no new comments" form. Expose this as a boolean
        # so callers can mechanically verify all three conditions from the
        # returned JSON without re-querying GitHub.
        $bodyText              = if ($latest.body) { $latest.body } else { '' }
        $result.NoNewComments  = ($bodyText -match '(?i)generated no new comments|generated\s+0\s+comments|reviewed\s+\d+\s+out\s+of\s+\d+\s+changed\s+files\s+in\s+this\s+pull\s+request\s+and\s+generated\s+no\s+new\s+comments')
        $result.BodyHead       = if ($bodyText.Length -gt 300) { $bodyText.Substring(0, 300) } else { $bodyText }
        $result.Detail         = "Copilot submitted review at $($latest.submittedAt) (state=$($latest.state)) against head $(Short $ExpectedHeadOid). NoNewComments=$($result.NoNewComments)."
        $result.ElapsedSec     = [int]((Get-Date) - $start).TotalSeconds
        Write-Host "[done] $($result.Detail)"
        $result | ConvertTo-Json -Depth 5
        return
    }

    $remaining = [int]($deadline - (Get-Date)).TotalSeconds
    $latestAt  = if ($latest) { $latest.submittedAt } else { '(none)' }
    $latestOid = if ($latest -and $latest.commit) { $(Short $latest.commit.oid) } else { '(none)' }
    Write-Host "[poll] no fresh review at HEAD yet (latestAt=$latestAt latestHead=$latestOid); ${remaining}s left"
}

$result.Status     = 'TimedOut'
$latestWorkStarted = Get-LatestCopilotWorkStarted
$result.LatestWorkStarted = $latestWorkStarted
$latestWorkStartedDt = ToUtcDt $latestWorkStarted
if ($latestWorkStartedDt -and $latestWorkStartedDt -gt $sinceDt) {
    $result.ReviewProgress = 'ReviewStartedNotCompleted'
    $result.Detail = "Copilot review started at $latestWorkStarted but no review submission at HEAD $(Short $ExpectedHeadOid) landed within $TimeoutMinutes min. Do NOT blindly retry; the bot may still be suppressing or delaying the submission. Remedy: push a substantive new commit (not whitespace), then re-run 01-request-review.ps1 + 02-wait-for-review.ps1 against the new HEAD."
} else {
    $result.ReviewProgress = 'NoReviewStarted'
    $result.Detail = "No copilot_work_started event newer than '$SinceTimestamp' and no Copilot review submission at HEAD $(Short $ExpectedHeadOid) landed within $TimeoutMinutes min. Re-run 01-request-review.ps1; if it cannot produce a verified pickup event, push a substantive new commit and retry."
}
$result.ElapsedSec = [int]((Get-Date) - $start).TotalSeconds
$result | ConvertTo-Json -Depth 5
