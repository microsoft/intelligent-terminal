<#
  test-events-wtcli.ps1
  Exercise the classic-COM EVENT path (Subscribe + OnEvent) -- the exact path
  that crashed in issue #197.

  RUN THIS IN A POWERSHELL PANE OF THE INSTALLED DEV INTELLIGENT TERMINAL
  (so wtcli inherits WT_COM_CLSID and can activate the COM server).

  What it does:
    1. Starts `wtcli --json listen` in the background (this calls Subscribe).
    2. Sends N broadcast events via `send-event` (these fan back to subscribers).
    3. Generates natural events (new-tab / send-keys / kill-pane).
    4. Stops the listener, parses everything it received, and reports:
       - how many broadcast events round-tripped back,
       - natural events received, grouped by method/type,
       - any OS-bug signature (0x80010105 / 0xc0000005),
       - whether WindowsTerminal stayed alive.

  PASS  = listener subscribed, events were received, no bug signature, WT alive.

  Keep this file PURE ASCII -- Windows PowerShell 5.1 mis-decodes non-ASCII in
  BOM-less .ps1 files and silently breaks parsing.

  Usage:
    .\test-events-wtcli.ps1
    .\test-events-wtcli.ps1 -BroadcastCount 200 -NoTabActivity
#>

[CmdletBinding()]
param(
    [int]$BroadcastCount = 50,     # broadcast events to send and expect echoed back
    [int]$ListenWarmupMs = 1500,   # let the listener Subscribe before sending
    [int]$DrainMs        = 2000,   # let events flush before stopping the listener
    [int]$StepDelayMs    = 400,    # wait after creating a pane before using it
    [switch]$NoTabActivity,        # skip the new-tab/send-keys/kill-pane part
    [string]$LogDir      = $env:TEMP
)

$ErrorActionPreference = 'Continue'

# -- Resolve wtcli (prefer PATH, else the installed package) --
$wtcli = (Get-Command wtcli.exe -ErrorAction SilentlyContinue).Source
if (-not $wtcli) {
    $pkg = Get-AppxPackage *IntelligentTerminal* -ErrorAction SilentlyContinue |
        Sort-Object Version -Descending | Select-Object -First 1
    if ($pkg) {
        $cand = Join-Path $pkg.InstallLocation 'wtcli.exe'
        if (Test-Path $cand) { $wtcli = $cand }
    }
}
if (-not $wtcli) {
    Write-Host "ERROR: wtcli.exe not found. Run this inside the installed dev Terminal." -ForegroundColor Red
    return
}
if (-not $env:WT_COM_CLSID) {
    Write-Host "ERROR: WT_COM_CLSID not set. Run this inside a Windows Terminal pane." -ForegroundColor Red
    return
}

$stamp   = Get-Date -Format 'yyyyMMdd-HHmmss'
$marker  = "evt-test-$stamp"
$lout    = Join-Path $LogDir "evt-wtcli_listen_$stamp.out"
$lerr    = Join-Path $LogDir "evt-wtcli_listen_$stamp.err"
$logPath = Join-Path $LogDir "evt-wtcli_$stamp.log"

Write-Host "wtcli         : $wtcli"        -ForegroundColor DarkGray
Write-Host "broadcast cnt : $BroadcastCount" -ForegroundColor DarkGray
Write-Host "listen stdout : $lout"         -ForegroundColor DarkGray
Write-Host ""

$wtPidsBefore = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)

# -- 1) Start the listener (this is the Subscribe path) --
Write-Host "Starting listener (wtcli --json listen) ..." -ForegroundColor Cyan
$proc = Start-Process -FilePath $wtcli -ArgumentList '--json','listen' `
    -RedirectStandardOutput $lout -RedirectStandardError $lerr -PassThru -WindowStyle Hidden
Start-Sleep -Milliseconds $ListenWarmupMs

if ($proc.HasExited) {
    Write-Host "ERROR: listener exited immediately (exit $($proc.ExitCode)). stderr:" -ForegroundColor Red
    Get-Content $lerr -ErrorAction SilentlyContinue | Write-Host
    return
}

# -- 2) Send broadcast events (these should fan back to the subscriber) --
Write-Host "Sending $BroadcastCount broadcast events ..." -ForegroundColor Cyan
$sendFails = 0
for ($i = 1; $i -le $BroadcastCount; $i++) {
    $payload = '{"seq":' + $i + ',"marker":"' + $marker + '"}'
    $o = (& $wtcli send-event -e wtcli.test.ping $payload 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0 -or $o -match '0x80010105|0xc0000005') {
        $sendFails++
        Add-Content -Path $logPath -Value ("send-event seq=$i FAIL: " + $o.Trim())
    }
}

# -- 3) Natural events: new-tab -> send-keys -> kill-pane --
$natExpected = 0
if (-not $NoTabActivity) {
    Write-Host "Generating natural events (new-tab / send-keys / kill-pane) ..." -ForegroundColor Cyan
    $newOut = (& $wtcli --json new-tab -c 'cmd.exe /k' -n evt-test 2>&1 | Out-String)
    $tabSid = $null
    try { $tabSid = ($newOut.Trim() | ConvertFrom-Json).session_id } catch {}
    if ($tabSid) {
        $natExpected = 1
        Start-Sleep -Milliseconds $StepDelayMs
        & $wtcli send-keys -t $tabSid 'echo evt-test' 'Enter' 2>&1 | Out-Null
        Start-Sleep -Milliseconds $StepDelayMs
        & $wtcli kill-pane -t $tabSid 2>&1 | Out-Null
    } else {
        Write-Host "  (could not create a tab; skipping natural-event part)" -ForegroundColor Yellow
    }
}

# -- 4) Drain, then stop the listener --
Start-Sleep -Milliseconds $DrainMs
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300

# -- Collect + parse what the listener received --
$outLines = @(Get-Content $lout -ErrorAction SilentlyContinue)
$errText  = (Get-Content $lerr -ErrorAction SilentlyContinue) -join "`n"

