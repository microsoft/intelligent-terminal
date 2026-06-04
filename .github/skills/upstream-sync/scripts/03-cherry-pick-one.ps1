<#
.SYNOPSIS
  Cherry-pick one upstream commit with Tier-0/Tier-1 auto-resolution.

.DESCRIPTION
  Runs `git cherry-pick -x <sha>`. On conflict, attempts Tier-0 (known
  take-{upstream,ours} files from known-conflicts.md), then Tier-1 (empty
  after staging → skip). Anything else returns 'stuck' and leaves the
  cherry-pick aborted.

.PARAMETER Sha
  The upstream commit to pick.

.OUTPUTS
  JSON status object on stdout:
  { "sha": "...", "status": "picked|skipped-empty|stuck",
    "tier0_paths": ["..."], "conflict_paths": ["..."] }
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Sha
)

. "$PSScriptRoot/Common.ps1"

function Get-KnownConflicts {
    $md = Join-Path (Split-Path $PSScriptRoot -Parent) 'references/known-conflicts.md'
    if (-not (Test-Path $md)) { return @() }
    $lines = Get-Content -LiteralPath $md
    $entries = @()
    $current = $null
    foreach ($l in $lines) {
        if ($l -match '^##\s+`([^`]+)`\s*$') {
            if ($current) { $entries += $current }
            $current = @{ Path = $Matches[1]; Strategy = $null }
        } elseif ($current -and $l -match '^\*\*Strategy:\*\*\s+`(take-upstream|take-ours|union)`') {
            $current.Strategy = $Matches[1]
        }
    }
    if ($current) { $entries += $current }
    return $entries | Where-Object { $_.Strategy }
}

function Get-ConflictPaths {
    # core.quotepath=off keeps non-ASCII paths in raw UTF-8 so Tier-0
    # path matching against known-conflicts.md works without C-quoting.
    $u = git -c core.quotepath=off diff --name-only --diff-filter=U
    if (-not $u) { return @() }
    return @($u -split "`n" | Where-Object { $_ })
}

$result = [ordered] @{
    sha            = $Sha
    status         = 'unknown'
    tier0_paths    = @()
    conflict_paths = @()
}

# Capture upstream's author AND committer identity + dates so the
# resulting commit is per-commit identical (modulo SHA + GPG signature)
# to the upstream original. cherry-pick preserves author by default;
# we pin both sides via env for symmetry and clarity. Each iteration of
# the pick loop runs this lookup against the specific upstream SHA, so
# every commit gets its own original dates — never a single "run time"
# fixed timestamp.
#
# Note: this does NOT reproduce GPG signatures (we don't hold upstream's
# keys). The fork doesn't enforce signed commits, so "committed by X but
# unsigned" is acceptable.
$fullSha = (git rev-parse $Sha).Trim()
if ($LASTEXITCODE -ne 0) { throw "Could not resolve upstream commit $Sha." }
$prePickHead = (git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw "Could not record pre-pick HEAD before cherry-picking $Sha." }

$info = (git log -1 --format='%an%x09%ae%x09%aI%x09%cn%x09%ce%x09%cI' $fullSha) -split "`t"
$env:GIT_AUTHOR_NAME     = $info[0]
$env:GIT_AUTHOR_EMAIL    = $info[1]
$env:GIT_AUTHOR_DATE     = $info[2]
$env:GIT_COMMITTER_NAME  = $info[3]
$env:GIT_COMMITTER_EMAIL = $info[4]
$env:GIT_COMMITTER_DATE  = $info[5]

try {

# Attempt the pick.
git cherry-pick --keep-redundant-commits -x $fullSha 2>&1 | Out-Host
$pickCode = $LASTEXITCODE

if ($pickCode -eq 0) {
    # Tier-1 check: did we just create an empty commit (allowed by --keep-redundant-commits)?
    $changed = git diff-tree --no-commit-id --name-only -r HEAD
    if (-not $changed) {
        $commitMessage = (git log -1 --format='%B' HEAD) -join "`n"
        $expectedFooter = "\(cherry picked from commit $([regex]::Escape($fullSha))\)"
        if ($commitMessage -notmatch $expectedFooter) {
            throw "Refusing to reset --hard ${prePickHead}: HEAD does not contain the cherry-pick footer for $fullSha. Investigate before retrying."
        }
        git reset --hard $prePickHead | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Failed to reset empty cherry-pick back to $prePickHead." }
        $result.status = 'skipped-empty'
    } else {
        $result.status = 'picked'
    }
    $result | ConvertTo-Json -Compress
    return
}

# Conflict. Try Tier-0.
$conflicts = Get-ConflictPaths
$result.conflict_paths = $conflicts
$known = Get-KnownConflicts
$unhandled = @()
foreach ($p in $conflicts) {
    $e = $known | Where-Object { $_.Path -eq $p } | Select-Object -First 1
    if (-not $e) { $unhandled += $p; continue }
    switch ($e.Strategy) {
        'take-upstream' { git checkout --theirs -- $p | Out-Null }
        'take-ours'     { git checkout --ours    -- $p | Out-Null }
        'union'         { Write-Warning "union strategy not implemented yet for $p"; $unhandled += $p; continue }
    }
    git add -- $p | Out-Null
    $result.tier0_paths += $p
}

if ($unhandled.Count -gt 0) {
    git cherry-pick --abort | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git cherry-pick --abort failed after unhandled conflicts; repository may still be mid-cherry-pick." }
    $result.status = 'stuck'
    $result.conflict_paths = $unhandled
    $result | ConvertTo-Json -Compress
    return
}

# All conflicts handled by Tier-0; continue the pick (preserve original message).
git cherry-pick --continue --no-edit 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) {
    # Could still be empty after Tier-0.
    $staged = git diff --cached --name-only
    if (-not $staged) {
        git cherry-pick --skip | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "git cherry-pick --skip failed after an empty Tier-0 continuation." }
        $result.status = 'skipped-empty'
        $result | ConvertTo-Json -Compress
        return
    }
    git cherry-pick --abort | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git cherry-pick --abort failed after Tier-0 continuation failed; repository may still be mid-cherry-pick." }
    $result.status = 'stuck'
    $result | ConvertTo-Json -Compress
    return
}

$result.status = 'picked'
$result | ConvertTo-Json -Compress

} finally {
    Remove-Item Env:GIT_AUTHOR_NAME     -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_AUTHOR_EMAIL    -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_AUTHOR_DATE     -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_COMMITTER_NAME  -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_COMMITTER_EMAIL -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_COMMITTER_DATE  -ErrorAction SilentlyContinue
}
