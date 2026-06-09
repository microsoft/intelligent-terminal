<#
  soak-test-wtcli.ps1
  Stress / soak test for the classic-COM wtcli surface.

  RUN THIS IN A POWERSHELL PANE OF THE INSTALLED DEV INTELLIGENT TERMINAL
  (so wtcli inherits WT_COM_CLSID and can activate the COM server).

  Each iteration runs a full cycle and records pass/fail per call type:
    1. new-tab        - open a tab
    2. split-pane     - open a pane
    3. send-keys      - execute a harmless command (echo)
    4. pane-status    - get process status (pane info)
    5. active-pane    - get PaneInfo
    6. kill-pane x2   - clean up the split pane and the tab (so we don't leak)

  Stops early if a crash signature (0x80010105 / 0xc0000005 / connection loss)
  appears, reporting how far it got. Writes a summary + a failures CSV.

  Usage:
    .\soak-test-wtcli.ps1                 # 1000 iterations
    .\soak-test-wtcli.ps1 -Iterations 50  # quick run
    .\soak-test-wtcli.ps1 -StepDelayMs 150 -ProgressEvery 25
#>

[CmdletBinding()]
param(
    [int]$Iterations   = 1000,   # full cycles to run
    [int]$StepDelayMs  = 250,    # wait after creating a pane before using it
    [int]$ProgressEvery = 25,    # print progress every N iterations
    [string]$Command   = "echo soak-test",   # harmless command to execute
    [switch]$StopOnFirstFailure,             # halt on any failed call (default: keep going)
    [string]$LogDir    = $env:TEMP
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
$logPath = Join-Path $LogDir "soak-wtcli_$stamp.log"
$csvPath = Join-Path $LogDir "soak-wtcli_failures_$stamp.csv"

Write-Host "wtcli       : $wtcli"            -ForegroundColor DarkGray
Write-Host "iterations  : $Iterations"        -ForegroundColor DarkGray
Write-Host "log         : $logPath"           -ForegroundColor DarkGray
Write-Host ""

# Snapshot the WindowsTerminal processes so we can detect a host crash.
$wtPidsBefore = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
Write-Host ("WindowsTerminal PIDs before: {0}" -f ($wtPidsBefore -join ', ')) -ForegroundColor DarkGray

# -- per-call stats + failure log --
$stats    = [ordered]@{}
$failures = New-Object System.Collections.ArrayList
$bugHits  = 0
$crashed  = $false
$crashMsg = $null
# Initialize the stopwatch up front so the summary never hits a null $sw,
# even if the preflight below returns early.
$sw = [System.Diagnostics.Stopwatch]::StartNew()

function Record-Call {
    param([string]$Name, [bool]$Ok)
    if (-not $stats.Contains($Name)) { $stats[$Name] = [pscustomobject]@{ Pass = 0; Fail = 0 } }
    if ($Ok) { $stats[$Name].Pass++ } else { $stats[$Name].Fail++ }
}

# Run one wtcli call. Returns @{ Ok; Out; Ec; Bug }. Updates stats + failure log.
function Invoke-Wt {
    param([int]$Iter, [string]$Name, [string[]]$WtArgs)
    $out = (& $wtcli @WtArgs 2>&1 | Out-String)
    $ec  = $LASTEXITCODE
    $bug = ($out -match '0x80010105|0xc0000005|server threw an exception')
    $err = ($out -match 'failed: 0x|Connection failed|Subscribe failed|WT_COM_CLSID not set')
    $ok  = (-not $bug) -and (-not $err) -and ($ec -eq 0)

    Record-Call -Name $Name -Ok $ok
    if ($bug) { $script:bugHits++ }
    if (-not $ok) {
        [void]$failures.Add([pscustomobject]@{
            Iteration = $Iter
            Call      = $Name
            ExitCode  = $ec
            BugSig    = [bool]$bug
            Args      = ($WtArgs -join ' ')
            Output    = $out.Trim()
        })
        Add-Content -Path $logPath -Value ("[iter {0}] FAIL {1} (ec={2} bug={3}) :: {4}" -f $Iter, $Name, $ec, $bug, $out.Trim())
    }
    return [pscustomobject]@{ Ok = $ok; Out = $out; Ec = $ec; Bug = $bug; Err = $err }
}

# Try to pull session_id out of a --json response.
function Get-SessionId {
    param([string]$JsonText)
    try { return ($JsonText.Trim() | ConvertFrom-Json).session_id } catch { return $null }
}

# -- Connectivity preflight --
$pre = Invoke-Wt -Iter 0 -Name 'preflight(info)' -WtArgs @('info')
if (-not $pre.Ok) {
    Write-Host "ERROR: preflight 'wtcli info' failed - aborting before the loop." -ForegroundColor Red
    Write-Host $pre.Out
    return
}

Add-Content -Path $logPath -Value "=== soak-test-wtcli $stamp : $Iterations iterations ==="
$sw.Restart()   # time the loop itself

# -- Main loop --
for ($i = 1; $i -le $Iterations; $i++) {

    # 1) open a tab
    $newOut = Invoke-Wt -Iter $i -Name 'new-tab' -WtArgs @('--json','new-tab','-c','cmd.exe /k','-n','soak-test')
    $tabSid = Get-SessionId $newOut.Out
    if ($newOut.Bug) { $crashed = $true; $crashMsg = "bug signature on new-tab (iter $i)"; break }
    if (-not $tabSid) {
        Record-Call -Name '(tab sid parse)' -Ok $false
        if ($StopOnFirstFailure) { break }
        continue   # can't run the rest of the cycle without a tab
    }
    Start-Sleep -Milliseconds $StepDelayMs

    # 2) open a pane (split)
    $splitOut = Invoke-Wt -Iter $i -Name 'split-pane' -WtArgs @('--json','split-pane','-t',$tabSid,'-d','right','-c','cmd.exe /k')
    $splitSid = Get-SessionId $splitOut.Out
    if ($splitOut.Bug) { $crashed = $true; $crashMsg = "bug signature on split-pane (iter $i)"; break }
    Start-Sleep -Milliseconds $StepDelayMs

    # 3) execute a harmless command (non-raw so "Enter" -> carriage return runs it)
    $r = Invoke-Wt -Iter $i -Name 'send-keys' -WtArgs @('send-keys','-t',$tabSid,("{0}-{1}" -f $Command,$i),'Enter')
    if ($r.Bug) { $crashed = $true; $crashMsg = "bug signature on send-keys (iter $i)"; break }

    # 4) get pane info - process status
    $r = Invoke-Wt -Iter $i -Name 'pane-status' -WtArgs @('pane-status','-t',$tabSid)
    if ($r.Bug) { $crashed = $true; $crashMsg = "bug signature on pane-status (iter $i)"; break }

    # 5) get pane info - active pane (PaneInfo)
    $r = Invoke-Wt -Iter $i -Name 'active-pane' -WtArgs @('--json','active-pane')
    if ($r.Bug) { $crashed = $true; $crashMsg = "bug signature on active-pane (iter $i)"; break }

    # 6) clean up - kill the split pane, then the tab's pane (closes the tab)
    if ($splitSid) {
        $r = Invoke-Wt -Iter $i -Name 'kill-pane(split)' -WtArgs @('kill-pane','-t',$splitSid)
        if ($r.Bug) { $crashed = $true; $crashMsg = "bug signature on kill-pane(split) (iter $i)"; break }
    }
    $r = Invoke-Wt -Iter $i -Name 'kill-pane(tab)' -WtArgs @('kill-pane','-t',$tabSid)
    if ($r.Bug) { $crashed = $true; $crashMsg = "bug signature on kill-pane(tab) (iter $i)"; break }

    if ($StopOnFirstFailure -and $failures.Count -gt 0) { break }

    # progress + periodic host-alive check
    if (($i % $ProgressEvery) -eq 0) {
        $fails = $failures.Count
        $rate  = [math]::Round($i / $sw.Elapsed.TotalSeconds, 1)
        Write-Host ("  iter {0,5}/{1}  fails={2}  bugHits={3}  {4} cyc/s" -f $i, $Iterations, $fails, $bugHits, $rate) -ForegroundColor Cyan

        $now = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
        $gone = $wtPidsBefore | Where-Object { $_ -notin $now }
        if ($gone) { $crashed = $true; $crashMsg = "WindowsTerminal PID(s) $($gone -join ',') disappeared (iter $i)"; break }
    }
}

if ($sw) { $sw.Stop() }
$completed = if ($crashed) { $i } elseif ($i) { $Iterations } else { 0 }

# -- Summary --
Write-Host ""
Write-Host "############ SUMMARY ############" -ForegroundColor Magenta
$rows = foreach ($k in $stats.Keys) {
    $p = $stats[$k].Pass; $f = $stats[$k].Fail; $t = $p + $f
    [pscustomobject]@{
        Call    = $k
        Pass    = $p
        Fail    = $f
        Total   = $t
        'Fail%' = if ($t) { [math]::Round(100.0 * $f / $t, 2) } else { 0 }
    }
}
$rows | Format-Table -AutoSize

$totalCalls = ($rows | Measure-Object Total -Sum).Sum
$totalFails = ($rows | Measure-Object Fail  -Sum).Sum
$wtAfter    = @(Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
$wtAlive    = -not ($wtPidsBefore | Where-Object { $_ -notin $wtAfter })

Write-Host ("Iterations run     : {0} / {1}" -f $completed, $Iterations)
Write-Host ("Total calls        : {0}" -f $totalCalls)
$failColor = if ($totalFails -gt 0) { 'Red' } else { 'Green' }
Write-Host ("Total failures     : {0}" -f $totalFails) -ForegroundColor $failColor
$bugColor = if ($bugHits -gt 0) { 'Red' } else { 'Green' }
Write-Host ("OS-bug signatures  : {0}  (0x80010105 / 0xc0000005)" -f $bugHits) -ForegroundColor $bugColor
Write-Host ("Elapsed            : {0:n1}s  ({1:n1} cycles/s)" -f $sw.Elapsed.TotalSeconds, ($completed / [math]::Max($sw.Elapsed.TotalSeconds,1)))
$aliveColor = if ($wtAlive) { 'Green' } else { 'Red' }
Write-Host ("WindowsTerminal OK : {0}  (before: {1} | after: {2})" -f $wtAlive, ($wtPidsBefore -join ','), ($wtAfter -join ',')) -ForegroundColor $aliveColor

if ($crashed) {
    Write-Host ("`n[X] STOPPED EARLY: {0}" -f $crashMsg) -ForegroundColor Red
}

if ($failures.Count -gt 0) {
    $failures | Export-Csv -Path $csvPath -NoTypeInformation -Encoding UTF8
    Write-Host ("`nFailures CSV       : {0}" -f $csvPath) -ForegroundColor Yellow
    Write-Host "First few failures:" -ForegroundColor Yellow
    $failures | Select-Object -First 5 Iteration, Call, ExitCode, BugSig, Output | Format-List
}

# Machine-readable one-liner (handy to paste back)
$verdict = if (-not $crashed -and $totalFails -eq 0 -and $bugHits -eq 0 -and $wtAlive) { 'PASS' } else { 'FAIL' }
$summaryLine = "RESULT=$verdict iterations=$completed/$Iterations totalCalls=$totalCalls failures=$totalFails bugHits=$bugHits wtAlive=$wtAlive elapsed=$([math]::Round($sw.Elapsed.TotalSeconds,1))s"
Add-Content -Path $logPath -Value $summaryLine
Write-Host ""
if ($verdict -eq 'PASS') {
    Write-Host "[OK] $summaryLine" -ForegroundColor Green
} else {
    Write-Host "[X]  $summaryLine" -ForegroundColor Red
}
Write-Host ("Full log           : {0}" -f $logPath) -ForegroundColor DarkGray
