<#
.SYNOPSIS
  Generate a sync run report markdown file under
  `Generated Files/upstream-sync/<YYYY-MM-DD>/` (gitignored).

.DESCRIPTION
  Reports are TRANSIENT artifacts: they live in the gitignored
  `Generated Files/` workspace and are NEVER committed. They exist
  so a human running the skill (or reviewing a stuck issue) can read
  what happened in a single run. The stuck-issue body (07 / 07b)
  inlines the relevant parts of the report so issue readers don't
  need to fetch the local file.

.PARAMETER Ctx
  The run-context object built by 04-run-batch.ps1.

.PARAMETER From
  Baseline upstream SHA before the run (Get-LastSyncedUpstreamSha).

.PARAMETER To
  Upstream HEAD SHA at fetch time.

.PARAMETER Status
  ok | no-op | dry-run | stuck | skipped-locked | skipped-pr-open
  | stuck-static-scan | stuck-build-failed | stuck-build-inconclusive
  | stuck-toolchain-missing

.OUTPUTS
  Absolute path to the written report file.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] $Ctx,
    [Parameter(Mandatory)] [string] $From,
    [Parameter(Mandatory)] [string] $To,
    [Parameter(Mandatory)] [ValidateSet('ok','no-op','dry-run','stuck','skipped-locked','skipped-pr-open','stuck-static-scan','stuck-build-failed','stuck-build-inconclusive','stuck-toolchain-missing')] [string] $Status
)

. "$PSScriptRoot/Common.ps1"

$started = $Ctx.StartedAt
$ended   = Get-Date
$dur     = $ended - $started
$durStr  = "{0}m {1}s" -f [int]$dur.TotalMinutes, ($dur.Seconds)

function Get-Subj([string] $sha) {
    if (-not $sha) { return '' }
    try { return (git log -1 --format='%s' $sha 2>$null) } catch { return '' }
}

$fromSubj = Get-Subj $From
$toSubj   = Get-Subj $To

$lines = New-Object System.Collections.Generic.List[string]
$lines.Add("# Upstream sync - $Status - $(Format-Iso8601 $started)")
$lines.Add("")
$lines.Add("**Status:** $Status  ")
$lines.Add("**Host:** $($Ctx.Host)  ")
$lines.Add("**Duration:** $durStr  ")
$lines.Add("**Baseline (before run):** ``$From`` - $fromSubj  ")
$lines.Add("**Upstream HEAD:** ``$To`` - $toSubj  ")
$lines.Add("**Branch:** ``$($Ctx.Branch)``  ")
$lines.Add("")
$lines.Add("## Summary")
$lines.Add("")
$lines.Add("- Commits picked: **$($Ctx.Picked.Count)**")
$lines.Add("- Revert pairs dropped: **$($Ctx.DroppedPairs.Count)** (= $($Ctx.DroppedPairs.Count * 2) commits skipped, net zero)")
$lines.Add("- Upstream-empty commits skipped: **$($Ctx.SkippedEmpty.Count)**")
$lines.Add("- Tier-0 auto-resolutions: **$($Ctx.Tier0.Count)**")
$lines.Add("- Tier-2 LLM resolutions: **$($Ctx.Tier2.Count)**")
if ($Ctx.StuckSha) {
    $lines.Add("- Tier-3 stuck at: ``$($Ctx.StuckSha)``")
}
$lines.Add("")

if ($Status -eq 'dry-run' -and $Ctx.Pending.Count -gt 0) {
    $lines.Add("## Pending commits (oldest -> newest)")
    $lines.Add("")
    $lines.Add("| # | SHA | Subject | Author |")
    $lines.Add("|---|---|---|---|")
    $i = 0
    foreach ($sha in $Ctx.Pending) {
        $i++
        $s = (git log -1 --format='%s' $sha) -replace '\|','\|'
        $a = git log -1 --format='%an' $sha
        $lines.Add("| $i | ``$($sha.Substring(0,9))`` | $s | $a |")
    }
    $lines.Add("")
}

if ($Ctx.Picked.Count -gt 0) {
    $lines.Add("## Picked commits (oldest -> newest)")
    $lines.Add("")
    $lines.Add("| # | SHA | Subject | Author |")
    $lines.Add("|---|---|---|---|")
    $i = 0
    foreach ($sha in $Ctx.Picked) {
        $i++
        $s = (git log -1 --format='%s' $sha) -replace '\|','\|'
        $a = git log -1 --format='%an' $sha
        $lines.Add("| $i | ``$($sha.Substring(0,9))`` | $s | $a |")
    }
    $lines.Add("")
}

if ($Ctx.DroppedPairs.Count -gt 0) {
    $lines.Add("## Dropped revert pairs")
    $lines.Add("")
    $lines.Add("| Original SHA | Original subject | Revert SHA |")
    $lines.Add("|---|---|---|")
    foreach ($pair in $Ctx.DroppedPairs) {
        $os = (git log -1 --format='%s' $pair[0]) -replace '\|','\|'
        $lines.Add("| ``$($pair[0].Substring(0,9))`` | $os | ``$($pair[1].Substring(0,9))`` |")
    }
    $lines.Add("")
}

