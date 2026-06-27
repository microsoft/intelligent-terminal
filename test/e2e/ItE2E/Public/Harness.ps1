# Harness.ps1 — lifecycle: resolve, (safely) configure, launch, attach, teardown.
# Non-destructive by default: settings.json/state.json are backed up and restored.

function Backup-WtConfig {
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    foreach ($f in @($App.SettingsPath, $App.StatePath)) {
        $bak = "$f.e2ebak"
        # A leftover .e2ebak means a prior run crashed before restoring. Recover by
        # restoring it first (revert that run's changes) so we snapshot the real
        # pre-test state — not a state already mutated by the crashed run.
        if (Test-Path $bak) {
            Copy-Item -LiteralPath $bak -Destination $f -Force
            Remove-Item -LiteralPath $bak -Force
            Write-ItLog -Level WARN -Message "Recovered stale backup for $f (prior run did not clean up)"
        }
        if (Test-Path $f) { Copy-Item -LiteralPath $f -Destination $bak -Force; Write-ItLog -Level INFO -Message "Backed up $f" }
    }
}

function Get-DescendantWtaIds {
    <# wta.exe PIDs that are descendants of the given WindowsTerminal pid (master spawned by
       SharedWta, helpers as conpty children). Only these belong to this test run. #>
    [CmdletBinding()] param([Parameter(Mandatory)][int]$RootPid)
    $all = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
    if (-not $all) { return @() }
    $byParent = @{}
    foreach ($p in $all) { $byParent[[int]$p.ParentProcessId] += @($p) }
    # BFS from the WT root to collect all descendant PIDs.
    $descendants = [System.Collections.Generic.HashSet[int]]::new()
    $queue = [System.Collections.Generic.Queue[int]]::new(); $queue.Enqueue($RootPid)
    while ($queue.Count) {
        $cur = $queue.Dequeue()
        foreach ($child in $byParent[$cur]) {
            $cpid = [int]$child.ProcessId
            if ($descendants.Add($cpid)) { $queue.Enqueue($cpid) }
        }
    }
    $all | Where-Object { $_.Name -ieq 'wta.exe' -and $descendants.Contains([int]$_.ProcessId) } |
        Select-Object -ExpandProperty ProcessId
}

function Restore-WtConfig {
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    foreach ($f in @($App.SettingsPath, $App.StatePath)) {
        $bak = "$f.e2ebak"
        if (Test-Path $bak) { Copy-Item -LiteralPath $bak -Destination $f -Force; Remove-Item -LiteralPath $bak -Force; Write-ItLog -Level INFO -Message "Restored $f" }
    }
}

function Get-WtProcessesForApp {
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    $loc = $App.InstallLocation
    Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue | Where-Object {
        try { $loc -and $_.Path -and $_.Path.StartsWith($loc, [StringComparison]::OrdinalIgnoreCase) } catch { $false }
    }
}

function Stop-AppInstances {
    <#
    .SYNOPSIS
        Force a COLD start by terminating every running WindowsTerminal of THIS package.
    .DESCRIPTION
        WT is single-instance: a launch hands off to an existing monarch instead of starting
        fresh, and the monarch keeps `agentFreCompleted` (and the rest of ApplicationState)
        cached in memory — it never re-reads state.json. So driving the FRE overlay, or any
        test that depends on cold-start behaviour, requires no monarch to be alive first.
        Closes gracefully (CloseMainWindow), then force-kills only the specific stragglers by
        pid. ONLY ever targets this IT package's processes (filtered by install location) — it
        never touches the user's stock Windows Terminal.
    #>
    [CmdletBinding()] param([Parameter(Mandatory)]$App, [int]$GraceSec = 6)
    $ids = @(Get-WtProcessesForApp -App $App | Select-Object -ExpandProperty Id)
    if (-not $ids.Count) { return }
    Write-ItLog -Level INFO -Message "Cold start: closing existing $($App.Package) instance(s) [$($ids -join ',')]"
    foreach ($id in $ids) {
        $p = Get-Process -Id $id -ErrorAction SilentlyContinue
        if ($p) { try { $p.CloseMainWindow() | Out-Null } catch {} }
    }
    Test-Until -TimeoutSec $GraceSec -IntervalSec 0.5 -Condition {
        -not @(Get-WtProcessesForApp -App $App).Count
    } | Out-Null
    # Force-kill any window-less / multi-window monarch that ignored CloseMainWindow.
    foreach ($id in @(Get-WtProcessesForApp -App $App | Select-Object -ExpandProperty Id)) {
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
        Write-ItLog -Level WARN -Message "Cold start: force-killed straggler pid=$id"
    }
    # Let the OS tear down the COM monarch registration before the next launch.
    Start-Sleep -Milliseconds 500
}

