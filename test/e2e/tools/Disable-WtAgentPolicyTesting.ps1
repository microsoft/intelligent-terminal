<#
.SYNOPSIS
    Revert Enable-WtAgentPolicyTesting.ps1 — remove the agent policy key (and the write grant)
    from the current user's hive, leaving the machine clean.

.DESCRIPTION
    Self-elevates via UAC and deletes
        HKCU\Software\Policies\Microsoft\IntelligentTerminal
    from YOUR hive (HKEY_USERS\<your-SID>, captured before elevation). Safe to run even if the
    key doesn't exist.

.EXAMPLE
    pwsh -File test/e2e/tools/Disable-WtAgentPolicyTesting.ps1
#>
[CmdletBinding()]
param(
    [string]$UserSid,
    [string]$UserName
)

$ErrorActionPreference = 'Stop'

function Test-IsAdmin {
    ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not $UserSid) {
    $id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $UserSid = $id.User.Value
    $UserName = $id.Name
}

if (-not (Test-IsAdmin)) {
    Write-Host "Elevating to remove '$UserName' agent-policy key (approve the UAC prompt)..."
    $pwsh = (Get-Process -Id $PID).Path
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-UserSid', $UserSid, '-UserName', "`"$UserName`"")
    $p = Start-Process -FilePath $pwsh -ArgumentList $argList -Verb RunAs -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "Elevated cleanup failed (exit $($p.ExitCode))." }
    Write-Host "Done."
    return
}

$path = "Registry::HKEY_USERS\$UserSid\SOFTWARE\Policies\Microsoft\IntelligentTerminal"
if (Test-Path $path) { Remove-Item -Path $path -Recurse -Force; Write-Host "Removed $path." }
else { Write-Host "Nothing to remove ($path absent)." }
exit 0
