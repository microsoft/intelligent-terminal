# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateSet('fre', 'fre-settings', 'agents')]
    [string]$Surface,

    [Parameter(Mandatory)]
    [string]$ManifestPath,

    [Parameter(Mandatory)]
    [string]$AxePath,

    [Parameter(Mandatory)]
    [string]$OutputDirectory,

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-fA-F]{40}$')]
    [string]$SourceSha,

    [ValidateRange(10, 300)]
    [int]$LaunchTimeoutSeconds = 60,

    [ValidateRange(30, 600)]
    [int]$ScanTimeoutSeconds = 180,

    [switch]$KeepRegistered
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace IntelligentTerminal.Accessibility
{
    [Flags]
    public enum ActivateOptions
    {
        None = 0
    }

    [ComImport]
    [Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager
    {
        int ActivateApplication(
            [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
            [MarshalAs(UnmanagedType.LPWStr)] string arguments,
            ActivateOptions options,
            out uint processId);
    }

    [ComImport]
    [Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
    class ApplicationActivationManager
    {
    }

    public static class PackagedApp
    {
        [DllImport("user32.dll", SetLastError = true)]
        private static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool CloseDesktop(IntPtr desktop);

        public static void VerifyInteractiveDesktop()
        {
            if (!Environment.UserInteractive || System.Diagnostics.Process.GetCurrentProcess().SessionId == 0)
                throw new InvalidOperationException("Native accessibility requires an interactive user session, not session 0.");
            var desktop = OpenInputDesktop(0, false, 1);
            if (desktop == IntPtr.Zero)
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "Cannot access the input desktop.");
            if (!CloseDesktop(desktop))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error(), "Cannot close the input desktop handle.");
        }

        public static uint Activate(string appUserModelId, string arguments)
        {
            var manager = (IApplicationActivationManager)new ApplicationActivationManager();
            var result = manager.ActivateApplication(
                appUserModelId,
                arguments,
                ActivateOptions.None,
                out var processId);
            Marshal.ThrowExceptionForHR(result);
            return processId;
        }
    }
}
'@

function Get-OwnedProcess
{
    param(
        [uint32]$ProcessId,
        [string]$ExpectedExecutable,
        [datetime]$ActivationStarted
    )

    $process = Get-Process -Id $ProcessId -ErrorAction Stop
    try
    {
        # Pin the kernel process handle before inspecting identity or retaining the object.
        $null = $process.Handle
        $started = $process.StartTime.ToUniversalTime()
        $executable = $process.MainModule.FileName
        if ($process.HasExited -or $started -lt $ActivationStarted -or
            -not [string]::Equals($executable, $ExpectedExecutable, [StringComparison]::OrdinalIgnoreCase))
        {
            throw 'Activated process identity does not match the newly launched test host.'
        }
        return [pscustomobject]@{
            Process = $process
            ProcessId = $process.Id
            StartTime = $started
            Executable = $executable
            ExpectedExecutable = $ExpectedExecutable
        }
    }
    catch
    {
        $process.Dispose()
        throw
    }
}

function Stop-OwnedProcess
{
    param([object]$OwnedProcess)

    $evidence = [ordered]@{
        status = 'PASS'
        process_id = $OwnedProcess.ProcessId
        start_time = $OwnedProcess.StartTime
        executable = $OwnedProcess.Executable
        action = 'already-exited'
        reason = ''
    }
    $process = $OwnedProcess.Process
    try
    {
        if (-not $process.HasExited)
        {
            if ($process.Id -ne $OwnedProcess.ProcessId -or
                $process.StartTime.ToUniversalTime() -ne $OwnedProcess.StartTime -or
                -not [string]::Equals($process.MainModule.FileName, $OwnedProcess.Executable, [StringComparison]::OrdinalIgnoreCase) -or
                -not [string]::Equals($OwnedProcess.Executable, $OwnedProcess.ExpectedExecutable, [StringComparison]::OrdinalIgnoreCase))
            {
                throw 'Owned process identity changed; refusing termination.'
            }
            $evidence.action = 'graceful-close'
            $process.CloseMainWindow() | Out-Null
            if (-not $process.WaitForExit(5000))
            {
                $evidence.action = 'force-kill'
                # Kill uses the retained handle, never a fresh lookup of a reusable PID.
                $process.Kill()
                if (-not $process.WaitForExit(5000))
                {
                    throw 'Owned process did not exit after forced termination.'
                }
            }
        }
    }
    catch
    {
        $failure = $_.Exception.Message
        $exited = $false
        try { $exited = $process.HasExited } catch {}
        if ($exited)
        {
            $evidence.action = 'exited-during-cleanup'
        }
        else
        {
            $evidence.status = 'BLOCKED'
            $evidence.reason = $failure
            Write-Warning "Failed to clean up the owned test host process: $failure" -WarningAction Continue
        }
    }
    finally
    {
        try { $process.Dispose() }
        catch
        {
            $evidence.status = 'BLOCKED'
            $evidence.reason = "Failed to release the owned process handle: $($_.Exception.Message)"
            Write-Warning $evidence.reason -WarningAction Continue
        }
    }
    return [pscustomobject]$evidence
}

$resolvedManifest = (Resolve-Path -LiteralPath $ManifestPath).Path
$resolvedAxe = (Resolve-Path -LiteralPath $AxePath).Path
$surfaceOutput = Join-Path $OutputDirectory $Surface
New-Item -ItemType Directory -Force -Path $surfaceOutput | Out-Null
$stdoutPath = Join-Path $surfaceOutput 'axe.stdout.log'
$stderrPath = Join-Path $surfaceOutput 'axe.stderr.log'
$axeResultPath = Join-Path $surfaceOutput 'axe-results.json'
$resultPath = Join-Path $surfaceOutput 'result.json'
$package = $null
$previousPackage = Get-AppxPackage -Name 'WindowsTerminal.TestHost' |
    Sort-Object Version -Descending |
    Select-Object -First 1
$processId = 0
$ownedProcess = $null
$axe = $null
$cleanup = [System.Collections.Generic.List[object]]::new()
$status = 'BLOCKED'
$reason = ''
$axeExitCode = $null

try
{
    if (Test-Path -LiteralPath $axeResultPath)
    {
        Remove-Item -LiteralPath $axeResultPath -Force
    }
    [IntelligentTerminal.Accessibility.PackagedApp]::VerifyInteractiveDesktop()
    [xml]$manifest = Get-Content -LiteralPath $resolvedManifest -Raw
    foreach ($dependency in $manifest.Package.Dependencies.PackageDependency)
    {
        $installed = @(Get-AppxPackage -Name $dependency.Name | Where-Object {
            $_.Architecture -eq 'X64' -and
            [version]$_.Version -ge [version]$dependency.MinVersion -and
            $_.Publisher -eq $dependency.Publisher
        })
        if ($installed.Count -eq 0)
        {
            throw "Required x64 framework is not installed for this user: $($dependency.Name) >= $($dependency.MinVersion)."
        }
    }
    Add-AppxPackage -Register $resolvedManifest -ForceApplicationShutdown
    $package = Get-AppxPackage -Name 'WindowsTerminal.TestHost' |
        Sort-Object Version -Descending |
        Select-Object -First 1
    if (-not $package)
    {
        throw 'WindowsTerminal.TestHost was not registered.'
    }

    $appUserModelId = "$($package.PackageFamilyName)!taef.executionengine.universal.App"
    $application = $manifest.Package.Applications.Application |
        Where-Object { $_.Id -eq 'taef.executionengine.universal.App' } |
        Select-Object -First 1
    $expectedExecutable = [IO.Path]::GetFullPath(
        (Join-Path (Split-Path -Parent $resolvedManifest) $application.Executable))
    $launchSurface = if ($Surface -eq 'fre-settings') { 'fre' } else { $Surface }
    $activationStarted = [datetime]::UtcNow
    $processId = [IntelligentTerminal.Accessibility.PackagedApp]::Activate(
        $appUserModelId,
        "--accessibility-page=$launchSurface")
    $ownedProcess = Get-OwnedProcess -ProcessId $processId `
        -ExpectedExecutable $expectedExecutable -ActivationStarted $activationStarted
    $process = $ownedProcess.Process

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($LaunchTimeoutSeconds)
    do
    {
        $process.Refresh()
        if ($process.HasExited)
        {
            throw 'The owned test host exited before becoming responsive.'
        }
        if ($process.Responding)
        {
            break
        }
        Start-Sleep -Milliseconds 500
    } while ([DateTimeOffset]::UtcNow -lt $deadline)

    if (-not $process.Responding)
    {
        throw "The $Surface accessibility surface did not become responsive within $LaunchTimeoutSeconds seconds."
    }
    if ($process.SessionId -ne (Get-Process -Id $PID).SessionId)
    {
        throw 'The test host and scanner are not in the same user session.'
    }

    Start-Sleep -Seconds 2
    $axeDirectory = Split-Path -Parent $resolvedAxe
    $scanScript = Join-Path $PSScriptRoot 'Invoke-AxeWindowsScan.ps1'
    $arguments = @(
        '-NoProfile',
        '-File', "`"$scanScript`"",
        '-ProcessId', $processId,
        '-AxeDirectory', "`"$axeDirectory`"",
        '-OutputPath', "`"$axeResultPath`"",
        '-ScanId', $Surface
    )
    $axe = Start-Process -FilePath 'pwsh.exe' `
        -ArgumentList $arguments `
        -PassThru `
        -NoNewWindow `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath
    $null = $axe.Handle
    if (-not $axe.WaitForExit($ScanTimeoutSeconds * 1000))
    {
        throw "Axe.Windows timed out after $ScanTimeoutSeconds seconds (PID $($axe.Id))."
    }

    $axeExitCode = $axe.ExitCode
    switch ($axeExitCode)
    {
        0
        {
            if (-not (Test-Path -LiteralPath $axeResultPath))
            {
                throw 'Axe.Windows exited successfully without producing scan evidence.'
            }
            $status = 'PASS'
            $reason = 'Axe.Windows completed and found no Error-level rules.'
        }
        1
        {
            if (-not (Test-Path -LiteralPath $axeResultPath))
            {
                $scannerError = Get-Content -LiteralPath $stderrPath -Raw -ErrorAction SilentlyContinue
                throw "Axe.Windows failed before producing results: $($scannerError.Trim())"
            }
            $status = 'FAIL'
            $reason = 'Axe.Windows completed and found one or more Error-level rules.'
        }
        default
        {
            throw "Axe.Windows failed to complete (exit code $axeExitCode)."
        }
    }
}
catch
{
    $status = 'BLOCKED'
    $reason = $_.Exception.Message
}
finally
{
    $primaryStatus = $status
    $primaryReason = $reason
    if ($axe)
    {
        try
        {
            if (-not $axe.HasExited)
            {
                $axe.Kill()
                if (-not $axe.WaitForExit(5000)) { throw 'Scanner did not exit after forced termination.' }
            }
        }
        catch
        {
            $cleanup.Add([pscustomobject]@{ status = 'BLOCKED'; action = 'scanner-stop'; reason = $_.Exception.Message })
            Write-Warning "Failed to clean up the Axe.Windows scanner: $($_.Exception.Message)" -WarningAction Continue
        }
        finally
        {
            try { $axe.Dispose() }
            catch
            {
                $cleanup.Add([pscustomobject]@{ status = 'BLOCKED'; action = 'scanner-dispose'; reason = $_.Exception.Message })
                Write-Warning "Failed to release the Axe.Windows scanner handle: $($_.Exception.Message)" -WarningAction Continue
            }
        }
    }
    if ($ownedProcess)
    {
        try
        {
            $cleanup.Add((Stop-OwnedProcess -OwnedProcess $ownedProcess))
        }
        catch
        {
            $cleanup.Add([pscustomobject]@{ status = 'BLOCKED'; action = 'test-host-stop'; reason = $_.Exception.Message })
            Write-Warning "Failed to clean up the owned test host process: $($_.Exception.Message)" -WarningAction Continue
        }
    }
    if ($package -and -not $KeepRegistered)
    {
        try
        {
            Remove-AppxPackage -Package $package.PackageFullName -ErrorAction Stop
        }
        catch
        {
            $cleanup.Add([pscustomobject]@{ status = 'BLOCKED'; action = 'package-remove'; reason = $_.Exception.Message })
            Write-Warning "Failed to remove the WindowsTerminal.TestHost registration: $($_.Exception.Message)" -WarningAction Continue
        }
        if ($previousPackage)
        {
            try
            {
                $previousManifest = Join-Path $previousPackage.InstallLocation 'AppxManifest.xml'
                if (-not (Test-Path -LiteralPath $previousManifest))
                {
                    throw 'The previous package manifest is no longer available.'
                }
                Add-AppxPackage -Register $previousManifest -DisableDevelopmentMode
            }
            catch
            {
                $cleanup.Add([pscustomobject]@{ status = 'BLOCKED'; action = 'package-restore'; reason = $_.Exception.Message })
                Write-Warning "Failed to restore the previous WindowsTerminal.TestHost registration: $($_.Exception.Message)" -WarningAction Continue
            }
        }
    }

    if (@($cleanup | Where-Object { $_.status -eq 'BLOCKED' }).Count -gt 0)
    {
        $status = 'BLOCKED'
        $reason = "Cleanup failed after $primaryStatus`: $primaryReason"
    }
    [ordered]@{
        version = 1
        source_sha = $SourceSha.ToLowerInvariant()
        surface = $Surface
        status = $status
        reason = $reason
        primary_status = $primaryStatus
        primary_reason = $primaryReason
        cleanup = @($cleanup.ToArray())
        axe_exit_code = $axeExitCode
        process_id = $processId
        axe_results = if (Test-Path -LiteralPath $axeResultPath) { $axeResultPath } else { $null }
    } |
        ConvertTo-Json -Depth 4 |
        Set-Content -LiteralPath $resultPath -Encoding utf8NoBOM
}

Get-Content -LiteralPath $resultPath
if ($status -eq 'PASS')
{
    exit 0
}
if ($status -eq 'FAIL')
{
    exit 1
}
exit 2
