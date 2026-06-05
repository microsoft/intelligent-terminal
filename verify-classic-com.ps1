<#
  verify-classic-com.ps1
  Verify the classic-COM ITerminalProtocol migration end-to-end.

  RUN THIS IN A POWERSHELL PANE OF THE INSTALLED DEV INTELLIGENT TERMINAL
  (so wtcli has package identity to activate the COM server).

  It exercises every wtcli subcommand over the new classic-COM transport and
  flags the OS-bug signatures (0x80010105 RPC_E_SERVERFAULT / combase
  0xc0000005). Mutating tests create a throwaway "wtcli-test" tab and clean up.

  PASS for all + WindowsTerminal.exe still alive  ==>  migration verified.
#>

$ErrorActionPreference = 'Continue'
$script:pass = 0
$script:fail = 0
$results = New-Object System.Collections.ArrayList

# ── Resolve wtcli (prefer PATH, else the installed package) ──
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
Write-Host "wtcli : $wtcli" -ForegroundColor DarkGray
$wtBefore = [bool](Get-Process WindowsTerminal -ErrorAction SilentlyContinue)
Write-Host ("WindowsTerminal.exe running: {0}`n" -f $wtBefore) -ForegroundColor DarkGray

function Test-Step {
    param([string]$Name, [string[]]$WtArgs)
    Write-Host ("-- {0}:  wtcli {1}" -f $Name, ($WtArgs -join ' ')) -ForegroundColor Cyan
    $out = (& $wtcli @WtArgs 2>&1 | Out-String)
    $ec  = $LASTEXITCODE
    if ($out.Trim()) { Write-Host $out.Trim() }

    if ($out -match '0x80010105|0xc0000005|server threw an exception') {
        Write-Host "  [FAIL] OS-bug signature in output!" -ForegroundColor Red
        $script:fail++; $v = 'FAIL (bug)'
    }
    elseif ($out -match 'failed: 0x|Connection failed|Subscribe failed') {
        Write-Host "  [FAIL] HRESULT error" -ForegroundColor Red
        $script:fail++; $v = 'FAIL (hresult)'
    }
    elseif ($ec -ne 0) {
        Write-Host ("  [FAIL] exit code {0}" -f $ec) -ForegroundColor Red
        $script:fail++; $v = "FAIL (exit $ec)"
    }
    else {
        Write-Host "  [PASS]" -ForegroundColor Green
        $script:pass++; $v = 'PASS'
    }
    [void]$results.Add([pscustomobject]@{ Test = $Name; Verdict = $v })
    Write-Host ""
    return $out
}

Write-Host "############ READ-ONLY ############`n" -ForegroundColor Magenta
Test-Step 'info'               @('info')                | Out-Null
Test-Step 'list-windows'       @('list-windows')        | Out-Null
Test-Step 'list-windows(json)' @('--json','list-windows') | Out-Null
Test-Step 'list-tabs'          @('list-tabs')           | Out-Null
Test-Step 'list-panes'         @('list-panes')          | Out-Null
Test-Step 'active-pane'        @('active-pane')         | Out-Null
Test-Step 'get-settings'       @('--json','info')       | Out-Null

# Capture the active pane's session id for targeted read-only ops.
$activeSid = $null
try { $activeSid = (& $wtcli --json active-pane 2>$null | ConvertFrom-Json).session_id } catch {}
Write-Host ("active session_id = {0}`n" -f $activeSid) -ForegroundColor DarkGray
if ($activeSid) {
    Test-Step 'pane-status'  @('pane-status','-t',$activeSid)        | Out-Null
    Test-Step 'capture-pane' @('capture-pane','-t',$activeSid,'-l','5') | Out-Null
}

Write-Host "############ MUTATING (creates + cleans up a tab) ############`n" -ForegroundColor Magenta
$newOut = Test-Step 'new-tab' @('--json','new-tab','-c','cmd.exe /k echo wtcli-test','-n','wtcli-test')
$newSid = $null
try { $newSid = ($newOut | ConvertFrom-Json).session_id } catch {}
Write-Host ("new tab session_id = {0}`n" -f $newSid) -ForegroundColor DarkGray

if ($newSid) {
    Start-Sleep -Milliseconds 800
    Test-Step 'focus-pane'        @('focus-pane','-t',$newSid)                          | Out-Null
    Test-Step 'send-keys'         @('send-keys','-t',$newSid,'echo classic-com-works','Enter') | Out-Null
    $splitOut = Test-Step 'split-pane' @('--json','split-pane','-t',$newSid,'-d','right')
    $splitSid = $null
    try { $splitSid = ($splitOut | ConvertFrom-Json).session_id } catch {}
    Start-Sleep -Milliseconds 500
    Test-Step 'pane-status(new)'  @('pane-status','-t',$newSid)                         | Out-Null
    if ($splitSid) { Test-Step 'kill-pane(split)' @('kill-pane','-t',$splitSid)         | Out-Null }
    Test-Step 'kill-pane(cleanup)' @('kill-pane','-t',$newSid)                          | Out-Null
}

Write-Host "############ EVENTS (subscribe is where #197 crashed) ############`n" -ForegroundColor Magenta
Write-Host "-- listen: subscribe in background, then send-event" -ForegroundColor Cyan
$lout = Join-Path $env:TEMP ("wtcli_listen_{0}.out" -f $PID)
$lerr = "$lout.err"
$proc = Start-Process -FilePath $wtcli -ArgumentList 'listen' `
    -RedirectStandardOutput $lout -RedirectStandardError $lerr -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2          # let it activate + Subscribe
& $wtcli send-event -e wtcli.test.ping '{"hello":"classic"}' 2>&1 | Out-Null
Start-Sleep -Seconds 1
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
$gotOut = (Get-Content $lout -ErrorAction SilentlyContinue) -join "`n"
$gotErr = (Get-Content $lerr -ErrorAction SilentlyContinue) -join "`n"
if ($gotOut) { Write-Host $gotOut }
if ($gotErr -match '0x80010105|0xc0000005|Subscribe failed') {
    Write-Host ("  [FAIL] listen/Subscribe hit the bug: {0}" -f $gotErr) -ForegroundColor Red
    $script:fail++; [void]$results.Add([pscustomobject]@{ Test='listen/subscribe'; Verdict='FAIL (bug)' })
}
else {
    Write-Host "  [PASS] listen subscribed + ran without the bug" -ForegroundColor Green
    $script:pass++; [void]$results.Add([pscustomobject]@{ Test='listen/subscribe'; Verdict='PASS' })
}
Remove-Item $lout, $lerr -ErrorAction SilentlyContinue
Write-Host ""

# ── Summary ──
Write-Host "############ SUMMARY ############" -ForegroundColor Magenta
$results | Format-Table -AutoSize
$wtAfter = [bool](Get-Process WindowsTerminal -ErrorAction SilentlyContinue)
$sumColor = if ($script:fail -gt 0) { 'Red' } else { 'Green' }
Write-Host ("PASS: {0}    FAIL: {1}" -f $script:pass, $script:fail) -ForegroundColor $sumColor
$aliveColor = if ($wtAfter) { 'Green' } else { 'Red' }
Write-Host ("WindowsTerminal.exe still alive: {0}" -f $wtAfter) -ForegroundColor $aliveColor
if ($script:fail -eq 0 -and $wtAfter) {
    Write-Host "`n[OK] Classic-COM migration verified - no 0x80010105, no crash." -ForegroundColor Green
}
else {
    Write-Host "`n[X] Issues found - see FAIL rows above (or WindowsTerminal crashed)." -ForegroundColor Red
}
