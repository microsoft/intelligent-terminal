<#
.SYNOPSIS
  Finalize the sync by fast-forwarding main directly to the sync branch.
  Use when you have admin/bypass perms on main and want zero PR latency.

.DESCRIPTION
  Preserves per-commit content, order, and original author + committer
  dates, because the cherry-pick loop pins both from each upstream commit.

  Assumes the caller is currently on the sync branch with all picks
  applied. Performs:
    1. Add state.json + report, commit on the sync branch.
    2. Switch to main, pull --ff-only.
    3. Fast-forward main to the sync branch tip (refuses if main diverged).
    4. Push main.
    5. Delete the local sync branch (remote was never pushed).

.PARAMETER Ctx
  Run context.

.PARAMETER To
  Upstream HEAD SHA at fetch time (becomes new last_synced_upstream_sha).

.PARAMETER ReportPath
  Path to the markdown report (will be committed onto the sync branch).

.OUTPUTS
  Writes the new main HEAD SHA to stdout and to Ctx.MainHeadSha.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] $Ctx,
    [Parameter(Mandatory)] [string] $To,
    [Parameter(Mandatory)] [string] $ReportPath
)

. "$PSScriptRoot/Common.ps1"

$branch = $Ctx.Branch
$shortTo = $To.Substring(0,9)

# 1. Update state.json + commit (still on the sync branch).
$state = Read-State
$state.last_synced_upstream_sha = $To
$runSummary = [ordered] @{
    at                 = Format-Iso8601 $Ctx.StartedAt
    host               = $Ctx.Host
    status             = 'ok'
    branch             = $branch
    merge_mode         = 'direct-push'
    pr_url             = $null
    main_head_sha      = $null   # filled in after push
    picked_count       = $Ctx.Picked.Count
    dropped_pair_count = $Ctx.DroppedPairs.Count
    empty_count        = $Ctx.SkippedEmpty.Count
    tier0_resolutions  = $Ctx.Tier0.Count
}
$state.last_run = $runSummary
$state.history  = @($runSummary) + @($state.history) | Select-Object -First 20
Write-State $state

git add -- (ConvertTo-RepoRelativePath (Get-StatePath)) (ConvertTo-RepoRelativePath $ReportPath)
if ($LASTEXITCODE -ne 0) { throw "git add of state.json + report failed." }
git commit -m "chore(upstream-sync): advance baseline to $shortTo" | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git commit of state-update failed; aborting before touching main." }

$syncTip = (git rev-parse HEAD).Trim()

# 2. Switch to main and ensure we're current.
git switch main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git switch main failed." }
git pull --ff-only origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "main is not fast-forwardable from origin. Resolve manually." }

# 3. Fast-forward main to the sync tip. If main has moved (someone landed
# something concurrently), this fails — and that's the correct behaviour:
# direct-push assumes you own the merge moment. Re-run the sync to rebase.
git merge --ff-only $syncTip | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw "Cannot fast-forward main onto $syncTip — main has commits the sync branch does not. Re-run the sync to start from the new main, or finalize via the PR path."
}

# 4. Push main.
git push origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git push origin main failed. The picks are local-only — push manually before the next scheduler tick or it will re-pick everything." }

# 5. Local cleanup. Remote branch was never pushed in direct-push mode.
git branch -D $branch | Out-Host

$mainHead = (git rev-parse HEAD).Trim()
$Ctx.MainHeadSha = $mainHead

# Backfill main_head_sha into state.last_run (best-effort).
$state = Read-State
if ($state.last_run -and $state.history -and $state.history.Count -gt 0) {
    $state.last_run.main_head_sha = $mainHead
    $state.history[0].main_head_sha = $mainHead
    Write-State $state
    git add -- (ConvertTo-RepoRelativePath (Get-StatePath)) | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "git add of state.json failed (LASTEXITCODE=$LASTEXITCODE); skipping main_head_sha backfill (cosmetic only; sync content already on main)."
    } else {
        git commit -m "chore(upstream-sync): record main head $($mainHead.Substring(0,9))" | Out-Host
        if ($LASTEXITCODE -eq 0) {
            git push origin main | Out-Host
            if ($LASTEXITCODE -ne 0) { Write-Warning "Backfill push failed (cosmetic only; sync content already on main)." }
        }
    }
}

return $mainHead
