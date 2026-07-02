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
    [DllImport("user32.dll")] public static extern IntPtr SetActiveWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetFocus(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool AllowSetForegroundWindow(uint dwProcessId);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint idAttach, uint idAttachTo, bool fAttach);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, System.UIntPtr dwExtraInfo);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool SystemParametersInfo(uint uiAction, uint uiParam, ref uint pvParam, uint fWinIni);

    const uint SPI_GETFOREGROUNDLOCKTIMEOUT = 0x2000;
    const uint SPI_SETFOREGROUNDLOCKTIMEOUT = 0x2001;
    const uint SPIF_SENDCHANGE = 0x2;
    const int  SW_RESTORE = 9;
    const uint ASFW_ANY = unchecked((uint)-1);
    const byte VK_MENU = 0x12;   // ALT
    const uint KEYUP = 0x2;

    // Aggressively bring a window to the foreground, defeating the foreground-lock that otherwise
    // makes SetForegroundWindow a no-op when the caller doesn't own foreground. Combines every
    // documented workaround: zero the foreground-lock timeout, tap ALT (registers input from this
    // context so Windows permits the switch), AllowSetForegroundWindow, attach to the current
    // foreground thread's input queue, un-minimize, and push the window active/focused. Returns
    // true only if the window actually holds the foreground afterwards.
    public static bool ForceForeground(IntPtr hWnd) {
        if (GetForegroundWindow() == hWnd) return true;

        uint prev = 0; SystemParametersInfo(SPI_GETFOREGROUNDLOCKTIMEOUT, 0, ref prev, 0);
        uint zero = 0; SystemParametersInfo(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, ref zero, SPIF_SENDCHANGE);

        // Tap ALT: Windows allows a foreground change if the calling thread received the last input.
        keybd_event(VK_MENU, 0, 0, System.UIntPtr.Zero);
        keybd_event(VK_MENU, 0, KEYUP, System.UIntPtr.Zero);

        AllowSetForegroundWindow(ASFW_ANY);

        uint tidThis = GetCurrentThreadId();
        IntPtr fg = GetForegroundWindow();
        uint fgPid; uint tidFg = (fg == IntPtr.Zero) ? 0 : GetWindowThreadProcessId(fg, out fgPid);
        bool attached = (tidFg != 0 && tidFg != tidThis) && AttachThreadInput(tidThis, tidFg, true);
        try {
            if (IsIconic(hWnd)) { ShowWindow(hWnd, SW_RESTORE); }
            BringWindowToTop(hWnd);
            SetForegroundWindow(hWnd);
            SetActiveWindow(hWnd);
            SetFocus(hWnd);
        }
        finally {
            if (attached) { AttachThreadInput(tidThis, tidFg, false); }
            uint restore = prev; SystemParametersInfo(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, ref restore, SPIF_SENDCHANGE);
        }
        System.Threading.Thread.Sleep(120);
        return GetForegroundWindow() == hWnd;
    }
'@
}

function Set-WtWindowForeground {
    <#
    .SYNOPSIS
        Ensure the WT window IS in the foreground so a subsequent window-level key send lands on it.
        Applies the full foreground-forcing combo (see ForceForeground) and RETRIES until the window
        actually holds the foreground or the attempts run out.
    .OUTPUTS
        [bool] $true if the WT window is confirmed foreground; $false if it could not be forced
        (a competing foreground app is holding it — caller should treat as a precondition skip).
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$Attempts = 8, [int]$DelayMs = 200)
    process {
        if (-not $App.Hwnd) { throw "Set-WtWindowForeground needs `$App.Hwnd (launch via Start-Terminal)." }
        Initialize-WtWin32Input
        $hwnd = [IntPtr][int64]$App.Hwnd
        for ($i = 0; $i -lt $Attempts; $i++) {
            if ([ItE2E.ItWtWin32Input]::ForceForeground($hwnd)) { return $true }
            Start-Sleep -Milliseconds $DelayMs
        }
        [ItE2E.ItWtWin32Input]::ForceForeground($hwnd)
    }
}

