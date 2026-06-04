<#
.SYNOPSIS
  Toolchain preflight. Detects the required PlatformToolset from the
  repo's build props and verifies it's installed on this host.

.DESCRIPTION
  Returns a JSON document. Used by the orchestrator BEFORE try-build
  to distinguish "code broke the build" from "host doesn't have the
  toolset". Critical for unattended schedulers — an infra problem
  must not be filed as a code-stuck issue.

  Does NOT auto-bump v143→v145 or any other toolset. That recipe is
  kept as a local-only developer workaround.

.OUTPUTS
  JSON to stdout:
    {
      "required_toolsets": ["v143"],
      "available_toolsets": ["v143", "v145"],
      "missing":            [],
      "vs_installs":        ["C:\\Program Files\\Microsoft Visual Studio\\2022\\Enterprise"],
      "ok":                 true
    }

  Error model:
    Throws on wrapper error. The orchestrator (`04-run-batch.ps1`)
    catches and routes through its own exit-code mapping (0 ok / 10
    stuck / 20 error). Run standalone, an uncaught throw exits with
    PowerShell's default code (1) plus a stack trace. `exit 20` is
    intentionally NOT used here because this script is invoked via
    `&` from the orchestrator — `exit` in that context would terminate
    the orchestrator mid-pipeline.
#>
[CmdletBinding()]
param()

. "$PSScriptRoot/Common.ps1"

function Get-RequiredToolsets {
    $root = Get-RepoRoot
    # PlatformToolset can live at either the repo root (e.g. fork-local
    # common.openconsole.props) or under src/ (the upstream cascadia
    # layout). Probe both so a future relocation doesn't silently make
    # the preflight read a stale file. Tests/older layouts may differ —
    # missing files are skipped.
    $candidates = @(
        'common.openconsole.props',
        'src/common.openconsole.props',
        'src/common.build.pre.props',
        'src/common.build.post.props',
        'src/common.build.tests.props',
        'src/wap-common.build.pre.props',
        'src/wap-common.build.post.props'
    )
    $found = [System.Collections.Generic.HashSet[string]]::new()
    foreach ($rel in $candidates) {
        $p = Join-Path $root $rel
        if (-not (Test-Path -LiteralPath $p)) { continue }
        $text = [System.IO.File]::ReadAllText($p)
        foreach ($m in ([regex]'<PlatformToolset[^>]*>([^<]+)</PlatformToolset>').Matches($text)) {
            $val = $m.Groups[1].Value.Trim()
            # Skip MSBuild property references like $(DefaultPlatformToolset) — those resolve at build time.
            if ($val -and $val -notmatch '^\$\(') { [void]$found.Add($val) }
        }
    }
    return ,@($found)
}

function Get-VsInstalls {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { return ,@() }
    $out = & $vswhere -all -products * -property installationPath 2>$null
    if ($LASTEXITCODE -ne 0) { return ,@() }
    return ,@($out | Where-Object { $_ })
}

function Get-AvailableToolsets {
    param([string[]] $VsInstalls)
    $found = [System.Collections.Generic.HashSet[string]]::new()
    foreach ($inst in $VsInstalls) {
        # The authoritative location for installed platform toolsets is
        # MSBuild\Microsoft\VC\<msbuild-target-ver>\Platforms\<arch>\PlatformToolsets\<toolset>.
        # The bare `MSBuild\Microsoft\VC\v???` directories are MSBuild target
        # versions, not toolsets — older versions of this script confused the two.
        $vcRoot = Join-Path $inst 'MSBuild\Microsoft\VC'
        if (-not (Test-Path -LiteralPath $vcRoot)) { continue }
        Get-ChildItem -Directory -LiteralPath $vcRoot -ErrorAction SilentlyContinue | ForEach-Object {
            $ptRoot = Join-Path $_.FullName 'Platforms\x64\PlatformToolsets'
            if (Test-Path -LiteralPath $ptRoot) {
                Get-ChildItem -Directory -LiteralPath $ptRoot -ErrorAction SilentlyContinue | ForEach-Object {
                    if ($_.Name -match '^v\d{3}$') { [void]$found.Add($_.Name) }
                }
            }
        }
    }
    return ,@($found)
}

try {
    $required  = Get-RequiredToolsets
    $vsInstalls = Get-VsInstalls
    $available = Get-AvailableToolsets -VsInstalls $vsInstalls
    $missing   = @($required | Where-Object { $available -notcontains $_ })
    $ok        = ($missing.Count -eq 0) -and ($required.Count -gt 0 -or $vsInstalls.Count -gt 0)

    $doc = [ordered] @{
        required_toolsets  = @($required)
        available_toolsets = @($available)
        missing            = @($missing)
        vs_installs        = @($vsInstalls)
        ok                 = $ok
    }
    $doc | ConvertTo-Json -Depth 4
}
catch {
    Write-Error $_.Exception.Message
    Write-Error $_.ScriptStackTrace
    throw
}
