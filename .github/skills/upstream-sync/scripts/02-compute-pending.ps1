<#
.SYNOPSIS
  Compute the pending cherry-pick list with revert-pair detection.

.DESCRIPTION
  Reads state.last_synced_upstream_sha, lists commits in
  state.last_synced..upstream/main (oldest first), detects revert pairs
  within the range and drops them, detects upstream-empty commits and
  drops them, and emits the final pending list as JSON.

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

$state = Read-State
$from  = [string]$state.last_synced_upstream_sha
if (-not $from) { throw "state.last_synced_upstream_sha is empty. Run bootstrap." }

$to = (git rev-parse upstream/main).Trim()
if ($LASTEXITCODE -ne 0) { throw "git rev-parse upstream/main failed." }

if ($from -eq $to) {
    @{ from = $from; to = $to; pending = @(); dropped_pairs = @(); skipped_empty = @() } | ConvertTo-Json -Depth 5
    return
}

# Oldest-first list of full SHAs.
$all = git log --reverse --format='%H' "$from..$to"
if ($LASTEXITCODE -ne 0) { throw "git log failed." }
$all = @($all | Where-Object { $_ })

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
