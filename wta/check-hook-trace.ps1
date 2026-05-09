# check-hook-trace.ps1 — quick view of the most recent hook invocations
# Use after Ctrl+C-exiting Gemini to verify whether the SessionEnd hook fired.
$tracePath = Join-Path $env:LOCALAPPDATA 'IntelligentTerminal\logs\hook-trace.log'
if (-not (Test-Path -LiteralPath $tracePath)) {
    Write-Host "No trace file yet: $tracePath" -ForegroundColor Yellow
    exit 0
}
Write-Host "Last 30 lines of $tracePath" -ForegroundColor Cyan
Get-Content -LiteralPath $tracePath -Tail 30
