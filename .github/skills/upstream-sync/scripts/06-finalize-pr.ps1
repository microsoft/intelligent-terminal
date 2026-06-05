<#
.SYNOPSIS
  Push the sync branch and open a PR. Commits state.json + report onto
  the branch first so the merge atomically advances last_synced.

.PARAMETER Ctx
  Run context from 04-run-batch.ps1.

.PARAMETER To
  Upstream HEAD SHA at fetch time (becomes new last_synced_upstream_sha).

.PARAMETER ReportPath
  Absolute path to the report markdown to use as the PR body.

.OUTPUTS
  PR URL on stdout (and writes Ctx.PrUrl).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] $Ctx,
    [Parameter(Mandatory)] [string] $To,
    [Parameter(Mandatory)] [string] $ReportPath,
    [ValidateSet('rebase','merge','none')] [string] $AutoMergeStrategy = 'none'
)

. "$PSScriptRoot/Common.ps1"

# Prepend the squash-warning banner to the report so it lands as the
# first thing reviewers see in the PR body.
$banner = @"
> ⚠️ **DO NOT squash-merge this PR.** Squashing collapses every cherry-picked
> upstream commit into one, destroying per-commit attribution, original
> author dates, and ``git bisect`` resolution. Merge with **"Rebase and
> merge"** (preferred — flat history, all $($Ctx.Picked.Count) commits land
> individually) or **"Create a merge commit"** (also preserves per-commit
> content).
>
> 📝 **Review-fix policy.** Only build-blocking fixes (compile errors, dedup
> of conflicts surfaced at build time, CI gate failures on this PR itself)
> belong here — as **one** focused extra commit on this branch. All other
> Copilot / human review feedback (code-quality, logic, translation,
> spelling-list migrations, doc nits) goes into a **follow-up PR** based on
> this PR's head, not amended into the cherry-pick commits. Rationale and
> mechanics: [``.github/skills/upstream-sync/references/follow-up-pr.md``](../blob/main/.github/skills/upstream-sync/references/follow-up-pr.md).

---

"@
$bodyPath = New-TemporaryFile
$bodyContent = $banner + (Get-Content -Raw -LiteralPath $ReportPath)
[System.IO.File]::WriteAllText($bodyPath, $bodyContent, (New-Object System.Text.UTF8Encoding($false)))

$branch = $Ctx.Branch
$shortTo = $To.Substring(0,9)

# Update state.json with new baseline and run summary, plus commit the report.
$state = Read-State
$state.last_synced_upstream_sha = $To
$runSummary = [ordered] @{
    at                 = Format-Iso8601 $Ctx.StartedAt
    host               = $Ctx.Host
    status             = 'ok'
    branch             = $branch
    pr_url             = $null   # filled in after PR creation
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
if ($LASTEXITCODE -ne 0) { throw "git commit of state-update failed; aborting without push so baseline is not lost." }

git push -u origin $branch | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git push failed for $branch." }

$title = "chore(upstream): sync microsoft/terminal up to $shortTo"

# Same-repo PR: branch was pushed to origin (= microsoft/intelligent-terminal),
# so --head takes the bare branch name. `--head OWNER:BRANCH` would tell gh to
# look on a fork owned by OWNER, which is wrong for this scheduler.
#
# Retry up to 3 times with a short delay: `gh pr create` on Windows occasionally fails
# with "Head sha can't be blank" right after a push (see SKILL.md gotcha).
$prUrl = $null
for ($attempt = 1; $attempt -le 3; $attempt++) {
    $prUrl = gh pr create -R microsoft/intelligent-terminal --base main --head $branch --title $title --body-file $bodyPath 2>&1 | Select-Object -Last 1
    if ($LASTEXITCODE -eq 0 -and $prUrl -match '^https://github.com/') { break }
    Write-Warning "gh pr create attempt $attempt failed: $prUrl"
    Start-Sleep -Seconds 5
}
if ($LASTEXITCODE -ne 0 -or $prUrl -notmatch '^https://github.com/') {
    throw "gh pr create did not return a PR URL after 3 attempts. Last output: $prUrl"
}

$Ctx.PrUrl = $prUrl.Trim()

Remove-Item -LiteralPath $bodyPath -Force -ErrorAction SilentlyContinue

# Optional: arm GitHub auto-merge with the strategy that preserves per-commit
# history. 'rebase' is the recommended default when auto-merge is enabled —
# it lands all N commits flatly on main once CI + approvals pass.
if ($AutoMergeStrategy -ne 'none') {
    $strategyFlag = "--$AutoMergeStrategy"
    gh pr merge -R microsoft/intelligent-terminal $Ctx.PrUrl $strategyFlag --auto --delete-branch | Out-Host
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "gh pr merge --auto failed. PR is open at $($Ctx.PrUrl); merge manually with '$AutoMergeStrategy' strategy (NOT squash)."
    } else {
        Write-Host "Auto-merge armed with strategy: $AutoMergeStrategy" -ForegroundColor Green
    }
}

# Backfill PR URL into state.last_run AND state.history[0] (best-effort
# follow-up commit). The same run summary object was prepended to history
# earlier in this script — keep both views in sync so 'sessions' reports
# and bug-reports can find the PR link from either field. If this push
# fails the PR is still open and the baseline is still advanced on the
# branch — the only loss is the pr_url field in state, which is
# recoverable from the PR itself.
$state.last_run.pr_url = $Ctx.PrUrl
if ($state.history -and $state.history.Count -gt 0) {
    $state.history[0].pr_url = $Ctx.PrUrl
}
Write-State $state
git add -- (ConvertTo-RepoRelativePath (Get-StatePath)) | Out-Null
git commit -m "chore(upstream-sync): record PR url" | Out-Host
if ($LASTEXITCODE -eq 0) {
    git push origin $branch | Out-Host
    if ($LASTEXITCODE -ne 0) { Write-Warning "Could not push pr_url backfill; PR is still open at $($Ctx.PrUrl)." }
}

return $Ctx.PrUrl
