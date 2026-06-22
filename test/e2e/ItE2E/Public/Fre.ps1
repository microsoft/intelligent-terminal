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
