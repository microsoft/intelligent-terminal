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

# ── Window-level key injection (WT accelerators + Settings) ───────────────────────────
# winapp drives UIA elements but has no key-send verb, and wtcli send-keys reaches a pane's
# CONPTY (the wta TUI), NOT WT's XAML keybinding layer — so WT accelerators (Ctrl+, to open
# Settings, Ctrl+Shift+. to toggle the agent pane, Alt+Shift+B delegate, …) can't be driven that
# way. This sends OS-level keystrokes to the focused WT WINDOW via keybd_event, which DOES hit
# WT's accelerator handler. Foreground-focus dependent (so a bit fragile under parallel runs /
# a locked session), hence the SetForegroundWindow + verify + retry below.
function Initialize-WtWin32Input {
    if ('ItWtWin32Input' -as [type]) { return }
    Add-Type -Namespace 'ItE2E' -Name 'ItWtWin32Input' -MemberDefinition @'
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, System.UIntPtr dwExtraInfo);
    // Robustly foreground a window even when the caller doesn't own foreground, by briefly
    // attaching to the current foreground thread's input queue (the standard foreground-lock
    // workaround). Returns true if the window is foreground afterwards.
    public static bool ForceForeground(IntPtr hWnd) {
        IntPtr fg = GetForegroundWindow();
        if (fg == hWnd) return true;
        uint tidThis = GetCurrentThreadId();
        uint fgPid; uint tidFg = GetWindowThreadProcessId(fg, out fgPid);
        bool attached = false;
        if (tidFg != 0 && tidFg != tidThis) { attached = AttachThreadInput(tidThis, tidFg, true); }
        BringWindowToTop(hWnd);
        SetForegroundWindow(hWnd);
        if (attached) { AttachThreadInput(tidThis, tidFg, false); }
        System.Threading.Thread.Sleep(120);
        return GetForegroundWindow() == hWnd;
    }
'@
}

function Send-WtWindowKey {
    <#
    .SYNOPSIS
        Send an OS-level keystroke (optionally with modifiers) to the WT window itself, so WT
        ACCELERATORS fire (Ctrl+,, Ctrl+Shift+., Alt+Shift+B, …). Unlike Send-WtKeys (conpty) this
        reaches WT's keybinding layer.
    .PARAMETER Vk         Main virtual-key code (e.g. 0xBC = OEM_COMMA, 0xBE = OEM_PERIOD, 'B'=0x42).
    .PARAMETER Ctrl/Shift/Alt  Modifier switches held around the main key.
    .NOTES
        Foregrounds the WT window via AttachThreadInput (works even when the caller doesn't own
        foreground). Best-effort: returns even if foreground couldn't be taken (a competing
        foreground app in an agent-driven session) — the caller verifies the observable effect and
        treats "no effect" as a foreground/precondition SKIP rather than a product failure. Query
        the last result via the -PassThru object's boolean note if needed.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][int]$Vk,
        [switch]$Ctrl, [switch]$Shift, [switch]$Alt,
        [int]$Repeat = 1
    )
    process {
        if (-not $App.Hwnd) { throw "Send-WtWindowKey needs `$App.Hwnd (launch via Start-Terminal)." }
        Initialize-WtWin32Input
        $hwnd = [IntPtr][int64]$App.Hwnd
        $KEYUP = 0x2
        $mods = @()
        if ($Ctrl) { $mods += 0x11 }; if ($Shift) { $mods += 0x10 }; if ($Alt) { $mods += 0x12 }
        $script:ItLastForegroundOk = $false
        for ($r = 0; $r -lt $Repeat; $r++) {
            # Force foreground (AttachThreadInput bypasses the foreground lock). Best-effort retry.
            $fg = $false
            for ($f = 0; $f -lt 6 -and -not $fg; $f++) {
                $fg = [ItE2E.ItWtWin32Input]::ForceForeground($hwnd)
                if (-not $fg) { Start-Sleep -Milliseconds 200 }
            }
            $script:ItLastForegroundOk = $fg
            foreach ($m in $mods) { [ItE2E.ItWtWin32Input]::keybd_event([byte]$m, 0, 0, [UIntPtr]::Zero) }
            [ItE2E.ItWtWin32Input]::keybd_event([byte]$Vk, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 40
            [ItE2E.ItWtWin32Input]::keybd_event([byte]$Vk, 0, $KEYUP, [UIntPtr]::Zero)
            foreach ($m in ($mods | Sort-Object -Descending)) { [ItE2E.ItWtWin32Input]::keybd_event([byte]$m, 0, $KEYUP, [UIntPtr]::Zero) }
            Start-Sleep -Milliseconds 120
        }
        $App
    }
}

