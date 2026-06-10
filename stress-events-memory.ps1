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
  THREE process families to see if they plateau (healthy) or keep climbing:
    WindowsTerminal = the classic-COM server (event fan-out happens here)
    wtcli           = the persistent `wtcli listen` COM EventSink the helper
                      spawns (plus short-lived send-event processes in flight)
    wta             = the helper that reads wtcli stdout and routes events into
                      its session registry
  To exercise the REAL helper path end to end, open one or more AGENT PANES
  before running -- that is what puts a live wta.exe + its `wtcli listen`
  subscriber on the event fan-out. With no agent pane open, only the server
  side (WindowsTerminal) is exercised.

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
# PowerShell 7.3+ keeps backslashes in native args (\" stays \"), breaking the
# JSON we pass to wtcli; PS 5.1 strips them. 'Legacy' makes 7.x behave like 5.1
# so the \" escaping works in both. Harmless (ignored) on 5.1.
$PSNativeCommandArgumentPassing = 'Legacy'

# -- Keep the machine awake for the whole run --
# A long soak is worthless if the box sleeps mid-flood (the workers freeze while
# wall-clock keeps advancing). ES_CONTINUOUS | ES_SYSTEM_REQUIRED tells Windows
# to stay awake without changing the user's global power settings; cleared at the
# end. NOTE: this does NOT override a laptop *lid-close* sleep -- keep the lid
# open / stay plugged in for multi-hour runs.
try {
    Add-Type -ErrorAction Stop -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SleepGuard {
    [DllImport("kernel32.dll")]
    public static extern uint SetThreadExecutionState(uint esFlags);
}
'@
    $ES_CONTINUOUS       = [uint32]'0x80000000'
    $ES_SYSTEM_REQUIRED  = [uint32]'0x00000001'
    [void][SleepGuard]::SetThreadExecutionState($ES_CONTINUOUS -bor $ES_SYSTEM_REQUIRED)
    $sleepGuardOn = $true
    Write-Host "Sleep guard ON (system kept awake for the run)." -ForegroundColor DarkGray
} catch {
    $sleepGuardOn = $false
    Write-Host "WARNING: could not arm sleep guard ($($_.Exception.Message)). Disable sleep manually for long runs." -ForegroundColor Yellow
}

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

# Process families we watch (see header). Order matters for display.
$ProcNames = @('WindowsTerminal', 'wtcli', 'wta')

function Sample-Mem {
    # WindowsTerminal vanishing == the crash we are guarding against, so a null
    # return here is the crash signal the caller keys off.
    if (@(Get-Process WindowsTerminal -ErrorAction SilentlyContinue).Count -eq 0) { return $null }
    $per = [ordered]@{}
    $tP = 0.0; $tW = 0.0; $tH = 0; $tT = 0; $tN = 0
    foreach ($n in $ProcNames) {
        $p = @(Get-Process $n -ErrorAction SilentlyContinue)
        $priv = if ($p.Count) { [math]::Round((($p | Measure-Object PrivateMemorySize64 -Sum).Sum) / 1MB, 1) } else { 0.0 }
        $work = if ($p.Count) { [math]::Round((($p | Measure-Object WorkingSet64 -Sum).Sum) / 1MB, 1) } else { 0.0 }
        $hnd  = if ($p.Count) { [int](($p | Measure-Object HandleCount -Sum).Sum) } else { 0 }
        $thr  = if ($p.Count) { [int](($p | ForEach-Object { $_.Threads.Count } | Measure-Object -Sum).Sum) } else { 0 }
        $per[$n] = [pscustomobject]@{ Procs = $p.Count; PrivateMB = $priv; WorkingMB = $work; Handles = $hnd; Threads = $thr }
        $tP += $priv; $tW += $work; $tH += $hnd; $tT += $thr; $tN += $p.Count
    }
    [pscustomobject]@{
        Per       = $per
        PrivateMB = [math]::Round($tP, 1)
        WorkingMB = [math]::Round($tW, 1)
        Handles   = $tH
        Threads   = $tT
        Procs     = $tN
    }
}

