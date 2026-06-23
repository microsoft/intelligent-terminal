# Fre.ps1 — First Run Experience primitives.
# FRE completion persists as `agentFreCompleted` in the shared state.json
# (ApplicationState.h:46; read at TerminalPage.cpp:920).

function Get-WtStateObject {
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        if (-not (Test-Path $App.StatePath)) { return $null }
        (Get-Content -LiteralPath $App.StatePath -Raw -Encoding utf8) | ConvertFrom-JsonC
    }
}

function Set-WtState {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][AllowNull()]$Value)
    process {
        $obj = Get-WtStateObject -App $App
        if (-not $obj) { $obj = [pscustomobject]@{} }
        if ($obj.PSObject.Properties.Name -contains $Key) { $obj.$Key = $Value }
        else { $obj | Add-Member -NotePropertyName $Key -NotePropertyValue $Value -Force }
        $dir = Split-Path $App.StatePath -Parent
        if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
        Set-Content -LiteralPath $App.StatePath -Value ($obj | ConvertTo-Json -Depth 64) -Encoding utf8
        $App
    }
}

function Invoke-FrePass {
    <#
    .SYNOPSIS
        Fast path: mark the agent FRE complete in state.json so it does not show on launch.
        Self-verifies. Call BEFORE Start-Terminal for it to take effect.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        Set-WtState -App $App -Key 'agentFreCompleted' -Value $true | Out-Null
        if (-not (Get-FreCompleted -App $App)) { throw "Invoke-FrePass: state.json agentFreCompleted not set." }
        Write-ItLog -Level INFO -Message "FRE marked complete (state.json)."
        $App
    }
}

function Reset-Fre {
    <# Force the FRE to show again (to test the FRE itself). #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process { Set-WtState -App $App -Key 'agentFreCompleted' -Value $false | Out-Null; $App }
}

function Get-FreCompleted {
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process { [bool]((Get-WtStateObject -App $App).agentFreCompleted) }
}

function Invoke-FrePassViaUi {
    <#
    .SYNOPSIS
        Drive the FRE overlay through the UI (Next -> Save) via winapp ui. Used to test
        the FRE flow itself. Requires WT running and the FRE showing.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 20)
    process {
        Invoke-UiElement -App $App -Selector 'NextButton' -TimeoutSec $TimeoutSec
        Invoke-UiElement -App $App -Selector 'SaveButton' -TimeoutSec $TimeoutSec
        Wait-Until -TimeoutSec $TimeoutSec -Because "FRE to complete" -Condition { Get-FreCompleted -App $App } | Out-Null
        $App
    }
}

function Test-FreShowing {
    <# Is the FRE overlay currently visible? (UIA check) #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process { Test-UiElementExists -App $App -Selector 'WelcomePage' -TimeoutSec 3 }
}

# ── Execution-policy control (deterministic FRE EP-block coverage) ────────────
# The FRE Save probes each PowerShell host's execution policy and blocks shell
# integration when it refuses unsigned local scripts (Restricted/AllSigned). These
# helpers force the *Windows PowerShell* effective policy via the CurrentUser
# registry scope — which outranks LocalMachine and needs NO admin — so a test can
# deterministically exercise BOTH the blocked and the not-blocked verdict, then
# restore the machine. (pwsh 7 keeps its policy in powershell.config.json, not the
# registry; the FRE blocks if EITHER host is blocked, so forcing WinPS is enough.)

$script:WtWinPSExecutionPolicyKey = 'HKCU:\SOFTWARE\Microsoft\PowerShell\1\ShellIds\Microsoft.PowerShell'

function Get-WtExecutionPolicyState {
    <# Snapshot the WinPS CurrentUser execution-policy registry value for restore. #>
    [CmdletBinding()] param()
    $val = (Get-ItemProperty -Path $script:WtWinPSExecutionPolicyKey -Name ExecutionPolicy -ErrorAction SilentlyContinue).ExecutionPolicy
    [pscustomobject]@{ Key = $script:WtWinPSExecutionPolicyKey; HadValue = ($null -ne $val); Value = $val }
}

