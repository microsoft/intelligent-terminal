<#
.SYNOPSIS
    Ensure Copilot is reviewing the given PR. Single-job script.

.DESCRIPTION
    ONE job: request Copilot review on the PR and verify the trigger
    landed. Returns a JSON status object. The caller (agent) decides
    what to do next — including how long to wait for the review to
    actually submit. THIS SCRIPT DOES NOT WAIT FOR THE REVIEW.

    Success contract (exit 0):
      - JSON with Status = "TriggerLanded" — Copilot has accepted the
        request (verified by a copilot_work_started event newer than
        the snapshot taken before the trigger), OR
      - JSON with Status = "InFlight" — Copilot is already actively
        reviewing (recent copilot_work_started newer than the latest
        submitted review). No re-trigger needed.

    Failure contract (throw, exit 1):
      - All trigger mechanisms attempted, no verifiable
        copilot_work_started event. The thrown error explains likely
        cause (quiet-period, repo config, etc.). Caller should push a
        substantive commit and retry.

    Trigger mechanisms attempted (in order, until one verifies):
      1. GraphQL `requestReviewsByLogin` with
         `botLogins:["copilot-pull-request-reviewer"]` (primary)
      2. REST POST `requested_reviewers[]=Copilot` (fallback)
      3. `gh pr edit --add-reviewer Copilot` (last-ditch)

    Three GraphQL traps documented in api-quirks.md (took ~2 hours to
    find): use `requestReviewsByLogin` not `requestReviews`,
    `botLogins` not `userLogins`, `copilot-pull-request-reviewer` slug
    not `Copilot` display login.

    In-flight protection: GitHub does not expose the head SHA on
    copilot_work_started events, so this script treats a recent event
    that is newer than the latest submitted review as in-flight and
    returns Status="InFlight" without re-triggering. Re-triggering would
    risk cancelling the in-flight review.

    DO NOT post `@copilot` PR comments as a trigger — that summons the
    Copilot Coding Agent, not the reviewer bot.

.PARAMETER PrNumber
    The pull request number. The only required parameter.

.PARAMETER Owner
    Repository owner. OPTIONAL — auto-resolved from `gh repo view`.

.PARAMETER Repo
    Repository name. OPTIONAL — auto-resolved from `gh repo view`.

.PARAMETER TriggerWaitSeconds
    Max seconds to wait for the copilot_work_started event after
    triggering. Default 30. This is a short verification poll, NOT a
    wait-for-review-completion — that's the caller's job. Keep it
    short; if no event in 30s the trigger almost certainly failed.

.EXAMPLE
    # Canonical usage — agent calls this, gets JSON back, decides
    # when/how to check for the actual review submission.
    pwsh 01-request-review.ps1 -PrNumber 236

    # JSON output (on TriggerLanded):
    # {"Status":"TriggerLanded","HeadOid":"...","WorkStartedAt":"..."}

    # JSON output (on InFlight):
    # {"Status":"InFlight","HeadOid":"...","WorkStartedAt":"..."}
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [string]$Owner,
    [string]$Repo,

    [int]$TriggerWaitSeconds = 30
)

$ErrorActionPreference = 'Stop'

# ---------- helpers ----------

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

# Event log endpoint returns events oldest-first, 100/page. MUST use
# --paginate to see the newest events on busy PRs.
function Get-LatestCopilotWorkStarted {
    $json = gh api --paginate "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
        --jq '[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""' 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "gh api events failed (exit $LASTEXITCODE): $json"
    }
    $lines = $json -split "`n" | Where-Object { $_.Trim() } | ForEach-Object { $_.Trim() }
    if (-not $lines -or $lines.Count -eq 0) { return '' }
    ($lines | Sort-Object | Select-Object -Last 1)
}

function Get-CurrentSnapshot {
    # Single GraphQL call: head SHA + Copilot review state.
    # Uses `reviews(last:50)` not `latestReviews` (which has stale-cache behavior).
    $q = @'
query($o:String!,$r:String!,$n:Int!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      headRefOid
      state
      reviews(last:50){nodes{author{login} submittedAt commit{oid}}}
    }
  }
}
'@
    $j = gh api graphql -f "query=$q" -f "o=$Owner" -f "r=$Repo" -F "n=$PrNumber" 2>&1
    if ($LASTEXITCODE -ne 0) { throw "GraphQL snapshot failed (exit $LASTEXITCODE): $j" }
    $d = $j | ConvertFrom-Json
    if ($d.errors) {
        $msgs = ($d.errors | ForEach-Object { $_.message }) -join '; '
        throw "GraphQL snapshot returned errors: $msgs"
    }
    $pr = $d.data.repository.pullRequest
    if (-not $pr) { throw "PR #$PrNumber not found in $Owner/$Repo." }
    $copilotReviews = @($pr.reviews.nodes | Where-Object { $_.author.login -match '^(?i)(copilot-pull-request-reviewer|copilot)$' })
    $latest = if ($copilotReviews.Count -gt 0) { $copilotReviews | Sort-Object submittedAt -Descending | Select-Object -First 1 } else { $null }
    [pscustomobject]@{
        HeadOid             = $pr.headRefOid
        State               = $pr.state
        LatestCopilotReview = $latest
    }
}

function Wait-ForCopilotWorkStarted {
    param([string]$BeforeTs, [int]$TimeoutSeconds)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 5
        $now = Get-LatestCopilotWorkStarted
        if ($now -and $now -ne $BeforeTs) { return $now }
    }
    return ''
}

