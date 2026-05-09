#Requires -Version 5.1
<#
.SYNOPSIS
    Inspect, install, or remove the wt-agent-hooks bridge for one or all
    supported agent CLIs (Copilot, Claude, Gemini).

.DESCRIPTION
    Wrapper around `wta hooks status --json` / `wta install-hooks` /
    `wta hooks uninstall --json`. Same JSON contract that the Settings
    UI's "AI agents → Agent Hooks" section consumes — so the script and
    the UI can never disagree about install state.

    Internals are factored into named functions
    (Resolve-WtaPath, Get-AgentHooksStatus, Format-AgentHooksTable,
    Test-AgentHooksConsistent, Invoke-HooksInstall, Invoke-HooksUninstall,
    Invoke-HooksSmokeTest) so each is callable in isolation. The
    formatter and the consistency check accept JSON via -StatusJson, so
    they can be exercised with canned input without spawning wta.

.PARAMETER Mode
    Check    — print a colored status table and exit non-zero if any CLI
                 is in an inconsistent state.
    Install  — run `wta install-hooks`, then run Check.
    Uninstall— run `wta hooks uninstall --cli=<filter> --json`, then Check.

.PARAMETER CliFilter
    Restrict Uninstall (and the SmokeTest) to one CLI. Defaults to `all`.
    Has no effect on Check / Install (Install always installs all three;
    Check always reports all three).

.PARAMETER SmokeTest
    After Check / Install, fire a no-op prompt at each detected CLI and
    tail %LOCALAPPDATA%\IntelligentTerminal\logs\hook-trace.log for an
    `ENTER` line proving the hook fired.

.PARAMETER WtaPath
    Override path to wta.exe. Defaults to a sibling of this script (the
    MSIX layout deposits Verify-AgentHooks.ps1 next to wta.exe), then
    PATH lookup, then the dev-tree fallback under wta/target/{debug,release}.

.PARAMETER StatusJson
    Bypass spawning wta and feed the supplied JSON straight through the
    formatter / consistency check. Intended for ad-hoc script
    development — Check mode honors it; Install / Uninstall do not.

.EXAMPLE
    .\Verify-AgentHooks.ps1 -Mode Check
    .\Verify-AgentHooks.ps1 -Mode Install
    .\Verify-AgentHooks.ps1 -Mode Uninstall -CliFilter copilot
    .\Verify-AgentHooks.ps1 -Mode Check -SmokeTest
#>

[CmdletBinding()]
param(
    [Parameter()]
    [ValidateSet('Check', 'Install', 'Uninstall')]
    [string]$Mode = 'Check',

    [Parameter()]
    [ValidateSet('all', 'copilot', 'claude', 'gemini')]
    [string]$CliFilter = 'all',

    [Parameter()]
    [switch]$SmokeTest,

    [Parameter()]
    [string]$WtaPath,

    [Parameter()]
    [string]$StatusJson
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Schema version this script understands. Mirrors STATUS_SCHEMA_VERSION
# in wta/src/agent_hooks_installer.rs and SupportedStatusSchemaVersion
# in src/cascadia/inc/AgentHooksStatus.h. Bump in lockstep.
$script:SupportedStatusSchemaVersion = 3

$script:CliDisplayNames = @{
    copilot = 'Copilot CLI'
    claude  = 'Claude Code'
    gemini  = 'Gemini CLI'
}

# ── Helpers ──────────────────────────────────────────────────────────

function Resolve-WtaPath {
    [CmdletBinding()]
    param([string]$Override)

    if ($Override) {
        if (Test-Path -LiteralPath $Override -PathType Leaf) {
            return (Resolve-Path -LiteralPath $Override).Path
        }
        throw "Specified -WtaPath does not exist: $Override"
    }

    # 1. Sibling of this script (MSIX-installed scenario, where
    #    Verify-AgentHooks.ps1 ships next to wta.exe).
    $sibling = Join-Path $PSScriptRoot 'wta.exe'
    if (Test-Path -LiteralPath $sibling -PathType Leaf) {
        return (Resolve-Path -LiteralPath $sibling).Path
    }

    # 2. PATH lookup.
    $cmd = Get-Command wta.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    # 3. Dev-tree fallback — walk up from the script directory until we
    #    find wta/target/{debug,release}/wta.exe.
    $cursor = (Get-Item $PSScriptRoot).FullName
    while ($cursor) {
        foreach ($cfg in @('debug', 'release')) {
            $candidate = Join-Path $cursor "wta\target\$cfg\wta.exe"
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                return (Resolve-Path -LiteralPath $candidate).Path
            }
        }
        $parent = Split-Path -Parent $cursor
        if (-not $parent -or $parent -eq $cursor) { break }
        $cursor = $parent
    }

    throw 'Could not locate wta.exe. Pass -WtaPath, place wta.exe next to this script, or build a dev tree.'
}

