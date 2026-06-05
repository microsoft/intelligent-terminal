<#
.SYNOPSIS
    Re-request a Copilot code review on a pull request and verify the
    request actually landed.

.DESCRIPTION
    Triggers a Copilot review safely and verifies the trigger landed.
    Safety is the primary design constraint — the previous version of
    this script cancelled in-flight reviews via a blanket DELETE+POST
    fallback. This version protects in-flight work.

    Flow:
      1. SNAPSHOT current state: PR head SHA, latest Copilot
         `copilot_work_started` event, latest Copilot
         `review_requested` event, whether Copilot is currently in
         `requested_reviewers`, latest Copilot review's commit OID.
      2. EARLY RETURN if Copilot has already submitted a review at the
         current HEAD (nothing to trigger).
      3. EARLY RETURN if a recent `copilot_work_started` exists at the
         current HEAD without a follow-up review — that means a review
         is in flight. Triggering again would cancel it.
      4. RE-ARM if Copilot is in `requested_reviewers` but stuck (no
         work_started after the request for >5 min) — DELETE+POST the
         reviewer. This is the ONLY path that ever deletes; it never
         runs while a review is in flight.
      5. FRESH TRIGGER otherwise:
         a. REST POST `requested_reviewers[]=Copilot`. Verified by
            reading the POST response body's `requested_reviewers` and
            polling `requested_reviewers` for ~10s (the POST can return
            HTTP 201 while silently dropping Copilot — quiet-period after
            dismissal, Copilot not enabled on repo, bot not a
            collaborator).
         b. `gh pr edit --add-reviewer Copilot` as best-effort
            fallback. Known to return "not found" in many gh CLI
            versions but occasionally succeeds.
         The `copilot_work_started` event in the issue timeline is the
         authoritative success signal — HTTP / exit status alone is
         insufficient.

    If nothing triggers a `copilot_work_started` event, the script
    throws with actionable diagnostics. The canonical remedy when
    triggers are silently dropped is to push a substantive new commit
    (not whitespace / not comment-only) and retry — repo-level
    auto-assignment fires on `synchronize` and is generally reliable.

    DO NOT post `@copilot please review` (or any @copilot mention) as a
    PR comment as a workaround. That summons the Copilot **Coding
    Agent** (which makes commits), not the reviewer bot — it is a
    confirmed waste of time and has been observed across multiple
    Copilot CLI sessions. The only valid triggers are the API
    mechanisms above.

.PARAMETER Owner
    Repository owner (org or user). Defaults to the current repo's owner.

.PARAMETER Repo
    Repository name. Defaults to the current repo's name.

.PARAMETER PrNumber
    The pull request number to re-request review on.

.EXAMPLE
    pwsh 01-request-review.ps1 -PrNumber 122
#>
[CmdletBinding()]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber
)

$ErrorActionPreference = 'Stop'

