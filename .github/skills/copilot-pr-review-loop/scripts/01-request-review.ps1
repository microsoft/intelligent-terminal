<#
.SYNOPSIS
    Re-request a Copilot code review on a pull request and verify the
    request actually landed.

.DESCRIPTION
    Tries each known best-effort mechanism in turn — none is guaranteed
    to land server-side, so success is judged solely by observing a new
    `copilot_work_started` event within ~30 seconds (not by HTTP/exit
    status, and not by the upstream `review_requested` event which can
    land without the bot actually picking up the work). Without this
    verification, a no-op succeeds silently and the loop spins forever
    on `02-list-open-threads.ps1` waiting for a review that was never
    queued.

    The mechanisms (in order — see ../references/api-quirks.md for the
    inconsistencies that motivate trying multiple):

    1. `gh pr edit --add-reviewer copilot-pull-request-reviewer` —
       documented in api-quirks.md as the preferred path. Fails with
       "not found" in some gh-cli versions / accounts because the bot
       is not a regular collaborator. Returns exit 1 on those failures.

    2. REST `POST /pulls/{n}/requested_reviewers` with
       `reviewers[]=Copilot`. Best-effort fallback — api-quirks.md notes
       it is inconsistent across orgs (some return HTTP 201 even when
       the request is silently dropped server-side). Success here is
       determined exclusively by the `copilot_work_started` event check
       below.

    3. REST DELETE-then-POST cycle. Best-effort fallback that sometimes
       re-arms a bot GitHub considers "already requested" or "recently
       reviewed". Same event-based success determination.

    If none of the mechanisms produce a verified event, throw with
    diagnostics on the likely cause: Copilot suppression for unchanged
    HEAD / trivial diff, draft/closed/conflicted PR, or auth-scope
    issues. Pushing a substantive new commit and retrying is usually
    the right next step.

    DO NOT post `@copilot please review` (or any @copilot mention) as a
    PR comment as a workaround. That summons the Copilot **Coding
    Agent** (which makes commits), not the reviewer bot — it is a
    confirmed waste of time and has been observed across multiple
    Copilot CLI sessions. The only valid triggers are the three
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
function Get-LatestCopilotWorkStarted {
    $json = gh api "repos/$Owner/$Repo/issues/$PrNumber/events?per_page=100" `
        --jq '[.[] | select(.event=="copilot_work_started") | .created_at] | sort | .[-1] // ""'
    if ($LASTEXITCODE -ne 0) {
        throw "gh api events failed (exit $LASTEXITCODE) while snapshotting copilot_work_started events."
    }
    return $json.Trim()
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

$beforeTs = Get-LatestCopilotWorkStarted
$tried = @()
$mech1Stderr = ''

# Mechanism 1: gh pr edit
$mech1Stderr = (gh pr edit $PrNumber --repo $repoArg --add-reviewer copilot-pull-request-reviewer 2>&1 | Out-String)
$tried += "gh pr edit (exit=$LASTEXITCODE)"

# Specific failure: Copilot Code Review is not enabled on this repo at all.
# Detect this so we fail fast with a clear diagnostic instead of churning
# through the REST fallbacks and emitting a confusing "suppression"
# message for a problem that has nothing to do with suppression.
if ($mech1Stderr -match "'copilot-pull-request-reviewer'\s+not\s+found" -or
    $mech1Stderr -match "could not resolve to a User") {
    throw "Copilot Code Review is not enabled on $repoArg (gh reported: $($mech1Stderr.Trim())). Enable it under repo Settings -> Code & automation -> Copilot, OR run the loop against a repo where Copilot review is already on. None of the trigger mechanisms can work until this is fixed."
}

$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds 20
if ($afterTs) {
    Write-Host "Copilot review requested on PR #$PrNumber via gh pr edit (work started at $afterTs)."
    exit 0
}

# Mechanism 2: REST POST
gh api -X POST "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" `
    -f "reviewers[]=Copilot" --silent 2>&1 | Out-Null
$tried += "REST POST (exit=$LASTEXITCODE)"

$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds 30
if ($afterTs) {
    Write-Host "Copilot review requested on PR #$PrNumber via REST POST (work started at $afterTs)."
    exit 0
}

# Mechanism 3: DELETE then POST
gh api -X DELETE "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" `
    -f "reviewers[]=Copilot" --silent 2>&1 | Out-Null
$deleteExit = $LASTEXITCODE
Start-Sleep -Seconds 2
gh api -X POST "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" `
    -f "reviewers[]=Copilot" --silent 2>&1 | Out-Null
$postExit = $LASTEXITCODE
$tried += "DELETE+POST cycle (DELETE exit=$deleteExit, POST exit=$postExit)"

$afterTs = Wait-ForCopilotWorkStarted -BeforeTs $beforeTs -TimeoutSeconds 30
if ($afterTs) {
    Write-Host "Copilot review requested on PR #$PrNumber via DELETE+POST (work started at $afterTs)."
    exit 0
}

throw @'
Copilot review re-request: tried all 3 mechanisms, none produced a
copilot_work_started event within the timeout.
'@ + "`n  Tried: $($tried -join ', ')" + "`n  Latest copilot_work_started timestamp before: '$beforeTs'" + "`n  Latest copilot_work_started timestamp after:  '$(Get-LatestCopilotWorkStarted)'" + @'


Likely causes (in order of frequency):
  * Trivial / small initial diff suppressed by Copilot before any review
    has run. Push a substantive (non-whitespace, non-comment-only)
    commit and retry.
  * Copilot has already reviewed the current HEAD and is suppressing a
    redundant re-review. Push a substantive new commit and retry.
  * The PR is in a state that blocks bot review (draft, closed, merge
    conflict, branch protection requiring approvals first).
  * Auth-scope issue — confirm "gh auth status" shows the repo scope.

ANTI-PATTERN — DO NOT DO THIS: posting "@copilot please review" (or any
@copilot mention) as a PR comment summons the Copilot **Coding Agent**
(which makes commits), NOT the reviewer bot. It will not produce a
review. This has been observed across multiple Copilot CLI sessions and
is a confirmed waste of time. The three mechanisms tried above are the
only valid triggers — if all three fail, push a substantive commit and
retry; do not fall back to @-mentions.
'@
