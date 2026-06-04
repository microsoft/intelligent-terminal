<#
.SYNOPSIS
  Set the stuck-lock and open a GitHub issue. Pushes the stuck branch
  to origin so the human can pick it up.

.PARAMETER Ctx
  Run context (must have StuckSha, StuckPaths, Branch set).

.PARAMETER ReportPath
  Absolute path to the stuck report markdown.

.OUTPUTS
  Issue URL on stdout (and writes Ctx.IssueUrl).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] $Ctx,
    [Parameter(Mandatory)] [string] $ReportPath
)

. "$PSScriptRoot/Common.ps1"

if (-not $Ctx.StuckSha) { throw "Ctx.StuckSha is empty — nothing to escalate." }

# Push the stuck branch so the human can resume on it.
git push -u origin $Ctx.Branch 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Write-Warning "Could not push stuck branch — issue still being filed for visibility." }

$shortSha = $Ctx.StuckSha.Substring(0,9)
$subj = (git log -1 --format='%s' $Ctx.StuckSha).Trim()
$title = "Upstream sync stuck at ${shortSha}: $subj"

# Header prepended to report content.
$header = @"
🛑 **Upstream sync stopped at a conflict that needs human judgement.**

The scheduler will keep skipping its runs until this issue is resolved
and the stuck-lock is cleared. No alarm — the lock is intentional.

To unblock, follow "How to resume" in the report below, then close this
issue after ``clear-stuck.ps1`` runs cleanly.

---

"@
$body = $header + (Get-Content -Raw -LiteralPath $ReportPath)
$tmp  = New-TemporaryFile
[System.IO.File]::WriteAllText($tmp, $body, (New-Object System.Text.UTF8Encoding($false)))

# Ensure label exists (best-effort; ignore if already present).
gh label create 'upstream-sync-stuck' --color 'B60205' --description 'Upstream sync blocked on a manual conflict' 2>$null | Out-Null

# -R is explicit because the `upstream` remote can make gh default to microsoft/terminal.
$issueUrl = gh issue create -R microsoft/intelligent-terminal --title $title --label 'upstream-sync-stuck' --body-file $tmp 2>&1 | Select-Object -Last 1
Remove-Item $tmp -Force
if ($LASTEXITCODE -ne 0 -or $issueUrl -notmatch '^https://github.com/') {
    throw "gh issue create failed: $issueUrl"
}
$Ctx.IssueUrl = $issueUrl.Trim()

# Set the stuck-lock on main (direct push — the lock is the gate; PR path is blocked).
git switch main | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Could not switch to main before writing stuck-lock. Resolve manually and re-run; the stuck branch + issue are already in place." }
git pull --ff-only origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Could not fast-forward main before writing stuck-lock. Resolve manually and re-run; the stuck branch + issue are already in place." }

$state = Read-State
$state.stuck_on_sha    = $Ctx.StuckSha
$state.stuck_branch    = $Ctx.Branch
$state.stuck_at        = Format-Iso8601 $Ctx.StartedAt
$state.stuck_issue_url = $Ctx.IssueUrl
$runSummary = [ordered] @{
    at                 = Format-Iso8601 $Ctx.StartedAt
    host               = $Ctx.Host
    status             = 'stuck'
    branch             = $Ctx.Branch
    issue_url          = $Ctx.IssueUrl
    stuck_on_sha       = $Ctx.StuckSha
    picked_count       = $Ctx.Picked.Count
}
$state.last_run = $runSummary
$state.history  = @($runSummary) + @($state.history) | Select-Object -First 20
Write-State $state

# Copy the report into main too (so the report is visible without checking out the stuck branch).
$reportName = Split-Path -Leaf $ReportPath
$reportOnMain = Join-Path (Get-ReportsDir) $reportName
if ($ReportPath -ne $reportOnMain) {
    Copy-Item -LiteralPath $ReportPath -Destination $reportOnMain -Force
}

git add -- (Get-StatePath) $reportOnMain
if ($LASTEXITCODE -ne 0) { throw "git add of stuck state failed." }
git commit -m "chore(upstream-sync): stuck at $shortSha (#$($Ctx.IssueUrl -replace '.*/',''))" | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git commit of stuck-lock failed — lock NOT set on origin/main. The next scheduled run will not see the lock; resolve manually." }
git push origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git push origin main failed — stuck-lock is local only. Push manually before the next scheduler tick or it will re-run over the conflict." }

return $Ctx.IssueUrl
