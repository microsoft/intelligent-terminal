<#
.SYNOPSIS
    Ensure Copilot is reviewing the PR. Single job, single mechanism.

.DESCRIPTION
    ONE job: request Copilot review and verify the trigger landed.

    Single mechanism (no fallbacks — empirically the most reliable):
    GraphQL `requestReviewsByLogin` with
    `botLogins:["copilot-pull-request-reviewer"]`. If this fails, the
    script throws — the caller knows to push a substantive commit and
    retry, NOT to combine more "best-effort" mechanisms that lie about
    success.

    Success contract (exit 0, JSON):
      - Status="InFlight" — Copilot is currently a requested reviewer
        on the PR. Nothing to do; caller waits.
      - Status="TriggerLanded" — the script just triggered and
        verified the copilot_work_started event landed.

    Failure contract (throw, exit 1):
      - GraphQL mutation failed, OR no copilot_work_started event
        landed within the verification window. Caller should push a
        substantive commit (auto-assign on synchronize is the most
        reliable trigger).

    Three GraphQL traps documented in api-quirks.md (took ~2h to find):
      - Mutation is `requestReviewsByLogin` (NOT `requestReviews`)
      - Field is `botLogins` (NOT `userLogins`)
      - Slug is `copilot-pull-request-reviewer` (NOT `Copilot`)

    DO NOT post `@copilot` PR comments — summons the Coding Agent.

.PARAMETER PrNumber
    PR number. The only required parameter.

.PARAMETER Owner
    Optional — auto-resolved from `gh repo view`.

.PARAMETER Repo
    Optional — auto-resolved from `gh repo view`.

.PARAMETER VerifySeconds
    Seconds to wait for copilot_work_started event after triggering.
    Default 30. This is a verification poll, NOT a wait-for-review.

.EXAMPLE
    pwsh 01-request-review.ps1 -PrNumber 236
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [string]$Owner,
    [string]$Repo,

    [int]$VerifySeconds = 30
)

$ErrorActionPreference = 'Stop'

# ---------- repo resolve ----------

if (-not $Owner -or -not $Repo) {
    $repoJson = gh repo view --json owner,name
    if ($LASTEXITCODE -ne 0) {
        $repoErr = gh repo view --json owner,name 2>&1
        throw "gh repo view failed: $repoErr"
    }
    $repoInfo = $repoJson | ConvertFrom-Json
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

# ---------- state: is Copilot currently requested? ----------
# Single GraphQL query: requested reviewers + head SHA.

$stateQuery = @'
query($o:String!,$r:String!,$n:Int!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      headRefOid
      state
      reviewRequests(first:50){nodes{requestedReviewer{__typename ... on Bot{login}}}}
    }
  }
}
'@
$stateResp = gh api graphql -f "query=$stateQuery" -f "o=$Owner" -f "r=$Repo" -F "n=$PrNumber" 2>&1
if ($LASTEXITCODE -ne 0) { throw "state query failed: $stateResp" }
$stateData = $stateResp | ConvertFrom-Json
if ($stateData.errors) {
    throw "state query GraphQL errors: $(($stateData.errors | ForEach-Object {$_.message}) -join '; ')"
}
$pr = $stateData.data.repository.pullRequest
if (-not $pr) { throw "PR #$PrNumber not found in $Owner/$Repo." }
if ($pr.state -ne 'OPEN') {
    throw "PR #$PrNumber is not OPEN (state=$($pr.state))."
}

$headOid = $pr.headRefOid
$copilotPending = ($pr.reviewRequests.nodes | Where-Object { $_.requestedReviewer.login -match '(?i)^(copilot-pull-request-reviewer(\[bot\])?|copilot(\[bot\])?)$' }).Count -gt 0

# If Copilot is currently in requested_reviewers, it's in-flight by definition.
if ($copilotPending) {
    @{
        Status   = 'InFlight'
        PrNumber = $PrNumber
        HeadOid  = $headOid
        Detail   = "Copilot is currently in requested_reviewers; review is in flight."
    } | ConvertTo-Json -Compress
    exit 0
}

