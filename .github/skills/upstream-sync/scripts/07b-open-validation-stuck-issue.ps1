<#
.SYNOPSIS
  Tier-4 (post-pick validation failure) stuck-issue opener.

  Counterpart to 07-open-stuck-issue.ps1 (which handles Tier-3 = a
  cherry-pick stopped mid-pick on a real merge conflict). Tier-4 means
  all picks completed cleanly but the static scan, toolchain preflight,
  or try-build step said NO.

.PARAMETER Ctx
  Run context. Must have Branch set; uses Picked, Preflight, Scan, Build.

.PARAMETER ReportPath
  Absolute path to the stuck report markdown.

.PARAMETER Kind
  One of: 'static-scan', 'build-failed', 'build-inconclusive',
  'toolchain-missing'. Determines the issue title and report header.

.OUTPUTS
  Issue URL on stdout (and writes Ctx.IssueUrl + Ctx.StuckValidation).

  toolchain-missing is special: it's an INFRA problem (this host lacks
  the required VS toolset), not a CODE problem. We do not open a
  GitHub issue for it — issues are noise for things humans can't fix
  from the PR side. Instead we set the stuck-lock so the scheduler
  won't keep retrying, and the host owner notices via the next dev
  run / monitoring.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] $Ctx,
    [Parameter(Mandatory)] [string] $ReportPath,
    [Parameter(Mandatory)] [ValidateSet('static-scan','build-failed','build-inconclusive','toolchain-missing')] [string] $Kind
)

. "$PSScriptRoot/Common.ps1"

# Compute a findings hash so re-runs of the same broken batch are detectable.
# Use [ordered] hashtables so JSON serialization (and therefore the hash) is
# stable across runs — plain @{} property order is not guaranteed.
$findingsForHash = switch ($Kind) {
    'static-scan'         { $Ctx.Scan.findings }
    'build-failed'        { @([ordered] @{ exit_code = $Ctx.Build.exit_code; tail_excerpt = ($Ctx.Build.log_tail -split "`n" | Select-Object -Last 20) -join "`n" }) }
    'build-inconclusive'  { @([ordered] @{ kind = 'inconclusive'; duration_ms = $Ctx.Build.duration_ms }) }
    'toolchain-missing'   { @([ordered] @{ missing = @($Ctx.Preflight.missing | Sort-Object) }) }
}
$findingsHash = Get-FindingsHash $findingsForHash

# Establish base/head for this batch (best-effort; tolerate detached states).
# `base` must be the *pre-pick* origin/main, not its current tip, because
# origin/main may have advanced while the run was in progress. The merge-base
# of the sync tip and origin/main recovers the branch's original starting
# point reliably.
$headRaw = git rev-parse HEAD 2>$null
$head = if ($LASTEXITCODE -eq 0 -and $headRaw) { $headRaw.Trim() } else { $null }
$base = $null
if ($head) {
    $baseRaw = git merge-base HEAD origin/main 2>$null
    if ($LASTEXITCODE -eq 0 -and $baseRaw) { $base = $baseRaw.Trim() }
}
if (-not $base) {
    # Fallback: use origin/main tip if merge-base failed (e.g. no upstream ref).
    $baseRaw = git rev-parse origin/main 2>$null
    if ($LASTEXITCODE -eq 0 -and $baseRaw) { $base = $baseRaw.Trim() }
}

# Push the sync branch so the human can resume on it (even toolchain-missing —
# the picks are still useful artifacts for whoever owns the host).
git push -u origin $Ctx.Branch 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) {
    Write-Warning "Could not push sync branch — stuck-lock will still be set."
}

# Build validation payload (stored in state.json regardless of whether an issue is filed).
$validation = [ordered] @{
    kind            = $Kind
    base            = $base
    head            = $head
    branch          = $Ctx.Branch
    range           = @($Ctx.Picked)
    findings_hash   = $findingsHash
    at              = Format-Iso8601 $Ctx.StartedAt
    issue_url       = $null
}

