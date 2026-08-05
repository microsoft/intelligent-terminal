[CmdletBinding(DefaultParameterSetName = "Run")]
param(
    [Parameter(ParameterSetName = "Verify")]
    [switch]$Verify,

    [string]$LogPath = (Join-Path $env:TEMP "keep-running-test.log"),

    [ValidateRange(1, 3600)]
    [int]$IntervalSeconds = 1
)

if ($Verify)
{
    if (-not (Test-Path -LiteralPath $LogPath))
    {
        throw "The heartbeat log does not exist: $LogPath"
    }

    $before = Get-Item -LiteralPath $LogPath
    $beforeLength = $before.Length
    $beforeWriteTime = $before.LastWriteTimeUtc
    $waitSeconds = [Math]::Max(3, $IntervalSeconds * 2)

    Write-Host "Watching $LogPath for $waitSeconds seconds..."
    Start-Sleep -Seconds $waitSeconds

    $after = Get-Item -LiteralPath $LogPath
    if ($after.Length -le $beforeLength -and $after.LastWriteTimeUtc -le $beforeWriteTime)
    {
        throw "The log did not change. The keep-running command is not producing heartbeats."
    }

    Write-Host "PASS: the keep-running command is still writing heartbeats." -ForegroundColor Green
    Get-Content -LiteralPath $LogPath -Tail 3
    return
}

$psmux = Get-Command psmux.exe -ErrorAction Stop
$sessionId = $env:WT_SESSION
if ([string]::IsNullOrWhiteSpace($sessionId))
{
    throw "WT_SESSION is not set. Run this script inside an Intelligent Terminal shell pane."
}

$sessionName = "wt-$sessionId"
$sessionPattern = "^{0}:" -f [regex]::Escape($sessionName)
$session = & $psmux.Source -L windows-terminal list-sessions 2>$null |
    Select-String -Pattern $sessionPattern

if (-not $session)
{
    throw @"
This pane is not a keep-running pane.

1. Enable Put to keep running in Settings > Startup.
2. Open the terminal context menu and select Put to keep running.
3. Run this script in the NEW split pane created by that action.

The original pane intentionally remains a normal ConPTY pane.
"@
}

Remove-Item -LiteralPath $LogPath -Force -ErrorAction SilentlyContinue

Write-Host "Keep-running session: $sessionName"
Write-Host "Process ID:          $PID"
Write-Host "Heartbeat log:       $LogPath"
Write-Host ""
Write-Host "Close Intelligent Terminal without stopping this script."
Write-Host "Then run the following command from another PowerShell:"
Write-Host "  .\test-keep-running.ps1 -Verify -LogPath `"$LogPath`""
Write-Host "Restore the session from the Intelligent Terminal tray icon."

$count = 0
while ($true)
{
    $line = "{0:o} session={1} pid={2} count={3}" -f (Get-Date), $sessionId, $PID, $count
    $line | Tee-Object -FilePath $LogPath -Append
    $count++
    Start-Sleep -Seconds $IntervalSeconds
}
