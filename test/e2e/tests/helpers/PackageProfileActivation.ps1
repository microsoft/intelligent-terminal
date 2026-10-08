function Initialize-TestPackageProfileActivation {
    if ('ItE2E.PackageProfileActivation' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ItE2E {
    [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager {
        [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
            [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
    }
    public static class PackageProfileActivation {
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode)]
        static extern int GetPackageFullName(IntPtr process, ref uint length, System.Text.StringBuilder name);
        public static bool HasIdentity(IntPtr process) {
            uint length=0; int status=GetPackageFullName(process, ref length, null);
            if(status==122) return true;
            if(status==15700) return false;
            throw new COMException("Package identity query failed.",status);
        }
        public static long[] Activate(string aumid, string arguments) {
            object manager=Activator.CreateInstance(Type.GetTypeFromCLSID(new Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")));
            try {
                uint pid; int hr=((IApplicationActivationManager)manager).ActivateApplication(aumid,arguments,0,out pid);
                return new long[] { hr,pid };
            } finally { Marshal.ReleaseComObject(manager); }
        }
    }
}
'@
}

function Assert-TestPackagedProfileOwner {
    param([Parameter(Mandatory)]$App)
    $current = Get-Process -Id $App.Pid -ErrorAction Stop
    if (-not $App.Launched -or -not $App.OwnedProcess -or $App.OwnedProcess.HasExited -or
        $App.OwnedProcess.Id -ne $App.Pid -or $current.StartTime -ne $App.OwnedProcess.StartTime -or
        $current.Path -ne (Join-Path $App.InstallLocation 'WindowsTerminal.exe') -or -not $App.AppUserModelId) {
        throw 'Packaged profile activation requires the original owned application lease.'
    }
    $records = @(Get-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT | ForEach-Object { $_ | ConvertFrom-Json })
    if (-not $env:ITE2E_RUN_TOKEN -or @($records | Where-Object {
        $_.pid -eq $App.Pid -and $_.path -eq $current.Path -and $_.run_token -ceq $env:ITE2E_RUN_TOKEN -and
        ([datetimeoffset]$_.start_utc).UtcDateTime.Ticks -eq $current.StartTime.ToUniversalTime().Ticks
    }).Count -ne 1) { throw 'Packaged profile activation requires its exact original run identity.' }
    Initialize-TestPackageProfileActivation
    if (-not [ItE2E.PackageProfileActivation]::HasIdentity($App.OwnedProcess.Handle)) {
        throw 'Profile activation original host has no package identity.'
    }
}

function Assert-TestPackageProfileActivationResult {
    param([int]$HResult, [long]$ReturnedPid, $App)
    if ($HResult -ne 0) { throw ('Packaged profile activation failed: HRESULT 0x{0:X8}' -f $HResult) }
    # Desktop Terminal may forward activation to its resident packaged host.
    # ReturnedPid is observation only; the caller verifies the actual window owner.
}

function Invoke-TestPackagedProfileActivation {
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Profile,
        [Parameter(Mandatory)][string]$ReceiptPath)
    Assert-TestPackagedProfileOwner $App
    $profileId = [guid]::Parse($Profile)
    $arguments = '-p "' + $profileId.ToString('B') + '"'
    $helperPath = Join-Path $PSScriptRoot 'PackageProfileActivation.ps1'
    $escapedPath = $helperPath.Replace("'", "''")
    $aumid = ([string]$App.AppUserModelId).Replace("'", "''")
    $escapedArgs = $arguments.Replace("'", "''")
    $code = ". '$escapedPath'; Initialize-TestPackageProfileActivation; `$r=[ItE2E.PackageProfileActivation]::Activate('$aumid','$escapedArgs'); @{HResult=`$r[0];ReturnedPid=`$r[1]}|ConvertTo-Json -Compress"
    $start = [Diagnostics.ProcessStartInfo]::new((Get-Command pwsh).Source)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($arg in @('-NoProfile','-EncodedCommand',[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)))) {
        $start.ArgumentList.Add($arg)
    }
    $worker = [Diagnostics.Process]::new()
    $worker.StartInfo = $start
    $started = $false
    try {
        $started = $worker.Start()
        if (-not $started) { throw 'Owned packaged activation worker did not start.' }
        $stdout = $worker.StandardOutput.ReadToEndAsync()
        $stderr = $worker.StandardError.ReadToEndAsync()
        $timedOut = -not $worker.WaitForExit(20000)
        if ($timedOut) { throw 'Packaged profile activation exceeded the original 20-second bound.' }
        $streams = [Threading.Tasks.Task]::WhenAll([Threading.Tasks.Task[]]@($stdout,$stderr))
        if (-not $streams.Wait(2000)) { throw 'Packaged activation worker output did not drain within two seconds.' }
        if ($worker.ExitCode -ne 0) { throw 'Packaged activation worker failed; no fallback route used.' }
        $result = $stdout.Result | ConvertFrom-Json
        if (-not $result -or $result.PSObject.Properties.Name -notcontains 'HResult' -or
            $result.PSObject.Properties.Name -notcontains 'ReturnedPid') { throw 'Packaged activation returned no valid HRESULT/PID record.' }
        @{ HResult = $result.HResult; ReturnedPid = if ([int]$result.HResult -eq 0) { $result.ReturnedPid } else { $null }
            OriginalPid = $App.Pid; ReturnedInstanceOwnershipProven = $false } |
            ConvertTo-Json | Set-Content -LiteralPath $ReceiptPath
        Assert-TestPackageProfileActivationResult -HResult ([int]$result.HResult) -ReturnedPid ([long]$result.ReturnedPid) -App $App
        Assert-TestPackagedProfileOwner $App
        $result
    }
    finally {
        try {
            # Only terminate this retained PowerShell worker, never an activation-returned app.
            if ($started -and -not $worker.HasExited) {
                $worker.Kill()
                if (-not $worker.WaitForExit(2000)) { throw 'Owned activation worker remained after bounded shutdown.' }
            }
        } finally { $worker.Dispose() }
    }
}
