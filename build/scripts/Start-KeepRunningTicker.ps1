<#
.SYNOPSIS
Long-running ticker for manually verifying keep-running durable sessions.

.DESCRIPTION
Prints one line per second carrying a counter, the wall clock, and the elapsed
time since the shell started.

Leave it running, close the tab, then restore the session from the agent pane's
/shell-sessions view.

  * If the session was kept running, the counter and the elapsed time carry
    straight on from where they were, and the jump in the wall clock shows how
    long the terminal was closed. The PID is unchanged.
  * If a fresh shell was started instead and the scrollback was merely replayed,
    the counter restarts at 1, elapsed restarts at 0, and the PID is different.

The PID line is the tie-breaker: replayed scrollback can look convincing, but it
cannot change what the live process is.

.PARAMETER IntervalSeconds
Seconds between ticks. Defaults to 1.

.EXAMPLE
.\build\scripts\Start-KeepRunningTicker.ps1

.EXAMPLE
.\build\scripts\Start-KeepRunningTicker.ps1 -IntervalSeconds 5
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 3600)]
    [int]$IntervalSeconds = 1
)

$ErrorActionPreference = 'Stop'

$start = Get-Date
$session = if ($env:WT_SESSION) { $env:WT_SESSION } else { '<not set>' }

Write-Host ''
Write-Host 'keep-running ticker' -ForegroundColor Cyan
Write-Host "  shell PID  : $PID"
Write-Host "  WT_SESSION : $session"
Write-Host "  started    : $($start.ToString('yyyy-MM-dd HH:mm:ss'))"
Write-Host ''
Write-Host '  Close this tab, then restore it from the agent pane''s /shell-sessions view.' -ForegroundColor DarkGray
Write-Host '  Kept running  -> counter and elapsed carry on, PID unchanged.' -ForegroundColor DarkGray
Write-Host '  Fresh shell   -> counter back to 1, elapsed back to 0, new PID.' -ForegroundColor DarkGray
Write-Host '  Ctrl+C to stop.' -ForegroundColor DarkGray
Write-Host ''

$i = 0
while ($true) {
    $i++
    $now = Get-Date
    $elapsed = $now - $start
    Write-Host ('{0,6}  {1}  elapsed {2:hh\:mm\:ss}  pid {3}' -f $i, $now.ToString('HH:mm:ss'), $elapsed, $PID)
    Start-Sleep -Seconds $IntervalSeconds
}
