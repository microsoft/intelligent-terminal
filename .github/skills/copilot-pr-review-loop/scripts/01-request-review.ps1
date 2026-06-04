<#
.SYNOPSIS
    Re-request a Copilot code review on a pull request.

.DESCRIPTION
    Uses `gh pr edit --add-reviewer copilot-pull-request-reviewer` to trigger
    a fresh Copilot review. This form is idempotent and is the only one that
    currently works for the Copilot reviewer bot — see
    ../references/api-quirks.md for why the GraphQL and REST alternatives
    fail.

.PARAMETER Owner
    Repository owner (org or user). Defaults to the current repo's owner
    (resolved via `gh repo view`).

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

# Snapshot the latest review_requested event timestamp BEFORE making the
# request, so we can verify a fresh event landed afterward. Both
# `gh pr edit --add-reviewer` and REST `requested_reviewers` can return
# success while silently no-op'ing (e.g. the bot login isn't resolvable
# in this gh-cli session, or GitHub already considers the bot requested).
# Without this check the loop can spin forever waiting for a review that
# was never queued.
function Get-LatestReviewerEvent {
    $json = gh api "repos/$Owner/$Repo/issues/$PrNumber/events" `
        --jq '[.[] | select(.event=="review_requested" and (.requested_reviewer.login // "" | ascii_downcase | contains("copilot"))) | .created_at] | sort | .[-1] // ""'
    if ($LASTEXITCODE -ne 0) {
        throw "gh api events failed (exit $LASTEXITCODE) while snapshotting reviewer events."
    }
    return $json.Trim()
}

$beforeTs = Get-LatestReviewerEvent

# Primary path: documented in api-quirks.md. May fail with "not found" in
# some gh-cli versions / accounts because the Copilot bot is not a regular
# collaborator.
$primaryOk = $true
gh pr edit $PrNumber --repo $repoArg --add-reviewer copilot-pull-request-reviewer 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) {
    $primaryOk = $false
    Write-Host "Primary path (gh pr edit --add-reviewer copilot-pull-request-reviewer) failed; trying REST fallback."
}

# REST fallback: `reviewers[]=Copilot` (capital C, the bot's display name).
# Documented in api-quirks.md as a working fallback in some orgs.
if (-not $primaryOk) {
    gh api -X POST "repos/$Owner/$Repo/pulls/$PrNumber/requested_reviewers" `
        -f "reviewers[]=Copilot" --silent 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "Both gh pr edit and REST requested_reviewers failed for Copilot on PR #$PrNumber."
    }
}

# Verify: a new review_requested event for Copilot must appear within a
# few seconds. If not, the request was silently dropped (most often
# because Copilot already reviewed this exact HEAD and is suppressing a
# redundant review, or the bot was just unrequested in the same call).
Start-Sleep -Seconds 4
$afterTs = Get-LatestReviewerEvent
if ($afterTs -eq $beforeTs) {
    throw @"
Copilot review re-request returned success but no new `review_requested` event landed.
Latest event timestamp is still '$afterTs'.
Possible causes:
  * Copilot has already reviewed the current HEAD and is suppressing a redundant review.
  * The PR is in a state that blocks bot review (draft, closed, merge conflict).
  * The bot was requested and unrequested in the same API call.
Push a new commit, mark the PR ready, or wait a few minutes and retry.
"@
}

Write-Host "Copilot review requested on PR #$PrNumber (event timestamp: $afterTs)."
