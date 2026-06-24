# Ui.ps1 — UI automation via `winapp ui` (Windows App CLI). Replaces WinAppDriver.
# Targets the WT window by stable HWND ($App.Hwnd), falling back to PID.

function Get-WinAppPath {
    if ($script:ItWinAppPath -and (Test-Path $script:ItWinAppPath)) { return $script:ItWinAppPath }
    $c = (Get-Command winapp -ErrorAction SilentlyContinue).Source
    if (-not $c) { throw "winapp (Windows App CLI) not found. Run bootstrap.ps1 or: winget install Microsoft.winappcli" }
    $script:ItWinAppPath = $c; $c
}

function Test-WinAppAvailable {
    <#
    .SYNOPSIS
        Non-throwing probe for the winapp (Windows App CLI) UI-automation tool.
    .DESCRIPTION
        Returns $true when winapp is on PATH (or already resolved), $false otherwise — unlike
        Get-WinAppPath, which throws. Use this in a Describe readiness gate so UI-dependent
        suites SKIP cleanly when winapp is missing instead of blowing up in BeforeAll with a
        raw "winapp not found" exception. Install winapp via test/e2e/bootstrap.ps1.
    #>
    [CmdletBinding()]
    [OutputType([bool])]
    param()
    if ($script:ItWinAppPath -and (Test-Path $script:ItWinAppPath)) { return $true }
    [bool](Get-Command winapp -ErrorAction SilentlyContinue)
}

function Get-UiTarget {
    param([Parameter(Mandatory)]$App)
    if ($App.Hwnd) { return @('-w', [string]$App.Hwnd) }
    if ($App.Pid) { return @('-a', [string]$App.Pid) }
    throw "App has no Hwnd or Pid for UI targeting. Launch via Start-Terminal first."
}

function Invoke-WinAppUi {
    <#
    .SYNOPSIS
        Run a `winapp ui` command against the WT window. Returns an Invoke-Native result
        (ExitCode/StdOut/StdErr). Telemetry is opted out.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string[]]$UiArgs, [int]$TimeoutSec = 30, [switch]$NoTarget)
    $winapp = Get-WinAppPath
    $args = @('ui') + $UiArgs
    if (-not $NoTarget) { $args += (Get-UiTarget -App $App) }
    Invoke-Native -FilePath $winapp -Arguments $args -TimeoutSec $TimeoutSec -Environment @{ WINAPP_CLI_TELEMETRY_OPTOUT = '1' }
}

function Get-WtWindowHwnds {
    <# Return normalized @{ hwnd; pid; title } for WT windows (via winapp list-windows). #>
    [CmdletBinding()] param([Parameter(Mandatory)]$App)
    $r = Invoke-WinAppUi -App $App -NoTarget -UiArgs @('list-windows', '-a', 'WindowsTerminal', '--json') -TimeoutSec 15
    $j = $r.StdOut | ConvertFrom-JsonSafe
    if ($null -ne $j) {
        $rows = if ($j -is [System.Array]) { $j } elseif ($j.windows) { $j.windows } else { @($j) }
        return $rows | ForEach-Object {
            $wpid = if ($null -ne $_.processId) { $_.processId } else { $_.pid }
            [pscustomobject]@{ hwnd = [int]$_.hwnd; pid = [int]$wpid; title = [string]$_.title }
        }
    }
    # Fallback: parse the text form "HWND 985238: "title" ... (WindowsTerminal, PID 21228)".
    @($r.StdOut -split "`n" | ForEach-Object {
            if ($_ -match 'HWND\s+(\d+):\s+"?(.*?)"?\s+.*\(WindowsTerminal,\s*PID\s+(\d+)\)') {
                [pscustomobject]@{ hwnd = [int]$Matches[1]; title = $Matches[2]; pid = [int]$Matches[3] }
            }
        })
}

function Get-UiTree {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$Selector, [int]$Depth = 3, [switch]$Interactive)
    process {
        $a = @('inspect'); if ($Selector) { $a += $Selector }; $a += @('--depth', $Depth); if ($Interactive) { $a += '--interactive' }
        (Invoke-WinAppUi -App $App -UiArgs $a).StdOut
    }
}

