# Harness.ps1 — lifecycle: resolve, (safely) configure, launch, attach, teardown.
# Non-destructive by default: settings.json/state.json are backed up and restored.

function Test-ItProcessDeadline {
    param([datetimeoffset]$DeadlineUtc, [double]$ElapsedSeconds, [double]$TimeoutSeconds,
        [datetimeoffset]$NowUtc = [datetimeoffset]::UtcNow)
    $NowUtc -ge $DeadlineUtc -or $ElapsedSeconds -ge $TimeoutSeconds
}

function Wait-ItProcessDeadline {
    param([Parameter(Mandatory)]$Process, [ValidateRange(1, 86400)][int]$TimeoutSec,
        [datetimeoffset]$StartedUtc = [datetimeoffset]::UtcNow, [string]$PrerequisiteFailurePath)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $deadline = $StartedUtc.AddSeconds($TimeoutSec)
    while ($true) {
        if ($PrerequisiteFailurePath -and (Test-Path -LiteralPath $PrerequisiteFailurePath)) { return $false }
        # Windows handle waits exclude system sleep; check UTC BEFORE accepting an exit.
        if (Test-ItProcessDeadline $deadline $clock.Elapsed.TotalSeconds $TimeoutSec) { return $false }
        if ($Process.HasExited) { return $true }
        [void]$Process.WaitForExit(500)
    }
}