function Set-WtExecutionPolicy {
    <#
    .SYNOPSIS
        Force the Windows PowerShell CurrentUser execution policy (HKCU, no admin) so the FRE
        EP probe deterministically returns it. Pass 'Undefined' to clear the scope. Returns the
        prior state object — pass it to Restore-WtExecutionPolicy (always restore in a finally).
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][ValidateSet('Restricted', 'AllSigned', 'RemoteSigned', 'Unrestricted', 'Bypass', 'Undefined')][string]$Value)
    $state = Get-WtExecutionPolicyState
    if (-not (Test-Path $state.Key)) { New-Item -Path $state.Key -Force | Out-Null }
    if ($Value -eq 'Undefined') { Remove-ItemProperty -Path $state.Key -Name ExecutionPolicy -ErrorAction SilentlyContinue }
    else { Set-ItemProperty -Path $state.Key -Name ExecutionPolicy -Value $Value -Type String -ErrorAction Stop }
    Write-ItLog -Level INFO -Message "Set WinPS CurrentUser ExecutionPolicy = $Value (was '$($state.Value)')"
    $state
}

function Restore-WtExecutionPolicy {
    <# Restore the snapshot returned by Set-WtExecutionPolicy / Get-WtExecutionPolicyState. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$State)
    process {
        if ($State.HadValue) { Set-ItemProperty -Path $State.Key -Name ExecutionPolicy -Value $State.Value -Type String -ErrorAction Stop }
        else { Remove-ItemProperty -Path $State.Key -Name ExecutionPolicy -ErrorAction SilentlyContinue }
        Write-ItLog -Level INFO -Message "Restored WinPS CurrentUser ExecutionPolicy to '$($State.Value)'"
    }
}

function Test-WtExecutionPolicyControllable {
    <#
    .SYNOPSIS
        Returns $true when the Windows PowerShell effective execution policy can be forced
        via the HKCU CurrentUser scope — i.e. no Group Policy override is in effect.
    .DESCRIPTION
        Group Policy scopes (MachinePolicy / UserPolicy) outrank CurrentUser, so when one is
        set the registry value these tests write is NOT the effective policy and the FRE
        verdict becomes non-deterministic. The EP suite must skip in that environment. GPO
        scopes are host-independent (HKLM/HKCU ...\Policies\...\PowerShell), so probing them
        from whatever host runs the test is valid for the WinPS the FRE will probe.
    #>
    [CmdletBinding()] param()
    $gpo = Get-ExecutionPolicy -List |
        Where-Object { $_.Scope -in 'MachinePolicy', 'UserPolicy' -and $_.ExecutionPolicy -ne 'Undefined' }
    -not [bool]$gpo
}

function Test-WtPwshBlocksShellIntegration {
    <#
    .SYNOPSIS
        Returns $true when pwsh 7 is installed AND its effective execution policy refuses
        unsigned local scripts (Restricted / AllSigned).
    .DESCRIPTION
        The FRE blocks shell integration if EITHER PowerShell host blocks, but these helpers
        only control Windows PowerShell via HKCU (pwsh keeps its policy in
        powershell.config.json, not the registry). So the not-blocked case must skip when a
        present pwsh would independently block — otherwise the FRE blocks regardless of the
        RemoteSigned WinPS policy under test.
    #>
    [CmdletBinding()] param()
    $pwsh = Get-Command pwsh.exe -ErrorAction SilentlyContinue
    if (-not $pwsh) { return $false }
    try {
        $policy = & $pwsh.Source -NoProfile -NonInteractive -Command 'Get-ExecutionPolicy' 2>$null
        return ([string]$policy -in 'Restricted', 'AllSigned')
    }
    catch { return $false }
}
