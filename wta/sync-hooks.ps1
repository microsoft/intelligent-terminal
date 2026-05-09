# sync-hooks.ps1 — copy the latest hook scripts from the repo into the live
# Copilot/Gemini plugin install locations under the user's profile.
#
# Run this from a normal PowerShell prompt (not the agent session) any time
# the files under wta/wt-agent-hooks/agent-hooks-plugin/hooks/ or
# wta/wt-agent-hooks/gemini-extension/hooks/ change. Restart the agent CLIs
# (or close+reopen WT) for the new hooks to take effect.

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot   # repo root = parent of wta/
$wta  = Join-Path $repo 'wta'
$hooksRoot = Join-Path $wta 'wt-agent-hooks'

$pairs = @(
    @{
        Src = (Join-Path $hooksRoot 'agent-hooks-plugin\hooks\send-event.ps1')
        Dst = (Join-Path $env:USERPROFILE '.copilot\installed-plugins\wt-local\wt-agent-hooks\hooks\send-event.ps1')
    },
    @{
        Src = (Join-Path $hooksRoot 'gemini-extension\hooks\send-event.ps1')
        Dst = (Join-Path $env:USERPROFILE '.gemini\extensions\wt-agent-hooks\hooks\send-event.ps1')
    },
    @{
        Src = (Join-Path $hooksRoot 'agent-hooks-plugin\hooks\hooks.json')
        Dst = (Join-Path $env:USERPROFILE '.copilot\installed-plugins\wt-local\wt-agent-hooks\hooks\hooks.json')
    },
    @{
        Src = (Join-Path $hooksRoot 'gemini-extension\hooks\hooks.json')
        Dst = (Join-Path $env:USERPROFILE '.gemini\extensions\wt-agent-hooks\hooks\hooks.json')
    }
)

foreach ($p in $pairs) {
    if (-not (Test-Path -LiteralPath $p.Src)) {
        Write-Host "  SKIP (no source): $($p.Src)"
        continue
    }
    $dstDir = Split-Path -Parent $p.Dst
    if (-not (Test-Path -LiteralPath $dstDir)) {
        Write-Host "  SKIP (install missing): $dstDir"
        continue
    }
    Copy-Item -LiteralPath $p.Src -Destination $p.Dst -Force
    Write-Host "  OK   $($p.Dst)"
}

Write-Host ""
Write-Host "Done. Restart the agent CLI (or close+reopen the WT pane) to pick up the new hooks."