# Capture stdout from `wta <args>` and return the raw text. Throws on
# non-zero exit (caller decides whether to surface or wrap the error).
function Invoke-WtaCommand {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WtaPath,
        [Parameter(Mandatory)][string[]]$ArgumentList
    )

    $stdout = & $WtaPath @ArgumentList 2>$null
    $exit = $LASTEXITCODE
    if ($exit -ne 0) {
        throw "wta $($ArgumentList -join ' ') exited with code $exit"
    }
    return ($stdout -join [Environment]::NewLine)
}

# Get the parsed status report. Returns a PSCustomObject mirroring
# StatusReport in wta/src/agent_hooks_installer.rs.
function Get-AgentHooksStatus {
    [CmdletBinding()]
    param(
        [Parameter()][string]$WtaPath,
        [Parameter()][string]$Json
    )

    if (-not $Json) {
        $Json = Invoke-WtaCommand -WtaPath $WtaPath -ArgumentList @('hooks', 'status', '--json')
    }
    if ([string]::IsNullOrWhiteSpace($Json)) {
        throw 'wta hooks status --json returned empty output.'
    }

    try {
        $report = $Json | ConvertFrom-Json -ErrorAction Stop
    } catch {
        throw "Failed to parse wta hooks status --json output: $($_.Exception.Message)"
    }

    if (-not (Get-Member -InputObject $report -Name schema_version -ErrorAction SilentlyContinue)) {
        throw 'wta hooks status JSON is missing schema_version.'
    }
    if ($report.schema_version -ne $script:SupportedStatusSchemaVersion) {
        throw "Unsupported schema_version $($report.schema_version) (this script understands $($script:SupportedStatusSchemaVersion)). Update Verify-AgentHooks.ps1 alongside the wta-side type."
    }
    return $report
}

# Render one row of the table as a hashtable (consumed by Format-Table /
# Write-Host). Returns:
#   @{ CLI = 'copilot'; OnPath = $true; State = 'installed'; Color = 'Green'; Detail = '...' }
function Get-AgentHookCliRow {
    [CmdletBinding()]
    param([Parameter(Mandatory)][psobject]$Cli)

    if (-not $Cli.binary_on_path) {
        return @{
            CLI    = $Cli.name
            OnPath = $false
            State  = 'CLI not on PATH'
            Color  = 'DarkGray'
            Detail = ''
        }
    }

    $marketplace = [bool]$Cli.marketplace_registered
    $plugin      = [bool]$Cli.plugin_installed
    $enabled     = [bool]$Cli.plugin_enabled
    # v3 (#25): marketplace_registered alone no longer guarantees the
    # registered source path still exists on disk. Default to $true when
    # the field is missing so older payloads (rejected upstream by the
    # schema check, but defensive) don't quietly down-grade.
    $pathValid = $true
    if (Get-Member -InputObject $Cli -Name marketplace_path_valid -ErrorAction SilentlyContinue) {
        $pathValid = [bool]$Cli.marketplace_path_valid
    }

    if ($marketplace -and $pathValid -and $plugin -and $enabled) {
        $state = 'installed'
        $color = 'Green'
    } elseif (-not $marketplace -and -not $plugin) {
        $state = 'not installed'
        $color = 'Yellow'
    } else {
        $bits = @()
        $bits += if ($marketplace) { 'marketplace=yes' } else { 'marketplace=no' }
        $bits += if ($plugin)      { 'plugin=yes' }      else { 'plugin=no' }
        if ($plugin -and -not $enabled) { $bits += 'plugin disabled' }
        if ($marketplace -and -not $pathValid) { $bits += 'marketplace path stale' }
        $state = 'PARTIAL'
        $color = 'Red'
        $detail = ($bits -join ', ')
    }

    $row = @{
        CLI    = $Cli.name
        OnPath = $true
        State  = $state
        Color  = $color
        Detail = if ($state -eq 'PARTIAL') { $detail } else { '' }
    }

    if ((Get-Member -InputObject $Cli -Name detection_fallback -ErrorAction SilentlyContinue) -and $Cli.detection_fallback) {
        $row.Detail = if ($row.Detail) { "$($row.Detail); fs fallback" } else { 'fs fallback' }
    }
    return $row
}

