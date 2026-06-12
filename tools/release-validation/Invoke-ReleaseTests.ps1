<#
.SYNOPSIS
    Release test runner for Intelligent Terminal — step 1 of the release-validation flow.

.DESCRIPTION
    Does two things, in order:

      1. Copies `doc\release-check-list.md` into a timestamped run folder so the
         tester gets a fresh, pristine working copy to tick and sign off.
      2. Builds and runs the unit tests on BOTH sides:
           * Rust  — WTA crate via `cargo test` (build + test in one step).
           * C++   — TAEF unit tests via razzle + te.exe:
                       - SettingsModel.Unit.Tests.dll  (CustomAgentAndPolicyTests, KeyBindingsTests, ...)
                       - Terminal.App.Unit.Tests.dll   (CustomAgentIdTests, AgentHooksStatusTests, ...)

    All output is logged under the run folder, and a run-summary.md is written
    with per-side results. The script exits non-zero if any side fails, so it
    can gate a release.

.PARAMETER Configuration
    Build configuration: Debug (default) or Release.

.PARAMETER Platform
    Build platform for locating C++ test output. Default: x64.

.PARAMETER OutputDir
    Run folder. Default: artifacts\release-validation\run-<timestamp>.

.PARAMETER Clean
    Clean-build the C++ test projects (bcx) instead of incremental (bx).

.PARAMETER SkipRust
    Skip the Rust (WTA) unit tests.

.PARAMETER SkipCpp
    Skip the C++ (TAEF) unit tests.

.PARAMETER AgentOnly
    Scope the C++ TAEF run to the agent-feature test classes only
    (CustomAgentAndPolicyTests, KeyBindingsTests, CustomAgentIdTests,
    AgentHooksStatusTests, RtlHelperTests). Use for a deterministic release
    gate; the full SettingsModel suite includes inherited Windows Terminal
    tests that can fail on a dev box for environment reasons unrelated to the
    agent features.

.EXAMPLE
    pwsh -File tools\release-validation\Invoke-ReleaseTests.ps1

.EXAMPLE
    pwsh -File tools\release-validation\Invoke-ReleaseTests.ps1 -AgentOnly

.EXAMPLE
    pwsh -File tools\release-validation\Invoke-ReleaseTests.ps1 -Configuration Release -Clean
