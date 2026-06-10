<#
  stress-events-memory.ps1
  Sustained event-flood soak: confirm WindowsTerminal memory does NOT grow
  unbounded when events are streamed continuously from every pane, the way
  multiple LLM agents stream events to the terminal.

  RUN THIS IN A POWERSHELL PANE OF THE INSTALLED DEV INTELLIGENT TERMINAL
  (so wtcli inherits WT_COM_CLSID and can activate the COM server).

  MULTI-WINDOW / MULTI-TAB: wtcli cannot open new *windows*, so set the stage
  yourself first -- open several Windows Terminal windows, each with several
  tabs/panes. This script enumerates EVERY pane across ALL windows/tabs.

  Concurrency model: each pane gets its OWN worker thread (runspace) with its
  OWN ~500ms timer, all firing concurrently -- closer to real multi-agent
  traffic than a single sequential sweep. Meanwhile the main thread samples
  WindowsTerminal private bytes / handles / threads to see if they plateau
  (healthy) or keep climbing (leak).

  PASS = no crash, WT alive, memory + handles plateau (no unbounded growth).

  Keep this file PURE ASCII (Windows PowerShell 5.1 mis-decodes non-ASCII in
  BOM-less .ps1 files and silently breaks parsing).

  Usage:
    .\stress-events-memory.ps1                          # 5 min, 500ms per pane
    .\stress-events-memory.ps1 -DurationSec 900         # 15 min
    .\stress-events-memory.ps1 -CreateTabsPerWindow 3   # add 3 tabs/window first
    .\stress-events-memory.ps1 -WithListener            # also deliver to a subscriber
#>

[CmdletBinding()]
param(
    [int]$DurationSec         = 300,   # total run time
    [int]$IntervalMs          = 500,   # each pane's own send cadence
    [int]$SampleEverySec      = 5,     # memory sampling interval
    [int]$CreateTabsPerWindow = 0,     # add N tabs to each existing window first
    [string]$EventType        = 'agent.llm.token',
    [switch]$WithListener,             # also run a draining listener (end-to-end delivery)
    [int]$GrowthFailMB        = 300,   # heuristic: leak if private bytes grow > this AND still climbing
    [int]$HandleFailDelta     = 8000,  # heuristic: leak if handle count grows > this
    [string]$LogDir           = $env:TEMP
)

$ErrorActionPreference = 'Continue'

# -- Resolve wtcli --
$wtcli = (Get-Command wtcli.exe -ErrorAction SilentlyContinue).Source
if (-not $wtcli) {
    $pkg = Get-AppxPackage *IntelligentTerminal* -ErrorAction SilentlyContinue |
        Sort-Object Version -Descending | Select-Object -First 1
    if ($pkg) {
        $cand = Join-Path $pkg.InstallLocation 'wtcli.exe'
        if (Test-Path $cand) { $wtcli = $cand }
    }
}
if (-not $wtcli) { Write-Host "ERROR: wtcli.exe not found. Run inside the installed dev Terminal." -ForegroundColor Red; return }
if (-not $env:WT_COM_CLSID) { Write-Host "ERROR: WT_COM_CLSID not set. Run inside a Windows Terminal pane." -ForegroundColor Red; return }

$stamp   = Get-Date -Format 'yyyyMMdd-HHmmss'
$marker  = "memflood-$stamp"
$logPath = Join-Path $LogDir "stress-events_$stamp.log"
$csvPath = Join-Path $LogDir "stress-events_mem_$stamp.csv"

function Get-Json([string[]]$WtArgs) {
    $o = (& $wtcli @WtArgs 2>&1 | Out-String).Trim()
    try { return ($o | ConvertFrom-Json) } catch { return $null }
}

function Sample-WtMem {
    $p = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue)
    if ($p.Count -eq 0) { return $null }
    [pscustomobject]@{
        PrivateMB = [math]::Round((($p | Measure-Object PrivateMemorySize64 -Sum).Sum) / 1MB, 1)
        WorkingMB = [math]::Round((($p | Measure-Object WorkingSet64 -Sum).Sum) / 1MB, 1)
        Handles   = [int](($p | Measure-Object HandleCount -Sum).Sum)
        Threads   = [int](($p | ForEach-Object { $_.Threads.Count } | Measure-Object -Sum).Sum)
        Procs     = $p.Count
    }
}

