<#
.SYNOPSIS
    One-time elevated provisioning so the agent Group-Policy e2e suite
    (Feature.AgentPolicy.Tests.ps1) can run WITHOUT per-run elevation.

.DESCRIPTION
    The IntelligentTerminal agent GPO values live under
        HKCU\Software\Policies\Microsoft\IntelligentTerminal
    and the `Software\Policies` subtree is ACL-restricted to administrators, so a standard user
    cannot write it. Rather than run the whole test session elevated (which would also elevate
    the terminal under test and could mask real behavior), this script grants the CURRENT user
    write access to just that policy key, ONCE. After that the e2e suite drives policy values
    non-elevated and the terminal under test stays a normal user process.

    Self-elevates via UAC. Because UAC may run the elevated instance under a DIFFERENT admin
    account (whose HKCU is not yours), the elevated half operates on YOUR hive explicitly via
    HKEY_USERS\<your-SID> — captured before elevation and forwarded as an argument.

    Reverse with Disable-WtAgentPolicyTesting.ps1.

.EXAMPLE
    pwsh -File test/e2e/tools/Enable-WtAgentPolicyTesting.ps1
#>
[CmdletBinding()]
param(
    # The SID + name of the user to grant. Auto-detected on the first (non-elevated) launch and
    # forwarded to the elevated relaunch so we grant YOU, not the elevating admin.
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
    Write-Host "Elevating to grant '$UserName' write access to its agent-policy key (approve the UAC prompt)..."
    $pwsh = (Get-Process -Id $PID).Path
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-UserSid', $UserSid, '-UserName', "`"$UserName`"")
    $p = Start-Process -FilePath $pwsh -ArgumentList $argList -Verb RunAs -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "Elevated provisioning failed (exit $($p.ExitCode))." }
    Write-Host "Done. The agent GPO e2e suite can now run non-elevated."
    return
}

# ── Elevated half: operate on the ORIGINAL user's hive (HKU\<sid>), not the admin's HKCU. ──
$path = "Registry::HKEY_USERS\$UserSid\SOFTWARE\Policies\Microsoft\IntelligentTerminal"
if (-not (Test-Path $path)) { New-Item -Path $path -Force | Out-Null }

$sid = New-Object System.Security.Principal.SecurityIdentifier($UserSid)
$rights = [System.Security.AccessControl.RegistryRights]'SetValue,CreateSubKey,Delete,ReadKey,EnumerateSubKeys,ReadPermissions'
$rule = New-Object System.Security.AccessControl.RegistryAccessRule($sid, $rights, 'ContainerInherit', 'None', 'Allow')

$acl = Get-Acl -Path $path
$acl.AddAccessRule($rule)
Set-Acl -Path $path -AclObject $acl

Write-Host "Granted write on $path to $UserName ($UserSid)."
exit 0
