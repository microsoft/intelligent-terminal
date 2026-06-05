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

    [ValidateRange(1, 600)]
    [int]$VerifySeconds = 30
)

$ErrorActionPreference = 'Stop'

# Single-call helper: capture stdout + stderr separately in one invocation
# so we never re-run gh just to recover stderr on failure, and never feed
# stderr into ConvertFrom-Json on success.
function Invoke-Gh {
    param([Parameter(Mandatory)][string[]]$GhArgs)
    $errFile = [IO.Path]::GetTempFileName()
    try {
        $out = & gh @GhArgs 2>$errFile
        $ec = $LASTEXITCODE
        $err = (Get-Content -Raw -LiteralPath $errFile -ErrorAction SilentlyContinue) ?? ''
        [pscustomobject]@{ ExitCode = $ec; Stdout = ($out | Out-String); Stderr = $err }
    } finally {
        Remove-Item -LiteralPath $errFile -ErrorAction SilentlyContinue
    }
}

# ---------- repo resolve ----------

if (-not $Owner -or -not $Repo) {
    $r = Invoke-Gh -GhArgs @('repo','view','--json','owner,name')
    if ($r.ExitCode -ne 0) { throw "gh repo view failed: $($r.Stderr)" }
    $repoInfo = $r.Stdout | ConvertFrom-Json
    if (-not $Owner) { $Owner = $repoInfo.owner.login }
    if (-not $Repo)  { $Repo  = $repoInfo.name }
}

# ---------- state: is Copilot currently requested? ----------
# Single GraphQL query: requested reviewers + head SHA, followed by
# pagination for the full requested-reviewer set.

$stateQuery = @'
query($o:String!,$r:String!,$n:Int!){
  repository(owner:$o,name:$r){
    pullRequest(number:$n){
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

$r = Invoke-Gh -GhArgs @('api','--paginate',"repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100",'--jq','[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""')
if ($r.ExitCode -ne 0) { throw "events query failed: $($r.Stderr)" }
$beforeTs = (@($r.Stdout -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ }) | Sort-Object | Select-Object -Last 1)
if (-not $beforeTs) { $beforeTs = '' }

# ---------- trigger via GraphQL requestReviewsByLogin ----------

$prIdQuery = "query{repository(owner:`"$Owner`",name:`"$Repo`"){pullRequest(number:$PrNumber){id}}}"
$r = Invoke-Gh -GhArgs @('api','graphql','-f',"query=$prIdQuery")
if ($r.ExitCode -ne 0) { throw "PR node id query failed: $($r.Stderr)" }
$prIdJson = $r.Stdout | ConvertFrom-Json
if ($prIdJson.errors) {
    $msgs = ($prIdJson.errors | ForEach-Object { $_.message }) -join '; '
    throw "PR node id query returned GraphQL errors: $msgs"
}
$prNodeId = [string]$prIdJson.data.repository.pullRequest.id
if ([string]::IsNullOrWhiteSpace($prNodeId) -or $prNodeId -eq 'null') {
    throw "Failed to resolve PR node id for $Owner/$Repo PR #$PrNumber. GraphQL returned '$prNodeId'."
}

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
$lastErr = ''
do {
    $r = Invoke-Gh -GhArgs @('api','--paginate',"repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100",'--jq','[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""')
    if ($r.ExitCode -eq 0) {
        $lastErr = ''
        $now = (@($r.Stdout -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ }) | Sort-Object | Select-Object -Last 1)
        if ($now -and [string]::CompareOrdinal($now, $beforeTs) -gt 0) { $afterTs = $now; break }
    } else {
        $lastErr = $r.Stderr.Trim()
    }
    if ((Get-Date) -ge $deadline) { break }
    $remaining = [int]($deadline - (Get-Date)).TotalSeconds
    Start-Sleep -Seconds ([Math]::Min(5, [Math]::Max(1, $remaining)))
} while ((Get-Date) -lt $deadline)

if (-not $afterTs) {
    $errTail = if ($lastErr) { "`n  Last events-query error: $lastErr" } else { '' }
    throw @"
GraphQL mutation returned success but no copilot_work_started event landed within $VerifySeconds seconds. The server may have silently dropped the request, or the events query kept failing transiently.
  Latest copilot_work_started before: '$beforeTs'
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
