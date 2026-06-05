# Common.ps1 — shared helpers for upstream-sync scripts.
# Dot-source from each script:  . "$PSScriptRoot/Common.ps1"

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-RepoRoot {
    $r = git rev-parse --show-toplevel 2>$null
    if ($LASTEXITCODE -ne 0) { throw "Not inside a git repo." }
    return $r.Trim()
}

function Get-StateDir {
    Join-Path (Get-RepoRoot) '.github/upstream-sync'
}

function Get-StatePath {
    Join-Path (Get-StateDir) 'state.json'
}

function Get-ReportsDir {
    $d = Join-Path (Get-StateDir) 'reports'
    if (-not (Test-Path $d)) { New-Item -ItemType Directory -Path $d | Out-Null }
    return $d
}

function ConvertTo-RepoRelativePath {
    # `git add` path arguments behave differently when absolute vs.
    # repo-relative, depending on the worktree-root vs. cwd interaction.
    # Normalize every path our scripts hand to `git add` to a
    # forward-slash, repo-relative form so the call is portable and
    # will not silently no-op (or worse, leak a path-outside-tree
    # error) on platforms where git treats absolute paths strictly.
    param([Parameter(Mandatory)] [string] $Path)
    $root = ((Get-RepoRoot) -replace '\\','/').TrimEnd('/')
    $abs  = $Path -replace '\\','/'
    if ($abs.Equals($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "ConvertTo-RepoRelativePath: refusing to return empty (path == repo root): $Path"
    }
    # Require a path-segment boundary after the prefix so 'C:/repo' does not
    # match 'C:/repo-old/file' (prior implementation took the substring after
    # the root length and only trimmed leading '/', which silently mangled
    # sibling paths into garbage relative ones).
    $prefix = "$root/"
    if ($abs.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $abs.Substring($prefix.Length)
    }
    throw "ConvertTo-RepoRelativePath: '$Path' is not under repo root '$root'."
}

function Read-State {
    $p = Get-StatePath
    if (-not (Test-Path $p)) {
        throw "state.json not found at $p. Run .github/skills/upstream-sync/scripts/00-bootstrap.ps1 (from the repo root) first — see references/bootstrap.md."
    }
    return Get-Content -Raw -LiteralPath $p | ConvertFrom-Json
}

function Write-State {
    param([Parameter(Mandatory)] $State)
    $p = Get-StatePath
    $json = ($State | ConvertTo-Json -Depth 12) + [Environment]::NewLine
    # Use UTF-8 *without* BOM to match git's default text handling on this repo.
    [System.IO.File]::WriteAllText($p, $json, (New-Object System.Text.UTF8Encoding($false)))
}

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
    $full = (git rev-parse "$Sha^{commit}" 2>$null).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $full) { throw "Could not resolve commit SHA '$Sha'." }
    return $full
}

function Get-GhUserLogin {
    $login = gh api user --jq '.login' 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $login) { throw "gh CLI is not authenticated. Run 'gh auth login'." }
    return $login.Trim()
}

function Format-Iso8601 {
    param([DateTime] $When = (Get-Date))
    return $When.ToString('yyyy-MM-ddTHH:mm:sszzz')
}

function Format-ReportFilename {
    param([DateTime] $When = (Get-Date), [string] $Suffix = '')
    $stamp = $When.ToString('yyyy-MM-ddTHHmm')
    if ($Suffix) { return "$stamp-$Suffix.md" }
    return "$stamp.md"
}

function New-RunContext {
    [pscustomobject] @{
        StartedAt        = Get-Date
        Host             = $env:COMPUTERNAME
        Branch           = "upstream-sync/$((Get-Date).ToString('yyyy-MM-dd'))"
        Picked           = @()
        Pending          = @()
        DroppedPairs     = @()
        SkippedEmpty     = @()
        Tier0            = @()
        Tier2            = @()
        StuckSha         = $null
        StuckPaths       = @()
        # Tier-4 (post-pick validation failure):
        StuckValidation  = $null   # hashtable { kind, base, head, range, findings_hash, branch, at, issue_url, ... }
        # Validation step results (whether or not they blocked):
        Preflight        = $null   # JSON from 09-toolchain-preflight.ps1
        Scan             = $null   # JSON from 08-static-scan.ps1
        Build            = $null   # JSON from 10-try-build.ps1
        Status           = 'unknown'
        ReportPath       = $null
        PrUrl            = $null
        IssueUrl         = $null
    }
}

function Get-FindingsHash {
    param([Parameter(Mandatory)] $Findings)
    # Stable hash of a findings list — used as a stuck_validation.findings_hash
    # so repeat-runs of the same broken batch can be detected (and not re-issued).
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