function Send-WtWindowKey {
    <#
    .SYNOPSIS
        Send an OS-level keystroke (optionally with modifiers) to the WT window itself, so WT
        ACCELERATORS fire (Ctrl+,, Ctrl+Shift+., Alt+Shift+B, …). Unlike Send-WtKeys (conpty) this
        reaches WT's keybinding layer.
    .PARAMETER Vk         Main virtual-key code (e.g. 0xBC = OEM_COMMA, 0xBE = OEM_PERIOD, 'B'=0x42).
    .PARAMETER Ctrl/Shift/Alt  Modifier switches held around the main key.
    .PARAMETER RequireForeground  Throw if the WT window can't be brought to the foreground (so the
                          key would go to the wrong window). Default best-effort; callers that must
                          guarantee delivery pass -RequireForeground and catch/skip on failure.
    .NOTES
        Uses Set-WtWindowForeground to guarantee the window is foreground before injecting keys.
        `$script:ItLastForegroundOk` records whether foreground was confirmed for the last send.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][int]$Vk,
        [switch]$Ctrl, [switch]$Shift, [switch]$Alt,
        [int]$Repeat = 1,
        [switch]$RequireForeground
    )
    process {
        if (-not $App.Hwnd) { throw "Send-WtWindowKey needs `$App.Hwnd (launch via Start-Terminal)." }
        Initialize-WtWin32Input
        $KEYUP = 0x2
        $mods = @()
        if ($Ctrl) { $mods += 0x11 }; if ($Shift) { $mods += 0x10 }; if ($Alt) { $mods += 0x12 }
        for ($r = 0; $r -lt $Repeat; $r++) {
            # Guarantee foreground BEFORE injecting, so the keystroke can't land on the wrong window.
            $fg = Set-WtWindowForeground -App $App
            $script:ItLastForegroundOk = $fg
            if (-not $fg) {
                if ($RequireForeground) {
                    throw "Send-WtWindowKey: WT window could not be brought to the foreground (competing foreground app)."
                }
                # Foreground not acquired: do NOT inject — the keys (esp. Ctrl/Alt/Shift accelerators)
                # would land on whatever app currently owns the foreground. Skip this iteration; the
                # caller detects the no-effect via Test-WtWindowKeyFocusable and treats it as a
                # skippable foreground precondition rather than a spurious keystroke.
                continue
            }
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
        Set-WtWindowForeground -App $App -Attempts 3 -DelayMs 150
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

function Test-CommandPaletteOpen {
    <#
    .SYNOPSIS
        Locale-robust check that the command palette is open, centralizing the detection that was
        previously duplicated as a hard-coded English "Command palette" literal across suites.
        The palette's accessible name is the localized `CommandPaletteControlName` resource, and it
        exposes no stable AutomationId to winapp, so we SEARCH by each localized name value (winapp
        search needs a literal query) and confirm the hit against an all-locales regex of the same
        key. This works on any build language, not just en-US.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        $rx = Get-WtReswTextRegex -Key 'CommandPaletteControlName'
        if (-not $rx) { $rx = '(?i)Command palette' }
        # Try each localized palette name as a search query; the running build's language matches on
        # its own value. Fall back to the en-US literal if the resources couldn't be read.
        $queries = @(Get-WtReswTextValues -Key 'CommandPaletteControlName')
        if (-not $queries.Count) { $queries = @('Command palette') }
        foreach ($q in $queries) {
            if ((Find-UiElement -App $App -Selector $q) -match $rx) { return $true }
        }
        $false
    }
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
        # Flatten to a single list of element objects (each window's `elements` may itself be an
        # array; @(... ForEach-Object) unrolls them into one flat list).
        $els = @($j.windows | ForEach-Object { $_.elements } | Where-Object { $_ })
        # Return ONLY an element that actually matches the requested selector (by AutomationId,
        # winapp slug, or name). Do NOT fall back to "first inspected element": winapp inspect can
        # return the window root / unrelated nodes when the selector doesn't resolve, and returning
        # those would make Test-UiElementEnabled / .toggleState assert against the wrong control
        # (false positives). No match => $null, so callers correctly see "absent/disabled".
        $els |
            Where-Object { $_.automationId -eq $Selector -or $_.selector -eq $Selector -or $_.name -eq $Selector } |
            Select-Object -First 1
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
