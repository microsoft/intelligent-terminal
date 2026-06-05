<#
.SYNOPSIS
  Cherry-pick one upstream commit with Tier-0/Tier-1 auto-resolution.

.DESCRIPTION
  Runs `git cherry-pick -x <sha>`. On conflict, attempts Tier-0 (known
  take-{upstream,ours} files from 03-known-conflicts.md), then Tier-1 (empty
  after staging → skip). Anything else returns 'stuck' and leaves the
  cherry-pick aborted.

.PARAMETER Sha
  The upstream commit to pick.

.OUTPUTS
  JSON status object on stdout:
  { "sha":            "...",
    "status":         "picked" | "skipped-empty" | "stuck",
    "tier0_paths":    ["..."],     // files Tier-0 auto-resolved (may be empty)
    "conflict_paths": ["..."],     // unmerged files when status = 'stuck'
    "error":          "..."        // human-readable explanation when status = 'stuck'; empty otherwise
  }
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Sha
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-KnownConflicts {
    $md = Join-Path (Split-Path $PSScriptRoot -Parent) 'references/03-known-conflicts.md'
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
    # path matching against 03-known-conflicts.md works without C-quoting.
    $u = git -c core.quotepath=off diff --name-only --diff-filter=U 2>&1
    if ($LASTEXITCODE -ne 0) { throw "git diff --name-only --diff-filter=U failed: $u" }
    if (-not $u) { return @() }
    return @($u -split "`n" | ForEach-Object { $_.TrimEnd("`r") } | Where-Object { $_ })
}