function Backup-WtConfig {
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    foreach ($f in @($App.SettingsPath, $App.StatePath)) {
        $bak = "$f.e2ebak"
        $missing = "$bak.missing"
        # A leftover backup or missing-file marker means a prior run crashed before
        # restoring. Recover first so we snapshot the real pre-test state.
        if (Test-Path $bak) {
            Copy-Item -LiteralPath $bak -Destination $f -Force
            Remove-Item -LiteralPath $bak -Force
            Remove-Item -LiteralPath $missing -Force -ErrorAction SilentlyContinue
            Write-ItLog -Level WARN -Message "Recovered stale backup for $f (prior run did not clean up)"
        }
        elseif (Test-Path $missing) {
            Remove-Item -LiteralPath $f -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $missing -Force
            Write-ItLog -Level WARN -Message "Recovered stale missing-file marker for $f (prior run did not clean up)"
        }
        if (Test-Path $f) {
            Copy-Item -LiteralPath $f -Destination $bak -Force
            Write-ItLog -Level INFO -Message "Backed up $f"
        }
        else {
            $parent = Split-Path $missing -Parent
            if (-not (Test-Path $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
            [System.IO.File]::WriteAllBytes($missing, [byte[]]::new(0))
            Write-ItLog -Level INFO -Message "Recorded that $f did not exist before the test"
        }
    }
}

function Get-DescendantWtaIds {
    <# wta.exe PIDs that are descendants of the given WindowsTerminal pid (master spawned by
       SharedWta, helpers as conpty children). AsProcess also captures their wtcli listeners,
       which can outlive a killed WTA parent. Only these belong to this test run. #>
    [CmdletBinding()] param([Parameter(Mandatory)][int]$RootPid, [switch]$AsProcess, [datetime]$RootStartTime, $RootProcess)
    $all = Get-CimInstance Win32_Process -ErrorAction Stop
    if (-not $all -and -not $AsProcess) { return @() }
    $root = $all | Where-Object ProcessId -eq $RootPid | Select-Object -First 1
    if ($AsProcess -and -not $root) {
        if ($RootProcess -and $RootProcess.Id -eq $RootPid -and $RootProcess.HasExited) { return @() }
        throw 'Terminal process disappeared during descendant discovery without a confirmed owned exit.'
    }
    # CIM's DMTF timestamp has microsecond precision; Process.StartTime has 100ns ticks.
    if ($AsProcess) {
        if (-not $PSBoundParameters.ContainsKey('RootStartTime')) {
            throw 'Creation-proven descendant discovery requires RootStartTime.'
        }
        $rootTicks = $RootStartTime.ToUniversalTime().Ticks
        if ($root.CreationDate.ToUniversalTime().Ticks -ne ($rootTicks - $rootTicks % 10)) {
            throw 'Terminal process identity changed during descendant discovery.'
        }
    }
    $byParent = @{}
    foreach ($p in $all) { $byParent[[int]$p.ParentProcessId] += @($p) }
    # BFS from the WT root to collect all descendant PIDs.
    $descendants = [System.Collections.Generic.HashSet[int]]::new()
    $queue = [System.Collections.Generic.Queue[int]]::new(); $queue.Enqueue($RootPid)
    while ($queue.Count) {
        $cur = $queue.Dequeue()
        $parent = $all | Where-Object ProcessId -eq $cur | Select-Object -First 1
        foreach ($child in $byParent[$cur]) {
            if ($AsProcess -and $child.CreationDate -lt $parent.CreationDate) { continue }
            $cpid = [int]$child.ProcessId
            if ($descendants.Add($cpid)) { $queue.Enqueue($cpid) }
        }
    }
    $owned = @($all | Where-Object {
        if (-not $descendants.Contains([int]$_.ProcessId)) { return $false }
        if ($_.Name -ieq 'wta.exe') { return $true }
        if (-not $AsProcess -or $_.Name -ine 'wtcli.exe') { return $false }
        $parentId = [int]$_.ParentProcessId
        $parentWta = $all | Where-Object { $_.ProcessId -eq $parentId -and $_.Name -ieq 'wta.exe' } | Select-Object -First 1
        $parentWta -and $_.CommandLine -match "(?:^|\s)listen\s.*--parent-pid\s+$parentId(?:\s|$)"
    } | Sort-Object { if ($_.Name -ieq 'wta.exe') { 0 } else { 1 } })
    foreach ($child in $owned) {
        if (-not $AsProcess) { $child.ProcessId; continue }
        try { $process = Get-Process -Id $child.ProcessId -ErrorAction Stop }
        catch {
            if ($_.FullyQualifiedErrorId -like 'NoProcessFoundForGivenId,*') { continue }
            throw
        }
        # Pin the process handle before checking the snapshot identity or sending a kill.
        try {
            $null = $process.Handle
            if ($process.HasExited) { continue }
            $ticks = $process.StartTime.ToUniversalTime().Ticks
            if (($ticks - $ticks % 10) -ne $child.CreationDate.ToUniversalTime().Ticks -or $process.Path -ne $child.ExecutablePath) {
                throw "WTA process identity changed during discovery (pid=$($child.ProcessId))."
            }
        }
        catch {
            if ($_.Exception -is [InvalidOperationException] -and $process.HasExited) { continue }
            throw
        }
        $process
    }
}

function Restore-WtConfig {
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    foreach ($f in @($App.SettingsPath, $App.StatePath)) {
        $bak = "$f.e2ebak"
        $missing = "$bak.missing"
        if (Test-Path $bak) {
            Copy-Item -LiteralPath $bak -Destination $f -Force
            Remove-Item -LiteralPath $bak -Force
            Remove-Item -LiteralPath $missing -Force -ErrorAction SilentlyContinue
            Write-ItLog -Level INFO -Message "Restored $f"
        }
        elseif (Test-Path $missing) {
            Remove-Item -LiteralPath $f -Force -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $missing -Force
            Write-ItLog -Level INFO -Message "Removed test-created $f"
        }
    }
}

function Clear-WtConfig {
    <#
    .SYNOPSIS
        Strip agent/AI keys from settings.json so NO stale provider-specific value leaks into a
        test that only patches a subset of keys.
    .DESCRIPTION
        Tests apply settings by PATCHING individual keys (Set-WtSetting), layering on top of the
        existing settings.json. If the user's real file carries provider-specific keys — e.g.
        `acpModel`/`acpBaseUrl` pointing at a Foundry-local model that is only valid for
        `acpAgent: native` — a test that merely flips `acpAgent` to `copilot` would launch
        `copilot --acp --stdio --model <foundry>`, which the Copilot CLI rejects ("Invalid model
        …") and the agent handshake dies. Starting every test from an agent-clean config makes the
        launch deterministic and provider-pure.

        We REMOVE every agent/AI top-level key (acp*, delegate*, agentPane*, autoFix*,
        showToken*, aiIntegration*) while PRESERVING the rest of the file (profiles, theme, keybindings). A
        full schema-only wipe is deliberately NOT used: WindowsTerminal rejects a settings.json
        with no usable profile and pops a "Failed to load settings" dialog that would destabilize
        UI tests. When called via Start-Terminal (the normal path) the user's real settings are
        preserved in `.e2ebak` (Backup-WtConfig) and restored by Stop-Terminal, so this stays
        non-destructive across a run. If called standalone WITHOUT a prior backup, there is no
        `.e2ebak` to restore from — the caller owns preserving the original.
    #>
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    $obj = Get-WtSettingsObject -App $App
    if (-not $obj) { Write-ItLog -Level INFO -Message "Clear-WtConfig: no parseable settings.json to clean"; return }
    $agentKeys = @($obj.PSObject.Properties.Name | Where-Object { $_ -match '^(acp|delegate|agentPane|autoFix|showToken|aiIntegration)' })
    foreach ($k in $agentKeys) { $obj.PSObject.Properties.Remove($k) }
    ($obj | ConvertTo-Json -Depth 64) | Set-Content -LiteralPath $App.SettingsPath -Encoding utf8
    $preserved = if (Test-Path "$($App.SettingsPath).e2ebak") { "; original preserved in .e2ebak" } else { "" }
    Write-ItLog -Level INFO -Message "Cleared agent/AI keys from settings.json (removed: $($agentKeys -join ', '))$preserved"
}

function Get-WtProcessesForApp {
    [CmdletBinding()] param([Parameter(Mandatory)]$App, [switch]$IncludePackageExecutables)
    $loc = $App.InstallLocation
    if ($IncludePackageExecutables) {
        if (-not $loc) { throw 'Package-wide process discovery requires an installation directory.' }
        $root = [IO.Path]::GetFullPath([string]$loc).TrimEnd('\') + '\'
        $executableNames = @(Get-ChildItem -LiteralPath $loc -Filter '*.exe' -File -Recurse -ErrorAction Stop |
            Select-Object -ExpandProperty BaseName -Unique)
        if (-not $executableNames.Count) { throw 'Cannot establish package inactivity: no package executables were discoverable.' }
        # Package-wide preflight includes helpers; terminal-only queries remain below.
        foreach ($process in @(Get-Process -ErrorAction Stop)) {
            $path = $process.Path
            if (-not $path) {
                if ($process.HasExited) { continue }
                if (-not $process.ProcessName -or $process.ProcessName -in $executableNames) {
                    try { $path = Get-ItProcessImagePath -Id $process.Id }
                    catch {
                        if ($process.HasExited) { continue }
                        throw "Cannot establish package inactivity: executable path unavailable for pid=$($process.Id) ($($process.ProcessName))."
                    }
                    if (-not $path) { throw "Cannot establish package inactivity: empty executable path for pid=$($process.Id)." }
                } else {
                    continue
                }
            }
            if ([IO.Path]::GetFullPath($path).StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { $process }
        }
        return
    }
    Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue | Where-Object {
        try { $loc -and $_.Path -and $_.Path.StartsWith($loc, [StringComparison]::OrdinalIgnoreCase) } catch { $false }
    }
}

function Stop-AppInstances {
    <#
    .SYNOPSIS
        Prepare the selected package for a cold test launch.
    .DESCRIPTION
        Only the exact Dev package may be closed automatically. Other packages remain
        protected; Stop-Terminal still requires captured creation ownership at teardown.
    #>
    [CmdletBinding()] param([Parameter(Mandatory)]$App, [int]$GraceSec = 6)
    Stop-StaleItInstances -App $App -GraceSec $GraceSec
}

function Assert-WtPackageInactive {
    param([Parameter(Mandatory)]$App)
    if (@(Get-WtProcessesForApp -App $App -IncludePackageExecutables).Count) {
        throw 'Refusing cold start: pre-existing or unknown package processes are not test-owned.'
    }
}

function Assert-ItDevProcessesNotChat {
    param([Parameter(Mandatory)][object[]]$Processes)
    $ancestors = [Collections.Generic.HashSet[int]]::new()
    $oldestObserved = $null
    $childStart = $null
    $id = [int]$PID
    while ($id -gt 0) {
        if (-not $ancestors.Add($id)) {
            throw "Cannot establish current chat ancestry: process cycle at pid=$id."
        }
        $current = Get-CimInstance Win32_Process -Filter "ProcessId=$id" -ErrorAction Stop
        if (-not $current) {
            if (-not $oldestObserved) {
                throw "Cannot establish current chat ancestry for pid=$id."
            }
            foreach ($process in $Processes) {
                if (-not $process.StartTime -or
                    $process.StartTime.ToUniversalTime() -le $oldestObserved) {
                    throw "Cannot establish current chat ancestry for pid=${id}: Dev may predate an observed ancestor."
                }
            }
            break
        }
        if ([int]$current.ProcessId -ne $id -or -not $current.CreationDate) {
            throw "Cannot establish current chat ancestry: missing or changed process identity at pid=$id."
        }
        $start = $current.CreationDate.ToUniversalTime()
        if ($childStart -and $start -gt $childStart) {
            throw "Cannot establish current chat ancestry: parent pid=$id was reused after its child started."
        }
        $childStart = $start
        if (-not $oldestObserved -or $start -lt $oldestObserved) { $oldestObserved = $start }
        $parent = [int]$current.ParentProcessId
        if ($parent -eq $id) {
            throw "Cannot establish current chat ancestry: self-parented pid=$id."
        }
        if ($parent -le 0) { break }
        $id = $parent
    }
    foreach ($process in $Processes) {
        if ($ancestors.Contains([int]$process.Id)) {
            throw "Refusing to close the current chat process tree (pid=$($process.Id))."
        }
    }
}

function Test-ItDevPackageQuietWindow {
    param([Parameter(Mandatory)]$App)
    foreach ($sample in 1..5) {
        Start-Sleep -Milliseconds 200
        if (@(Get-WtProcessesForApp -App $App -IncludePackageExecutables).Count) { return $false }
    }
    $true
}

function Stop-ItDevPackageProcesses {
    param([Parameter(Mandatory)]$App, [int]$GraceSec = 6)
    $root = [IO.Path]::GetFullPath([string]$App.InstallLocation).TrimEnd('\') + '\'
    for ($pass = 1; $pass -le 3; $pass++) {
        $processes = @(Get-WtProcessesForApp -App $App -IncludePackageExecutables)
        if (-not $processes.Count) {
            if (Test-ItDevPackageQuietWindow -App $App) { return }
            continue
        }
        $separator = ([string]$App.Package).LastIndexOf('_')
        if ($separator -lt 1 -or -not $App.PackageFullName -or
            $App.PackageFullName -cnotlike "$($App.Package.Substring(0, $separator))_*__rd9vj3e6a2mbr") {
            throw 'The registered Dev package identity is missing or unexpected.'
        }
        Assert-ItDevProcessesNotChat -Processes $processes

        $targets = @(
            foreach ($process in $processes) {
                if ($process.HasExited) { continue }
                $null = $process.Handle
                $path = if ($process.Path) { [string]$process.Path }
                    else { Get-ItProcessImagePath -Id $process.Id }
                if (-not $path) {
                    throw "Dev process executable path unavailable (pid=$($process.Id))."
                }
                $path = [IO.Path]::GetFullPath($path)
                if (-not $path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase) -or
                    (Get-ItCreatedProcessPackage -Process $process) -cne $App.PackageFullName) {
                    throw "Dev process package identity or path does not match the registered Dev package (pid=$($process.Id))."
                }
                [pscustomobject]@{
                    Process = $process
                    Id = [int]$process.Id
                    Path = $path
                    StartTime = $process.StartTime.ToUniversalTime()
                }
            }
        )

        foreach ($target in $targets) {
            $process = $target.Process
            if ($process.HasExited -or $process.ProcessName -ne 'WindowsTerminal') { continue }
            try { $null = $process.CloseMainWindow() }
            catch { if (-not $process.HasExited) { throw } }
        }
        if ($targets.Count -and -not (Test-Until -TimeoutSec $GraceSec -IntervalSec 0.2 -Condition {
            -not @($targets | Where-Object { -not $_.Process.HasExited }).Count
        })) {
            foreach ($target in $targets) {
                if ($target.Process.HasExited) { continue }
                Assert-ItDevProcessesNotChat -Processes @($target.Process)
                try { $live = Get-Process -Id $target.Id -ErrorAction Stop }
                catch {
                    if ($_.FullyQualifiedErrorId -like 'NoProcessFoundForGivenId,*' -and
                        $target.Process.HasExited) { continue }
                    throw
                }
                $null = $live.Handle
                # The retained handle prevents Windows from reusing this PID before shutdown.
                $livePath = if ($live.Path) { [string]$live.Path }
                    else { Get-ItProcessImagePath -Id $target.Id }
                if (-not $livePath) {
                    throw "Dev process executable path unavailable before shutdown (pid=$($target.Id))."
                }
                $livePath = [IO.Path]::GetFullPath($livePath)
                if ($live.HasExited -or
                    $live.StartTime.ToUniversalTime() -ne $target.StartTime -or
                    -not $livePath.Equals($target.Path, [StringComparison]::OrdinalIgnoreCase) -or
                    (Get-ItCreatedProcessPackage -Process $live) -cne $App.PackageFullName) {
                    throw "Dev process identity changed before shutdown (pid=$($target.Id))."
                }
                try { Stop-Process -Id $target.Id -Force -ErrorAction Stop }
                catch {
                    if ($_.FullyQualifiedErrorId -like 'NoProcessFoundForGivenId,*' -and
                        $target.Process.HasExited) { continue }
                    throw
                }
            }
        }
        if (Test-ItDevPackageQuietWindow -App $App) { return }
    }
    throw 'Dev package is still active after three verified cleanup passes.'
}

function Stop-StaleItInstances {
    <#
    .SYNOPSIS
        Close exact Dev package processes while protecting other package families.
    .DESCRIPTION
        The Dev family has explicit test-cleanup permission, except for the current
        chat ancestry. Store and other families may not be closed implicitly.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$App,
        [int]$GraceSec = 6
    )
    if ($App.Package -ceq $script:ItKnownFamilies.Dev) {
        Stop-ItDevPackageProcesses -App $App -GraceSec $GraceSec
    }
    elseif (@(Get-WtProcessesForApp -App $App -IncludePackageExecutables).Count) {
        throw 'Refusing cold start: protected package processes are not test-owned.'
    }
}

function Invoke-ItTerminalActivation {
    param([Parameter(Mandatory)][string]$AppUserModelId)
    if (-not ('ItE2EActivation.Native' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace ItE2EActivation {
 [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
 interface IActivation {
  [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string id,
   [MarshalAs(UnmanagedType.LPWStr)] string args, uint options, out uint pid);
 }
 public static class Native {
  public static uint Activate(string id) {
   var instance = Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("45ba127d-10a8-46ea-8ab7-56ea9078943c"), true));
   try { uint pid; Marshal.ThrowExceptionForHR(((IActivation)instance).ActivateApplication(id, "", 0, out pid)); return pid; }
   finally { Marshal.FinalReleaseComObject(instance); }
  }
 }
}
'@
    }
    [ItE2EActivation.Native]::Activate($AppUserModelId)
}

function Initialize-ItCreatedPackageNative {
    if (-not ('ItE2ECreatedPackage.Native' -as [type])) {
        Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
namespace ItE2ECreatedPackage {
 public static class Native {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode)]
  static extern int GetPackageFullName(IntPtr process, ref uint length, StringBuilder name);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder name, ref uint length);
  [DllImport("kernel32.dll", SetLastError=true)]
  static extern IntPtr OpenProcess(uint access, bool inherit, uint processId);
  [DllImport("kernel32.dll")]
  static extern bool CloseHandle(IntPtr process);
  public static string ImageNameForId(uint processId) {
   var process = OpenProcess(0x1000, false, processId);
   if(process == IntPtr.Zero) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
   try { return ImageName(process); }
   finally { CloseHandle(process); }
  }
  public static string ImageName(IntPtr process) {
   uint length = 32768; var name = new StringBuilder((int)length);
   if(!QueryFullProcessImageName(process, 0, name, ref length))
    throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
   return name.ToString();
  }
  public static string FullName(IntPtr process) {
   uint length = 0;
   int error = GetPackageFullName(process, ref length, null);
   if(error != 122) throw new System.ComponentModel.Win32Exception(error);
   var name = new StringBuilder((int)length);
   error = GetPackageFullName(process, ref length, name);
   if(error != 0) throw new System.ComponentModel.Win32Exception(error);
   return name.ToString();
  }
 }
}
'@
    }
}