function Test-WtWindowKeyFocusable {
    <#
    .SYNOPSIS
        $true if the WT window could be brought to the foreground for window-level key injection.
        Use to SKIP accelerator tests when a competing foreground app (e.g. the agent's own
        terminal driving the run) makes Send-WtWindowKey unreliable, instead of failing flakily.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        if (-not $App.Hwnd) { return $false }
        Initialize-WtWin32Input
        [ItE2E.ItWtWin32Input]::ForceForeground([IntPtr][int64]$App.Hwnd)
    }
}

function Open-WtSettings {
    <#
    .SYNOPSIS
        Open the Windows Terminal SETTINGS editor (Ctrl+, via window-level key injection) and wait
        until its UI renders. Returns $App. The editor opens as a tab in the WT window; drive its
        controls with the normal Get-UiElement / Invoke-UiElement / Set-UiValue primitives.
    .NOTES
        The Settings UI is a real XAML surface (SettingsNav, per-page controls). This refutes the
        earlier assumption that the editor "can't be opened" by the harness — it can, via the OS
        accelerator. Idempotent: returns immediately if Settings is already showing.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 20)
    process {
        $shown = { Test-UiElementExists -App $App -Selector 'SettingsNav' -TimeoutSec 1 }
        if (& $shown) { return $App }
        for ($try = 0; $try -lt 3; $try++) {
            Send-WtWindowKey -App $App -Vk 0xBC -Ctrl | Out-Null   # Ctrl + OEM_COMMA
            if (Test-Until -TimeoutSec ([Math]::Max(4, [int]($TimeoutSec / 3))) -IntervalSec 0.5 -Condition $shown) { return $App }
        }
        throw "Open-WtSettings: the Settings editor did not render after Ctrl+, (is the WT window able to take foreground?)."
    }
}

function Invoke-SettingsNav {
    <# Navigate the open Settings editor to a nav page (e.g. 'AIAgentsNavItem', 'AppearanceNavItem'). #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$NavItem)
    process {
        Invoke-UiElement -App $App -Selector $NavItem -TimeoutSec 10 | Out-Null
        Start-Sleep -Milliseconds 500
        $App
    }
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

function Get-UiElement {
    <#
    .SYNOPSIS
        Return the `winapp ui inspect --json` property bag of the first matching element
        (type, name, automationId, isEnabled, isOffscreen, toggleState, bounds, …) or $null.
        Unlike Get-UiTree (a text summary that only shows [on]/[off]), this exposes UIA
        properties like isEnabled — needed to assert enabled/disabled (greyed) control state.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector)
    process {
        $r = Invoke-WinAppUi -App $App -UiArgs @('inspect', $Selector, '--json', '--depth', '1')
        $j = $r.StdOut | ConvertFrom-JsonSafe
        if (-not $j -or -not $j.windows) { return $null }
        $els = @($j.windows | ForEach-Object { $_.elements } | Where-Object { $_ })
        $match = $els | Where-Object { $_.automationId -eq $Selector -or $_.selector -eq $Selector } | Select-Object -First 1
        if ($match) { return $match }
        $els | Select-Object -First 1
    }
}

function Test-UiElementEnabled {
    <# $true when the element's UIA IsEnabled is true (i.e. NOT greyed/disabled). #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector)
    process { $el = Get-UiElement -App $App -Selector $Selector; [bool]($el -and $el.isEnabled) }
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
