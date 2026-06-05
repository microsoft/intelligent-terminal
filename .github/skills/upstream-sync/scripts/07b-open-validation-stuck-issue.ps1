<#
.SYNOPSIS
  Tier-4 (post-pick validation failure) stuck-issue opener.

.DESCRIPTION
  Counterpart to 07-open-stuck-issue.ps1 (which handles Tier-3 = cherry-pick
  stopped mid-pick on a real merge conflict). Tier-4 means all picks
  completed cleanly but the static scan, toolchain preflight, or try-build
  step said NO.

  No state.json - the lock is the open labeled issue itself (cleared by
  the human closing the issue). The fenced ```yaml # wta-state``` block
  in the body carries findings_hash so re-runs of the same broken batch
  can be matched against the same issue.

  `toolchain-missing` is special: it's an INFRA problem (this host lacks
  the required VS toolset), not a CODE problem. We do not open a GitHub
  issue for it - issues are noise for things humans can't fix via the PR
  surface. The scheduler simply retries next tick from another host (or
  after provisioning).

.PARAMETER Ctx
  Run context. Must have Branch set; uses Picked, Preflight, Scan, Build.

.PARAMETER ReportPath
  Absolute path to the stuck report markdown.

.PARAMETER Kind
  One of: 'static-scan', 'build-failed', 'build-inconclusive',
  'toolchain-missing'. Determines the issue title and report header.

.OUTPUTS
  Issue URL on stdout (and writes Ctx.IssueUrl + Ctx.StuckValidation).
  For toolchain-missing, returns $null.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] $Ctx,
    [Parameter(Mandatory)] [string] $ReportPath,
    [Parameter(Mandatory)] [ValidateSet('static-scan','build-failed','build-inconclusive','toolchain-missing')] [string] $Kind
)

. "$PSScriptRoot/Common.ps1"

# Compute a findings hash so re-runs of the same broken batch are detectable
# from a future run's gh issue body. Use [ordered] hashtables so JSON
# serialization (and therefore the hash) is stable across runs.
$findingsForHash = switch ($Kind) {
    'static-scan'         { $Ctx.Scan.findings }
    'build-failed'        { @([ordered] @{ exit_code = $Ctx.Build.exit_code; tail_excerpt = ($Ctx.Build.log_tail -split "`n" | Select-Object -Last 20) -join "`n" }) }
    'build-inconclusive'  { @([ordered] @{ kind = 'inconclusive'; duration_ms = $Ctx.Build.duration_ms }) }
    'toolchain-missing'   { @([ordered] @{ missing = @($Ctx.Preflight.missing | Sort-Object) }) }
}
$findingsHash = Get-FindingsHash $findingsForHash

# Push the sync branch so the human can resume on it (even toolchain-missing -
# the picks are still useful artifacts for whoever owns the host).
git push -u origin $Ctx.Branch 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) {
    Write-Warning "Could not push sync branch - issue will still be filed for visibility (when applicable)."
}

# Tier-4 metadata stashed on the context for the orchestrator's logs.
$validation = [ordered] @{
    kind           = $Kind
    branch         = $Ctx.Branch
    range          = @($Ctx.Picked)
    findings_hash  = $findingsHash
    at             = Format-Iso8601 $Ctx.StartedAt
    issue_url      = $null
}

# For toolchain-missing we do NOT open an issue (infra problem, not code -
# and no lock either: the next tick simply retries from any properly
# provisioned host).
if ($Kind -eq 'toolchain-missing') {
    $Ctx.StuckValidation = $validation
    return $null
}

$titleKindLabel = switch ($Kind) {
    'static-scan'         { 'static scan' }
    'build-failed'        { 'build failure' }
    'build-inconclusive'  { 'build inconclusive (timeout)' }
}
$title = "Upstream sync stuck after $($Ctx.Picked.Count) clean picks: $titleKindLabel ($findingsHash)"

$yamlBlock = Format-StuckYamlBlock @{
    tier           = '4'
    kind           = $Kind
    branch         = $Ctx.Branch
    findings_hash  = $findingsHash
    picked_count   = $Ctx.Picked.Count
    at             = Format-Iso8601 $Ctx.StartedAt
    host           = $Ctx.Host
}

$header = @"
> [!CAUTION]
> **Upstream sync stopped after validation failed.**
>
> All $($Ctx.Picked.Count) cherry-pick(s) applied cleanly, but the post-batch
> validation step said NO before any PR was opened. Stop reason: **$Kind**.
>
> The scheduler will keep skipping its runs until this issue is **closed**.
> Closing the issue IS the lock-clear signal - no separate script needed.

Sync branch: ``$($Ctx.Branch)`` (pushed to origin).
Findings hash: ``$findingsHash`` (re-runs of the same broken batch will match).

$yamlBlock

---

"@
$body = $header + (Get-Content -Raw -LiteralPath $ReportPath)
$tmp  = New-TemporaryFile
[System.IO.File]::WriteAllText($tmp, $body, (New-Object System.Text.UTF8Encoding($false)))

# Ensure label exists (best-effort). -R pinned for the same reason as the
# issue-create call below (avoid the `upstream` remote tricking gh into
# microsoft/terminal).
gh label create 'upstream-sync-stuck' --color 'B60205' --description 'Upstream sync blocked on a manual issue' -R microsoft/intelligent-terminal 2>$null | Out-Null

# Capture stderr to a separate temp file so a `gh` warning on stderr can't
# displace the URL as the "last line" of merged output.
$errFile  = [System.IO.Path]::GetTempFileName()
$errText  = ''
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
$validation.issue_url = $issueUrl.Trim()
$Ctx.IssueUrl        = $validation.issue_url
$Ctx.StuckValidation = $validation

return $validation.issue_url
