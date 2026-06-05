# Common.ps1 — shared helpers for upstream-sync scripts.
# Dot-source from each script:  . "$PSScriptRoot/Common.ps1"
#
# State model
# -----------
# This skill does NOT keep a state.json file. Every persistent fact lives in
# the authoritative source that owns it:
#
#   * "What's already been picked?" -> the `cherry picked from commit <sha>`
#     trailers we write on every `git cherry-pick -x` (parsed from origin/main).
#   * "What's pending?"             -> `git log --cherry-pick --right-only`.
#   * "Is the scheduler locked?"    -> any OPEN gh issue carrying the
#     `upstream-sync-stuck` label. Lock metadata (kind, tier, stuck_on_sha,
#     findings_hash) is encoded in a fenced ```yaml # wta-state ... ``` block
#     in the issue body so re-runs can recognize the same failure.
#   * "Transient artifacts" (build logs, generated reports) -> written under
#     `Generated Files/upstream-sync/<YYYY-MM-DD>/` which is gitignored at the
#     repo root (`**/Generated Files/`). Never committed.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# ---------------------------------------------------------------------------
# Repo + path helpers
# ---------------------------------------------------------------------------

function Get-RepoRoot {
    $r = git rev-parse --show-toplevel 2>$null
    if ($LASTEXITCODE -ne 0) { throw "Not inside a git repo." }
    return $r.Trim()
}