if (-not $Owner -or -not $Repo) {
    $repoJson = gh repo view --json owner,name
    if ($LASTEXITCODE -ne 0) {
        throw "gh repo view failed (exit $LASTEXITCODE). Pass -Owner and -Repo explicitly or run from inside a gh-detected repo."
    }
    $repoInfo = $repoJson | ConvertFrom-Json
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

$repoArg = "$Owner/$Repo"

# Snapshot the latest "Copilot is now working on this PR" event before
# any attempt. We treat copilot_work_started — not review_requested — as
# the real success signal because it's emitted by the bot only after it
# actually picks up the request server-side. A review_requested event
# without a follow-up copilot_work_started means the bot saw the request
# but declined to queue a review.
#
# Pagination note: the REST events endpoint returns events oldest-first,
# 100 per page. We MUST --paginate (which fetches all pages) so the
# newest events are in the result; otherwise on PRs with >100 events the
# latest copilot_work_started will be silently missed and the
# verification logic will spin / falsely report "no event".
function Get-LatestCopilotWorkStarted {
    $json = gh api --paginate "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
        --jq '[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""'
    if ($LASTEXITCODE -ne 0) {
        throw "gh api events failed (exit $LASTEXITCODE) while snapshotting copilot_work_started events."
    }
    # --paginate concatenates jq output from each page; take the last line which is the newest.
    $lines = $json -split "`n" | Where-Object { $_.Trim() } | ForEach-Object { $_.Trim() }
    if (-not $lines -or $lines.Count -eq 0) { return '' }
    # Each page's --jq emitted a single timestamp; take the maximum across all pages.
    ($lines | Sort-Object | Select-Object -Last 1)
}

function Get-LatestReviewRequested {
    $json = gh api --paginate "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
        --jq '[.[] | select(.event=="review_requested" and (.requested_reviewer.login // "" | test("^(?i)copilot"))) | .created_at] | sort | .[-1] // ""'
    if ($LASTEXITCODE -ne 0) { return '' }
    $lines = $json -split "`n" | Where-Object { $_.Trim() } | ForEach-Object { $_.Trim() }
    if (-not $lines -or $lines.Count -eq 0) { return '' }
    ($lines | Sort-Object | Select-Object -Last 1)
}

function Get-PrStateSnapshot {
    # Returns a hashtable with: HeadOid, CopilotPending, LatestReviewAtHead, LatestReviewAt
    $q = @'
query($o:String!,$r:String!,$n:Int!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      headRefOid
      reviewRequests(first:50){nodes{requestedReviewer{__typename ... on User{login} ... on Bot{login}}}}
      latestReviews(first:50){nodes{author{login} submittedAt commit{oid}}}
    }
  }
}
'@
    $j = gh api graphql -f "query=$q" -f "o=$Owner" -f "r=$Repo" -F "n=$PrNumber" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "GraphQL snapshot failed (exit $LASTEXITCODE): $j" }
    $d = $j | ConvertFrom-Json
    # gh api graphql can exit 0 with HTTP 200 while returning a top-level
    # `errors` array. Check explicitly or the next line dereferences null.
    if ($d.errors) {
        $msgs = ($d.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL snapshot returned errors: $msgs"
    }
    $pr = $d.data.repository.pullRequest
    if (-not $pr) { throw "GraphQL snapshot: PR #$PrNumber not found in $Owner/$Repo." }
    $copilotPending = $false
    foreach ($n in $pr.reviewRequests.nodes) {
        if ($n.requestedReviewer.login -match '^(?i)copilot') { $copilotPending = $true; break }
    }
    $copilotReviews = @($pr.latestReviews.nodes | Where-Object { $_.author.login -match '^(?i)copilot' })
    $latest = if ($copilotReviews.Count -gt 0) { $copilotReviews | Sort-Object submittedAt -Descending | Select-Object -First 1 } else { $null }
    @{
        HeadOid              = $pr.headRefOid
        CopilotPending       = $copilotPending
        LatestCopilotReview  = $latest
    }
}

function Wait-ForCopilotWorkStarted {
    param([string]$BeforeTs, [int]$TimeoutSeconds = 30)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 5
        $now = Get-LatestCopilotWorkStarted
        if ($now -and $now -ne $BeforeTs) {
            return $now
        }
    }
    return ''
}

function Wait-ForCopilotInReviewRequests {
    param([int]$TimeoutSeconds = 15)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 3
        $snap = Get-PrStateSnapshot
        if ($snap.CopilotPending) { return $true }
    }
    return $false
}

# === PRE-CHECKS ===
# Before triggering anything, snapshot the current state. We need to handle:
#   (a) Copilot has ALREADY reviewed the current HEAD — nothing to do.
#   (b) Copilot is mid-review of the current HEAD (work_started landed but
#       no review submitted yet) — do NOT trigger again; the in-flight
#       review will land. Triggering again risks cancellation (e.g. via
#       the DELETE+POST fallback) which kills the in-flight review and
#       costs another full review cycle.
#   (c) Copilot is queued but stuck (in requested_reviewers without a
#       follow-up work_started for >5 min) — re-trigger via DELETE+POST.
#   (d) Copilot is not in requested_reviewers at all — try fresh triggers.

$snapshot = Get-PrStateSnapshot
$beforeTs = Get-LatestCopilotWorkStarted
$lastReqAt = Get-LatestReviewRequested
$headOid = $snapshot.HeadOid

# Case (a): already reviewed current HEAD
if ($snapshot.LatestCopilotReview -and $snapshot.LatestCopilotReview.commit.oid -eq $headOid) {
    Write-Host "Copilot has already submitted a review at the current HEAD ($($headOid.Substring(0,7))) on $($snapshot.LatestCopilotReview.submittedAt). Nothing to trigger. Run scripts/02-list-open-threads.ps1 to see open threads. NOTE: do NOT proceed to scripts/02-wait-for-review.ps1 -- it would wait for a newer review that was not requested."
    exit 0
}

