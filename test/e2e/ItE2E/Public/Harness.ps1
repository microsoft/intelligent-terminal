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

function Start-Terminal {
    <#
    .SYNOPSIS
        Resolve, (optionally) configure, launch, and attach to a deployed Intelligent
        Terminal. Returns the app context object used by every primitive.
    .PARAMETER Package   Auto|Store|Dev|<PackageFamilyName>
    .PARAMETER Settings  Hashtable of top-level settings.json keys to apply.
    .PARAMETER PassFre   Mark the agent FRE complete before launch (default $true).
    .PARAMETER Backup    Back up settings/state for restore on Stop-Terminal (default $true).
    #>
    [CmdletBinding()]
    param(
        [string]$Package = 'Auto',
        [hashtable]$Settings,
        [bool]$PassFre = $true,
        [bool]$Backup = $true,
        [int]$TimeoutSec = 60
    )
    $app = Resolve-ItApp -Package $Package
    Write-ItLog -Level INFO -Message "Resolved package $($app.Package) v$($app.Version); wtcli=$($app.WtcliPath)"

    # Per-run framework log file under TEMP.
    $script:ItE2ELogFile = Join-Path $env:TEMP ("ite2e-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

    if ($Backup) { Backup-WtConfig -App $app }
    if ($PassFre) { Invoke-FrePass -App $app | Out-Null }
    if ($Settings) { Set-WtSettings -App $app -Settings $Settings | Out-Null }

    $existing = @(Get-WtProcessesForApp -App $app | Select-Object -ExpandProperty Id)
    Write-ItLog -Level INFO -Message "Launching $($app.AppUserModelId)"
    Start-Process -FilePath 'explorer.exe' -ArgumentList "shell:AppsFolder\$($app.AppUserModelId)" | Out-Null

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

    # Bring COM online and resolve the brand CLSID.
    Resolve-WtComClsid -App $app -TimeoutSec $TimeoutSec | Out-Null

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
    <# Close the terminal and (by default) restore the backed-up config. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [bool]$RestoreSettings = $true)
    process {
        # Only tear down processes WE launched. If Start-Terminal attached to a pre-existing
        # WindowsTerminal (single-instance), leave it (and its wta) alone.
        if ($App.PSObject.Properties.Name -contains 'Launched' -and -not $App.Launched) {
            Write-ItLog -Level WARN -Message "Stop-Terminal: not killing pre-existing WindowsTerminal (pid=$($App.Pid))."
            if ($RestoreSettings) { Restore-WtConfig -App $App }
            return
        }
        # Collect OUR wta descendants before killing WT (parent links vanish afterwards).
        $wtaIds = if ($App.Pid) { @(Get-DescendantWtaIds -RootPid ([int]$App.Pid)) } else { @() }
        try {
            if ($App.Pid) { Stop-Process -Id $App.Pid -Force -ErrorAction SilentlyContinue }
        }
        catch { Write-ItLog -Level WARN -Message "Stop-Process failed: $_" }
        # Kill only the wta helpers/master spawned by THIS run (not every wta on the machine).
        if ($wtaIds.Count) { Stop-Process -Id $wtaIds -Force -ErrorAction SilentlyContinue }
        if ($RestoreSettings) { Restore-WtConfig -App $App }
        Write-ItLog -Level INFO -Message "Terminal stopped (pid=$($App.Pid), wta killed=$($wtaIds.Count))."
    }
}

function Start-TerminalFre {
    <#
    .SYNOPSIS
        Launch with the agent FRE overlay SHOWING (resets agentFreCompleted first) so the
        FRE flow can be driven via UIA. Backs up config for restore on Stop-Terminal.
    #>
    [CmdletBinding()]
    param([string]$Package = 'Store', [int]$TimeoutSec = 60)
    $app = Resolve-ItApp -Package $Package
    $script:ItE2ELogFile = Join-Path $env:TEMP ("ite2e-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
    Backup-WtConfig -App $app
    Reset-Fre -App $app | Out-Null   # force the FRE to show
    return (Start-Terminal -Package $Package -PassFre $false -Backup $false -TimeoutSec $TimeoutSec)
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