function ConvertTo-RepoRelativePath {
    # Normalize a path to forward-slash, repo-relative form so callers can
    # safely embed it in committed text without leaking machine-specific
    # drive letters / user dirs.
    param([Parameter(Mandatory)] [string] $Path)
    $root = ((Get-RepoRoot) -replace '\\','/').TrimEnd('/')
    $abs  = $Path -replace '\\','/'
    if ($abs.Equals($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "ConvertTo-RepoRelativePath: refusing to return empty (path == repo root): $Path"
    }
    $prefix = "$root/"
    if ($abs.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $abs.Substring($prefix.Length)
    }
    throw "ConvertTo-RepoRelativePath: '$Path' is not under repo root '$root'."
}

function Get-GeneratedDir {
    # Per-skill, per-day artifact directory under the repo's gitignored
    # `Generated Files/` root (matches the workspace convention used by other
    # skills; the repo's top-level .gitignore has `**/Generated Files/`).
    # Optional -Sub appends a subdirectory (e.g. 'build-logs').
    param([string] $Sub)
    $root = Get-RepoRoot
    $date = (Get-Date).ToString('yyyy-MM-dd')
    $path = Join-Path $root "Generated Files/upstream-sync/$date"
    if ($Sub) { $path = Join-Path $path $Sub }
    if (-not (Test-Path -LiteralPath $path)) {
        New-Item -ItemType Directory -Path $path -Force | Out-Null
    }
    return $path
}

# ---------------------------------------------------------------------------
# Git + remote setup
# ---------------------------------------------------------------------------

function Ensure-UpstreamRemote {
    param(
        [string] $Name = 'upstream',
        [string] $Url  = 'https://github.com/microsoft/terminal.git'
    )
    $existing = git remote get-url $Name 2>$null
    if ($LASTEXITCODE -ne 0) {
        git remote add $Name $Url | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Failed to add remote $Name." }
    } elseif ($existing.Trim() -ne $Url) {
        throw "Remote '$Name' points at '$($existing.Trim())' (expected '$Url'). Fix the remote before running upstream-sync."
    }
}

function Assert-CleanWorktree {
    $dirty = git status --porcelain
    if ($LASTEXITCODE -ne 0) { throw "git status failed." }
    if ($dirty) {
        throw "Working tree is not clean:`n$dirty`nCommit or stash first."
    }
}

function Resolve-FullCommitSha {
    param([Parameter(Mandatory)] [string] $Sha)
    $full = (git rev-parse "$Sha^{commit}" 2>$null)
    if ($LASTEXITCODE -ne 0 -or -not $full) { throw "Could not resolve commit SHA '$Sha'." }
    return $full.Trim()
}

# ---------------------------------------------------------------------------
# Derived state — replaces the old Read-State/Write-State on state.json
# ---------------------------------------------------------------------------

function Get-LastSyncedUpstreamSha {
    # "How far have we already synced from upstream/main?" Derived from the
    # `(cherry picked from commit <sha>)` trailers that `git cherry-pick -x`
    # writes on every pick. We walk origin/main newest-first and return the
    # FIRST trailer that points at a commit reachable from upstream/main.
    #
    # Why newest-first instead of "highest topological position": cherry-picks
    # land in chronological order on origin/main, so the most recent trailer
    # is the watermark. A picked-then-reverted upstream commit will appear in
    # an OLDER trailer (with a corresponding `Revert "..."` commit later) -
    # `git cherry` (used by Get-PendingUpstreamShas) will see the revert and
    # correctly re-list it as pending if needed, so this watermark only needs
    # to be the high-water-mark of progress, not the strict frontier.
    #
    # Known limitation: a HUMAN who manually cherry-picks an upstream hotfix
    # onto origin/main jumps the watermark forward even though earlier
    # upstream commits remain unsynced. Mitigation: Get-PendingUpstreamShas
    # uses patch-id comparison against ALL of origin/main (not just the
    # commits after the watermark), so the unsynced earlier commits will
    # still be picked up on the next scheduler run. The watermark only
    # narrows the `git log` walk for speed - it isn't load-bearing for
    # correctness of the pending list.
    #
    # Performance: capped at the most recent 5000 origin/main commits via
    # --max-count. Even years of fork history fits well under that cap; if
    # a real deployment ever exceeds it we re-throw with the standard
    # not-found message so the operator can re-seed with the fast path.
    # Requires `upstream` remote fetched: caller must Ensure-UpstreamRemote +
    # `git fetch upstream main --no-tags` first.
    $commits = @(git log origin/main --max-count=5000 --grep='cherry picked from commit' --format='%H' 2>$null)
    if ($LASTEXITCODE -ne 0) { throw "git log on origin/main failed while deriving last-synced SHA." }
    foreach ($c in $commits) {
        $body = git log -1 --format='%B' $c 2>$null
        if ($body -match '\(cherry picked from commit ([0-9a-f]{7,40})\)') {
            $rawSha = $matches[1]
            # Resolve to full 40-char SHA first so the ancestry check below
            # works against a canonical object name (an abbreviated or ambiguous
            # prefix could cause `git merge-base --is-ancestor` to fail and
            # silently skip an otherwise-valid watermark candidate).
            $fullSha = $null
            try { $fullSha = Resolve-FullCommitSha $rawSha } catch { continue }
            $null = git merge-base --is-ancestor $fullSha upstream/main 2>$null
            if ($LASTEXITCODE -eq 0) {
                return $fullSha
            }
        }
    }
    throw "No 'cherry picked from commit' trailer pointing at upstream/main was found on origin/main (scanned the most recent 5000 commits). Either the fork hasn't synced via this skill before, or the trailer convention drifted. The very first sync needs an operator to seed the watermark commit (see SKILL.md - 'First-time sync')."
}

function Get-PendingUpstreamShas {
    # Returns upstream/main commits that don't have an equivalent on
    # origin/main, in chronological (oldest-first) order. Uses
    # `git log --cherry-pick --right-only` which compares patch IDs (not just
    # trailers), so picked-and-reverted commits correctly re-appear.
    #
    # The range we walk is ALWAYS `origin/main...upstream/main` regardless of
    # -Since. -Since (the watermark) is treated as a known floor: we drop any
    # commit that is an ancestor of -Since. This way:
    #   * The patch-id filter still considers ALL of origin/main (so commits
    #     that landed on origin/main outside the scheduler's trailer trail -
    #     e.g. a manual cherry-pick - still get filtered out).
    #   * The watermark only trims the obviously-old tail at the bottom of
    #     the resulting list, which is what makes the walk fast.
    param(
        [string] $Since,
        [int]    $Limit = 0
    )
    $out = @(git log --cherry-pick --right-only --no-merges --format='%H' --reverse 'origin/main...upstream/main' 2>$null)
    if ($LASTEXITCODE -ne 0) { throw "git log --cherry-pick failed while computing pending list." }
    $shas = @($out | Where-Object { $_ -match '^[0-9a-f]{40}$' })
    if ($Since) {
        # Skip commits that are ancestors of the watermark. We can't use
        # `<sha>..upstream/main` for the range because that would re-include
        # commits filtered by --cherry-pick; explicit per-sha ancestry check
        # is O(n) git calls but n is small (only the suffix matters).
        $filtered = New-Object 'System.Collections.Generic.List[string]'
        foreach ($sha in $shas) {
            $null = git merge-base --is-ancestor $sha $Since 2>$null
            if ($LASTEXITCODE -ne 0) { [void] $filtered.Add($sha) }
        }
        $shas = @($filtered)
    }
    if ($Limit -gt 0) { $shas = @($shas | Select-Object -First $Limit) }
    return ,$shas
}

# ---------------------------------------------------------------------------
# Stuck-lock — derived from open `upstream-sync-stuck` labeled issues
# ---------------------------------------------------------------------------

$script:StuckLabel    = 'upstream-sync-stuck'
$script:WtaStateFence = '# wta-state'  # marker inside ```yaml ... ``` blocks

function Get-StuckIssues {
    # Returns all OPEN issues carrying the upstream-sync-stuck label. -R is
    # pinned because an `upstream` remote can trick gh into defaulting to
    # microsoft/terminal, where this account has no permission. Stderr goes
    # to a temp file so a gh deprecation/version notice can't break the JSON.
    $errFile = [System.IO.Path]::GetTempFileName()
    $errText = ''
    $ghExit  = 0
    try {
        $json = gh issue list --repo microsoft/intelligent-terminal --label $script:StuckLabel --state open --json number,title,body,url,labels,createdAt 2>$errFile
        $ghExit = $LASTEXITCODE
        if (Test-Path -LiteralPath $errFile) { $errText = (Get-Content -Raw -LiteralPath $errFile) }
    }
    finally {
        Remove-Item -LiteralPath $errFile -Force -ErrorAction SilentlyContinue
    }
    if ($ghExit -ne 0) {
        throw "gh issue list failed (exit $ghExit): $errText"
    }
    if (-not $json) { return @() }
    return @($json | ConvertFrom-Json)
}

function Get-StuckMetaFromIssue {
    # Parse a fenced ```yaml ... # wta-state ... ``` block out of an issue body.
    # Accepts the single-quoted form Format-StuckYamlBlock emits; values are
    # un-escaped (`''` -> `'`). Returns $null if no block is found (degraded
    # but safe: callers should still treat the open issue as a lock - the
    # metadata is for findings_hash compare and resume hints, not for the
    # lock decision itself).
    param([Parameter(Mandatory)] $Issue)
    if (-not $Issue.body) { return $null }
    $pattern = '(?ms)```yaml\s*\r?\n#\s*wta-state\s*\r?\n(.+?)\r?\n```'
    if ($Issue.body -notmatch $pattern) { return $null }
    $yaml = $matches[1]
    $h = [ordered] @{}
    foreach ($l in $yaml -split '\r?\n') {
        if ($l -match "^\s*([a-z_][a-z0-9_]*)\s*:\s*'((?:[^']|'')*)'\s*$") {
            $h[$matches[1]] = $matches[2] -replace "''", "'"
        } elseif ($l -match '^\s*([a-z_][a-z0-9_]*)\s*:\s*(.+?)\s*$') {
            # Tolerate bare scalars for backward compatibility with hand-edited
            # issues; the writer always quotes, but a human edit might not.
            $h[$matches[1]] = $matches[2]
        }
    }
    return [pscustomobject] $h
}

function Format-StuckYamlBlock {
    # Build the fenced YAML block that 07/07b embed in stuck-issue bodies.
    # Values are always single-quoted with `'` -> `''` escaping so embedded
    # colons, newlines, leading dashes, etc. round-trip without breaking the
    # parser in Get-StuckMetaFromIssue or any other YAML reader. Multiline
    # values are folded to spaces (we don't need full block-scalar support;
    # the lock decision is just "is the list non-empty").
    param([Parameter(Mandatory)] [hashtable] $Fields)
    $lines = @('```yaml', $script:WtaStateFence)
    foreach ($k in $Fields.Keys) {
        $raw = "$($Fields[$k])"
        $folded = $raw -replace '\r?\n', ' '
        $escaped = $folded -replace "'", "''"
        $lines += ("{0}: '{1}'" -f $k, $escaped)
    }
    $lines += '```'
    return ($lines -join "`n")
}

# ---------------------------------------------------------------------------
# Misc helpers
# ---------------------------------------------------------------------------

function Get-GhUserLogin {
    $login = gh api user --jq '.login' 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $login) { throw "gh CLI is not authenticated. Run 'gh auth login'." }
    return $login.Trim()
}

