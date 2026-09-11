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

function Test-FreProgressOrder {
    <# Verify that stable FRE progress log events appear in the requested order. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Log,
        [Parameter(Mandatory)][string[]]$Events
    )

    $offset = 0
    foreach ($progressEvent in $Events) {
        $needle = "[FRE] Progress: $progressEvent"
        $index = $Log.IndexOf($needle, $offset, [System.StringComparison]::Ordinal)
        if ($index -lt 0) { return $false }
        $offset = $index + $needle.Length
    }
    return $true
}

# ── Execution-policy control (deterministic FRE remediation coverage) ────────

function Get-WtExecutionPolicyHosts {
    <# Resolve every PowerShell host whose CurrentUser policy FRE may modify. #>
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
    <#
        Snapshot every supported engine's CurrentUser execution policy.
        Process-scope Bypass keeps the management cmdlet loadable even when a
        previous test deliberately left CurrentUser at AllSigned/Restricted.
    #>
    [CmdletBinding()] param()
    @(
        foreach ($hostInfo in Get-WtExecutionPolicyHosts) {
            $value = & $hostInfo.Path -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command 'Get-ExecutionPolicy -Scope CurrentUser' 2>$null
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
        FRE remediation is deterministic. Always restore the returned state.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][ValidateSet('Restricted', 'AllSigned', 'RemoteSigned', 'Unrestricted', 'Bypass', 'Undefined')][string]$Value)
    $state = Get-WtExecutionPolicyState
    try {
        foreach ($entry in $state) {
            $command = "Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy $Value -Force -ErrorAction SilentlyContinue; if ((Get-ExecutionPolicy -Scope CurrentUser) -ne '$Value') { exit 1 }"
            & $entry.Path -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command $command 2>&1 | Out-Null
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
    <# Restore snapshots returned by Set-WtExecutionPolicy / Get-WtExecutionPolicyState. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$State)
    process {
        $failures = @()
        foreach ($entry in @($State)) {
            try {
                $command = "Set-ExecutionPolicy -Scope CurrentUser -ExecutionPolicy $($entry.Value) -Force -ErrorAction SilentlyContinue; if ((Get-ExecutionPolicy -Scope CurrentUser) -ne '$($entry.Value)') { exit 1 }"
                & $entry.Path -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command $command 2>&1 | Out-Null
                if ($LASTEXITCODE -ne 0) {
                    throw "setter exited with code $LASTEXITCODE"
                }
                $restored = & $entry.Path -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command 'Get-ExecutionPolicy -Scope CurrentUser' 2>$null
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
        Returns $true when no supported PowerShell host has a Group Policy override.
    #>
    [CmdletBinding()] param()
    foreach ($hostInfo in Get-WtExecutionPolicyHosts) {
        try {
            $gpo = & $hostInfo.Path -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command 'Get-ExecutionPolicy -List | Where-Object { $_.Scope -in "MachinePolicy", "UserPolicy" -and $_.ExecutionPolicy -ne "Undefined" }' 2>$null
            if ($LASTEXITCODE -ne 0 -or $gpo) { return $false }
        }
        catch {
            return $false
        }
    }
    $true
}

function Test-WtPwshBlocksShellIntegration {
    <#
    .SYNOPSIS
        Returns $true when pwsh 7 is installed AND its effective execution policy refuses
        unsigned local scripts (Restricted / AllSigned).
    .DESCRIPTION
        Used by suites that do not change execution policy themselves and therefore need
        to skip when the machine's existing pwsh policy would block shell integration.
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
