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

function Read-State {
    $p = Get-StatePath
    if (-not (Test-Path $p)) {
        throw "state.json not found at $p. Run scripts/00-bootstrap.ps1 first — see references/bootstrap.md."
    }
    return Get-Content -Raw -LiteralPath $p | ConvertFrom-Json -AsHashtable
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
    $hash = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($norm))
    return ([System.BitConverter]::ToString($hash) -replace '-','').ToLowerInvariant().Substring(0,16)
}