# We do NOT short-circuit on AlreadyReviewed — the user wants re-request
# as a first-class flow. Re-trigger; the GraphQL mutation handles both
# initial-add and re-request identically.

# ---------- snapshot copilot_work_started before triggering ----------

$beforeTs = gh api --paginate "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
    --jq '[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""' 2>&1
if ($LASTEXITCODE -ne 0) { throw "events query failed: $beforeTs" }
$beforeTs = (@($beforeTs -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ }) | Sort-Object | Select-Object -Last 1)
if (-not $beforeTs) { $beforeTs = '' }

# ---------- trigger via GraphQL requestReviewsByLogin ----------

$prIdQuery = "query{repository(owner:`"$Owner`",name:`"$Repo`"){pullRequest(number:$PrNumber){id}}}"
$prIdResp = gh api graphql -f "query=$prIdQuery"
if ($LASTEXITCODE -ne 0) {
    $prIdErr = gh api graphql -f "query=$prIdQuery" 2>&1
    throw "PR node id query failed: $prIdErr"
}
$prIdJson = $prIdResp | ConvertFrom-Json
if ($prIdJson.errors) {
    $msgs = ($prIdJson.errors | ForEach-Object { $_.message }) -join '; '
    throw "PR node id query returned GraphQL errors: $msgs"
}
$prNodeId = [string]$prIdJson.data.repository.pullRequest.id
if ([string]::IsNullOrWhiteSpace($prNodeId) -or $prNodeId -eq 'null') {
    throw "Failed to resolve PR node id for $Owner/$Repo PR #$PrNumber. GraphQL returned '$prNodeId'."
}

$mut = 'mutation($p:ID!){requestReviewsByLogin(input:{pullRequestId:$p,botLogins:["copilot-pull-request-reviewer"]}){pullRequest{number}}}'
$mutResp = gh api graphql -f "query=$mut" -f "p=$prNodeId" 2>&1
if ($LASTEXITCODE -ne 0) {
    throw @"
GraphQL requestReviewsByLogin failed: $mutResp

Most likely causes:
  * Quiet-period after a recent dismissal of Copilot — wait 5-10 min, or push a substantive commit.
  * Copilot Code Review not enabled on the repo / account.
  * PR in a state that blocks bot review (draft, conflict, branch protection).

DO NOT post @copilot comments as a workaround — that summons the Coding Agent.
"@
}
try {
    $mutJson = $mutResp | ConvertFrom-Json
    if ($mutJson.errors) {
        throw (($mutJson.errors | ForEach-Object { $_.message }) -join '; ')
    }
} catch {
    throw "GraphQL requestReviewsByLogin returned errors: $_"
}

# ---------- verify copilot_work_started event landed ----------

$deadline = (Get-Date).AddSeconds($VerifySeconds)
$afterTs = ''
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 5
    $now = gh api --paginate "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
        --jq '[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""' 2>&1
    if ($LASTEXITCODE -eq 0) {
        $now = (@($now -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ }) | Sort-Object | Select-Object -Last 1)
        if ($now -and [string]::CompareOrdinal($now, $beforeTs) -gt 0) { $afterTs = $now; break }
    }
}

if (-not $afterTs) {
    throw @"
GraphQL mutation returned success but no copilot_work_started event landed within $VerifySeconds seconds. The server may have silently dropped the request.
  Latest copilot_work_started before: '$beforeTs'
  HEAD: $headOid

Push a substantive commit (auto-assign on synchronize is the most reliable trigger) and retry.
"@
}

@{
    Status        = 'TriggerLanded'
    PrNumber      = $PrNumber
    HeadOid       = $headOid
    WorkStartedAt = $afterTs
    Detail        = "Triggered via GraphQL requestReviewsByLogin; copilot_work_started at $afterTs."
} | ConvertTo-Json -Compress
exit 0
