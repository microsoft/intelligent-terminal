<#
.SYNOPSIS
    Revert Enable-WtAgentPolicyTesting.ps1 — remove the agent policy key that Enable CREATED (and
    that it marked as test-provisioned), leaving the machine clean.

.DESCRIPTION
    Self-elevates via UAC and, in YOUR hive (HKEY_USERS\<your-SID>, captured before elevation),
    removes
        HKCU\Software\Policies\Microsoft\IntelligentTerminal
    ONLY when Enable created it (the `__wt_e2e_provisioned` marker is present). If the key
    pre-existed (real policy values), Enable did not mark it, so this script intentionally LEAVES IT
    UNTOUCHED — including any ACL write-grant Enable added to it — rather than risk clobbering real
    policy. Safe to run even if the key doesn't exist.

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
if (-not (Test-Path $path)) { Write-Host "Nothing to remove ($path absent)."; exit 0 }

# Only delete the whole key if Enable-WtAgentPolicyTesting created it (marker present). If the key
# pre-existed, Enable did NOT stamp the marker, so we land here and leave the key — and any real
# policy values — untouched. (Enable's ACL grant on a pre-existing key is harmless and left as-is.)
$provisioned = $null -ne (Get-ItemProperty -Path $path -Name '__wt_e2e_provisioned' -ErrorAction SilentlyContinue)
if ($provisioned) {
    Remove-Item -Path $path -Recurse -Force
    Write-Host "Removed $path (was provisioned by Enable-WtAgentPolicyTesting)."
}
else {
    Write-Host "Key pre-existed (no provisioning marker) — leaving it untouched."
}
exit 0