function Stop-StaleItInstances {
    <#
    .SYNOPSIS
        Close any leftover Intelligent Terminal windows (BOTH the store and dev packages)
        before a test launches.
    .DESCRIPTION
        The harness OWNS every Intelligent Terminal window for the duration of a run: this
        unconditionally closes/kills ALL running IT windows (store + dev) at launch time — not
        only crashed-test leftovers, but also a window a developer started by hand, and even the
        IT window hosting the current shell if the tests are launched from inside Intelligent
        Terminal. So do NOT run this suite from an IT window you want to keep. (The user's stock
        Windows Terminal is never touched — its image lives under Microsoft.WindowsTerminal_*,
        which never matches the *IntelligentTerminal* install-location filter below.)

        Any IT window already running at launch is treated as a leftover from a previous test
        whose AfterAll/Stop-Terminal didn't run (e.g. a BeforeAll that threw). Such a leftover
        causes two real flakes:
          * the single-instance AUMID launch hands off to the stale (often half-initialised)
            window instead of starting fresh, so the harness attaches to a broken instance and
            `new-tab` returns CreateTab E_FAIL (0x80004005);
          * the store and dev packages share one per-brand COM CLSID, so a stale window of the
            OTHER package steals wtcli's CoCreateInstance and misroutes every protocol call.
        Closing all IT windows first makes each launch deterministic and freshly-owned.
    #>
    [CmdletBinding()] param([int]$GraceSec = 6)
    $locs = @(Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' } |
            ForEach-Object { $_.InstallLocation } | Where-Object { $_ })
    if (-not $locs) { return }
    $find = {
        Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue | Where-Object {
            $path = $null; try { $path = $_.Path } catch {}
            $path -and ($locs | Where-Object { $path.StartsWith($_, [StringComparison]::OrdinalIgnoreCase) })
        }
    }
    $procs = @(& $find)
    if (-not $procs.Count) { return }
    Write-ItLog -Level INFO -Message "Cleaning $($procs.Count) stale IT instance(s) before launch: [$(($procs | ForEach-Object Id) -join ',')]"
    foreach ($p in $procs) { try { $p.CloseMainWindow() | Out-Null } catch {} }
    Test-Until -TimeoutSec $GraceSec -IntervalSec 0.5 -Condition { -not @(& $find).Count } | Out-Null
    foreach ($p in @(& $find)) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        Write-ItLog -Level WARN -Message "Force-killed stale IT straggler pid=$($p.Id)"
    }
    Start-Sleep -Milliseconds 500   # let the OS tear down the shared COM monarch registration
}

function Get-ItTestPackage {
    <#
    .SYNOPSIS
        Resolve which package selector the feature/self-test suites should launch.
        Honors the ITE2E_PACKAGE env var (Auto|Store|Dev|<PackageFamilyName>); defaults
        to 'Auto', which prefers a fully-resolvable Store install and falls back to Dev.
        This is the single knob that lets the suites run against a dev-only machine
        (where only the sideload package is installed) without editing each Describe.
    #>
    [CmdletBinding()]
    param()
    if ($env:ITE2E_PACKAGE) { return $env:ITE2E_PACKAGE }
    return 'Auto'
}