# For toolchain-missing we do NOT open an issue (infra problem, not code).
if ($Kind -ne 'toolchain-missing') {
    $titleKindLabel = switch ($Kind) {
        'static-scan'         { 'static scan' }
        'build-failed'        { 'build failure' }
        'build-inconclusive'  { 'build inconclusive (timeout)' }
    }
    $title = "Upstream sync stuck after $($Ctx.Picked.Count) clean picks: $titleKindLabel ($findingsHash)"

    $header = @"
🛑 **Upstream sync stopped after validation failed.**

All $($Ctx.Picked.Count) cherry-pick(s) applied cleanly, but the post-batch
validation step said NO before any PR was opened. Stop reason: **$Kind**.

The scheduler will keep skipping its runs until this is resolved and
``clear-stuck.ps1`` runs cleanly. No alarm — the lock is intentional.

Sync branch: ``$($Ctx.Branch)`` (pushed to origin).
Findings hash: ``$findingsHash`` (re-runs of the same broken batch will match).

---

"@
    $body = $header + (Get-Content -Raw -LiteralPath $ReportPath)
    $tmp  = New-TemporaryFile
    [System.IO.File]::WriteAllText($tmp, $body, (New-Object System.Text.UTF8Encoding($false)))

    # Ensure label exists (best-effort).
    gh label create 'upstream-sync-stuck' --color 'B60205' --description 'Upstream sync blocked on a manual issue' 2>$null | Out-Null

    $issueUrl = gh issue create -R microsoft/intelligent-terminal --title $title --label 'upstream-sync-stuck' --body-file $tmp 2>&1 | Select-Object -Last 1
    Remove-Item $tmp -Force
    if ($LASTEXITCODE -ne 0 -or $issueUrl -notmatch '^https://github.com/') {
        throw "gh issue create failed: $issueUrl"
    }
    $validation.issue_url = $issueUrl.Trim()
    $Ctx.IssueUrl = $validation.issue_url
}

$Ctx.StuckValidation = $validation

# Write the lock onto origin/main.
git switch main | Out-Null
if ($LASTEXITCODE -ne 0) { throw "Could not switch to main before writing stuck-lock. Resolve manually and re-run." }
git pull --ff-only origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "Could not fast-forward main before writing stuck-lock. Resolve manually and re-run." }

$state = Read-State
$state.stuck_validation = $validation
# Single active lock: clear the Tier-3 fields when setting a Tier-4 lock so
# state.json reflects one stuck reason, not two stale ones.
foreach ($f in 'stuck_on_sha','stuck_branch','stuck_at','stuck_issue_url') {
    if ($state.PSObject.Properties.Name -contains $f) {
        $state.$f = $null
    }
}
$runSummary = [ordered] @{
    at             = Format-Iso8601 $Ctx.StartedAt
    host           = $Ctx.Host
    status         = "stuck-$Kind"
    branch         = $Ctx.Branch
    issue_url      = $validation.issue_url
    findings_hash  = $findingsHash
    picked_count   = $Ctx.Picked.Count
}
$state.last_run = $runSummary
$state.history  = @($runSummary) + @($state.history) | Select-Object -First 20
Write-State $state

# Copy report into main reports/ so it's discoverable without checking out the branch.
$reportName   = Split-Path -Leaf $ReportPath
$reportOnMain = Join-Path (Get-ReportsDir) $reportName
if ($ReportPath -ne $reportOnMain) { Copy-Item -LiteralPath $ReportPath -Destination $reportOnMain -Force }

git add -- (ConvertTo-RepoRelativePath (Get-StatePath)) (ConvertTo-RepoRelativePath $reportOnMain) | Out-Null
if ($LASTEXITCODE -ne 0) { throw "git add failed for Tier-4 stuck state." }

$msgIssueRef = if ($validation.issue_url) { " (#$($validation.issue_url -replace '.*/',''))" } else { ' (no issue — infra)' }
git commit -m "chore(upstream-sync): stuck post-pick: $Kind$msgIssueRef" | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git commit of Tier-4 stuck-lock failed — lock NOT set on origin/main." }
git push origin main | Out-Host
if ($LASTEXITCODE -ne 0) { throw "git push origin main failed — Tier-4 stuck-lock local only." }

return $validation.issue_url