Write-Host "wtcli       : $wtcli" -ForegroundColor DarkGray
Write-Host ("duration    : {0}s   per-pane cadence: {1}ms (concurrent)" -f $DurationSec, $IntervalMs) -ForegroundColor DarkGray
Write-Host ""

# -- Optionally add tabs to existing windows --
if ($CreateTabsPerWindow -gt 0) {
    $w = Get-Json @('--json','list-windows')
    $wl = if ($w.windows) { @($w.windows) } else { @($w) }
    foreach ($win in $wl) {
        for ($k = 0; $k -lt $CreateTabsPerWindow; $k++) {
            & $wtcli new-tab -c 'cmd.exe /k' -n 'memflood' 2>&1 | Out-Null
            Start-Sleep -Milliseconds 150
        }
    }
    Start-Sleep -Milliseconds 500
}

# -- Enumerate every pane across all windows/tabs --
Write-Host "Enumerating panes across all windows/tabs ..." -ForegroundColor Cyan
$paneSids = New-Object System.Collections.ArrayList
$winCount = 0; $tabCount = 0
$wins = Get-Json @('--json','list-windows')
$winList = if ($wins.windows) { @($wins.windows) } else { @($wins) }
foreach ($w in $winList) {
    $wid = $w.window_id
    if ($null -eq $wid) { continue }
    $winCount++
    $tabs = Get-Json @('--json','list-tabs','-w',"$wid")
    $tabList = if ($tabs.tabs) { @($tabs.tabs) } else { @($tabs) }
    foreach ($t in $tabList) {
        $tid = $t.tab_id
        if ($null -eq $tid) { continue }
        $tabCount++
        $panes = Get-Json @('--json','list-panes','-w',"$wid",'-t',"$tid")
        $paneList = if ($panes.panes) { @($panes.panes) } else { @($panes) }
        foreach ($p in $paneList) {
            if ($p.session_id) { [void]$paneSids.Add([string]$p.session_id) }
        }
    }
}
Write-Host ("Found: {0} window(s), {1} tab(s), {2} pane(s)" -f $winCount, $tabCount, $paneSids.Count) -ForegroundColor DarkGray
if ($paneSids.Count -eq 0) { Write-Host "ERROR: no panes found. Open a tab, or use -CreateTabsPerWindow." -ForegroundColor Red; return }
if ($winCount -lt 2) { Write-Host "NOTE: only 1 window. For the multi-window case, open more WT windows first." -ForegroundColor Yellow }