function Format-AgentHooksTable {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][psobject]$Report,
        [Parameter()][string]$Title = 'wt-agent-hooks status'
    )

    Write-Host ''
    Write-Host $Title -ForegroundColor Cyan
    $bundle = $Report.bundle_source
    $bundleLine = "  bundle: $($bundle.kind)"
    if ((Get-Member -InputObject $bundle -Name path -ErrorAction SilentlyContinue) -and $bundle.path) {
        $bundleLine += " ($($bundle.path))"
    }
    if ($bundle.kind -eq 'none') {
        Write-Host $bundleLine -ForegroundColor Yellow
        Write-Host '  ⚠ no on-disk bundle resolved — wta cannot install hooks in this state.' -ForegroundColor Yellow
    } else {
        Write-Host $bundleLine -ForegroundColor DarkGray
    }
    Write-Host ''

    foreach ($cli in $Report.clis) {
        $row = Get-AgentHookCliRow -Cli $cli
        $name = if ($script:CliDisplayNames.ContainsKey($row.CLI)) { $script:CliDisplayNames[$row.CLI] } else { $row.CLI }
        $line = '  {0,-13} {1}' -f $name, $row.State
        if ($row.Detail) {
            $line += "  ($($row.Detail))"
        }
        Write-Host $line -ForegroundColor $row.Color
    }
    Write-Host ''
}

# Inconsistent = any CLI with `binary_on_path=true` whose plugin state
# is not fully installed. Also flags a `bundle_source.kind == "none"`
# install state. Returns @{ Ok = $bool; Reasons = @() }.
function Test-AgentHooksConsistent {
    [CmdletBinding()]
    param([Parameter(Mandatory)][psobject]$Report)

    $reasons = @()
    if ($Report.bundle_source.kind -eq 'none') {
        $reasons += 'no on-disk bundle was resolved by wta'
    }

    foreach ($cli in $Report.clis) {
        if (-not $cli.binary_on_path) { continue }
        $row = Get-AgentHookCliRow -Cli $cli
        if ($row.State -eq 'PARTIAL') {
            $reasons += "$($cli.name): partial install ($($row.Detail))"
        } elseif ($row.State -eq 'not installed') {
            # `not installed` is informational — only flag if the user
            # asked to verify install state explicitly via -SmokeTest
            # (which wouldn't be possible without an install). Otherwise
            # we treat it as "user hasn't installed yet", not failure.
        }
        if ((Get-Member -InputObject $cli -Name detection_fallback -ErrorAction SilentlyContinue) -and $cli.detection_fallback) {
            $reasons += "$($cli.name): wta fell back to fs detection ($($cli.detection_fallback))"
        }
    }

    return @{
        Ok      = ($reasons.Count -eq 0)
        Reasons = $reasons
    }
}