function Get-ItProcessImagePath {
    param([Parameter(Mandatory)][int]$Id)
    Initialize-ItCreatedPackageNative
    [ItE2ECreatedPackage.Native]::ImageNameForId([uint32]$Id)
}

function Get-ItCreatedProcessPackage {
    param([Parameter(Mandatory)]$Process)
    Initialize-ItCreatedPackageNative
    [ItE2ECreatedPackage.Native]::FullName($Process.Handle)
}

function Start-ItCreatedDevTerminal {
    param([Parameter(Mandatory)]$App, [int]$TimeoutSec = 60)
    if ($App.Package -cne 'IntelligentTerminal_rd9vj3e6a2mbr') {
        throw 'This creation path is restricted to the explicitly registered Dev PFN.'
    }
    $alias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\$($App.Package)\wtai.exe"
    if (-not (Test-Path -LiteralPath $alias -PathType Leaf)) {
        throw "Registered per-PFN Dev alias is unavailable; recover registration first: $alias"
    }
    Assert-WtPackageInactive -App $App
    $start = [Diagnostics.ProcessStartInfo]::new($alias)
    $start.UseShellExecute = $false
    # This retained object comes from CreateProcess, not an activation-observed PID.
    $launcher = [Diagnostics.Process]::Start($start)
    $App | Add-Member OwnedLauncherProcess $launcher -Force
    $ledgerRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Split-Path $script:ItE2ELogFile -Parent }
    $ledger = Join-Path $ledgerRoot ("created-launch-$($launcher.Id)-$([guid]::NewGuid().ToString('N')).jsonl")
    $App | Add-Member CreationObservationPath $ledger -Force
    $observe = {
        param([string]$Phase, [hashtable]$Details)
        @{ utc = [datetimeoffset]::UtcNow.ToString('o'); phase = $Phase; created_pid = $launcher.Id
            source = 'Retained Process.Start creation; phase observations are not an ownership lease'
            alias = $alias; expected_package = $App.PackageFullName; details = $Details } |
            ConvertTo-Json -Depth 6 -Compress | Add-Content -LiteralPath $ledger -ErrorAction Stop
    }
    & $observe 'created-api-return' @{}
    $launcherHandle = $launcher.Handle
    $launcherStartUtc = $launcher.StartTime.ToUniversalTime()
    & $observe 'retained-creator-before-package-query' @{
        native_handle = $launcherHandle.ToInt64(); start_utc = $launcherStartUtc.ToString('o')
        has_exited = $launcher.HasExited
    }
    $launcherPackage = Get-ItCreatedProcessPackage -Process $launcher
    & $observe 'creator-package-query-result' @{
        start_utc = $launcherStartUtc.ToString('o'); actual_package = $launcherPackage
        exact_package_match = ($launcherPackage -ceq $App.PackageFullName); has_exited = $launcher.HasExited
    }
    if ($launcherPackage -cne $App.PackageFullName) {
        throw 'The actually created alias process has the wrong package identity.'
    }
    $terminalPath = Join-Path $App.InstallLocation 'WindowsTerminal.exe'
    $launcherImage = $null
    $exitedCreator = $launcher.HasExited
    & $observe 'creator-before-image-query' @{
        start_utc = $launcherStartUtc.ToString('o'); native_handle = $launcherHandle.ToInt64()
        actual_package = $launcherPackage; has_exited = $exitedCreator
    }
    if (-not $exitedCreator) {
        try { $launcherImage = [ItE2ECreatedPackage.Native]::ImageName($launcherHandle) }
        catch {
            $failure = $_
            $native = $failure.Exception
            while ($native.InnerException) { $native = $native.InnerException }
            $exitedCreator = $launcher.HasExited
            & $observe 'creator-image-query-failure' @{
                start_utc = $launcherStartUtc.ToString('o'); actual_package = $launcherPackage
                exception_type = $native.GetType().FullName; hresult = $native.HResult
                native_code = $(if ($native -is [ComponentModel.Win32Exception]) { $native.NativeErrorCode } else { $null })
                freshly_has_exited = $exitedCreator
            }
            if ($native -isnot [ComponentModel.Win32Exception] -or
                $native.NativeErrorCode -ne 31 -or -not $exitedCreator) { throw $failure }
        }
    }
    if ($launcherImage -ceq $terminalPath) { return $launcher }
    if (-not $exitedCreator -and $launcherImage -cne (Join-Path $App.InstallLocation 'wtai.exe')) {
        throw 'Created alias resolved to an undocumented executable; no resident adoption.'
    }
    & $observe 'creator-child-discovery' @{
        start_utc = $launcherStartUtc.ToString('o'); actual_package = $launcherPackage
        image = $launcherImage; has_exited = $exitedCreator
        exit_utc = $(if ($exitedCreator) { $launcher.ExitTime.ToUniversalTime().ToString('o') } else { $null })
    }
    # wt/shim.cpp creates exactly one adjacent WindowsTerminal.exe child and exits.
    # Holding the creator handle prevents its PID from being recycled during discovery.
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ($clock.Elapsed.TotalSeconds -lt $TimeoutSec) {
        $snapshots = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($launcher.Id)" `
            -OperationTimeoutSec 2 -ErrorAction Stop | Where-Object {
            $_.ExecutablePath -ceq $terminalPath -and
                $_.CreationDate.ToUniversalTime() -ge $launcher.StartTime.ToUniversalTime()
        })
        foreach ($snapshot in $snapshots) {
            if ($launcher.HasExited -and $snapshot.CreationDate.ToUniversalTime() -gt $launcher.ExitTime.ToUniversalTime()) { continue }
            $child = Get-Process -Id $snapshot.ProcessId -ErrorAction Stop
            $null = $child.Handle
            $ticks = $child.StartTime.ToUniversalTime().Ticks
            if ($child.HasExited -or $child.Path -cne $terminalPath -or
                ($ticks - $ticks % 10) -ne $snapshot.CreationDate.ToUniversalTime().Ticks -or
                (Get-ItCreatedProcessPackage -Process $child) -cne $App.PackageFullName) {
                throw 'Creator-child identity changed or lost package identity; no adoption.'
            }
            if ($snapshots.Count -ne 1) { throw 'Created launcher has an ambiguous Terminal child set.' }
            & $observe 'validated-terminal-creator-chain' @{
                child_pid = $child.Id; parent_pid = $snapshot.ParentProcessId
                child_start_utc = $child.StartTime.ToUniversalTime().ToString('o')
                child_native_handle = $child.Handle.ToInt64(); child_path = $child.Path
                child_package = $App.PackageFullName
            }
            $App | Add-Member CreatedTerminalParentPid $launcher.Id -Force
            return $child
        }
        Start-Sleep -Milliseconds 100
    }
    throw 'No live Terminal child of the retained created launcher; handoff/resident adoption is refused.'
}