#>
[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',

    [string]$Platform = 'x64',

    [string]$OutputDir,

    [switch]$Clean,
    [switch]$SkipRust,
    [switch]$SkipCpp,

    # Scope the C++ TAEF run to the agent-feature test classes only. Use this
    # for a deterministic release gate: the full SettingsModel suite includes
    # inherited Windows Terminal tests (e.g. DeserializationTests) that depend
    # on the local profile-generator environment and can fail on a dev box for
    # reasons unrelated to the agent features under test.
    [switch]$AgentOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── Resolve paths ───────────────────────────────────────────────────────────
# Script lives at <repo>\tools\release-validation\, so the repo root is two
# levels up. Resolve absolute so .NET / cmd cwd quirks don't bite (see repo
# memory: PowerShell cd does not update [Environment]::CurrentDirectory).
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Checklist = Join-Path $RepoRoot 'doc\release-check-list.md'
$WtaManifest = Join-Path $RepoRoot 'tools\wta\Cargo.toml'
$Razzle = Join-Path $RepoRoot 'tools\razzle.cmd'

if (-not $OutputDir) {
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $OutputDir = Join-Path $RepoRoot "artifacts\release-validation\run-$stamp"
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

# Results accumulator: name -> @{ Ok = <bool>; Summary = <string> }
$results = [ordered]@{}

function Write-Step($msg) {
    Write-Host ""
    Write-Host "==> $msg" -ForegroundColor Cyan
}

# Run an external command line through cmd.exe, tee output to a log, and return
# ONLY the exit code. Tee-Object's pass-through is sent to Out-Host so it shows
# on the console without leaking into this function's output stream (otherwise
# the caller's `$exit` would capture the whole command output, not the int).
function Invoke-CmdLogged([string]$CommandLine, [string]$LogPath) {
    & cmd.exe /c $CommandLine 2>&1 | Tee-Object -FilePath $LogPath | Out-Host
    return $LASTEXITCODE
}

# ── Step 1: copy the release checklist ───────────────────────────────────────
Write-Step "Copying release checklist"
if (-not (Test-Path $Checklist)) {
    throw "release checklist not found at $Checklist"
}
$ChecklistCopy = Join-Path $OutputDir 'release-check-list.md'
Copy-Item -LiteralPath $Checklist -Destination $ChecklistCopy -Force
Write-Host "    -> $ChecklistCopy"

# ── Step 2a: Rust (WTA) unit tests ───────────────────────────────────────────
if ($SkipRust) {
    Write-Step "Rust (WTA) unit tests — SKIPPED"
    $results['Rust (cargo test)'] = @{ Ok = $true; Summary = 'skipped' }
}
else {
    Write-Step "Rust (WTA) unit tests — cargo test"

    # Build rule (tools/wta/AGENTS.md): kill any live wta.exe first, or the
    # build can fail with 'Access is denied' on the locked target binary.
    # Stop-Process must be by -Id (environment rule: no -Name kills).
    Get-Process -Name 'wta' -ErrorAction SilentlyContinue |
        ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }

    $rustLog = Join-Path $OutputDir 'rust-tests.log'
    $cargoArgs = @('test', '--manifest-path', $WtaManifest)
    if ($Configuration -eq 'Release') { $cargoArgs += '--release' }

    & cargo @cargoArgs 2>&1 | Tee-Object -FilePath $rustLog
    $rustExit = $LASTEXITCODE

    # Scrape the last "test result:" line for a compact summary.
    $rustSummary = (Select-String -Path $rustLog -Pattern '^test result:' |
        Select-Object -Last 1).Line
    if (-not $rustSummary) { $rustSummary = "exit code $rustExit" }

    $results['Rust (cargo test)'] = @{ Ok = ($rustExit -eq 0); Summary = $rustSummary.Trim() }
}

# ── Step 2b: C++ (TAEF) unit tests ───────────────────────────────────────────
if ($SkipCpp) {
    Write-Step "C++ (TAEF) unit tests — SKIPPED"
    $results['C++ build'] = @{ Ok = $true; Summary = 'skipped' }
}
else {
    if (-not (Test-Path $Razzle)) {
        throw "razzle.cmd not found at $Razzle — cannot build C++ tests"
    }

    # Build verb: bx = incremental exclusive (current project), bcx = clean.
    # Append 'rel' for Release builds.
    $buildVerb = if ($Clean) { 'bcx' } else { 'bx' }
    if ($Configuration -eq 'Release') { $buildVerb = "$buildVerb rel" }

    $settingsDir = Join-Path $RepoRoot 'src\cascadia\UnitTests_SettingsModel'
    $utAppDir = Join-Path $RepoRoot 'src\cascadia\ut_app'

    Write-Step "C++ (TAEF) unit tests — building test projects ($buildVerb)"
    $cppBuildLog = Join-Path $OutputDir 'cpp-build.log'
    # One cmd process: razzle sets env, then build both projects. Explicit
    # `cd /d` before each project because razzle may change the directory.
    $buildCmd =
        "cd /d `"$RepoRoot`" && call `"$Razzle`" && " +
        "cd /d `"$settingsDir`" && $buildVerb && " +
        "cd /d `"$utAppDir`" && $buildVerb"
    $cppBuildExit = Invoke-CmdLogged $buildCmd $cppBuildLog
    $results['C++ build'] = @{
        Ok      = ($cppBuildExit -eq 0)
        Summary = if ($cppBuildExit -eq 0) { 'build succeeded' } else { "build FAILED (exit $cppBuildExit) — see cpp-build.log" }
    }

    if ($cppBuildExit -ne 0) {
        Write-Host "    C++ build failed; skipping C++ test execution." -ForegroundColor Yellow
    }
    else {
        # (friendly name, output subdir, dll, agent-test name filters) — te.exe
        # is the copy colocated with each DLL. The Filters are the TAEF /name:
        # globs applied when -AgentOnly is set.
        $binBase = Join-Path $RepoRoot "bin\$Platform\$Configuration"
        $cppTargets = @(
            @{
                Name    = 'SettingsModel.Unit.Tests'
                Dir     = 'UnitTests_SettingsModel'
                Dll     = 'SettingsModel.Unit.Tests.dll'
                Filters = @('*CustomAgentAndPolicyTests*', '*KeyBindingsTests*')
            },
            @{
                Name    = 'Terminal.App.Unit.Tests'
                Dir     = 'UnitTests_TerminalApp'
                Dll     = 'Terminal.App.Unit.Tests.dll'
                Filters = @('*CustomAgentIdTests*', '*AgentHooksStatusTests*', '*RtlHelperTests*')
            }
        )

        foreach ($t in $cppTargets) {
            $scope = if ($AgentOnly) { ' (agent-only)' } else { '' }
            Write-Step "C++ (TAEF) unit tests — running $($t.Name)$scope"
            $testDir = Join-Path $binBase $t.Dir
            $teExe = Join-Path $testDir 'te.exe'
            $dllPath = Join-Path $testDir $t.Dll
            $log = Join-Path $OutputDir "cpp-$($t.Name).log"

            if (-not (Test-Path $teExe) -or -not (Test-Path $dllPath)) {
                $results["C++ $($t.Name)"] = @{
                    Ok      = $false
                    Summary = "missing te.exe or dll under $testDir"
                }
                Write-Host "    missing te.exe or $($t.Dll) under $testDir" -ForegroundColor Yellow
                continue
            }

            # Build the te.exe argument string, adding /name: filters when
            # -AgentOnly is requested.
            $teArgs = "`"$dllPath`""
            if ($AgentOnly) {
                foreach ($f in $t.Filters) { $teArgs += " /name:$f" }
            }

            # Run inside razzle so any inherited test deps resolve.
            $runCmd = "cd /d `"$RepoRoot`" && call `"$Razzle`" && `"$teExe`" $teArgs"
            $exit = Invoke-CmdLogged $runCmd $log

            $summary = (Select-String -Path $log -Pattern '^Summary:' |
                Select-Object -Last 1).Line
            if (-not $summary) { $summary = "exit code $exit" }
            $results["C++ $($t.Name)"] = @{ Ok = ($exit -eq 0); Summary = $summary.Trim() }
        }
    }
}

# ── Write run summary + final verdict ────────────────────────────────────────
Write-Step "Summary"

$overallOk = $true
$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine('# Release test run')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("- **Timestamp:** $(Get-Date -Format 'u')")
[void]$sb.AppendLine("- **Configuration:** $Configuration ($Platform)")
[void]$sb.AppendLine("- **Checklist copy:** ``release-check-list.md`` (in this folder)")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| Step | Result | Detail |')
[void]$sb.AppendLine('|------|--------|--------|')

foreach ($name in $results.Keys) {
    $r = $results[$name]
    $mark = if ($r.Ok) { 'PASS' } else { 'FAIL' }
    if (-not $r.Ok) { $overallOk = $false }
    $detail = ($r.Summary -replace '\|', '\|')
    [void]$sb.AppendLine("| $name | $mark | $detail |")

    $color = if ($r.Ok) { 'Green' } else { 'Red' }
    Write-Host ("    [{0}] {1} — {2}" -f $mark, $name, $r.Summary) -ForegroundColor $color
}

[void]$sb.AppendLine('')
[void]$sb.AppendLine("**Overall: $(if ($overallOk) { 'PASS' } else { 'FAIL' })**")

$summaryPath = Join-Path $OutputDir 'run-summary.md'
$sb.ToString() | Set-Content -LiteralPath $summaryPath -Encoding UTF8

Write-Host ""
Write-Host "Run folder: $OutputDir"
Write-Host "Summary:    $summaryPath"

if (-not $overallOk) {
    Write-Host ""
    Write-Host "One or more test steps FAILED." -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "All unit-test steps passed." -ForegroundColor Green
exit 0