function Format-Iso8601 {
    param([DateTime] $When = (Get-Date))
    return $When.ToString('yyyy-MM-ddTHH:mm:sszzz')
}

function New-RunContext {
    [pscustomobject] @{
        StartedAt        = Get-Date
        Host             = $env:COMPUTERNAME
        # Branch name carries date + UTC timestamp + 4 random hex chars so
        # repeated runs on the same day - or two consecutive runs after a
        # rebase-merge that didn't auto-delete the previous branch - never
        # check out a stale branch and replay already-merged commits.
        Branch           = "upstream-sync/$((Get-Date).ToString('yyyy-MM-dd'))-$((Get-Date).ToUniversalTime().ToString('HHmmss'))-$(([guid]::NewGuid().ToString('N').Substring(0,4)))"
        Picked           = @()
        Pending          = @()
        DroppedPairs     = @()
        SkippedEmpty     = @()
        Tier0            = @()
        Tier2            = @()
        StuckSha         = $null
        StuckPaths       = @()
        StuckError       = $null
        StuckValidation  = $null
        Preflight        = $null
        Scan             = $null
        Build            = $null
        Status           = 'unknown'
        ReportPath       = $null
        PrUrl            = $null
        IssueUrl         = $null
    }
}

function Get-FindingsHash {
    param([Parameter(Mandatory)] $Findings)
    # Stable hash of a findings list — used as a stuck-issue findings_hash
    # so repeat-runs of the same broken batch can detect "same failure as
    # last time" and avoid re-opening duplicate issues.
    $norm = ($Findings | ConvertTo-Json -Depth 8 -Compress)
    $sha  = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($norm))
        return ([System.BitConverter]::ToString($hash) -replace '-','').ToLowerInvariant().Substring(0,16)
    }
    finally {
        $sha.Dispose()
    }
}