if ($Ctx.SkippedEmpty.Count -gt 0) {
    $lines.Add("## Empty / no-op commits skipped")
    $lines.Add("")
    $lines.Add("| SHA | Subject |")
    $lines.Add("|---|---|")
    foreach ($sha in $Ctx.SkippedEmpty) {
        $s = (git log -1 --format='%s' $sha) -replace '\|','\|'
        $lines.Add("| ``$($sha.Substring(0,9))`` | $s |")
    }
    $lines.Add("")
}

if ($Ctx.Tier0.Count -gt 0) {
    $lines.Add("## Tier-0 auto-resolutions")
    $lines.Add("")
    $lines.Add("| Commit SHA | File |")
    $lines.Add("|---|---|")
    foreach ($r in $Ctx.Tier0) {
        $lines.Add("| ``$($r.Sha.Substring(0,9))`` | ``$($r.Path)`` |")
    }
    $lines.Add("")
}

if ($Status -eq 'stuck' -and $Ctx.StuckSha) {
    $stuckSubj = Get-Subj $Ctx.StuckSha
    $stuckAuthor = git log -1 --format='%an <%ae>' $Ctx.StuckSha
    $lines.Add("## Conflict diagnostics")
    $lines.Add("")
    $lines.Add("**Conflicting commit:** [`$($Ctx.StuckSha)`](https://github.com/microsoft/terminal/commit/$($Ctx.StuckSha)) - $stuckSubj  ")
    $lines.Add("**Author:** $stuckAuthor")
    $lines.Add("")
    $stuckError = if ($Ctx.PSObject.Properties.Name -contains 'StuckError') { $Ctx.StuckError } else { $null }
    if ($Ctx.StuckPaths -and $Ctx.StuckPaths.Count -gt 0) {
        $lines.Add("**Files in conflict:**")
        $lines.Add("")
        foreach ($p in $Ctx.StuckPaths) { $lines.Add("- ``$p``") }
        $lines.Add("")
    } else {
        $lines.Add("**No unmerged paths** - ``git cherry-pick`` failed for a reason other than a merge conflict (e.g. attempting to pick a merge commit without ``-m``, hook failure, or another non-conflict error).")
        if ($stuckError) {
            $lines.Add("")
            $lines.Add("**Reported error:** ``$stuckError``")
        }
        $lines.Add("")
    }
    $lines.Add("**Pickup branch:** ``$($Ctx.Branch)`` (push attempted - run ````git ls-remote --heads origin $($Ctx.Branch)```` to verify it landed)")
    $lines.Add("")
    $lines.Add("**How to resume:**")
    $lines.Add("")
    $lines.Add("1. ``git switch $($Ctx.Branch)``")
    $lines.Add("2. Manually cherry-pick the stuck commit and resolve. Pin upstream identity/dates so the resolved commit matches the rest of this batch:")
    $lines.Add("   ``````pwsh")
    $lines.Add("   `$info = (git log -1 --pretty=format:'%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI' $($Ctx.StuckSha)) -split [char]0")
    $lines.Add("   `$env:GIT_AUTHOR_NAME=`$info[0]; `$env:GIT_AUTHOR_EMAIL=`$info[1]; `$env:GIT_AUTHOR_DATE=`$info[2]")
    $lines.Add("   `$env:GIT_COMMITTER_NAME=`$info[3]; `$env:GIT_COMMITTER_EMAIL=`$info[4]; `$env:GIT_COMMITTER_DATE=`$info[5]")
    $lines.Add("   git cherry-pick -x $($Ctx.StuckSha)")
    $lines.Add("   # resolve conflicts, then:")
    $lines.Add("   git add -A; git cherry-pick --continue --no-edit")
    $lines.Add("   Remove-Item Env:GIT_AUTHOR_NAME,Env:GIT_AUTHOR_EMAIL,Env:GIT_AUTHOR_DATE,Env:GIT_COMMITTER_NAME,Env:GIT_COMMITTER_EMAIL,Env:GIT_COMMITTER_DATE -ErrorAction SilentlyContinue")
    $lines.Add("   ``````")
    $lines.Add("3. Push and open a PR titled ``chore(upstream-sync): manual resolution for $($Ctx.StuckSha.Substring(0,9))``, merge it. The ``-x`` trailer is what the next sync run reads as the new watermark - **do not strip it.**")
    $lines.Add("4. Close the stuck issue. That's the lock-cleared signal; no script needed.")
    $lines.Add("5. The next scheduled sync derives the new watermark from your merged PR and resumes from the commit after this one.")
    $lines.Add("")
}

