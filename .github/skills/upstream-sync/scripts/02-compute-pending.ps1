<#
.SYNOPSIS
  Compute the pending cherry-pick list with revert-pair detection.

.DESCRIPTION
  Reads the last-synced upstream watermark from origin/main's
  `cherry picked from commit <sha>` trailers, lists commits in
  watermark..upstream/main (oldest first), detects revert pairs within the
  range and drops them, detects upstream-empty commits and drops them, and
  emits the final pending list as JSON.

.OUTPUTS
  JSON object on stdout:
  {
    "from": "<old_sha>",
    "to":   "<new_sha>",
    "pending":       [ "<sha>", ... ],          # in pick order
    "dropped_pairs": [ ["<orig>", "<revert>"], ... ],
    "skipped_empty": [ "<sha>", ... ]
  }
#>
[CmdletBinding()]
param()

. "$PSScriptRoot/Common.ps1"

# `git fetch upstream main` must have been run already (orchestrator calls
# 01-fetch-upstream.ps1 before us). Get-LastSyncedUpstreamSha walks the
# `cherry picked from commit <sha>` trailers on origin/main back to the most
# recent one that resolves to a commit on upstream/main — no state.json.
$from = Get-LastSyncedUpstreamSha
$to = (git rev-parse upstream/main).Trim()
if ($LASTEXITCODE -ne 0) { throw "git rev-parse upstream/main failed." }

if ($from -eq $to) {
    @{ from = $from; to = $to; pending = @(); dropped_pairs = @(); skipped_empty = @() } | ConvertTo-Json -Depth 5
    return
}

# Patch-id-aware list of full SHAs (oldest-first). Uses Get-PendingUpstreamShas
# from Common.ps1, which wraps `git log --cherry-pick --right-only --no-merges`:
# any upstream commit whose patch ID matches a commit already on origin/main is
# excluded (so picked-then-reverted commits stay out unless their patch is no
# longer on origin/main, in which case they correctly re-appear as pending).
# The revert-pair detection below stays as defense-in-depth and as the source
# of the `dropped_pairs` report field; in practice --cherry-pick already drops
# most pairs, but a same-batch original+revert that wasn't yet on origin/main
# at the time of computation is still useful to surface.
$all = @(Get-PendingUpstreamShas -Since $from)

# Build sha -> first line and body map (single git invocation per commit is fine for typical batch sizes).
$info = @{}
foreach ($sha in $all) {
    $subj = git log -1 --format='%s' $sha
    $body = git log -1 --format='%B' $sha
    $info[$sha] = @{ subject = $subj; body = $body }
}

# Detect revert pairs. Note: when a revert body lists multiple SHAs
# (e.g. "This reverts commit A. This also undoes parts of B"), the first
# match wins — that is, the SHA following the canonical "This reverts
# commit <sha>" line introduced by `git revert`. Bodies that list a
# secondary SHA outside the canonical form are ignored on purpose.
$dropped = New-Object 'System.Collections.Generic.HashSet[string]'
$pairs   = @()
foreach ($sha in $all) {
    if ($dropped.Contains($sha)) { continue }
    $body = $info[$sha].body
    $subj = $info[$sha].subject

    $targetSha = $null
    if ($body -match 'This reverts commit ([0-9a-f]{40})\b') {
        $targetSha = $Matches[1]
    } elseif ($subj -match '^Revert "') {
        # Best-effort fallback: match the quoted original subject. To
        # avoid pairing the revert with a *later* unrelated commit
        # that happens to share the subject, search only the prefix of
        # $all up to (but not including) the current revert — the
        # original must precede its revert in oldest-first order.
        # Also require exactly one match; if subjects repeat, fall
        # through and let the revert land as a normal pick (safer
        # than dropping the wrong commit).
        $origSubject = $subj -replace '^Revert "', '' -replace '"\s*$', '' -replace '"\.?\s*$',''
        $prefix = @()
        foreach ($candidateSha in $all) {
            if ($candidateSha -eq $sha) { break }
            $prefix += $candidateSha
        }
        $candidates = @($prefix | Where-Object {
            $info[$_].subject -eq $origSubject -and -not $dropped.Contains($_)
        })
        if ($candidates.Count -eq 1) { $targetSha = $candidates[0] }
    }

    if ($targetSha -and $info.ContainsKey($targetSha) -and -not $dropped.Contains($targetSha)) {
        [void] $dropped.Add($sha)
        [void] $dropped.Add($targetSha)
        $pairs += ,@($targetSha, $sha)
    }
}

# Detect upstream-empty (no files touched).
$empty = @()
foreach ($sha in $all) {
    if ($dropped.Contains($sha)) { continue }
    $files = git diff-tree --no-commit-id --name-only -r $sha
    if (-not $files) {
        $empty += $sha
        [void] $dropped.Add($sha)
    }
}

$pending = $all | Where-Object { -not $dropped.Contains($_) }

$result = [ordered] @{
    from          = $from
    to            = $to
    pending       = @($pending)
    dropped_pairs = @($pairs)
    skipped_empty = @($empty)
}
$result | ConvertTo-Json -Depth 5
