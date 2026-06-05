<#
.SYNOPSIS
  Push the sync branch and open a PR. No state file, no extra commits.

.DESCRIPTION
  The branch already carries the cherry-picked commits (each with its
  `(cherry picked from commit <sha>)` trailer - that IS the watermark
  the next run reads). We just push it and open the PR. No state.json
  to commit, no pr_url backfill commit, no extra round-trip after PR
  creation.

.PARAMETER Ctx
  Run context from 04-run-batch.ps1.

.PARAMETER To
  Upstream HEAD SHA at fetch time (used only in the PR title).

.PARAMETER ReportPath
  Absolute path to the report markdown to use as the PR body. The report
  itself is NOT committed - just inlined into the PR body text.

.PARAMETER AutoMergeStrategy
  rebase | merge | none. Passed to `gh pr merge --auto`.

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

# Prepend the squash-warning + review-policy banner to the report so it
# lands as the first thing reviewers see.
$banner = @"
> [!WARNING]
> **DO NOT squash-merge this PR.** Squashing collapses every cherry-picked
> upstream commit into one, destroying per-commit attribution, original
> author dates, the ``(cherry picked from commit <sha>)`` trailers that the
> NEXT upstream sync uses as its watermark, and ``git bisect`` resolution.
> Merge with **"Rebase and merge"** (preferred - flat history, all
> $($Ctx.Picked.Count) commits land individually) or **"Create a merge
> commit"** (also preserves per-commit content).

> [!NOTE]
> **Review-fix policy.** Only build-blocking fixes (compile errors, dedup
> of conflicts surfaced at build time, CI gate failures on this PR itself)
> belong here - as **one** focused extra commit on this branch. All other
> Copilot / human review feedback (code-quality, logic, translation,
> spelling-list migrations, doc nits) goes into a **follow-up PR** based on
> this PR's head. Rationale and mechanics:
> [``.github/skills/upstream-sync/references/follow-up-pr.md``](https://github.com/microsoft/intelligent-terminal/blob/main/.github/skills/upstream-sync/references/follow-up-pr.md).

---

"@
$bodyPath = New-TemporaryFile
$bodyContent = $banner + (Get-Content -Raw -LiteralPath $ReportPath)
[System.IO.File]::WriteAllText($bodyPath, $bodyContent, (New-Object System.Text.UTF8Encoding($false)))

$branch  = $Ctx.Branch
$shortTo = $To.Substring(0,9)

# Push the sync branch (cherry-pick commits already have their `-x`
# trailers; those trailers ARE the watermark - nothing else to commit).
git push -u origin $branch | Out-Host
if ($LASTEXITCODE -ne 0) {
    Remove-Item -LiteralPath $bodyPath -Force -ErrorAction SilentlyContinue
    throw "git push failed for $branch."
}

$title = "chore(upstream): sync microsoft/terminal up to $shortTo"

# Same-repo PR: `--head` takes the bare branch name. Retry up to 3 times
# with a short delay - `gh pr create` on Windows occasionally fails with
# "Head sha can't be blank" right after a push.
$prUrl   = $null
$errFile = [System.IO.Path]::GetTempFileName()
try {
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        # Capture stderr to a separate temp file: a `gh` version-update /
        # deprecation notice on stderr can otherwise become the last line
        # of merged output, breaking URL match.
        Set-Content -LiteralPath $errFile -Value '' -NoNewline
        $prUrl = gh pr create -R microsoft/intelligent-terminal --base main --head $branch --title $title --body-file $bodyPath 2>$errFile | Select-Object -Last 1
        if ($LASTEXITCODE -eq 0 -and $prUrl -match '^https://github.com/') { break }
        $errText = if (Test-Path -LiteralPath $errFile) { (Get-Content -Raw -LiteralPath $errFile) } else { '' }
        Write-Warning "gh pr create attempt $attempt failed (exit $LASTEXITCODE): stdout='$prUrl' stderr='$errText'"
        Start-Sleep -Seconds 5
    }
    if ($LASTEXITCODE -ne 0 -or $prUrl -notmatch '^https://github.com/') {
        $errText = if (Test-Path -LiteralPath $errFile) { (Get-Content -Raw -LiteralPath $errFile) } else { '' }
        throw "gh pr create did not return a PR URL after 3 attempts. Last stdout: '$prUrl'. Last stderr: '$errText'."
    }
}
finally {
    Remove-Item -LiteralPath $bodyPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $errFile  -Force -ErrorAction SilentlyContinue
}

$Ctx.PrUrl = $prUrl.Trim()

# Optional: arm GitHub auto-merge with a strategy that preserves per-commit
# history. 'rebase' is the recommended default - it lands all N commits
# flatly on main once CI + approvals pass. Never squash.
if ($AutoMergeStrategy -ne 'none') {
    $strategyFlag = "--$AutoMergeStrategy"
    gh pr merge -R microsoft/intelligent-terminal $Ctx.PrUrl $strategyFlag --auto --delete-branch | Out-Host
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "gh pr merge --auto failed. PR is open at $($Ctx.PrUrl); merge manually with '$AutoMergeStrategy' strategy (NOT squash)."
    } else {
        Write-Host "Auto-merge armed with strategy: $AutoMergeStrategy" -ForegroundColor Green
    }
}

return $Ctx.PrUrl