$bugInListen = ($errText -match '0x80010105|0xc0000005|Subscribe failed') -or
               (($outLines -join "`n") -match '0x80010105|0xc0000005')

$broadcastBack = 0
$byMethod      = [ordered]@{}
$parsed        = 0
foreach ($line in $outLines) {
    $t = $line.Trim()
    if (-not $t) { continue }
    if ($t -match [regex]::Escape($marker)) { $broadcastBack++ }
    try {
        $ev = $t | ConvertFrom-Json
        $parsed++
        $key = $null
        if ($ev.PSObject.Properties.Name -contains 'method') { $key = [string]$ev.method }
        elseif ($ev.PSObject.Properties.Name -contains 'type') { $key = [string]$ev.type }
        else { $key = '(unkeyed)' }
        if (-not $byMethod.Contains($key)) { $byMethod[$key] = 0 }
        $byMethod[$key]++
    } catch {}
}

# -- Report --
Write-Host ""
Write-Host "############ EVENT TEST SUMMARY ############" -ForegroundColor Magenta
Write-Host ("Listener exited cleanly : {0}" -f $proc.HasExited)
Write-Host ("send-event failures     : {0} / {1}" -f $sendFails, $BroadcastCount)
$rtColor = if ($broadcastBack -gt 0) { 'Green' } else { 'Yellow' }
Write-Host ("Broadcast round-tripped : {0} / {1}" -f $broadcastBack, $BroadcastCount) -ForegroundColor $rtColor
Write-Host ("Total event lines recv  : {0}  (parsed JSON: {1})" -f $outLines.Count, $parsed)

if ($byMethod.Count -gt 0) {
    Write-Host "`nEvents received by method/type:" -ForegroundColor Cyan
    $byMethod.GetEnumerator() | ForEach-Object {
        [pscustomobject]@{ Method = $_.Key; Count = $_.Value }
    } | Format-Table -AutoSize
}

$wtAfter = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
$wtAlive = -not ($wtPidsBefore | Where-Object { $_ -notin $wtAfter })

$bugColor = if ($bugInListen) { 'Red' } else { 'Green' }
Write-Host ("OS-bug signature        : {0}  (0x80010105 / 0xc0000005 / Subscribe failed)" -f $bugInListen) -ForegroundColor $bugColor
$aliveColor = if ($wtAlive) { 'Green' } else { 'Red' }
Write-Host ("WindowsTerminal alive   : {0}" -f $wtAlive) -ForegroundColor $aliveColor

if ($errText.Trim()) {
    Write-Host "`nlistener stderr:" -ForegroundColor Yellow
    Write-Host $errText.Trim()
}

# Verdict: subscribed without crash, received some events, WT alive.
$verdict = if (-not $bugInListen -and $wtAlive -and $outLines.Count -gt 0 -and $sendFails -eq 0) { 'PASS' } else { 'FAIL' }
$summary = "RESULT=$verdict broadcastBack=$broadcastBack/$BroadcastCount eventsRecv=$($outLines.Count) sendFails=$sendFails bug=$bugInListen wtAlive=$wtAlive"
Add-Content -Path $logPath -Value $summary
Write-Host ""
if ($verdict -eq 'PASS') {
    Write-Host "[OK] $summary" -ForegroundColor Green
} else {
    Write-Host "[X]  $summary" -ForegroundColor Red
    if ($outLines.Count -eq 0) {
        Write-Host "     (No events received. If broadcast didn't echo back, the listener may still be fine --" -ForegroundColor Yellow
        Write-Host "      check stderr above for 'Subscribe failed' to tell a real crash from a routing no-op.)" -ForegroundColor Yellow
    }
}
Write-Host ("Full log + captures     : {0} , {1}" -f $logPath, $lout) -ForegroundColor DarkGray

# Clean up the throwaway capture files (keep the log)
Remove-Item $lout, $lerr -ErrorAction SilentlyContinue