function Find-UiElement {
    <# winapp ui search — returns the raw match listing. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector, [int]$Max)
    process {
        $a = @('search', $Selector); if ($Max) { $a += @('--max', $Max) }
        (Invoke-WinAppUi -App $App -UiArgs $a).StdOut
    }
}

function Invoke-UiElement {
    <# winapp ui invoke (Invoke/Toggle/Selection/ExpandCollapse). Throws on failure. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector, [int]$TimeoutSec = 20)
    process {
        # Make sure the element exists first (clearer failure + sync).
        Wait-UiElement -App $App -Selector $Selector -TimeoutSec $TimeoutSec | Out-Null
        $r = Invoke-WinAppUi -App $App -UiArgs @('invoke', $Selector)
        if ($r.ExitCode -ne 0) { throw "winapp ui invoke '$Selector' failed: $($r.StdErr.Trim())$($r.StdOut.Trim())" }
        $App
    }
}

function Invoke-UiClick {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector, [switch]$Double, [switch]$Right)
    process {
        $a = @('click', $Selector); if ($Double) { $a += '--double' }; if ($Right) { $a += '--right' }
        $r = Invoke-WinAppUi -App $App -UiArgs $a
        if ($r.ExitCode -ne 0) { throw "winapp ui click '$Selector' failed: $($r.StdErr.Trim())" }
        $App
    }
}

function Set-UiValue {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector, [Parameter(Mandatory)][string]$Value)
    process {
        $r = Invoke-WinAppUi -App $App -UiArgs @('set-value', $Selector, $Value)
        if ($r.ExitCode -ne 0) { throw "winapp ui set-value '$Selector' failed: $($r.StdErr.Trim())" }
        $App
    }
}

function Get-UiValue {
    <# Read an element value (smart fallback chain). Returns the text. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector)
    process {
        $r = Invoke-WinAppUi -App $App -UiArgs @('get-value', $Selector, '--json')
        $j = $r.StdOut | ConvertFrom-JsonSafe
        if ($null -ne $j -and ($j.PSObject.Properties.Name -contains 'text')) { return $j.text }
        $r.StdOut.Trim()
    }
}

function Wait-UiElement {
    <#
    .SYNOPSIS
        Wait for an element to appear (or -Gone / -Value). Uses winapp ui wait-for, which
        returns exit code 1 on timeout. Throws on timeout unless -Quiet.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$Selector,
        [int]$TimeoutSec = 15,
        [switch]$Gone,
        [string]$Value,
        [string]$Property,
        [switch]$Contains,
        [switch]$Quiet
    )
    process {
        $a = @('wait-for', $Selector, '--timeout', ($TimeoutSec * 1000))
        if ($Gone) { $a += '--gone' }
        if ($PSBoundParameters.ContainsKey('Value')) { $a += @('--value', $Value) }
        if ($Property) { $a += @('--property', $Property) }
        if ($Contains) { $a += '--contains' }
        $r = Invoke-WinAppUi -App $App -UiArgs $a -TimeoutSec ($TimeoutSec + 5)
        if ($r.ExitCode -ne 0) {
            if ($Quiet) { return $false }
            throw "winapp ui wait-for '$Selector' timed out/failed: $($r.StdErr.Trim())"
        }
        if ($Quiet) { return $true }
        $App
    }
}

function Test-UiElementExists {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector, [int]$TimeoutSec = 5)
    process { Wait-UiElement -App $App -Selector $Selector -TimeoutSec $TimeoutSec -Quiet }
}

function Save-UiScreenshot {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Path, [switch]$CaptureScreen)
    process {
        $a = @('screenshot', '--output', $Path); if ($CaptureScreen) { $a += '--capture-screen' }
        $r = Invoke-WinAppUi -App $App -UiArgs $a
        if ($r.ExitCode -ne 0) { Write-ItLog -Level WARN -Message "screenshot failed: $($r.StdErr.Trim())" }
        $Path
    }
}