function Start-Terminal {
    <#
    .SYNOPSIS
        Resolve, (optionally) configure, launch, and attach to a deployed Intelligent
        Terminal. Returns the app context object used by every primitive.
    .PARAMETER Package   Auto|Store|Dev|<PackageFamilyName>
    .PARAMETER Settings  Hashtable of top-level settings.json keys to apply.
    .PARAMETER PassFre   Mark the agent FRE complete before launch (default $true).
    .PARAMETER Backup    Back up settings/state for restore on Stop-Terminal (default $true).
    .PARAMETER ShowFre   Leave the agent FRE overlay SHOWING (writes agentFreCompleted=false).
                         COM resolution is best-effort in this mode. A fresh monarch is always
                         started (see Stop-StaleItInstances below), which is what lets the FRE
                         re-read state.json — a running monarch caches ApplicationState.
    #>
    [CmdletBinding()]
    param(
        [string]$Package = 'Auto',
        [hashtable]$Settings,
        [bool]$PassFre = $true,
        [bool]$Backup = $true,
        [switch]$ShowFre,
        [int]$TimeoutSec = 60
    )
    $app = Resolve-ItApp -Package $Package
    Write-ItLog -Level INFO -Message "Resolved package $($app.Package) v$($app.Version); wtcli=$($app.WtcliPath)"

    # Per-run framework log file under TEMP.
    $script:ItE2ELogFile = Join-Path $env:TEMP ("ite2e-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

    # Always clear leftover IT instances (store + dev) BEFORE writing config: a stale window
    # from a crashed prior test would otherwise be attached-to in a broken state (new-tab ->
    # CreateTab E_FAIL 0x80004005) or steal the shared per-brand COM CLSID and misroute wtcli.
    # Doing it before config write also stops a closing monarch's flush from clobbering the
    # FRE/settings values we are about to write. This ALWAYS enforces cold-start semantics (a
    # fresh monarch re-reads state.json); -ShowFre separately controls whether the FRE overlay
    # is left showing.
    Stop-StaleItInstances

    if ($Backup) { Backup-WtConfig -App $app }
    if ($ShowFre) { Reset-Fre -App $app | Out-Null }
    elseif ($PassFre) { Invoke-FrePass -App $app | Out-Null }
    if ($Settings) { Set-WtSettings -App $app -Settings $Settings | Out-Null }

    $existing = @(Get-WtProcessesForApp -App $app | Select-Object -ExpandProperty Id)
    # Launch via AUMID shell activation — this is package-specific by construction
    # (shell:AppsFolder\<PackageFamilyName>!App) and therefore launches EXACTLY the
    # target package. The global `wtai` AppExecutionAlias is owned by only one package,
    # so when both the store and a dev/sideloaded IT build are installed it is ambiguous
    # and would launch the wrong one (silently timing out the dev-targeted tests). The
    # earlier crash-on-AUMID-activation was the state.json corruption bug (now fixed via
    # the unary-comma ConvertFrom-ItJsonElement change), not the activation method.
    if ($app.AppUserModelId) {
        Write-ItLog -Level INFO -Message "Launching via AUMID: $($app.AppUserModelId)"
        Start-Process -FilePath 'explorer.exe' -ArgumentList "shell:AppsFolder\$($app.AppUserModelId)" | Out-Null
    }
    elseif ($app.LaunchAlias -and (Test-Path $app.LaunchAlias)) {
        Write-ItLog -Level WARN -Message "No AUMID; falling back to wtai alias ($($app.LaunchAlias)) — may be ambiguous across packages."
        Start-Process -FilePath $app.LaunchAlias | Out-Null
    }

    # Find our WindowsTerminal.exe process (prefer a newly-spawned pid).
    $proc = Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 1 -Because "WindowsTerminal process for $($app.Package)" -Condition {
        $ps = Get-WtProcessesForApp -App $app
        $new = $ps | Where-Object { $_.Id -notin $existing } | Select-Object -First 1
        if ($new) { $new } elseif ($ps) { $ps | Select-Object -First 1 } else { $null }
    }
    $app.Pid = $proc.Id
    # Track whether WE launched this process or merely attached to a pre-existing one
    # (WT is single-instance — a launch can join an already-running window). Stop-Terminal
    # only kills processes we launched, so it never terminates a user's existing terminal.
    $app | Add-Member -NotePropertyName Launched -NotePropertyValue ($app.Pid -notin $existing) -Force
    if (-not $app.Launched) {
        Write-ItLog -Level WARN -Message "Attached to a pre-existing WindowsTerminal (pid=$($app.Pid)); Stop-Terminal will NOT kill it."
    }
    Write-ItLog -Level INFO -Message "WindowsTerminal pid=$($app.Pid) launched=$($app.Launched)"

    # Bring COM online and resolve the brand CLSID. Best-effort while the FRE overlay is up
    # (the overlay replaces the window content, so the COM tab/pane surface may not be ready).
    if ($ShowFre) {
        try { Resolve-WtComClsid -App $app -TimeoutSec ([Math]::Min($TimeoutSec, 15)) | Out-Null }
        catch { Write-ItLog -Level WARN -Message "COM not resolved during FRE (expected): $_" }
    }
    else {
        Resolve-WtComClsid -App $app -TimeoutSec $TimeoutSec | Out-Null
    }

    # Resolve the window HWND for this pid (for winapp ui targeting).
    $hwnd = Wait-Until -TimeoutSec 20 -IntervalSec 1 -Quiet -Because "WT window HWND" -Condition {
        $w = Get-WtWindowHwnds -App $app | Where-Object { [int]$_.pid -eq [int]$app.Pid } | Select-Object -First 1
        if ($w) { $w.hwnd } else { $null }
    }
    if ($hwnd) { $app.Hwnd = $hwnd; Write-ItLog -Level INFO -Message "WT window hwnd=$hwnd" }
    else { Write-ItLog -Level WARN -Message "Could not resolve WT HWND; UI primitives will fall back to -a pid." }

    Initialize-LogOffsets -App $app | Out-Null
    $app
}

Set-Alias -Name Start-TerminalClean -Value Start-Terminal

function Stop-Terminal {
    <#
    .SYNOPSIS
        Close the terminal and (by default) restore the backed-up config.
    .DESCRIPTION
        Closes GRACEFULLY first (CloseMainWindow), giving WindowEmperor time to deregister
        its single-instance/COM-protocol server cleanly. Only force-kills as a fallback after
        -GraceSec. Graceful close is preferred so the COM monarch handoff between runs is
        clean; force-kill is a last resort for an unresponsive window.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [bool]$RestoreSettings = $true,
        [int]$GraceSec = 8
    )
    process {
        # Only tear down processes WE launched. If Start-Terminal attached to a pre-existing
        # WindowsTerminal (single-instance), leave it (and its wta) alone.
        if ($App.PSObject.Properties.Name -contains 'Launched' -and -not $App.Launched) {
            Write-ItLog -Level WARN -Message "Stop-Terminal: not killing pre-existing WindowsTerminal (pid=$($App.Pid))."
            if ($RestoreSettings) { Restore-WtConfig -App $App }
            return
        }
        # Collect OUR wta descendants before WT exits (parent links vanish afterwards).
        $wtaIds = if ($App.Pid) { @(Get-DescendantWtaIds -RootPid ([int]$App.Pid)) } else { @() }

        $forced = $false
        if ($App.Pid) {
            $proc = Get-Process -Id $App.Pid -ErrorAction SilentlyContinue
            if ($proc) {
                # 1) Graceful close: post WM_CLOSE to the main window so WindowEmperor runs
                #    its normal shutdown (deregisters COM monarch / protocol server cleanly).
                $closed = $false
                try { $closed = $proc.CloseMainWindow() } catch { }
                if ($closed -or $proc.MainWindowHandle -eq 0) {
                    $closed = Test-Until -TimeoutSec $GraceSec -IntervalSec 0.5 -Condition {
                        $null -eq (Get-Process -Id $App.Pid -ErrorAction SilentlyContinue)
                    }
                }
                # 2) Fallback: force-kill only if it did not exit gracefully in time.
                if (-not (Get-Process -Id $App.Pid -ErrorAction SilentlyContinue)) {
                    Write-ItLog -Level INFO -Message "Terminal closed gracefully (pid=$($App.Pid))."
                }
                else {
                    Write-ItLog -Level WARN -Message "Graceful close timed out after ${GraceSec}s; force-killing pid=$($App.Pid)."
                    Stop-Process -Id $App.Pid -Force -ErrorAction SilentlyContinue
                    $forced = $true
                }
            }
        }

        # Reap any of OUR wta helpers/master still alive (they normally exit with their helper
        # conpty once WT closes; force only the stragglers, never every wta on the machine).
        $alive = @($wtaIds | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
        if ($alive.Count) { Stop-Process -Id $alive -Force -ErrorAction SilentlyContinue }

        if ($RestoreSettings) { Restore-WtConfig -App $App }
        Write-ItLog -Level INFO -Message "Terminal stopped (pid=$($App.Pid), graceful=$(-not $forced), wta reaped=$($alive.Count))."
    }
}

function Start-TerminalFre {
    <#
    .SYNOPSIS
        Launch with the agent FRE overlay SHOWING so the FRE flow can be driven via UIA.
        Forces a COLD start (kills any running monarch) because a running monarch caches
        ApplicationState and would otherwise just open a normal tab instead of the overlay.
        Backs up config for restore on Stop-Terminal.
    #>
    [CmdletBinding()]
    param([string]$Package = (Get-ItTestPackage), [int]$TimeoutSec = 60)
    return (Start-Terminal -Package $Package -ShowFre -Backup $true -TimeoutSec $TimeoutSec)
}

function Reset-TerminalState {
    <#
    .SYNOPSIS
        Apply a clean baseline to a (running or not) app: optional minimal settings.json,
        FRE state. Use -Replace to overwrite settings.json with a minimal schema-only file.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [hashtable]$Settings,
        [bool]$PassFre = $true,
        [switch]$Replace
    )
    process {
        if ($Replace) {
            $minimal = [pscustomobject]@{ '$schema' = 'https://aka.ms/terminal-profiles-schema' }
            Set-Content -LiteralPath $App.SettingsPath -Value ($minimal | ConvertTo-Json) -Encoding utf8
        }
        if ($PassFre) { Invoke-FrePass -App $App | Out-Null } else { Reset-Fre -App $App | Out-Null }
        if ($Settings) { Set-WtSettings -App $App -Settings $Settings | Out-Null }
        $App
    }
}
