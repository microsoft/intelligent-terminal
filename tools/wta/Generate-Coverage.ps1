<#
.SYNOPSIS
    Generate source-based code coverage for the WTA Rust crate.

.DESCRIPTION
    Runs `cargo llvm-cov` over `tools/wta` and emits an HTML report (and
    optionally lcov) plus a console summary.

    Toolchain note: `tools/wta/rust-toolchain.toml` pins the MS-internal
    `ms-prod-1.93` channel, which isn't installed on dev boxes. This script
    forces `RUSTUP_TOOLCHAIN=stable` (the same bypass the third-party-notices
    generator uses) and runs from the repo root with `--manifest-path`, so the
    pin never kicks in. Stable has had source-based coverage since 1.60.

    Prerequisites (auto-installed unless -NoInstall):
      * rustup component `llvm-tools-preview` (on the stable toolchain)
      * `cargo-llvm-cov` (cargo subcommand)

.PARAMETER OutputDir
    Where to write the report. Default: tools\wta\target\coverage.

.PARAMETER Lcov
    Also emit lcov.info (for CI / editor coverage gutters).

.PARAMETER Open
    Open the HTML report in the default browser when done.

.PARAMETER NoInstall
    Fail instead of auto-installing missing tooling.

.EXAMPLE
    pwsh -File tools\wta\Generate-Coverage.ps1 -Open

.EXAMPLE
    pwsh -File tools\wta\Generate-Coverage.ps1 -Lcov
#>
[CmdletBinding()]
param(
    [string]$OutputDir,
    [switch]$Lcov,
    [switch]$Open,
    [switch]$NoInstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Repo root = two levels up from this script (tools\wta\).
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Manifest = Join-Path $RepoRoot 'tools\wta\Cargo.toml'
if (-not $OutputDir) {
    $OutputDir = Join-Path $RepoRoot 'tools\wta\target\coverage'
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

# Bypass the ms-prod toolchain pin; use stable everywhere in this process.
$env:RUSTUP_TOOLCHAIN = 'stable'

function Test-Command([string]$Probe) {
    try {
        & cmd.exe /c $Probe *> $null
        return ($LASTEXITCODE -eq 0)
    }
    catch {
        return $false
    }
}

Write-Host "==> Coverage for WTA crate" -ForegroundColor Cyan
Write-Host "    repo:   $RepoRoot"
Write-Host "    output: $OutputDir"
Write-Host "    toolchain: stable (pin bypassed)"

# Build rule (tools/wta/AGENTS.md): kill any live wta.exe first, or the
# instrumented build can fail with 'Access is denied' on the locked binary.
Get-Process -Name 'wta' -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }

# ── Ensure llvm-tools-preview on stable ──────────────────────────────────────
$hasLlvmTools = (& rustup component list --toolchain stable --installed 2>$null |
    Select-String -Pattern 'llvm-tools' -Quiet)
if (-not $hasLlvmTools) {
    if ($NoInstall) {
        throw "llvm-tools-preview is not installed on the stable toolchain. Run: rustup component add llvm-tools-preview --toolchain stable"
    }
    Write-Host "==> Installing rustup component llvm-tools-preview (stable)" -ForegroundColor Cyan
    & rustup component add llvm-tools-preview --toolchain stable
    if ($LASTEXITCODE -ne 0) { throw "failed to add llvm-tools-preview" }
}

# ── Ensure cargo-llvm-cov ─────────────────────────────────────────────────────
if (-not (Test-Command 'cargo llvm-cov --version')) {
    if ($NoInstall) {
        throw "cargo-llvm-cov is not installed. Run: cargo install cargo-llvm-cov"
    }
    Write-Host "==> Installing cargo-llvm-cov (one-time, may take a few minutes)" -ForegroundColor Cyan
    & cargo install cargo-llvm-cov
    if ($LASTEXITCODE -ne 0) { throw "failed to install cargo-llvm-cov" }
}

# ── Collect coverage (run tests once), then emit reports from the same data ──
Write-Host "==> Running instrumented tests (cargo llvm-cov --no-report)" -ForegroundColor Cyan
& cargo llvm-cov --no-report --manifest-path $Manifest
if ($LASTEXITCODE -ne 0) { throw "instrumented test run failed" }

Write-Host "==> Generating HTML report" -ForegroundColor Cyan
& cargo llvm-cov report --html --manifest-path $Manifest --output-dir $OutputDir
if ($LASTEXITCODE -ne 0) { throw "HTML report generation failed" }

if ($Lcov) {
    $lcovPath = Join-Path $OutputDir 'lcov.info'
    Write-Host "==> Generating lcov ($lcovPath)" -ForegroundColor Cyan
    & cargo llvm-cov report --lcov --manifest-path $Manifest --output-path $lcovPath
    if ($LASTEXITCODE -ne 0) { throw "lcov generation failed" }
}

Write-Host ""
Write-Host "==> Summary" -ForegroundColor Cyan
# Branch coverage is intentionally NOT collected. It requires a nightly
# toolchain (`-Z coverage-options=branch`), and on Windows `llvm-cov`'s branch
# report segfaults (0xc0000005) on this crate's profile data. This script forces
# `stable`, so the profile data has no branch regions and llvm-cov's summary prints
# an always-empty `Branches / Missed Branches / Cover` triplet. Strip it so the
# report shows only the metrics we actually measure: regions, functions, lines.
$summary = & cargo llvm-cov report --summary-only --manifest-path $Manifest
$summary | ForEach-Object {
    # Header: drop the trailing "Branches  Missed Branches  Cover" labels.
    $line = $_ -replace '\s+Branches\s+Missed Branches\s+Cover\s*$', ''
    # Data / TOTAL rows: drop the trailing branch triplet (count count cover),
    # which is always "0  0  -" under the stable (no-branch) profile.
    $line = $line -replace '\s+\d+\s+\d+\s+(?:[\d.]+%|-)\s*$', ''
    $line
}

$htmlIndex = Join-Path $OutputDir 'html\index.html'
Write-Host ""
Write-Host "HTML report: $htmlIndex"
if ($Lcov) { Write-Host "lcov:        $(Join-Path $OutputDir 'lcov.info')" }

if ($Open -and (Test-Path $htmlIndex)) {
    Start-Process 'msedge.exe' -ArgumentList ('"' + $htmlIndex + '"')
}