# Build one CSV/table row: totals plus a per-family breakdown.
function New-SampleRow([int]$elapsed, [int]$events, $m) {
    [pscustomobject]@{
        ElapsedSec = $elapsed
        Events     = $events
        PrivateMB  = $m.PrivateMB
        Handles    = $m.Handles
        Threads    = $m.Threads
        WT_Priv    = $m.Per.WindowsTerminal.PrivateMB
        WT_Hnd     = $m.Per.WindowsTerminal.Handles
        wtcli_Priv = $m.Per.wtcli.PrivateMB
        wtcli_Hnd  = $m.Per.wtcli.Handles
        wtcli_N    = $m.Per.wtcli.Procs
        wta_Priv   = $m.Per.wta.PrivateMB
        wta_Hnd    = $m.Per.wta.Handles
        wta_N      = $m.Per.wta.Procs
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
    $PSNativeCommandArgumentPassing = 'Legacy'   # runspace scope: keep \" working on PS 7.x
    $sent = 0; $fail = 0; $bug = $false; $firstErr = ''
    while (-not $stop.WaitOne(0)) {
        $payload = '{\"t\":\"token\",\"seq\":' + $sent + ',\"marker\":\"' + $marker + '\"}'
        $o = (& $wtcli send-event -e $eventType -p $sid $payload 2>&1 | Out-String)
        $sent++
        if ($LASTEXITCODE -ne 0) { $fail++; if (-not $firstErr) { $firstErr = $o.Trim() } }
        if ($o -match '0x80010105|0xc0000005|server threw an exception') { $bug = $true }
        [System.Threading.Monitor]::Enter($shared); $shared.Events++; [System.Threading.Monitor]::Exit($shared)
        if ($stop.WaitOne($intervalMs)) { break }   # wait the cadence, or exit early on stop
    }
    $results.Add([pscustomobject]@{ Sid = $sid; Sent = $sent; Fail = $fail; Bug = $bug; FirstErr = $firstErr })
}

# -- Baseline before the flood --
$baseline = Sample-Mem
$samples  = New-Object System.Collections.ArrayList
[void]$samples.Add((New-SampleRow 0 0 $baseline))
Write-Host ("baseline (sum) : Private={0}MB  Handles={1}  Threads={2}" -f $baseline.PrivateMB, $baseline.Handles, $baseline.Threads) -ForegroundColor Cyan
foreach ($n in $ProcNames) {
    $b = $baseline.Per.$n
    Write-Host ("   {0,-16} procs={1}  Private={2}MB  Handles={3}  Threads={4}" -f $n, $b.Procs, $b.PrivateMB, $b.Handles, $b.Threads) -ForegroundColor DarkCyan
}
if ($baseline.Per.wta.Procs -eq 0) {
    Write-Host "   NOTE: no wta.exe running -- open an AGENT PANE to put the REAL helper on the event path." -ForegroundColor Yellow
}

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
# Sleep/suspend detection: each loop turn is ~250ms. If two consecutive turns are
# seconds apart, the machine suspended (workers were frozen) -- the flood was NOT
# continuous, so the soak result is INVALID even though it may look like a PASS.
$prevElapsed = 0.0
$sleptSec    = 0.0
while ($sw.Elapsed.TotalSeconds -lt $DurationSec) {
    Start-Sleep -Milliseconds 250
    $now = $sw.Elapsed.TotalSeconds
    if (($now - $prevElapsed) -gt 30) {
        $gap = [math]::Round($now - $prevElapsed, 0)
        $sleptSec += $gap
        Write-Host ("  !! wall-clock jumped +{0}s at t={1}s -- machine likely SLEPT; flood was not continuous." -f $gap, [int]$now) -ForegroundColor Red
        Add-Content -Path $logPath -Value "WALL-CLOCK JUMP +${gap}s at t=$([int]$now)s (suspend)"
    }
    $prevElapsed = $now
    $elapsed = [int]$now
    if ($elapsed -ge $nextSample) {
        $m = Sample-Mem
        if (-not $m) { $crashed = $true; break }
        [System.Threading.Monitor]::Enter($shared); $ev = [int]$shared.Events; [System.Threading.Monitor]::Exit($shared)
        [void]$samples.Add((New-SampleRow $elapsed $ev $m))
        $dP = $m.PrivateMB - $baseline.PrivateMB
        Write-Host ("  t={0,4}s  ev={1,7}  Priv={2,6}MB ({3:+0.0;-0.0;0})  WT={4}MB/{5}h  wtcli={6}MB/{7}h(x{8})  wta={9}MB/{10}h(x{11})" -f `
            $elapsed, $ev, $m.PrivateMB, $dP, `
            $m.Per.WindowsTerminal.PrivateMB, $m.Per.WindowsTerminal.Handles, `
            $m.Per.wtcli.PrivateMB, $m.Per.wtcli.Handles, $m.Per.wtcli.Procs, `
            $m.Per.wta.PrivateMB, $m.Per.wta.Handles, $m.Per.wta.Procs) -ForegroundColor Gray
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

$final = Sample-Mem
if ($final) {
    [void]$samples.Add((New-SampleRow ([int]$sw.Elapsed.TotalSeconds) $events $final))
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
if ($sendFails -gt 0) {
    Write-Host "send-event error(s), distinct:" -ForegroundColor Yellow
    $results | Where-Object { $_.FirstErr } | Select-Object -ExpandProperty FirstErr | Sort-Object -Unique | ForEach-Object { Write-Host ("  | " + $_) -ForegroundColor Yellow }
}
Write-Host ("Private bytes      : baseline {0}MB -> final {1}MB  (delta {2:+0.0;-0.0;0}MB, peak {3}MB)" -f $baseline.PrivateMB, $final.PrivateMB, $deltaP, $peakP)
Write-Host ("  2nd-half growth  : {0:+0.0;-0.0;0}MB  (flat = plateaued; large = still climbing)" -f $grow2nd)
$hColor = if ($deltaH -gt $HandleFailDelta) { 'Red' } else { 'Green' }
Write-Host ("Handles            : baseline {0} -> final {1}  (delta {2:+0;-0;0})" -f $baseline.Handles, $final.Handles, $deltaH) -ForegroundColor $hColor
Write-Host ("Threads            : baseline {0} -> final {1}  (delta {2:+0;-0;0})" -f $baseline.Threads, $final.Threads, $deltaT)

# Per-family attribution: which side (server / COM-sink / helper) moved.
Write-Host ""
Write-Host "Per-process-family (baseline -> final):" -ForegroundColor Cyan
$famRows = foreach ($n in $ProcNames) {
    $b = $baseline.Per.$n; $f = $final.Per.$n
    [pscustomobject]@{
        Process   = $n
        Procs     = ("{0}->{1}" -f $b.Procs, $f.Procs)
        Priv_base = $b.PrivateMB
        Priv_fin  = $f.PrivateMB
        Priv_d    = [math]::Round($f.PrivateMB - $b.PrivateMB, 1)
        Hnd_d     = $f.Handles - $b.Handles
        Thr_d     = $f.Threads - $b.Threads
    }
}
$famRows | Format-Table -AutoSize
$helperSeen = ($baseline.Per.wta.Procs -gt 0) -or ($final.Per.wta.Procs -gt 0)
if (-not $helperSeen) {
    Write-Host "NOTE: no wta.exe seen the whole run -- the REAL helper path was NOT exercised." -ForegroundColor Yellow
    Write-Host "      Open an agent pane and re-run to stress server + COM-sink + helper together." -ForegroundColor Yellow
}
Write-Host "(wtcli column = persistent listen subscriber + short-lived send-event procs; it fluctuates -- watch the trend, not the absolute.)" -ForegroundColor DarkGray

$wtD    = [math]::Round($final.Per.WindowsTerminal.PrivateMB - $baseline.Per.WindowsTerminal.PrivateMB, 1)
$wtcliD = [math]::Round($final.Per.wtcli.PrivateMB - $baseline.Per.wtcli.PrivateMB, 1)
$wtaD   = [math]::Round($final.Per.wta.PrivateMB - $baseline.Per.wta.PrivateMB, 1)

$bColor = if ($bugHit) { 'Red' } else { 'Green' }
Write-Host ("OS-bug signature   : {0}" -f $bugHit) -ForegroundColor $bColor
$aColor = if ($wtAlive) { 'Green' } else { 'Red' }
Write-Host ("WindowsTerminal OK : {0}" -f $wtAlive) -ForegroundColor $aColor

$slept = $sleptSec -gt 60
if ($slept) {
    Write-Host ("Suspend detected   : machine slept ~{0}s during the run -- flood was NOT continuous." -f [int]$sleptSec) -ForegroundColor Red
}

$leaking = (($deltaP -gt $GrowthFailMB) -and ($grow2nd -gt ($GrowthFailMB / 2))) -or ($deltaH -gt $HandleFailDelta)
# A suspended run is INVALID, not PASS: the soak never actually ran continuously.
$verdict = if ($slept) { 'INVALID' } elseif ($wtAlive -and -not $bugHit -and -not $leaking) { 'PASS' } else { 'FAIL' }
$summary = "RESULT=$verdict events=$events privateDeltaMB=$deltaP grow2ndHalfMB=$grow2nd handleDelta=$deltaH bug=$bugHit wtAlive=$wtAlive wtPrivD=$wtD wtcliPrivD=$wtcliD wtaPrivD=$wtaD helperSeen=$helperSeen sleptSec=$([int]$sleptSec)"
Add-Content -Path $logPath -Value $summary

# Release the sleep guard so the machine can sleep normally again.
if ($sleepGuardOn) { [void][SleepGuard]::SetThreadExecutionState([uint32]'0x80000000') }
Write-Host ""
if ($verdict -eq 'PASS') {
    Write-Host "[OK] $summary" -ForegroundColor Green
    Write-Host "     Memory/handles plateaued -- no unbounded growth under the event flood." -ForegroundColor Green
} elseif ($verdict -eq 'INVALID') {
    Write-Host "[!]  $summary" -ForegroundColor Yellow
    Write-Host "     Machine slept mid-run -- this is NOT a valid soak. Disable sleep / keep the lid open and re-run." -ForegroundColor Yellow
} else {
    Write-Host "[X]  $summary" -ForegroundColor Red
    if ($leaking) { Write-Host "     Memory or handles kept climbing -- possible leak; inspect the CSV curve." -ForegroundColor Yellow }
}
Write-Host ("CSV (memory curve) : {0}" -f $csvPath) -ForegroundColor DarkGray
Write-Host ("Log                : {0}" -f $logPath) -ForegroundColor DarkGray
if ($lout) { Remove-Item $lout, ($lout + '.err') -ErrorAction SilentlyContinue }
