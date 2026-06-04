<#
.SYNOPSIS
  Clear the stuck-lock (Tier-3 or Tier-4) after a human has resolved
  the underlying issue.

.DESCRIPTION
  Detects which kind of stuck is currently set in state.json and clears it:

  - Tier-3 (stuck_on_sha set): the scheduler stopped mid-cherry-pick on
    a real merge conflict. -ResolvedThroughSha is REQUIRED and is
    validated to be on upstream/main and >= stuck_on_sha; it becomes
    the new last_synced_upstream_sha.

  - Tier-4 (stuck_validation set): the picks were clean but post-pick
    validation failed (static scan / build / toolchain). The human
    fixed it (e.g. resw dedup, restored fork invariant, fixed build,
    or provisioned the missing toolset). -ResolvedThroughSha is
    OPTIONAL — if omitted, last_synced_upstream_sha is left as-is so
    the next scheduler run re-attempts the same range; if provided,
    it advances the watermark just like Tier-3.

.PARAMETER ResolvedThroughSha
  See above.

.PARAMETER Reason
  Optional human-readable note recorded in the history entry.
#>
[CmdletBinding()]
param(
    [string] $ResolvedThroughSha,
    [string] $Reason = ''
)

. "$PSScriptRoot/Common.ps1"

$state = Read-State
$tier3 = [bool] $state.stuck_on_sha
$tier4 = [bool] $state.stuck_validation
if (-not ($tier3 -or $tier4)) {
    Write-Warning "No stuck-lock is set. Nothing to clear."
    return
}

Assert-CleanWorktree
Ensure-UpstreamRemote
git fetch upstream main --no-tags | Out-Null
if ($LASTEXITCODE -ne 0) { throw "git fetch upstream main failed; refusing to clear stuck-lock against stale refs." }

git switch main | Out-Null
if ($LASTEXITCODE -ne 0) { throw "git switch main failed; refusing to clear stuck-lock from the wrong branch." }
git pull --ff-only origin main | Out-Null
if ($LASTEXITCODE -ne 0) { throw "git pull --ff-only origin main failed; refusing to clear stuck-lock until main is current." }

$resolvedFullSha = if ($ResolvedThroughSha) { Resolve-FullCommitSha $ResolvedThroughSha } else { $null }

if ($tier3) {
    if (-not $resolvedFullSha) {
        throw "Tier-3 stuck_on_sha is set ($($state.stuck_on_sha)) — -ResolvedThroughSha is required to clear it."
    }
    $null = git merge-base --is-ancestor $resolvedFullSha upstream/main
    if ($LASTEXITCODE -ne 0) {
        throw "ResolvedThroughSha $resolvedFullSha is not on upstream/main. Refusing to clear lock."
    }
    $null = git merge-base --is-ancestor $state.stuck_on_sha $resolvedFullSha
    if ($LASTEXITCODE -ne 0) {
        throw "stuck_on_sha $($state.stuck_on_sha) is not an ancestor of $resolvedFullSha. Refusing — pass the same SHA or a later one."
    }
    $state.last_synced_upstream_sha = $resolvedFullSha
    $state.stuck_on_sha    = $null
    $state.stuck_branch    = $null
    $state.stuck_at        = $null
    $state.stuck_issue_url = $null
}

if ($tier4) {
    if ($resolvedFullSha) {
        $null = git merge-base --is-ancestor $resolvedFullSha upstream/main
        if ($LASTEXITCODE -ne 0) {
            throw "ResolvedThroughSha $resolvedFullSha is not on upstream/main. Refusing to clear lock."
        }
        $state.last_synced_upstream_sha = $resolvedFullSha
    }
    $state.stuck_validation = $null
}

# Append a history note so we can see when locks were cleared.
$entry = [ordered] @{
    at       = Format-Iso8601
    host     = $env:COMPUTERNAME
    status   = if ($tier3 -and $tier4) { 'cleared-stuck (tier3+tier4)' }
               elseif ($tier3)         { 'cleared-stuck (tier3)' }
               else                    { 'cleared-stuck (tier4)' }
    advanced_to = $resolvedFullSha
    reason   = $Reason
}
$state.history = @($entry) + @($state.history) | Select-Object -First 20
Write-State $state

git add -- (Get-StatePath) | Out-Null
$shortLabel = if ($resolvedFullSha) { $resolvedFullSha.Substring(0,9) } else { 'no-advance' }
git commit -m "chore(upstream-sync): clear stuck-lock ($shortLabel)" | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git commit failed (state unchanged?); lock is NOT cleared on origin/main." }
git push origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git push origin main failed — lock cleared locally only. Push manually." }
Write-Host "Stuck-lock cleared." -ForegroundColor Green