# -- Optional draining listener --
$listener = $null; $lout = $null
if ($WithListener) {
    $lout = Join-Path $LogDir "stress-events_listen_$stamp.out"
    $listener = Start-Process -FilePath $wtcli -ArgumentList '--json','listen' `
        -RedirectStandardOutput $lout -RedirectStandardError ($lout + '.err') -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 1200
    Write-Host "Listener started (end-to-end delivery)." -ForegroundColor DarkGray
}

# -- Shared state for the concurrent workers --
$stop    = New-Object System.Threading.ManualResetEvent($false)   # set -> all workers exit
$results = New-Object 'System.Collections.Concurrent.ConcurrentBag[object]'
$shared  = [hashtable]::Synchronized(@{ Events = [long]0 })       # live counter for progress

# One worker per pane: its own loop, its own IntervalMs timer.
$worker = {
    param([string]$wtcli, [string]$sid, [string]$eventType, [int]$intervalMs, [string]$marker, $stop, $results, $shared)
    $sent = 0; $fail = 0; $bug = $false
    while (-not $stop.WaitOne(0)) {
        $payload = '{\"t\":\"token\",\"seq\":' + $sent + ',\"marker\":\"' + $marker + '\"}'
        $o = (& $wtcli send-event -e $eventType -p $sid $payload 2>&1 | Out-String)
        $sent++
        if ($LASTEXITCODE -ne 0) { $fail++ }
        if ($o -match '0x80010105|0xc0000005|server threw an exception') { $bug = $true }
        [System.Threading.Monitor]::Enter($shared); $shared.Events++; [System.Threading.Monitor]::Exit($shared)
        if ($stop.WaitOne($intervalMs)) { break }   # wait the cadence, or exit early on stop
    }
    $results.Add([pscustomobject]@{ Sid = $sid; Sent = $sent; Fail = $fail; Bug = $bug })
}

# -- Baseline before the flood --
$baseline = Sample-WtMem
$samples  = New-Object System.Collections.ArrayList
[void]$samples.Add([pscustomobject]@{ ElapsedSec = 0; Events = 0; PrivateMB = $baseline.PrivateMB; WorkingMB = $baseline.WorkingMB; Handles = $baseline.Handles; Threads = $baseline.Threads })
Write-Host ("baseline: Private={0}MB Working={1}MB Handles={2} Threads={3} (WT procs={4})" -f $baseline.PrivateMB, $baseline.WorkingMB, $baseline.Handles, $baseline.Threads, $baseline.Procs) -ForegroundColor Cyan

# -- Start one concurrent worker per pane --
$pool = [runspacefactory]::CreateRunspacePool(1, [Math]::Max($paneSids.Count, 1))
$pool.Open()
$handles = @()
foreach ($sid in $paneSids) {
    $ps = [powershell]::Create()
    $ps.RunspacePool = $pool
    [void]$ps.AddScript($worker.ToString()).AddArgument($wtcli).AddArgument($sid).AddArgument($EventType).AddArgument($IntervalMs).AddArgument($marker).AddArgument($stop).AddArgument($results).AddArgument($shared)
    $handles += [pscustomobject]@{ PS = $ps; Async = $ps.BeginInvoke() }
}
Write-Host ("Flooding {0} pane(s), each every {1}ms, for {2}s ...`n" -f $paneSids.Count, $IntervalMs, $DurationSec) -ForegroundColor Cyan
Add-Content -Path $logPath -Value "=== stress-events-memory $stamp : $($paneSids.Count) panes (concurrent), ${DurationSec}s ==="

# -- Main thread: sample memory while the workers flood --
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$nextSample = $SampleEverySec
$crashed = $false
while ($sw.Elapsed.TotalSeconds -lt $DurationSec) {
    Start-Sleep -Milliseconds 250
    $elapsed = [int]$sw.Elapsed.TotalSeconds
    if ($elapsed -ge $nextSample) {
        $m = Sample-WtMem
        if (-not $m) { $crashed = $true; break }
        [System.Threading.Monitor]::Enter($shared); $ev = [int]$shared.Events; [System.Threading.Monitor]::Exit($shared)
        [void]$samples.Add([pscustomobject]@{ ElapsedSec = $elapsed; Events = $ev; PrivateMB = $m.PrivateMB; WorkingMB = $m.WorkingMB; Handles = $m.Handles; Threads = $m.Threads })
        $dP = $m.PrivateMB - $baseline.PrivateMB
        Write-Host ("  t={0,4}s  events={1,7}  Private={2,7}MB ({3:+0.0;-0.0;0}MB)  Handles={4,6}  Threads={5}" -f $elapsed, $ev, $m.PrivateMB, $dP, $m.Handles, $m.Threads) -ForegroundColor Gray
        $nextSample = $elapsed + $SampleEverySec
    }
}
$sw.Stop()

# -- Stop workers, collect totals --
[void]$stop.Set()
foreach ($h in $handles) { try { $h.PS.EndInvoke($h.Async) } catch {}; $h.PS.Dispose() }
$pool.Close(); $pool.Dispose()
if ($listener -and -not $listener.HasExited) { Stop-Process -Id $listener.Id -Force -ErrorAction SilentlyContinue }

$events    = [int](($results | Measure-Object Sent -Sum).Sum)
$sendFails = [int](($results | Measure-Object Fail -Sum).Sum)
$bugHit    = @($results | Where-Object { $_.Bug }).Count -gt 0

$final = Sample-WtMem
if ($final) {
    [void]$samples.Add([pscustomobject]@{ ElapsedSec = [int]$sw.Elapsed.TotalSeconds; Events = $events; PrivateMB = $final.PrivateMB; WorkingMB = $final.WorkingMB; Handles = $final.Handles; Threads = $final.Threads })
} else { $crashed = $true; $final = $baseline }

# -- Analysis --
Write-Host ""
Write-Host "############ MEMORY SOAK SUMMARY ############" -ForegroundColor Magenta
$samples | Format-Table -AutoSize
$samples | Export-Csv -Path $csvPath -NoTypeInformation -Encoding UTF8

$wtAlive = (@(Get-Process WindowsTerminal -ErrorAction SilentlyContinue).Count -gt 0) -and -not $crashed
$peakP   = ($samples | Measure-Object PrivateMB -Maximum).Maximum
$deltaP  = [math]::Round($final.PrivateMB - $baseline.PrivateMB, 1)
$deltaH  = $final.Handles - $baseline.Handles
$deltaT  = $final.Threads - $baseline.Threads
$midSec  = $sw.Elapsed.TotalSeconds / 2
$midS    = $samples | Sort-Object { [math]::Abs($_.ElapsedSec - $midSec) } | Select-Object -First 1
$grow2nd = [math]::Round($final.PrivateMB - $midS.PrivateMB, 1)

Write-Host ("Events sent        : {0}  ({1:n1}/s over {2:n0}s, {3} pane(s) concurrent)" -f $events, ($events / [math]::Max($sw.Elapsed.TotalSeconds,1)), $sw.Elapsed.TotalSeconds, $paneSids.Count)
Write-Host ("send-event fails   : {0}" -f $sendFails)
Write-Host ("Private bytes      : baseline {0}MB -> final {1}MB  (delta {2:+0.0;-0.0;0}MB, peak {3}MB)" -f $baseline.PrivateMB, $final.PrivateMB, $deltaP, $peakP)
Write-Host ("  2nd-half growth  : {0:+0.0;-0.0;0}MB  (flat = plateaued; large = still climbing)" -f $grow2nd)
$hColor = if ($deltaH -gt $HandleFailDelta) { 'Red' } else { 'Green' }
Write-Host ("Handles            : baseline {0} -> final {1}  (delta {2:+0;-0;0})" -f $baseline.Handles, $final.Handles, $deltaH) -ForegroundColor $hColor
Write-Host ("Threads            : baseline {0} -> final {1}  (delta {2:+0;-0;0})" -f $baseline.Threads, $final.Threads, $deltaT)
$bColor = if ($bugHit) { 'Red' } else { 'Green' }
Write-Host ("OS-bug signature   : {0}" -f $bugHit) -ForegroundColor $bColor
$aColor = if ($wtAlive) { 'Green' } else { 'Red' }
Write-Host ("WindowsTerminal OK : {0}" -f $wtAlive) -ForegroundColor $aColor

$leaking = (($deltaP -gt $GrowthFailMB) -and ($grow2nd -gt ($GrowthFailMB / 2))) -or ($deltaH -gt $HandleFailDelta)
$verdict = if ($wtAlive -and -not $bugHit -and -not $leaking) { 'PASS' } else { 'FAIL' }
$summary = "RESULT=$verdict events=$events privateDeltaMB=$deltaP grow2ndHalfMB=$grow2nd handleDelta=$deltaH bug=$bugHit wtAlive=$wtAlive"
Add-Content -Path $logPath -Value $summary
Write-Host ""
if ($verdict -eq 'PASS') {
    Write-Host "[OK] $summary" -ForegroundColor Green
    Write-Host "     Memory/handles plateaued -- no unbounded growth under the event flood." -ForegroundColor Green
} else {
    Write-Host "[X]  $summary" -ForegroundColor Red
    if ($leaking) { Write-Host "     Memory or handles kept climbing -- possible leak; inspect the CSV curve." -ForegroundColor Yellow }
}
Write-Host ("CSV (memory curve) : {0}" -f $csvPath) -ForegroundColor DarkGray
Write-Host ("Log                : {0}" -f $logPath) -ForegroundColor DarkGray
if ($lout) { Remove-Item $lout, ($lout + '.err') -ErrorAction SilentlyContinue }