# Case (b): in-flight review against the CURRENT HEAD.
# All three conditions must hold to treat as in-flight:
#   1. A copilot_work_started event landed recently (<12 min ago).
#   2. That work_started came AFTER the latest Copilot review_requested
#      event -- otherwise it's stale from a previous request that was
#      since superseded (e.g. HEAD advanced after the work_started).
#   3. No Copilot review exists at the current HEAD yet -- otherwise
#      it's case (a) above.
# Skipping any of these can recreate "wait for nothing": e.g. if HEAD
# advanced after a stale work_started, we'd skip the trigger and step 2
# would wait for a review that was never requested for this HEAD.
$workStartedRecent = $false
$workStartedAfterRequest = $true   # default true so the check passes when there's no review_requested record at all
if ($beforeTs) {
    $workStartedAge = (Get-Date) - [datetime]$beforeTs
    $workStartedRecent = $workStartedAge.TotalMinutes -lt 12
    if ($lastReqAt) {
        $workStartedAfterRequest = [datetime]$beforeTs -ge [datetime]$lastReqAt
    }
}
if ($workStartedRecent -and $workStartedAfterRequest) {
    Write-Host "Copilot is already reviewing the current HEAD ($($headOid.Substring(0,7))). Last copilot_work_started: $beforeTs (~$([int]$workStartedAge.TotalSeconds)s ago). NOT re-triggering — in-flight reviews must not be cancelled. Run scripts/02-wait-for-review.ps1 to wait for the submission."
    exit 0
}

$tried = @()

# Case (c): Copilot is pending but stuck (no work_started after a recent request).
# Use DELETE+POST to re-arm. This is the only path that should ever delete.
$stuckPending = $false
if ($snapshot.CopilotPending -and $lastReqAt) {
    $pendingAge = (Get-Date) - [datetime]$lastReqAt
    if ($pendingAge.TotalMinutes -gt 5 -and (-not $beforeTs -or [datetime]$lastReqAt -gt [datetime]$beforeTs)) {
        $stuckPending = $true
    }
}

if ($stuckPending) {
    Write-Host "Copilot is in requested_reviewers but stuck (no work_started after a review_requested $([int]$pendingAge.TotalMinutes)m ago). Re-arming via DELETE+POST."
    gh api -X DELETE "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" `
        -f "reviewers[]=Copilot" --silent 2>&1 | Out-Null
    $delExit = $LASTEXITCODE
    Start-Sleep -Seconds 2
    gh api -X POST "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" `
        -f "reviewers[]=Copilot" --silent 2>&1 | Out-Null
    $postExit = $LASTEXITCODE
    $tried += "DELETE+POST (re-arm stuck: DEL=$delExit POST=$postExit)"

    $afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds 30
    if ($afterTs) {
        Write-Host "Copilot review re-armed (work started at $afterTs)."
        exit 0
    }
}

# Case (d): no Copilot reviewer yet — try fresh triggers.
# We try REST POST first because `gh pr edit --add-reviewer` returns
# "not found" for the Copilot bot in current gh CLI versions regardless
# of whether Copilot is enabled on the repo — it's a `gh` limitation,
# not a repo-config signal. Don't conflate it with "not enabled".

# Mechanism 1: REST POST reviewers[]=Copilot
# Verify by reading the response body's requested_reviewers AND by
# polling. The POST can return HTTP 201 while silently dropping
# Copilot (quiet-period after recent dismissal, Copilot not enabled on
# repo, bot not a collaborator, etc.).
$postBody = gh api -X POST "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" -f "reviewers[]=Copilot" 2>&1
$postExit = $LASTEXITCODE
$tried += "REST POST (exit=$postExit)"
$postAccepted = $false
if ($postExit -eq 0) {
    try {
        $bodyJson = $postBody | ConvertFrom-Json
        foreach ($u in @($bodyJson.requested_reviewers)) {
            if ($u.login -match '^(?i)copilot') { $postAccepted = $true; break }
        }
    } catch { }
    if (-not $postAccepted) {
        # Fall back: poll requested_reviewers for ~10s in case the response body lagged.
        $postAccepted = Wait-ForCopilotInReviewRequests -TimeoutSeconds 10
    }
}