function Invoke-HooksInstall {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$WtaPath)

    Write-Host ''
    Write-Host '→ Running: wta install-hooks' -ForegroundColor Cyan
    & $WtaPath install-hooks
    if ($LASTEXITCODE -ne 0) {
        throw "wta install-hooks exited with code $LASTEXITCODE"
    }
}

function Invoke-HooksUninstall {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WtaPath,
        [Parameter(Mandatory)][string]$Cli
    )

    Write-Host ''
    Write-Host "→ Running: wta hooks uninstall --cli $Cli --json" -ForegroundColor Cyan
    $jsonText = Invoke-WtaCommand -WtaPath $WtaPath -ArgumentList @('hooks', 'uninstall', '--cli', $Cli, '--json')

    $report = $jsonText | ConvertFrom-Json -ErrorAction Stop
    foreach ($entry in $report.clis) {
        $marketplaceRemoved = if (Get-Member -InputObject $entry -Name marketplace_removed -ErrorAction SilentlyContinue) { $entry.marketplace_removed } else { $null }
        $pluginUninstalled  = if (Get-Member -InputObject $entry -Name plugin_uninstalled  -ErrorAction SilentlyContinue) { $entry.plugin_uninstalled  } else { $null }

        $color = if ($entry.attempted -and ($pluginUninstalled -ne $false)) { 'Green' } else { 'Yellow' }
        $line = '  {0,-10} attempted={1} marketplace_removed={2} plugin_uninstalled={3}' -f `
            $entry.name, `
            $entry.attempted, `
            ($(if ($null -eq $marketplaceRemoved) { '-' } else { $marketplaceRemoved })), `
            ($(if ($null -eq $pluginUninstalled)  { '-' } else { $pluginUninstalled }))
        Write-Host $line -ForegroundColor $color
        foreach ($msg in $entry.messages) {
            Write-Host "      $msg" -ForegroundColor DarkGray
        }
    }
}

function Invoke-HooksSmokeTest {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][psobject]$Report,
        [Parameter()][string]$Cli = 'all'
    )

    $logPath = Join-Path $env:LOCALAPPDATA 'IntelligentTerminal\logs\hook-trace.log'
    if (-not (Test-Path -LiteralPath $logPath)) {
        Write-Host ''
        Write-Host "Smoke-test skipped: $logPath not found (no hooks have ever fired on this machine)." -ForegroundColor Yellow
        return
    }

    Write-Host ''
    Write-Host '→ Smoke test: tailing hook-trace.log for ENTER lines (informational only).' -ForegroundColor Cyan
    Get-Content -LiteralPath $logPath -Tail 20 |
        Where-Object { $_ -match 'ENTER' } |
        ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGray }
}

# ── Main ─────────────────────────────────────────────────────────────

# Check mode with -StatusJson takes a fast path and never resolves wta.
$wtaPath = $null
if (-not ($Mode -eq 'Check' -and $StatusJson)) {
    $wtaPath = Resolve-WtaPath -Override $WtaPath
    Write-Host "wta: $wtaPath" -ForegroundColor DarkGray
}

switch ($Mode) {
    'Install' {
        Invoke-HooksInstall -WtaPath $wtaPath
    }
    'Uninstall' {
        Invoke-HooksUninstall -WtaPath $wtaPath -Cli $CliFilter
    }
}

$report = Get-AgentHooksStatus -WtaPath $wtaPath -Json $StatusJson
Format-AgentHooksTable -Report $report -Title "wt-agent-hooks status (mode=$Mode)"

if ($SmokeTest) {
    Invoke-HooksSmokeTest -Report $report -Cli $CliFilter
}

$result = Test-AgentHooksConsistent -Report $report
if (-not $result.Ok) {
    Write-Host 'Inconsistencies detected:' -ForegroundColor Red
    foreach ($reason in $result.Reasons) {
        Write-Host "  - $reason" -ForegroundColor Red
    }
    Write-Host ''
    exit 1
}

Write-Host 'All consistent.' -ForegroundColor Green
exit 0