function Get-ItTestPackage {
    <#
    .SYNOPSIS
        Resolve which package selector the feature/self-test suites should launch.
        Requires the ITE2E_PACKAGE env var (Store|Dev|<PackageFamilyName>). Live tests
        must never infer a package because they mutate package state and may send real
        agent requests.
    #>
    [CmdletBinding()]
    param()
    if (-not $env:ITE2E_PACKAGE -or $env:ITE2E_PACKAGE -eq 'Auto') {
        throw "Choose the live integration-test package explicitly: set `$env:ITE2E_PACKAGE = 'Dev' or 'Store' (or an explicit PackageFamilyName). 'Auto' is not allowed."
    }
    return $env:ITE2E_PACKAGE
}

function Start-Terminal {
    <#
    .SYNOPSIS
        Resolve, (optionally) configure, launch, and attach to a deployed Intelligent
        Terminal. Returns the app context object used by every primitive.
    .PARAMETER Package   Store|Dev|<PackageFamilyName>. Auto is rejected for live tests.
    .PARAMETER Settings  Hashtable of top-level settings.json keys to apply.
    .PARAMETER State     Opt-in state fixture applied and verified after owned backup.
                         Defaults remain unchanged, including fresh FRE behavior.
    .PARAMETER PassFre   Mark the agent FRE complete before launch (default $true).
    .PARAMETER Backup    Back up settings/state for restore on Stop-Terminal (default $true).
    .PARAMETER CleanSettings  Strip agent/AI keys from settings.json after backup so the user's
                         real config (e.g. a Foundry acpModel/acpBaseUrl set for acpAgent=native)
                         cannot leak into a test that only patches a subset of keys (default
                         $true; ignored when Backup is $false).
    .PARAMETER ShowFre   Leave the agent FRE overlay SHOWING (writes agentFreCompleted=false).
                         COM resolution is best-effort in this mode. A cold start is required:
                         verified Dev processes close automatically; active non-Dev packages are refused.
    #>
    [CmdletBinding()]
    param(
        [string]$Package = (Get-ItTestPackage),
        [hashtable]$Settings,
        [hashtable]$State,
        [bool]$PassFre = $true,
        [bool]$Backup = $true,
        [bool]$CleanSettings = $true,
        [switch]$ShowFre,
        [int]$TimeoutSec = 60
    )
    if ($Package -eq 'Auto') {
        throw "Choose the live integration-test package explicitly: use -Package Dev, -Package Store, or an explicit PackageFamilyName. 'Auto' is not allowed."
    }
    if ($State -and -not $Backup) { throw 'Explicit startup state requires an owned configuration backup.' }
    $app = Resolve-ItApp -Package $Package
    Write-ItLog -Level INFO -Message "Resolved package $($app.Package) v$($app.Version); wtcli=$($app.WtcliPath)"

    $logRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\..\artifacts' }
    New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
    $script:ItE2ELogFile = Join-Path $logRoot ("ite2e-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))

    # Only the exact Dev family may be closed before changing settings. Other package
    # processes remain protected; an active monarch would intercept the new launch.
    Stop-StaleItInstances -App $app
    Initialize-LogOffsets -App $app | Out-Null
    $preLaunchLogStartOffset = if ($app.LogStartOffset) { $app.LogStartOffset.Clone() } else { @{} }
    $app | Add-Member -NotePropertyName PreLaunchLogStartOffset -NotePropertyValue $preLaunchLogStartOffset -Force

    try {
    if ($Backup) { Backup-WtConfig -App $app }
    $app | Add-Member -NotePropertyName ConfigBackupOwned -NotePropertyValue $Backup -Force
    if ($State) {
        foreach ($key in $State.Keys) {
            Set-WtState -App $app -Key $key -Value $State[$key] | Out-Null
            $actual = Get-WtStateObject -App $app
            if ($actual.PSObject.Properties.Name -notcontains $key -or $actual.$key -cne $State[$key]) {
                throw "Startup state did not persist: $key"
            }
        }
    }
    # Strip agent/AI keys from settings.json so the user's real config (e.g. a Foundry
    # acpModel/acpBaseUrl set for acpAgent=native) cannot leak into a test that only patches a
    # subset of keys. Requires a backup so Stop-Terminal can restore the real settings.
    if ($CleanSettings -and $Backup) { Clear-WtConfig -App $app }
    elseif ($CleanSettings) { Write-ItLog -Level WARN -Message "CleanSettings requested but Backup is off; skipping clean to avoid destroying user settings." }
    if ($ShowFre) { Reset-Fre -App $app | Out-Null }
    elseif ($PassFre) { Invoke-FrePass -App $app | Out-Null }
    if ($Settings) { Set-WtSettings -App $app -Settings $Settings | Out-Null }

    # Snapshot the agent-pane-sessions.jsonl BEFORE launching WT so Get-AgentPaneSession can tell
    # OUR agent pane(s) apart from every pane recorded by prior runs / other windows. The file is
    # shared + append-only under the single-instance monarch and accumulates across all runs, so
    # any pane_session_id already present here is NOT ours. Captured before activation, so the
    # pre-warm agent pane this launch creates is guaranteed to be a NEW id.
    $agentJsonl = Join-Path $app.LocalStateDir 'IntelligentTerminal\agent-pane-sessions.jsonl'
    # Case-insensitive set: pane_session_id GUIDs can vary in casing between producer/serializer, so
    # an ordinal (case-sensitive) HashSet would miss a match and treat a pre-existing pane as new.
    $preIds = New-Object System.Collections.Generic.HashSet[string]([System.StringComparer]::OrdinalIgnoreCase)
    if (Test-Path $agentJsonl) {
        Get-Content -LiteralPath $agentJsonl | Where-Object { $_.Trim() } |
            ForEach-Object { $_ | ConvertFrom-JsonSafe } |
            Where-Object { $_ -and $_.pane_session_id } |
            ForEach-Object { [void]$preIds.Add([string]$_.pane_session_id) }
    }
    $app | Add-Member -NotePropertyName PreExistingAgentPaneIds -NotePropertyValue $preIds -Force
    Write-ItLog -Level INFO -Message "Snapshotted $($preIds.Count) pre-existing agent-pane id(s) before launch."

    Assert-WtPackageInactive -App $app
    $proc = Start-ItCreatedDevTerminal -App $app -TimeoutSec $TimeoutSec
    $app.Pid = $proc.Id
    $null = $proc.Handle
    if ($proc.HasExited -or $proc.Path -cne (Join-Path $app.InstallLocation 'WindowsTerminal.exe')) {
        throw 'Created Terminal exited or changed identity; handoff is refused.'
    }
    $app | Add-Member OwnedProcess $proc -Force
    $app | Add-Member Launched $true -Force
    @{ utc = [datetimeoffset]::UtcNow.ToString('o'); phase = 'owned-root-after-creation-chain-validation'
        source = 'Observation of established native creation proof, not a generated lease'
        root_pid = $proc.Id; root_start_utc = $proc.StartTime.ToUniversalTime().ToString('o')
        root_native_handle = $proc.Handle.ToInt64(); creator_pid = $app.OwnedLauncherProcess.Id } |
        ConvertTo-Json -Compress | Add-Content -LiteralPath $app.CreationObservationPath -ErrorAction Stop
    $app | Add-Member InputRunToken $(if ($env:ITE2E_RUN_TOKEN) { $env:ITE2E_RUN_TOKEN } else { [guid]::NewGuid().ToString('N') }) -Force
    $app | Add-Member InputReceiptPath $(if ($env:ITE2E_OWNED_PROCESS_RECEIPT) { $env:ITE2E_OWNED_PROCESS_RECEIPT } else {
        Join-Path $logRoot ("owned-$($app.InputRunToken).jsonl")
    }) -Force
    # The receipt records already-established native creation/parent proof.
    @{ pid = $proc.Id; path = $proc.Path; start_utc = $proc.StartTime.ToUniversalTime().ToString('o')
        run_token = $app.InputRunToken; creator_pid = $app.OwnedLauncherProcess.Id } |
        ConvertTo-Json -Compress | Add-Content -LiteralPath $app.InputReceiptPath
    Write-ItLog -Level INFO -Message "WindowsTerminal pid=$($app.Pid) launched=$($app.Launched)"

    # Wait until shell activation has created the first real window before probing COM.
    # An immediate COM activation while the process exists but has no HWND races startup
    # and can create a second logical window in the same WindowsTerminal process.
    $hwnd = Wait-Until -TimeoutSec 20 -IntervalSec 1 -Quiet -Because "WT window HWND" -Condition {
        $w = Get-WtWindowHwnds -App $app | Where-Object { [int]$_.pid -eq [int]$app.Pid } | Select-Object -First 1
        if ($w) { $w.hwnd } else { $null }
    }
    if ($hwnd) { $app.Hwnd = $hwnd; Write-ItLog -Level INFO -Message "WT window hwnd=$hwnd" }
    else { throw 'The created process owns no native window; resident handoff/target fallback is refused.' }

    # Bring COM online and resolve the brand CLSID. Best-effort while the FRE overlay is up
    # (the overlay replaces the window content, so the COM tab/pane surface may not be ready).
    if ($ShowFre) {
        try { Resolve-WtComClsid -App $app -TimeoutSec ([Math]::Min($TimeoutSec, 15)) | Out-Null }
        catch { Write-ItLog -Level WARN -Message "COM not resolved during FRE (expected): $_" }
    }
    else {
        Resolve-WtComClsid -App $app -TimeoutSec $TimeoutSec | Out-Null
    }

    # Capture the WT logical window id (informational; agent panes are XAML chrome and never
    # appear in list-panes, so window scoping of the agent pane itself relies on the
    # pre-existing-id snapshot above rather than on this).
    try {
        $active = Get-ActivePane -App $app
        if ($active -and $null -ne $active.window_id) {
            $app | Add-Member -NotePropertyName WindowId -NotePropertyValue ([string]$active.window_id) -Force
            Write-ItLog -Level INFO -Message "WT window_id=$($app.WindowId)"
        }
    }
    catch { Write-ItLog -Level WARN -Message "Could not resolve WT window_id: $_" }

    Initialize-LogOffsets -App $app | Out-Null
    $app
    }
    catch {
        $original = $_
        try {
            if ($app.PSObject.Properties['OwnedLauncherProcess'] -and $app.OwnedLauncherProcess -and
                -not $app.OwnedLauncherProcess.HasExited -and
                (-not $app.PSObject.Properties['OwnedProcess'] -or $app.OwnedProcess -ne $app.OwnedLauncherProcess)) {
                Stop-Process -InputObject $app.OwnedLauncherProcess -Force -ErrorAction Stop
            }
            if ($app.PSObject.Properties['OwnedProcess'] -and $app.OwnedProcess -and $app.Launched) { Stop-Terminal -App $app -RestoreSettings $Backup }
            elseif ($app.PSObject.Properties['ConfigBackupOwned'] -and $app.ConfigBackupOwned) {
                Assert-WtPackageInactive -App $app
                Restore-WtConfig -App $app
            }
        }
        catch { Write-ItLog -Level ERROR -Message "Launch recovery refused or failed; backups retained: $_" }
        throw $original
    }
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
        Stops captured, identity-checked WTA listeners before the COM host. Restoration
        runs in finally only for an owned backup and an affirmatively inactive package;
        live or undiscoverable package processes retain backups and surface an error.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [bool]$RestoreSettings = $true,
        [int]$GraceSec = 8
    )
    process {
        $cleanupError = $null
        try {
            # Only tear down processes WE launched. If Start-Terminal attached to a pre-existing
            # WindowsTerminal (single-instance), leave it (and its wta) alone.
            if ($App.PSObject.Properties.Name -contains 'Launched' -and -not $App.Launched) {
                Write-ItLog -Level WARN -Message "Stop-Terminal: not killing pre-existing WindowsTerminal (pid=$($App.Pid))."
                return
            }
            # Collect OUR wta descendants before WT exits (parent links vanish afterwards).
            $proc = $App.OwnedProcess
            if ($App.Pid -and -not $proc) { throw 'Terminal cleanup requires a captured owned process identity.' }
            if ($proc -and -not $proc.HasExited -and
                ($App.Launched -ne $true -or $proc.Id -ne $App.Pid -or
                 $proc.Path -ne (Join-Path $App.InstallLocation 'WindowsTerminal.exe'))) {
                throw 'Terminal cleanup refuses an unowned or mismatched captured process.'
            }
            if ($proc -and -not $proc.HasExited) {
                if (-not $App.InputRunToken -or -not $App.InputReceiptPath) {
                    throw 'Terminal cleanup requires its captured run/PID/path/start receipt.'
                }
                $records = @(Get-Content -LiteralPath $App.InputReceiptPath -ErrorAction Stop | ForEach-Object { $_ | ConvertFrom-Json })
                if (@($records | Where-Object { $_.pid -eq $App.Pid -and $_.path -eq $proc.Path -and
                    $_.run_token -ceq $App.InputRunToken -and ([datetimeoffset]$_.start_utc).UtcDateTime.Ticks -eq
                        $proc.StartTime.ToUniversalTime().Ticks }).Count -ne 1) {
                    throw 'Terminal cleanup receipt is missing, ambiguous or stale.'
                }
            }
            $wta = if ($proc -and -not $proc.HasExited) {
                @(Get-DescendantWtaIds -RootPid ([int]$App.Pid) -AsProcess -RootStartTime $proc.StartTime -RootProcess $proc)
            } else { @() }
            # Stop owned COM listeners before closing the server; reconnecting during
            # shutdown can activate a replacement headless Terminal.
            foreach ($child in $wta) {
                if ($child.HasExited) { continue }
                try { Stop-Process -InputObject $child -Force -ErrorAction Stop }
                catch {
                    if ($_.FullyQualifiedErrorId -notlike 'NoProcessFoundForGivenId,*' -or -not $child.HasExited) { throw }
                    Write-ItLog -Level INFO -Message "Owned WTA exited during stop (pid=$($child.Id))."
                }
            }
            if ($wta.Count -and -not (Test-Until -TimeoutSec $GraceSec -IntervalSec 0.2 -Condition {
                -not @($wta | Where-Object { -not $_.HasExited }).Count
            })) {
                throw 'Owned WTA listeners remain active; refusing to close the COM host.'
            }

            $forced = $false
            if ($App.Pid) {
                if ($proc -and -not $proc.HasExited) {
                    # 1) Graceful close: post WM_CLOSE to the main window so WindowEmperor runs
                    #    its normal shutdown (deregisters COM monarch / protocol server cleanly).
                    $closed = $false
                    try { $closed = $proc.CloseMainWindow() } catch { if (-not $proc.HasExited) { throw } }
                    if ($closed -or $proc.MainWindowHandle -eq 0) {
                        $closed = Test-Until -TimeoutSec $GraceSec -IntervalSec 0.5 -Condition {
                            $proc.HasExited
                        }
                    }
                    # 2) Fallback: force-kill only if it did not exit gracefully in time.
                    if ($proc.HasExited) {
                        Write-ItLog -Level INFO -Message "Terminal closed gracefully (pid=$($App.Pid))."
                    }
                    else {
                        Write-ItLog -Level WARN -Message "Graceful close timed out after ${GraceSec}s; force-killing pid=$($App.Pid)."
                        try { Stop-Process -InputObject $proc -Force -ErrorAction Stop }
                        catch {
                            if ($_.FullyQualifiedErrorId -notlike 'NoProcessFoundForGivenId,*' -or -not $proc.HasExited) { throw }
                        }
                        $forced = $true
                    }
                }
            }

            Write-ItLog -Level INFO -Message "Terminal stopped (pid=$($App.Pid), graceful=$(-not $forced), owned WTA/listeners reaped=$($wta.Count))."
        }
        catch { $cleanupError = $_; throw }
        finally {
            if ($RestoreSettings -and $App.ConfigBackupOwned) {
                try {
                    if (@(Get-WtProcessesForApp -App $App -IncludePackageExecutables).Count) {
                        throw 'Package remains active; retaining owned configuration backups.'
                    }
                    Restore-WtConfig -App $App
                }
                catch {
                    Write-ItLog -Level ERROR -Message "Configuration restoration refused or failed; backups retained: $_"
                    if (-not $cleanupError) { throw }
                }
            }
        }
    }
}

function Start-TerminalFre {
    <#
    .SYNOPSIS
        Launch with the agent FRE overlay SHOWING so the FRE flow can be driven via UIA.
        Requires a COLD start: exact Dev processes may be closed automatically, while
        an already-running protected package is refused before changing settings.
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