if ($Status -like 'stuck-*') {
    $kind = $Status -replace '^stuck-',''
    $lines.Add("## Validation diagnostics - Tier-4 ($kind)")
    $lines.Add("")
    $lines.Add("All $($Ctx.Picked.Count) cherry-pick(s) applied cleanly, but the post-batch validation step blocked the push.")
    $lines.Add("")
    switch ($kind) {
        'static-scan' {
            $sm = $Ctx.Scan.summary
            $lines.Add("**Static scan summary:** critical=$($sm.critical), high=$($sm.high), medium=$($sm.medium), low=$($sm.low), info=$($sm.info)")
            $lines.Add("")
            $blocking = @($Ctx.Scan.findings | Where-Object { $_.severity -in @('critical','high') })
            if ($blocking.Count -gt 0) {
                $lines.Add("**Blocking findings:**")
                $lines.Add("")
                $lines.Add("| Severity | Kind | Where | Detail |")
                $lines.Add("|---|---|---|---|")
                foreach ($f in $blocking) {
                    $where = if ($f.path) { "``$($f.path)``" } else { '-' }
                    $findingKind = if ($f.check) { $f.check } elseif ($f.kind) { $f.kind } else { 'unknown' }
                    $detail = ($f | ConvertTo-Json -Compress -Depth 4)
                    $lines.Add("| $($f.severity) | $findingKind | $where | ``$detail`` |")
                }
                $lines.Add("")
            }
        }
        'build-failed' {
            $lines.Add("**Build exit code:** $($Ctx.Build.exit_code)  ")
            $lines.Add("**Build duration:** $([int]($Ctx.Build.duration_ms / 1000))s  ")
            $lines.Add("**Build log:** ``$($Ctx.Build.log_path)``")
            $lines.Add("")
            $lines.Add("**Last lines of build log:**")
            $lines.Add("")
            $lines.Add('```')
            $lines.Add(($Ctx.Build.log_tail -split "`n" | Select-Object -Last 80) -join "`n")
            $lines.Add('```')
            $lines.Add("")
        }
        'build-inconclusive' {
            $lines.Add("**Build hit timeout** after $([int]($Ctx.Build.duration_ms / 1000))s.")
            $lines.Add("**Build log:** ``$($Ctx.Build.log_path)``")
            $lines.Add("")
            $lines.Add("If this was a legitimate hang, investigate. If it was a slow build host, re-run with ``-BuildTimeoutMinutes <larger>`` or ``-AllowInconclusiveBuild``.")
            $lines.Add("")
        }
        'toolchain-missing' {
            $lines.Add("**Required toolsets:** $($Ctx.Preflight.required_toolsets -join ', ')")
            $lines.Add("**Available toolsets:** $($Ctx.Preflight.available_toolsets -join ', ')")
            $lines.Add("**Missing:** $($Ctx.Preflight.missing -join ', ')")
            $lines.Add("")
            $lines.Add("This is an **infrastructure** problem - provision the host with the required Visual Studio toolset(s). No GitHub issue was opened because PR review cannot fix it.")
            $lines.Add("")
        }
    }
    $lines.Add("**Pickup branch:** ``$($Ctx.Branch)`` (push attempted - run ````git ls-remote --heads origin $($Ctx.Branch)```` to verify it landed)")
    $lines.Add("")
    if ($kind -eq 'toolchain-missing') {
        $lines.Add("**How to resume (infra-only - no PR needed):**")
        $lines.Add("")
        $lines.Add("1. Provision the host with the missing toolset(s) above, **or** rerun the sync from a correctly provisioned host.")
        $lines.Add("2. No issue was opened, so there is no lock to clear - the next scheduler tick just retries.")
        $lines.Add("3. The next scheduled sync re-runs the same range; nothing on ``$($Ctx.Branch)`` needs editing.")
        $lines.Add("")
    } else {
        $lines.Add("**How to resume:**")
        $lines.Add("")
        $lines.Add("1. ``git switch $($Ctx.Branch)``")
        $lines.Add("2. Fix the issue above (e.g. resw dedup, restored fork invariant, build fix).")
        $lines.Add("3. Push and open a PR titled ``chore(upstream-sync): manual validation fix for $($Ctx.Branch)``, merge it (keep the ``-x`` trailers on every cherry-picked commit so the next run's watermark derivation still works).")
        $lines.Add("4. Close the stuck issue. That's the lock-cleared signal; no script needed.")
        $lines.Add("5. The next scheduled sync runs the same range - validation must pass before any PR is opened.")
        $lines.Add("")
    }
}

$lines.Add("---")
$lines.Add("")
$lines.Add("_Generated by ``.github/skills/upstream-sync/scripts/05-write-report.ps1``. This file lives under `Generated Files/` and is gitignored - never committed._")

$suffix = if ($Status -eq 'skipped-locked') { 'skipped' }
          elseif ($Status -eq 'skipped-pr-open') { 'skipped' }
          elseif ($Status -eq 'dry-run')   { 'dry-run' }
          elseif ($Status -like 'stuck-*')  { $Status }
          elseif ($Status -eq 'stuck')      { 'stuck' }
          elseif ($Status -eq 'no-op')      { 'noop' }
          else { 'ok' }

# Filenames are sortable by run timestamp + disambiguated by status. The
# parent directory (Generated Files/upstream-sync/<date>/) is already per-day.
$stamp = $started.ToString('yyyy-MM-ddTHHmmss')
$name  = "$stamp-$suffix.md"
$path  = Join-Path (Get-GeneratedDir) $name
[System.IO.File]::WriteAllText($path, ($lines -join "`n"), (New-Object System.Text.UTF8Encoding($false)))
return $path