# ---------- resolve repo ----------

if (-not $Owner -or -not $Repo) {
    $repoJson = gh repo view --json owner,name
    if ($LASTEXITCODE -ne 0) {
        throw "gh repo view failed. Pass -Owner and -Repo explicitly."
    }
    $repoInfo = $repoJson | ConvertFrom-Json
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

# ---------- pre-trigger snapshot ----------

$snapshot = Get-CurrentSnapshot
$beforeTs = Get-LatestCopilotWorkStarted
$headOid  = $snapshot.HeadOid

if ($snapshot.State -ne 'OPEN') {
    throw "PR #$PrNumber is not OPEN (state=$($snapshot.State), head=$headOid)."
}

# In-flight protection: if a copilot_work_started exists that's newer
# than the latest Copilot review's submittedAt, treat as in-flight.
$beforeDt       = ToUtcDt $beforeTs
$lastReviewDt   = if ($snapshot.LatestCopilotReview) { ToUtcDt $snapshot.LatestCopilotReview.submittedAt } else { $null }
$inFlight       = $false
if ($beforeDt) {
    $ageMin = ((Get-Date).ToUniversalTime() - $beforeDt).TotalMinutes
    $consumed = $lastReviewDt -and $beforeDt -le $lastReviewDt
    $inFlight = ($ageMin -lt 35) -and (-not $consumed)
}
if ($inFlight) {
    @{
        Status        = 'InFlight'
        PrNumber      = $PrNumber
        HeadOid       = $headOid
        WorkStartedAt = $beforeTs
        Detail        = "Copilot work_started at $beforeTs (~$([int]$ageMin)min ago) is newer than any submitted review. Not re-triggering."
    } | ConvertTo-Json -Compress
    exit 0
}

# ---------- trigger ----------

$tried = @()

# Mechanism 1 (PRIMARY): GraphQL requestReviewsByLogin
$prIdResp = gh api graphql -f "query=query{repository(owner:`"$Owner`",name:`"$Repo`"){pullRequest(number:$PrNumber){id}}}" --jq '.data.repository.pullRequest.id' 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "Failed to resolve PR node id (exit $LASTEXITCODE): $prIdResp"
}
$prNodeId = ($prIdResp | Out-String).Trim()
if ([string]::IsNullOrWhiteSpace($prNodeId) -or $prNodeId -eq 'null') {
    throw "Failed to resolve PR node id for $Owner/$Repo PR #$PrNumber. GraphQL returned '$prNodeId'."
}
$mut = 'mutation($p:ID!){requestReviewsByLogin(input:{pullRequestId:$p,botLogins:["copilot-pull-request-reviewer"]}){pullRequest{number}}}'
$mutResp = gh api graphql -f "query=$mut" -f "p=$prNodeId" 2>&1
$mutExit = $LASTEXITCODE
$tried += "GraphQL requestReviewsByLogin (exit=$mutExit)"

$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds $TriggerWaitSeconds
if ($afterTs) {
    @{
        Status        = 'TriggerLanded'
        PrNumber      = $PrNumber
        HeadOid       = $headOid
        WorkStartedAt = $afterTs
        Detail        = "Triggered via GraphQL requestReviewsByLogin; copilot_work_started at $afterTs."
    } | ConvertTo-Json -Compress
    exit 0
}

# Mechanism 2 (FALLBACK): REST POST
$null = gh api -X POST "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" -f 'reviewers[]=Copilot' 2>&1
$tried += "REST POST (exit=$LASTEXITCODE)"
$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds $TriggerWaitSeconds
if ($afterTs) {
    @{
        Status        = 'TriggerLanded'
        PrNumber      = $PrNumber
        HeadOid       = $headOid
        WorkStartedAt = $afterTs
        Detail        = "Triggered via REST POST requested_reviewers; copilot_work_started at $afterTs."
    } | ConvertTo-Json -Compress
    exit 0
}

# Mechanism 3 (LAST-DITCH): gh pr edit
gh pr edit $PrNumber --repo "$Owner/$Repo" --add-reviewer Copilot 2>&1 | Out-Null
$tried += "gh pr edit (exit=$LASTEXITCODE)"
$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds $TriggerWaitSeconds
if ($afterTs) {
    @{
        Status        = 'TriggerLanded'
        PrNumber      = $PrNumber
        HeadOid       = $headOid
        WorkStartedAt = $afterTs
        Detail        = "Triggered via gh pr edit --add-reviewer; copilot_work_started at $afterTs."
    } | ConvertTo-Json -Compress
    exit 0
}

# All mechanisms attempted, no event. Throw with diagnostic.
throw @'
Copilot review trigger: all mechanisms attempted, none produced a
copilot_work_started event within the timeout.
'@ + "`n  Tried: $($tried -join ', ')" + "`n  Latest copilot_work_started before: '$beforeTs'" + "`n  Latest copilot_work_started after:  '$(Get-LatestCopilotWorkStarted)'" + "`n  Head SHA: $headOid" + @'


Most likely causes:
  * Quiet-period after a recent dismissal — wait 5-10 min, or push a
    substantive commit (auto-assign on synchronize is the most reliable
    trigger).
  * Trivial diff suppressed by Copilot — push a substantive commit.
  * Copilot Code Review not enabled on the repo / account.
  * PR in a state that blocks bot review (draft, closed, conflict).

DO NOT post @copilot comments as a workaround — that summons the
Coding Agent, not the reviewer.
'@
