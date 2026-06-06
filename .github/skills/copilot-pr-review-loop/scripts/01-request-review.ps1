<#
.SYNOPSIS
    Request a Copilot review on a PR and verify the trigger landed.

.DESCRIPTION
    Single mechanism: GraphQL `requestReviewsByLogin` with
    `botLogins:["copilot-pull-request-reviewer"]`. See
    references/api-quirks.md for the GraphQL traps and the
    "@copilot mention is the Coding Agent, not the reviewer" pitfall.

    Success contract (exit 0, single-line JSON):
      - Status="InFlight"      — Copilot already a requested reviewer.
      - Status="TriggerLanded" — mutation submitted and verified via a
                                 new `copilot_work_started` event id.

    Failure (throw, exit 1): mutation failed, or no new event landed
    within -VerifySeconds. Caller should push a substantive commit and
    retry (auto-assign on `synchronize` is the most reliable fallback).

.PARAMETER PrNumber       PR number (required).
.PARAMETER Owner / .PARAMETER Repo   Optional; auto-resolved from `gh repo view`.
.PARAMETER VerifySeconds  Verification poll window (1..600, default 30).

.EXAMPLE
    pwsh 01-request-review.ps1 -PrNumber 236
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [string]$Owner,
    [string]$Repo,

    [ValidateRange(1, 600)]
    [int]$VerifySeconds = 30
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/_lib.ps1"

function Get-LatestCopilotWorkStartedEvent {
    $eventsPath = "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100"
    $r = Invoke-Gh -GhArgs @('api','-i',$eventsPath)
    if ($r.ExitCode -ne 0) { throw "events query failed: $($r.Stderr)" }

    $m = [regex]::Match($r.Stdout, '(?s)\A(?<headers>.*?)\r?\n\r?\n(?<body>.*)\z')
    if (-not $m.Success) { throw 'events query returned an unexpected header/body shape.' }
    $headers = $m.Groups['headers'].Value
    $body = $m.Groups['body'].Value

    $lastPage = 1
    $lastMatch = [regex]::Match($headers, 'page=(\d+)>; rel="last"')
    if ($lastMatch.Success) { $lastPage = [int]$lastMatch.Groups[1].Value }
    if ($lastPage -gt 1) {
        $r = Invoke-Gh -GhArgs @('api',"repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100&page=$lastPage")
        if ($r.ExitCode -ne 0) { throw "events last-page query failed: $($r.Stderr)" }
        $body = $r.Stdout
    }

    $events = @($body | ConvertFrom-Json)
    $latest = $events | Where-Object { $_.event -eq 'copilot_work_started' } | Sort-Object id | Select-Object -Last 1
    if (-not $latest) { return [pscustomobject]@{ Id = 0L; CreatedAt = '' } }
    $createdAt = if ($latest.created_at -is [datetime]) {
        $latest.created_at.ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    } else {
        [string]$latest.created_at
    }
    [pscustomobject]@{ Id = [long]$latest.id; CreatedAt = $createdAt }
}

# ---------- repo resolve ----------

$coords = Resolve-RepoCoords -Owner $Owner -Repo $Repo
$Owner = $coords.Owner
$Repo  = $coords.Repo

# ---------- state: is Copilot currently requested? ----------
# Single GraphQL query: requested reviewers + head SHA, followed by
# pagination for the full requested-reviewer set.

$stateQuery = @'
query($o:String!,$r:String!,$n:Int!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      id
      headRefOid
      state
      reviewRequests(first:100){nodes{requestedReviewer{__typename ... on Bot{login} ... on User{login} ... on Mannequin{login}}} pageInfo{hasNextPage endCursor}}
    }
  }
}
'@
$r = Invoke-Gh -GhArgs @('api','graphql','-f',"query=$stateQuery",'-f',"o=$Owner",'-f',"r=$Repo",'-F',"n=$PrNumber")
if ($r.ExitCode -ne 0) { throw "state query failed: $($r.Stderr)" }
$stateData = $r.Stdout | ConvertFrom-Json
if ($stateData.errors) {
    throw "state query GraphQL errors: $(($stateData.errors | ForEach-Object {$_.message}) -join '; ')"
}
$pr = $stateData.data.repository.pullRequest
if (-not $pr) { throw "PR #$PrNumber not found in $Owner/$Repo." }
if ($pr.state -ne 'OPEN') {
    throw "PR #$PrNumber is not OPEN (state=$($pr.state))."
}