if ($postAccepted) {
    $afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds 30
    if ($afterTs) {
        Write-Host "Copilot review requested on PR #$PrNumber via REST POST (work started at $afterTs)."
        exit 0
    }
    # The REST API confirmed Copilot is in requested_reviewers, but no
    # copilot_work_started event fired within 30s. This is ambiguous:
    # the bot may simply be slow to pick up the request, OR it may have
    # dropped the request silently (rare but observed). We MUST NOT exit
    # 0 here — the script's contract is "verified by copilot_work_started
    # event". Exit 0 with a warning sends the caller into a 35-min wait
    # for a review that may never arrive ("wait for nothing").
    #
    # Try the gh pr edit fallback below; if THAT also fails, throw with
    # diagnostics. The caller can then make an informed decision (push a
    # substantive commit, wait longer, etc.) rather than blindly trusting
    # an unverified trigger.
    Write-Host "WARNING: Copilot is in requested_reviewers but no copilot_work_started event observed within 30s. Trying fallback mechanism."
}

# Mechanism 2: gh pr edit --add-reviewer Copilot
# Best-effort fallback. Known to return "not found" in many gh CLI
# versions for BOTH 'Copilot' and 'copilot-pull-request-reviewer' logins
# (the bot is not a regular collaborator). Kept as a fallback because
# behavior varies across gh-cli versions and account types.
$mech2Stderr = (gh pr edit $PrNumber --repo $repoArg --add-reviewer Copilot 2>&1 | Out-String)
$tried += "gh pr edit --add-reviewer Copilot (exit=$LASTEXITCODE)"

$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds 20
if ($afterTs) {
    Write-Host "Copilot review requested on PR #$PrNumber via gh pr edit (work started at $afterTs)."
    exit 0
}

# If the REST POST was accepted (Copilot in requested_reviewers) but no
# copilot_work_started event yet, throw a SPECIFIC diagnostic rather
# than the generic one — this is a different situation (bot might just
# be slow) and deserves its own next-step guidance.
if ($postAccepted) {
    throw @'
Copilot was successfully added to requested_reviewers, but no
copilot_work_started event landed within ~50 seconds. The bot may be
slow to pick up the request, or may have silently dropped it. The
script's contract is "verified by copilot_work_started event" -- exiting
0 here would send the caller into a long wait for a review that may
never arrive.

Recommended next steps (try in order):
  * Wait 2-5 min and rerun this script -- the bot may simply be slow.
  * Push a substantive commit -- repo auto-assign on synchronize is
    the most reliable trigger.
  * Verify Copilot Code Review is enabled on the repo + your account.
'@
}

throw @'
Copilot review trigger: all mechanisms attempted, none produced a
copilot_work_started event within the timeout.
'@ + "`n  Tried: $($tried -join ', ')" + "`n  Latest copilot_work_started before: '$beforeTs'" + "`n  Latest copilot_work_started after:  '$(Get-LatestCopilotWorkStarted)'" + "`n  Head SHA: $headOid" + @'


Most likely causes (in order of frequency):
  * Quiet-period after a recent dismissal of Copilot from this PR. After
    a `review_request_removed` event, GitHub typically suppresses
    re-adds for several minutes. Wait 5-10 min and rerun; or push a
    substantive new commit to bypass the quiet period.
  * Trivial / small diff suppressed by Copilot before any review has
    run. Push a substantive (non-whitespace, non-comment-only) commit
    and retry — this is also the canonical remedy on initial PR
    suppression.
  * Copilot Code Review is not enabled on the repo. Verify in
    repo Settings -> Code & automation -> Copilot, OR account-level
    Copilot Pro/Pro+ for personal repos.
  * The PR is in a state that blocks bot review (draft, closed, merge
    conflict, branch protection requiring approvals first).
  * Auth-scope issue — confirm "gh auth status" shows the repo scope.

ANTI-PATTERN — DO NOT DO THIS: posting "@copilot please review" (or any
@copilot mention) as a PR comment summons the Copilot **Coding Agent**
(which makes commits), NOT the reviewer bot. It will not produce a
review. This has been observed across multiple Copilot CLI sessions and
is a confirmed waste of time. The valid triggers are the API mechanisms
above — if they fail, push a substantive commit and retry; do not fall
back to @-mentions.
'@