$result = [ordered] @{
    sha            = $Sha
    status         = 'unknown'
    tier0_paths    = @()
    conflict_paths = @()
    error          = ''
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

# Use ASCII unit separator (\x1F) as the delimiter — that byte is forbidden
# in git ident strings, so it cannot collide with a tab inside an author
# or committer name (git allows tabs in idents). Validate field count
# defensively before binding to GIT_* env vars.
$us   = [char]0x1F
$info = (git log -1 --format="%an${us}%ae${us}%aI${us}%cn${us}%ce${us}%cI" $fullSha) -split $us
if ($LASTEXITCODE -ne 0) { throw "git log failed resolving author/committer for $fullSha (exit $LASTEXITCODE)." }
if ($info.Count -ne 6)   { throw "Unexpected field count parsing author/committer for ${fullSha}: got $($info.Count) (expected 6)." }

# Capture caller's existing GIT_AUTHOR_* / GIT_COMMITTER_* env so the finally
# block can restore them. Higher-level automation may have set these
# intentionally — we don't want to silently wipe them out after our pick.
$prevEnv = @{
    GIT_AUTHOR_NAME     = $env:GIT_AUTHOR_NAME
    GIT_AUTHOR_EMAIL    = $env:GIT_AUTHOR_EMAIL
    GIT_AUTHOR_DATE     = $env:GIT_AUTHOR_DATE
    GIT_COMMITTER_NAME  = $env:GIT_COMMITTER_NAME
    GIT_COMMITTER_EMAIL = $env:GIT_COMMITTER_EMAIL
    GIT_COMMITTER_DATE  = $env:GIT_COMMITTER_DATE
}

$env:GIT_AUTHOR_NAME     = $info[0]
$env:GIT_AUTHOR_EMAIL    = $info[1]
$env:GIT_AUTHOR_DATE     = $info[2]
$env:GIT_COMMITTER_NAME  = $info[3]
$env:GIT_COMMITTER_EMAIL = $info[4]
$env:GIT_COMMITTER_DATE  = $info[5]

try {

# Attempt the pick. Capture the stream so we can both forward it to our
# own stderr (for live operator visibility) AND surface the trailing
# context in the stuck result for non-conflict failures (e.g. merge
# commit without -m, hook failures, unsupported git version).
$pickOutput = git cherry-pick --keep-redundant-commits -x $fullSha 2>&1
$pickCode   = $LASTEXITCODE
$pickOutput | ForEach-Object { [Console]::Error.WriteLine($_) }
$pickTail   = (@($pickOutput) | Select-Object -Last 20 | Out-String).Trim()

if ($pickCode -eq 0) {
    # Tier-1 check: did we just create an empty commit (allowed by --keep-redundant-commits)?
    $changed = git diff-tree --no-commit-id --name-only -r HEAD
    if (-not $changed) {
        $commitMessage = @(git log -1 --format='%B' HEAD) -join "`n"
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

# Conflict — or some other non-zero exit (e.g. trying to cherry-pick a
# merge commit without -m, hook failures, etc.). For *real* conflicts
# Get-ConflictPaths returns the unmerged paths and we try Tier-0; for
# non-conflict failures the list is empty and we must NOT fall through
# to `cherry-pick --continue` (which would explode), nor leave the repo
# mid-cherry-pick. Abort and surface a controlled stuck result.
$conflicts = Get-ConflictPaths
if (-not $conflicts -or $conflicts.Count -eq 0) {
    Write-Warning "git cherry-pick exited $pickCode with no conflict paths — likely a non-conflict failure (e.g. merge commit without -m, unsupported git option). Aborting and marking stuck.`nLast git output:`n$pickTail"
    # If cherry-pick failed *before* starting (bad args / unsupported
    # option / old git), --abort itself fails with "no cherry-pick or
    # revert in progress". That's benign here — there is nothing to
    # clean up. Capture and surface the abort outcome in the stuck
    # result instead of throwing, so the operator still gets the JSON.
    $abortOut = git cherry-pick --abort 2>&1
    $abortCode = $LASTEXITCODE
    $abortNote = ''
    if ($abortCode -ne 0) {
        $abortText = ($abortOut | Out-String).Trim()
        if ($abortText -match 'no cherry-pick or revert in progress') {
            $abortNote = ' (no in-progress cherry-pick to abort — failed before starting)'
        } else {
            $abortNote = " (cherry-pick --abort also failed: exit $abortCode; $abortText)"
        }
    }
    $result.status = 'stuck'
    $result.conflict_paths = @()
    $result.error = "git cherry-pick exited $pickCode with no conflict paths$abortNote. Last output: $pickTail"
    $result | ConvertTo-Json -Compress
    return
}
$result.conflict_paths = $conflicts
$known = Get-KnownConflicts
$unhandled = @()
foreach ($p in $conflicts) {
    $e = $known | Where-Object { $_.Path -eq $p } | Select-Object -First 1
    if (-not $e) { $unhandled += $p; continue }
    $checkoutOk = $true
    switch ($e.Strategy) {
        'take-upstream' {
            git checkout --theirs -- $p 2>&1 | Out-Null
            if ($LASTEXITCODE -ne 0) { $checkoutOk = $false }
        }
        'take-ours' {
            git checkout --ours -- $p 2>&1 | Out-Null
            if ($LASTEXITCODE -ne 0) { $checkoutOk = $false }
        }
        'union' {
            Write-Warning "union strategy not implemented yet for $p"
            $unhandled += $p
            continue
        }
    }
    if (-not $checkoutOk) {
        Write-Warning "git checkout for Tier-0 strategy '$($e.Strategy)' failed on $p; falling back to stuck."
        $unhandled += $p
        continue
    }
    git add -- $p 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "git add failed for Tier-0 path $p; falling back to stuck."
        $unhandled += $p
        continue
    }
    $result.tier0_paths += $p
}

if ($unhandled.Count -gt 0) {
    git cherry-pick --abort | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git cherry-pick --abort failed after unhandled conflicts; repository may still be mid-cherry-pick." }
    $result.status = 'stuck'
    $result.conflict_paths = $unhandled
    $result.error = "Cherry-pick of $Sha hit conflicts not covered by Tier-0 known-conflicts rules in $($unhandled.Count) path(s)."
    $result | ConvertTo-Json -Compress
    return
}

# All conflicts handled by Tier-0; continue the pick (preserve original message).
git cherry-pick --continue --no-edit 2>&1 | ForEach-Object { [Console]::Error.WriteLine($_) }
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
    # Capture any unmerged files left after the failed --continue so the
    # Tier-3 issue body lists *something* concrete.
    $stillUnmerged = @(git diff --name-only --diff-filter=U 2>$null)
    if ($stillUnmerged.Count -gt 0) { $result.conflict_paths = $stillUnmerged }
    $result.error = "git cherry-pick --continue failed after Tier-0 auto-resolution for $Sha; the residual conflict is not mechanically resolvable."
    $result | ConvertTo-Json -Compress
    return
}

$result.status = 'picked'
$result | ConvertTo-Json -Compress

} finally {
    # Restore — not delete — so callers that intentionally set these
    # env vars don't see them wiped after our pick.
    foreach ($k in $prevEnv.Keys) {
        $prev = $prevEnv[$k]
        if ($null -eq $prev) {
            Remove-Item "Env:$k" -ErrorAction SilentlyContinue
        } else {
            Set-Item "Env:$k" -Value $prev
        }
    }
}
