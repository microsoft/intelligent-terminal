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
# helpers invoke each supported PowerShell engine directly so the tests do not
# depend on its underlying policy storage. Every touched CurrentUser scope is
# snapshotted and restored.

function Get-WtExecutionPolicyHosts {
    [CmdletBinding()] param()
    $hosts = @(
        [pscustomobject]@{
            Name = 'winPs'
            Path = (Join-Path ([Environment]::GetFolderPath('System')) 'WindowsPowerShell\v1.0\powershell.exe')
        }
    )
    $pwsh = Get-Command pwsh.exe -ErrorAction SilentlyContinue
    if ($pwsh) {
        $hosts += [pscustomobject]@{ Name = 'pwsh'; Path = $pwsh.Source }
    }
    $hosts
}

function Get-WtExecutionPolicyState {
    <# Snapshot every supported engine's CurrentUser execution policy. #>
    [CmdletBinding()] param()
    @(
        foreach ($hostInfo in Get-WtExecutionPolicyHosts) {
            $value = & $hostInfo.Path -NoProfile -NonInteractive -Command 'Get-ExecutionPolicy -Scope CurrentUser' 2>$null
            if ($LASTEXITCODE -ne 0 -or -not $value) {
                throw "Failed to read CurrentUser execution policy from $($hostInfo.Path)."
            }
            [pscustomobject]@{
                Name = $hostInfo.Name
                Path = $hostInfo.Path
                Value = [string]$value
            }
        }
    )
}

function Set-WtExecutionPolicy {
    <#
    .SYNOPSIS
        Force every supported PowerShell engine's CurrentUser execution policy so
        the FRE probe deterministically returns it. Returns the prior state array;
        always pass it to Restore-WtExecutionPolicy in a finally block.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][ValidateSet('Restricted', 'AllSigned', 'RemoteSigned', 'Unrestricted', 'Bypass', 'Undefined')][string]$Value)
    $state = Get-WtExecutionPolicyState
    try {
        foreach ($entry in $state) {
            & $entry.Path -NoProfile -NonInteractive -Command "Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy $Value -Force" 2>&1 | Out-Null
            if ($LASTEXITCODE -ne 0) {
                throw "Failed to set CurrentUser execution policy through $($entry.Path)."
            }
            Write-ItLog -Level INFO -Message "Set $($entry.Name) CurrentUser ExecutionPolicy = $Value (was '$($entry.Value)')"
        }
    }
    catch {
        Restore-WtExecutionPolicy -State $state
        throw
    }
    $state
}

function Restore-WtExecutionPolicy {
    <# Restore the snapshots returned by Set-WtExecutionPolicy / Get-WtExecutionPolicyState. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$State)
    process {
        $failures = @()
        foreach ($entry in @($State)) {
            try {
                & $entry.Path -NoProfile -NonInteractive -Command "Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy $($entry.Value) -Force" 2>&1 | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    throw "setter exited with code $LASTEXITCODE"
                }
                $restored = & $entry.Path -NoProfile -NonInteractive -Command 'Get-ExecutionPolicy -Scope CurrentUser' 2>$null
                if ($LASTEXITCODE -ne 0 -or [string]$restored -ne $entry.Value) {
                    throw "verification returned '$restored'"
                }
                Write-ItLog -Level INFO -Message "Restored $($entry.Name) CurrentUser ExecutionPolicy to '$($entry.Value)'"
            }
            catch {
                $failures += "$($entry.Name) ($($entry.Path)): $($_.Exception.Message)"
            }
        }
        if ($failures.Count) {
            throw "Failed to restore one or more execution policies: $($failures -join '; ')"
        }
    }
}

function Test-WtExecutionPolicyControllable {
    <#
    .SYNOPSIS
        Returns $true when no supported PowerShell engine has a Group Policy
        execution-policy override.
    .DESCRIPTION
        Group Policy scopes outrank CurrentUser, so the suite must skip when one
        prevents deterministic remediation.
    #>
    [CmdletBinding()] param()
    foreach ($hostInfo in Get-WtExecutionPolicyHosts) {
        try {
            $gpo = & $hostInfo.Path -NoProfile -NonInteractive -Command 'Get-ExecutionPolicy -List | Where-Object { $_.Scope -in "MachinePolicy", "UserPolicy" -and $_.ExecutionPolicy -ne "Undefined" }' 2>$null
            if ($LASTEXITCODE -ne 0 -or $gpo) { return $false }
        }
        catch {
            return $false
        }
    }
    $true
}