$headOid = $pr.headRefOid
$prNodeId = [string]$pr.id
if ([string]::IsNullOrWhiteSpace($prNodeId)) {
    throw "Failed to resolve PR node id for $Owner/$Repo PR #$PrNumber from state query."
}
$reviewRequests = @($pr.reviewRequests.nodes)
$after = $pr.reviewRequests.pageInfo.endCursor
while ($pr.reviewRequests.pageInfo.hasNextPage) {
    $pageQuery = @'
query($o:String!,$r:String!,$n:Int!,$after:String!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
      reviewRequests(first:100,after:$after){nodes{requestedReviewer{__typename ... on Bot{login} ... on User{login} ... on Mannequin{login}}} pageInfo{hasNextPage endCursor}}
    }
  }
}
'@
    $r = Invoke-Gh -GhArgs @('api','graphql','-f',"query=$pageQuery",'-f',"o=$Owner",'-f',"r=$Repo",'-F',"n=$PrNumber",'-f',"after=$after")
    if ($r.ExitCode -ne 0) { throw "reviewRequests page query failed: $($r.Stderr)" }
    $pageData = $r.Stdout | ConvertFrom-Json
    if ($pageData.errors) {
        throw "reviewRequests page query GraphQL errors: $(($pageData.errors | ForEach-Object {$_.message}) -join '; ')"
    }
    $page = $pageData.data.repository.pullRequest.reviewRequests
    $reviewRequests += @($page.nodes)
    $pr.reviewRequests.pageInfo = $page.pageInfo
    $after = $page.pageInfo.endCursor
}
$copilotPendingRequests = @($reviewRequests | Where-Object { $_.requestedReviewer.login -match '(?i)^copilot-pull-request-reviewer(\[bot\])?$' })
$copilotPending = $copilotPendingRequests.Count -gt 0

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

# Snapshot the latest copilot_work_started BEFORE triggering. Use the
# event's numeric `id` (monotonic) — `created_at` is second-resolution
# and would collide if a new event lands in the same second.
$beforeEvent = Get-LatestCopilotWorkStartedEvent
$beforeId = $beforeEvent.Id

# ---------- trigger via GraphQL requestReviewsByLogin ----------

$mut = 'mutation($p:ID!){requestReviewsByLogin(input:{pullRequestId:$p,botLogins:["copilot-pull-request-reviewer"]}){pullRequest{number}}}'
$r = Invoke-Gh -GhArgs @('api','graphql','-f',"query=$mut",'-f',"p=$prNodeId")
if ($r.ExitCode -ne 0) {
    throw @"
GraphQL requestReviewsByLogin failed: $($r.Stderr)

Most likely causes:
  * Quiet-period after a recent dismissal of Copilot — wait 5-10 min, or push a substantive commit.
  * Copilot Code Review not enabled on the repo / account.
  * PR in a state that blocks bot review (draft, conflict, branch protection).

DO NOT post @copilot comments as a workaround — that summons the Coding Agent.
"@
}
try {
    $mutJson = $r.Stdout | ConvertFrom-Json
    if ($mutJson.errors) {
        throw (($mutJson.errors | ForEach-Object { $_.message }) -join '; ')
    }
} catch {
    throw "GraphQL requestReviewsByLogin returned errors: $_"
}

# ---------- verify copilot_work_started event landed ----------

$deadline = (Get-Date).AddSeconds($VerifySeconds)
$afterTs = ''
$afterId = 0L
$lastErr = ''
do {
    try {
        $nowEvent = Get-LatestCopilotWorkStartedEvent
        $lastErr = ''
        if ($nowEvent.Id -gt $beforeId) {
            $afterId = $nowEvent.Id
            $afterTs = $nowEvent.CreatedAt
            break
        }
    } catch {
        $lastErr = $_.Exception.Message
    }
    if ((Get-Date) -ge $deadline) { break }
    $remaining = [int]($deadline - (Get-Date)).TotalSeconds
    Start-Sleep -Seconds ([Math]::Min(5, [Math]::Max(1, $remaining)))
} while ((Get-Date) -lt $deadline)

if (-not $afterId) {
    $errTail = if ($lastErr) { "`n  Last events-query error: $lastErr" } else { '' }
    throw @"
GraphQL mutation returned success but no new copilot_work_started event landed within $VerifySeconds seconds. The server may have silently dropped the request, or the events query kept failing transiently.
  Latest copilot_work_started event id before trigger: $beforeId
  HEAD: $headOid$errTail

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
