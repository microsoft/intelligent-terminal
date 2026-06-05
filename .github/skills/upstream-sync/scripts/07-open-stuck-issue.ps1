<#
.SYNOPSIS
  Tier-3 (cherry-pick stopped at a real merge conflict) stuck-issue opener.

.DESCRIPTION
  No state.json. The "lock" is the OPEN labeled issue itself - the next
  scheduler run calls Get-StuckIssues and bails when this issue is found.
  The issue body carries a fenced ```yaml # wta-state``` block with the
  same metadata the old state.json held (tier, stuck_on_sha, branch,
  findings_hash) so re-runs can reason about it.

  Cleared by: a human closes the issue. That's it - no separate script.

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

if (-not $Ctx.StuckSha) { throw "Ctx.StuckSha is empty - nothing to escalate." }

# Push the stuck branch so the human can resume on it.
git push -u origin $Ctx.Branch 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Write-Warning "Could not push stuck branch - issue still being filed for visibility." }

$shortSha = $Ctx.StuckSha.Substring(0,9)
$subj = (git log -1 --format='%s' $Ctx.StuckSha).Trim()
$title = "Upstream sync stuck at ${shortSha}: $subj"

# Build the lock-state YAML block. Schedulers read this on the next run
# via Get-StuckMetaFromIssue - it carries enough metadata to recognize a
# re-issue of the same failure without re-spamming a new ticket.
$stuckErrorVal = if ($Ctx.PSObject.Properties.Name -contains 'StuckError' -and $Ctx.StuckError) { $Ctx.StuckError } else { '' }
$yamlBlock = Format-StuckYamlBlock @{
    tier          = '3'
    kind          = 'cherry-pick-conflict'
    stuck_on_sha  = $Ctx.StuckSha
    branch        = $Ctx.Branch
    at            = Format-Iso8601 $Ctx.StartedAt
    host          = $Ctx.Host
    conflict_count = ($Ctx.StuckPaths | Measure-Object).Count
    error         = $stuckErrorVal
}

$header = @"
> [!CAUTION]
> **Upstream sync stopped at a conflict that needs human judgment.**
>
> The scheduler will keep skipping its runs until this issue is **closed**.
> Closing the issue IS the lock-clear signal - no separate script needed.

**How to unblock:** follow "How to resume" in the report excerpt below,
merge your manual-resolution PR (keeping the ``(cherry picked from commit
<sha>)`` trailer - that's what the next sync run reads as its watermark),
then close this issue.

$yamlBlock

---

"@
$body = $header + (Get-Content -Raw -LiteralPath $ReportPath)
$tmp  = New-TemporaryFile
[System.IO.File]::WriteAllText($tmp, $body, (New-Object System.Text.UTF8Encoding($false)))

# Ensure label exists (best-effort). -R pinned because an `upstream` remote
# can make `gh` default to microsoft/terminal where this account has no
# label-create permission.
gh label create 'upstream-sync-stuck' --color 'B60205' --description 'Upstream sync blocked on a manual conflict' -R microsoft/intelligent-terminal 2>$null | Out-Null

# Capture stderr to a separate temp file so a `gh` warning on stderr can't
# displace the URL as the "last line" of merged output.
$errFile = [System.IO.Path]::GetTempFileName()
$errText = ''
$issueUrl = $null
$ghExit   = 0
try {
    $issueUrl = gh issue create -R microsoft/intelligent-terminal --title $title --label 'upstream-sync-stuck' --body-file $tmp 2>$errFile | Select-Object -Last 1
    $ghExit   = $LASTEXITCODE
    if (Test-Path -LiteralPath $errFile) { $errText = (Get-Content -Raw -LiteralPath $errFile) }
}
finally {
    Remove-Item -LiteralPath $tmp     -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $errFile -Force -ErrorAction SilentlyContinue
}
if ($ghExit -ne 0 -or $issueUrl -notmatch '^https://github.com/') {
    throw "gh issue create failed (exit $ghExit): stdout='$issueUrl' stderr='$errText'"
}
$Ctx.IssueUrl = $issueUrl.Trim()

# That's it. The open labeled issue IS the lock - no state file to write,
# no main-branch commit, nothing to push. The next scheduler run will see
# the open issue via Get-StuckIssues and skip.
return $Ctx.IssueUrl
