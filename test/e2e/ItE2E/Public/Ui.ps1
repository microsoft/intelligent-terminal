# Ui.ps1 — UI automation via `winapp ui` (Windows App CLI). Replaces WinAppDriver.
# Targets the WT window by stable HWND ($App.Hwnd), falling back to PID.

function Get-WinAppPath {
    if ($script:ItWinAppPath -and (Test-Path $script:ItWinAppPath)) { return $script:ItWinAppPath }
    $c = (Get-Command winapp -ErrorAction SilentlyContinue).Source
    if (-not $c) { throw "winapp (Windows App CLI) not found. Run bootstrap.ps1 or: winget install Microsoft.WinAppCli" }
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
    if ('ItE2E.ItWtWin32Input' -as [type]) { return }
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
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr hWnd, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr hWnd, uint command);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hWnd);
    [DllImport("user32.dll", SetLastError=true)] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool GetCursorPos(out POINT point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT point);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint message, UIntPtr wParam, IntPtr lParam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int virtualKey);
    [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint count, INPUT[] inputs, int size);
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
    const uint INPUT_MOUSE = 0;
    const uint MOUSEEVENTF_WHEEL = 0x0800;

    [StructLayout(LayoutKind.Sequential)]
    public struct POINT {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    public struct MOUSEINPUT {
        public int dx;
        public int dy;
        public uint mouseData;
        public uint dwFlags;
        public uint time;
        public UIntPtr dwExtraInfo;
    }

    [StructLayout(LayoutKind.Explicit)]
    public struct INPUTUNION {
        [FieldOffset(0)] public MOUSEINPUT mouse;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT {
        public uint type;
        public INPUTUNION data;
    }

    public static uint GetWindowProcessId(IntPtr hWnd) {
        uint pid;
        GetWindowThreadProcessId(hWnd, out pid);
        return pid;
    }

    public static bool IsOwnedRootOrPopup(IntPtr window, IntPtr root, uint pid) {
        if (!IsWindow(root) || GetAncestor(root, 2) != root || GetWindowProcessId(root) != pid)
            return false;
        for (int depth = 0; window != IntPtr.Zero && depth < 32; depth++) {
            if (!IsWindow(window) || GetWindowProcessId(window) != pid) return false;
            window = GetAncestor(window, 2);
            if (window == root) return true;
            if (GetWindowProcessId(window) != pid) return false;
            window = GetWindow(window, 4); // GW_OWNER, bounded and same-process at every hop.
        }
        return false;
    }

    public static int[] GetCursorPosition() {
        POINT point;
        if (!GetCursorPos(out point)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        return new int[] { point.X, point.Y };
    }

    public static bool IsKeyDown(int virtualKey) {
        return (GetAsyncKeyState(virtualKey) & 0x8000) != 0;
    }

    public static bool SendMouseWheel(int delta, int count) {
        for (int wheel = 0; wheel < count; wheel++) {
            var inputs = new INPUT[1];
            inputs[0].type = INPUT_MOUSE;
            inputs[0].data.mouse.mouseData = unchecked((uint)delta);
            inputs[0].data.mouse.dwFlags = MOUSEEVENTF_WHEEL;
            if (SendInput(1, inputs, Marshal.SizeOf(typeof(INPUT))) != 1) return false;
        }
        return true;
    }

    public static int[] LastCaptionPoint;
    public static string CaptionActivationResult;

    public static bool ClickOwnedPoint(IntPtr root, uint pid, int x, int y) {
        var inputs = new INPUT[2];
        inputs[0].data.mouse.dwFlags = 2;
        inputs[1].data.mouse.dwFlags = 4;
        foreach (int key in new int[] { 1, 2, 4, 5, 6, 16, 17, 18, 91, 92 })
            if (IsKeyDown(key)) return false;
        POINT cursor;
        if (!IsWindow(root) || GetAncestor(root, 2) != root || GetWindowProcessId(root) != pid ||
            !GetCursorPos(out cursor) || cursor.X != x || cursor.Y != y) return false;
        IntPtr pointWindow = WindowFromPoint(cursor);
        if (GetAncestor(pointWindow, 2) != root || GetWindowProcessId(pointWindow) != pid) return false;
        return SendInput(2, inputs, Marshal.SizeOf(typeof(INPUT))) == 2;
    }

    // A real caption click can establish last-input ownership when background ASFW is denied.
    // Never click a covered point or a titlebar control, and never send keys to the old foreground.
    public static bool ClickOwnedCaption(IntPtr hWnd, uint pid) {
        LastCaptionPoint = null;
        CaptionActivationResult = "NoUncoveredOwnedCaption";
        if (!IsWindow(hWnd) || GetAncestor(hWnd, 2) != hWnd || GetWindowProcessId(hWnd) != pid ||
            !IsWindowVisible(hWnd) || IsIconic(hWnd)) return false;
        foreach (int key in new int[] { 1, 2, 4, 16, 17, 18 })
            if (IsKeyDown(key)) { CaptionActivationResult = "UserInputHeld"; return false; }
        IntPtr dpi = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (dpi == IntPtr.Zero) { CaptionActivationResult = "PhysicalCoordinateContextUnavailable"; return false; }
        int[] original = null;
        try {
            RECT rect;
            if (!GetWindowRect(hWnd, out rect)) return false;
            if (!SetWindowPos(hWnd, IntPtr.Zero, 0, 0, 0, 0, 0x13)) return false; // NOACTIVATE | NOMOVE | NOSIZE
            original = GetCursorPosition();
            for (int yOffset = 8; yOffset <= 32; yOffset += 8) {
                for (int fraction = 3; fraction <= 7; fraction++) {
                    POINT point = new POINT {
                        X = rect.Left + (rect.Right - rect.Left) * fraction / 10,
                        Y = rect.Top + yOffset
                    };
                    if (point.X < -32768 || point.X > 32767 || point.Y < -32768 || point.Y > 32767) continue;
                    if (GetAncestor(WindowFromPoint(point), 2) != hWnd) continue;
                    UIntPtr hit;
                    IntPtr packed = new IntPtr(unchecked((point.Y << 16) | (point.X & 0xffff)));
                    if (SendMessageTimeout(hWnd, 0x84, UIntPtr.Zero, packed, 0x22, 500, out hit) == IntPtr.Zero ||
                        hit.ToUInt64() != 2) continue; // WM_NCHITTEST must return HTCAPTION.
                    if (!SetCursorPos(point.X, point.Y)) return false;
                    if (!IsWindow(hWnd) || GetWindowProcessId(hWnd) != pid ||
                        GetAncestor(WindowFromPoint(point), 2) != hWnd) return false;
                    if (SendMessageTimeout(hWnd, 0x84, UIntPtr.Zero, packed, 0x22, 500, out hit) == IntPtr.Zero ||
                        hit.ToUInt64() != 2) return false;
                    LastCaptionPoint = new int[] { point.X, point.Y };
                    if (!ClickOwnedPoint(hWnd, pid, point.X, point.Y)) {
                        CaptionActivationResult = "OwnedPointOrInputRejected";
                        return false;
                    }
                    System.Threading.Thread.Sleep(150);
                    CaptionActivationResult = GetForegroundWindow() == hWnd ? "OwnedForegroundConfirmed" : "ClickDidNotAcquireForeground";
                    return GetForegroundWindow() == hWnd;
                }
            }
            return false;
        }
        finally {
            if (original != null) SetCursorPos(original[0], original[1]);
            if (dpi != IntPtr.Zero) SetThreadDpiAwarenessContext(dpi);
        }
    }

    // Attach only to the selected target. Never inject an ALT into an unrelated foreground app
    // or change the user's global foreground-lock settings.
    public static bool ForceForeground(IntPtr hWnd) {
        if (!IsWindow(hWnd) || GetAncestor(hWnd, 2) != hWnd) return false;
        AllowSetForegroundWindow(GetWindowProcessId(hWnd));
        uint tidThis = GetCurrentThreadId();
        uint targetPid; uint tidTarget = GetWindowThreadProcessId(hWnd, out targetPid);
        bool attached = tidTarget != tidThis && AttachThreadInput(tidThis, tidTarget, true);
        try {
            if (IsIconic(hWnd)) { ShowWindow(hWnd, SW_RESTORE); }
            BringWindowToTop(hWnd);
            SetForegroundWindow(hWnd);
            SetActiveWindow(hWnd);
            SetFocus(hWnd);
        }
        finally {
            if (attached) { AttachThreadInput(tidThis, tidTarget, false); }
        }
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
        Explicit RequireOwnedForeground contexts may then use one identity-checked, uncovered
        HTCAPTION mouse click, preserving the cursor. No input is sent to a foreign window.
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
        $root = [ItE2E.ItWtWin32Input]::GetAncestor($hwnd, 2)
        if ($root -ne [IntPtr]::Zero -and [ItE2E.ItWtWin32Input]::GetWindowProcessId($root) -eq $App.Pid) {
            $hwnd = $root
            $App.Hwnd = $root.ToInt64()
        }
        if ([ItE2E.ItWtWin32Input]::GetWindowProcessId($hwnd) -ne $App.Pid) {
            throw 'Foreground target no longer belongs to the selected Terminal process.'
        }
        if ($App.PSObject.Properties['RequireOwnedForeground'] -and $App.RequireOwnedForeground) {
            if (-not $App.Launched -or -not $App.OwnedProcess -or $App.OwnedProcess.HasExited -or
                $App.OwnedProcess.Id -ne $App.Pid -or
                $App.OwnedProcess.Path -ne (Join-Path $App.InstallLocation 'WindowsTerminal.exe')) {
                throw 'Foreground acquisition requires the original test-owned executable identity.'
            }
            $current = Get-Process -Id $App.Pid -ErrorAction Stop
            if ($current.StartTime -ne $App.OwnedProcess.StartTime -or $current.Path -ne $App.OwnedProcess.Path) {
                throw 'Foreground acquisition requires the captured process/start-time lease.'
            }
            if ([ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup(
                [ItE2E.ItWtWin32Input]::GetForegroundWindow(), $hwnd, [uint32]$App.Pid)) {
                return $true
            }
        }
        try {
            $desktop = [ItE2E.ItWtWin32Input]::OpenInputDesktop(0, $false, 1)
            if ($desktop -eq [IntPtr]::Zero) { throw 'Interactive input desktop unavailable; stop the batch before sending input.' }
            [void][ItE2E.ItWtWin32Input]::CloseDesktop($desktop)
            $null = [ItE2E.ItWtWin32Input]::GetCursorPosition()
        }
        catch {
            if ($env:ITE2E_INPUT_FAILURE_RECEIPT) {
                @{ reason = 'InputDesktopUnavailable'; at = [datetimeoffset]::UtcNow.ToString('o')
                    error = $_.Exception.Message; owned_pid = $App.Pid; hwnd = $hwnd.ToInt64()
                } | ConvertTo-Json | Set-Content -LiteralPath $env:ITE2E_INPUT_FAILURE_RECEIPT
            }
            throw
        }
        for ($i = 0; $i -lt $Attempts; $i++) {
            if ([ItE2E.ItWtWin32Input]::ForceForeground($hwnd)) { return $true }
            Start-Sleep -Milliseconds $DelayMs
        }
        if ($App.PSObject.Properties['RequireOwnedForeground'] -and $App.RequireOwnedForeground) {
            $current = Get-Process -Id $App.Pid -ErrorAction Stop
            if ($current.StartTime -ne $App.OwnedProcess.StartTime -or
                $current.Path -ne $App.OwnedProcess.Path -or $App.OwnedProcess.HasExited) {
                throw 'Caption activation requires the original live test-owned process/start-time identity.'
            }
            if ([ItE2E.ItWtWin32Input]::ClickOwnedCaption($hwnd, [uint32]$App.Pid)) { return $true }
        }
        if ($env:ITE2E_INPUT_FAILURE_RECEIPT) {
            @{
                reason = 'OwnedForegroundUnavailable'; at = [datetimeoffset]::UtcNow.ToString('o')
                hwnd = $hwnd.ToInt64(); owned_pid = $App.Pid
                foreground_hwnd = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
                foreground_pid = [ItE2E.ItWtWin32Input]::GetWindowProcessId([ItE2E.ItWtWin32Input]::GetForegroundWindow())
                caption_activation = [ItE2E.ItWtWin32Input]::CaptionActivationResult
                caption_point = [ItE2E.ItWtWin32Input]::LastCaptionPoint
            } | ConvertTo-Json | Set-Content -LiteralPath $env:ITE2E_INPUT_FAILURE_RECEIPT
        }
        $false
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

function Invoke-WtWindowWheel {
    <#
    .SYNOPSIS
        Send physical mouse-wheel input at an absolute screen coordinate in the test-owned window.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][int]$ScreenX,
        [Parameter(Mandatory)][int]$ScreenY,
        [ValidateSet(-120, 120)][int]$Delta,
        [ValidateRange(1, 100)][int]$Count = 1,
        [switch]$Ctrl,
        [bool]$RestoreCursor = $true
    )
    process {
        if (-not $App.Hwnd -or -not $App.Pid) {
            throw 'Invoke-WtWindowWheel requires App.Hwnd and App.Pid from Start-Terminal.'
        }
        Initialize-WtWin32Input
        $hwnd = [IntPtr][int64]$App.Hwnd
        $actualPid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($hwnd)
        if ([int]$actualPid -ne [int]$App.Pid) {
            throw "HWND $($App.Hwnd) belongs to process $actualPid, not expected process $($App.Pid)."
        }
        if (-not (Set-WtWindowForeground -App $App)) {
            throw "Invoke-WtWindowWheel could not bring HWND $($App.Hwnd) to the foreground; no input was sent."
        }

        $original = if ($RestoreCursor) { [ItE2E.ItWtWin32Input]::GetCursorPosition() } else { $null }
        $pressedCtrl = $false
        try {
            if (-not [ItE2E.ItWtWin32Input]::SetCursorPos($ScreenX, $ScreenY)) {
                throw "Invoke-WtWindowWheel could not move the cursor to screen coordinate ($ScreenX,$ScreenY)."
            }
            if ($Ctrl -and -not [ItE2E.ItWtWin32Input]::IsKeyDown(0x11)) {
                [ItE2E.ItWtWin32Input]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
                $pressedCtrl = $true
            }
            if (-not [ItE2E.ItWtWin32Input]::SendMouseWheel($Delta, $Count)) {
                $code = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
                throw "Invoke-WtWindowWheel SendInput failed with Win32 error $code."
            }
        }
        finally {
            if ($pressedCtrl) { [ItE2E.ItWtWin32Input]::keybd_event(0x11, 0, 0x2, [UIntPtr]::Zero) }
            if ($original) { [void][ItE2E.ItWtWin32Input]::SetCursorPos($original[0], $original[1]) }
        }

        Start-Sleep -Milliseconds 250
        $App
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
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][AllowEmptyString()][string[]]$UiArgs, [int]$TimeoutSec = 30, [switch]$NoTarget)
    if ($UiArgs[0] -eq 'drag') { throw 'Opaque winapp drag is refused; use guarded native Invoke-UiMouseDrag.' }
    if ($UiArgs[0] -eq 'hover') {
        if ($NoTarget) { throw 'Hover requires its exact owned root target.' }
        $dwell = 1200
        $index = [array]::IndexOf($UiArgs, '--dwell-time')
        if ($index -ge 0) { $dwell = [int]$UiArgs[$index + 1] }
        Invoke-ItOwnedHover -App $App -Selector $UiArgs[1] -DwellMs $dwell | Out-Null
        return [pscustomobject]@{ ExitCode = 0; StdOut = ''; StdErr = ''; TimedOut = $false }
    }
    $winapp = Get-WinAppPath
    $args = @('ui') + $UiArgs
    if ($App.PSObject.Properties['RequireOwnedForeground'] -and $App.RequireOwnedForeground -and
        $UiArgs[0] -in @('click', 'invoke', 'set-value')) {
        if ($NoTarget -or -not $App.Launched -or -not $App.OwnedProcess -or
            $App.OwnedProcess.HasExited -or $App.OwnedProcess.Id -ne $App.Pid) {
            throw 'UI input requires the captured live test-owned Terminal.'
        }
        Initialize-WtWin32Input
        $hwnd = [IntPtr][int64]$App.Hwnd
        $current = Get-Process -Id $App.Pid -ErrorAction Stop
        if ($current.StartTime -ne $App.OwnedProcess.StartTime -or
            $current.Path -ne (Join-Path $App.InstallLocation 'WindowsTerminal.exe') -or
            [ItE2E.ItWtWin32Input]::GetAncestor($hwnd, 2) -ne $hwnd -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($hwnd) -ne $App.Pid -or
            @($UiArgs | Where-Object { $_ -match '^(?:-a|-w|--app|--window)(?:=|$)' }).Count) {
            throw 'UI input requires the original root-scoped HWND and process/start-time lease.'
        }
        # invoke and set-value use UIA patterns, not physical input (winapp 0.6.1 help).
        # Refocusing the root here can dismiss the owned popup containing the target.
        if ($UiArgs[0] -eq 'click' -and -not (Set-WtWindowForeground -App $App -Attempts 3 -DelayMs 150)) {
            $foregroundPid = [ItE2E.ItWtWin32Input]::GetWindowProcessId(
                [ItE2E.ItWtWin32Input]::GetForegroundWindow())
            throw "Owned Terminal cannot acquire foreground; competing PID=$foregroundPid. No UI input sent."
        }
    }
    if (-not $NoTarget) { $args += (Get-UiTarget -App $App) }
    $start = [Diagnostics.ProcessStartInfo]::new($winapp)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.Environment['WINAPP_CLI_TELEMETRY_OPTOUT'] = '1'
    foreach ($argument in $args) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $timedOut = -not (Wait-ItProcessDeadline -Process $process -TimeoutSec $TimeoutSec)
        if ($timedOut -and -not $process.HasExited) {
            [void]$process.CloseMainWindow()
            if (-not (Wait-ItProcessDeadline -Process $process -TimeoutSec 1)) {
                if (-not $process.HasExited) { $process.Kill() }
                if (-not (Wait-ItProcessDeadline -Process $process -TimeoutSec 2)) {
                    throw 'Owned winapp process did not exit after bounded shutdown.'
                }
            }
        }
        # Descendants retaining redirected pipes must not turn a bounded process wait into an
        # unbounded GetResult. Keep failed drains explicit rather than fabricating UI success.
        $drainDeadline = [datetimeoffset]::UtcNow.AddSeconds(2)
        $drainClock = [Diagnostics.Stopwatch]::StartNew()
        while (-not ($stdout.IsCompleted -and $stderr.IsCompleted)) {
            if (Test-ItProcessDeadline $drainDeadline $drainClock.Elapsed.TotalSeconds 2) {
                throw 'winapp output streams did not close within the drain deadline.'
            }
            Start-Sleep -Milliseconds 50
        }
        [pscustomobject]@{
            ExitCode = if ($timedOut) { -1 } else { $process.ExitCode }
            StdOut = $stdout.GetAwaiter().GetResult()
            StdErr = $stderr.GetAwaiter().GetResult()
            TimedOut = $timedOut
            Command = "$winapp $($args -join ' ')"
        }
    }
    finally { $process.Dispose() }
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

function Find-ItExactTextRange {
    param(
        [Parameter(Mandatory)]$DocumentRange,
        [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$Text
    )
    $range = $DocumentRange.FindText($Text, $false, $false)
    if (-not $range) { throw 'UIA text was not found in the requested control.' }
    $observed = $range.GetText(-1)
    if ([string]::Equals($observed, $Text, [StringComparison]::Ordinal)) { return $range }
    if (-not $observed.StartsWith($Text, [StringComparison]::Ordinal)) {
        throw 'UIA range does not exactly match the requested text.'
    }

    # Some providers include a trailing cell. Shorten in provider units, not UTF-16 offsets.
    $remaining = $observed.Length - $Text.Length
    while ($remaining-- -gt 0) {
        $moved = $range.MoveEndpointByUnit(
            [System.Windows.Automation.Text.TextPatternRangeEndpoint]::End,
            [System.Windows.Automation.Text.TextUnit]::Character, -1)
        if ($moved -ne -1) { throw 'UIA could not adjust the text range endpoint.' }
        $next = $range.GetText(-1)
        if ([string]::Equals($next, $Text, [StringComparison]::Ordinal)) { return $range }
        if ($next.Length -ge $observed.Length -or -not $next.StartsWith($Text, [StringComparison]::Ordinal)) {
            throw 'UIA range does not exactly match the requested text after endpoint adjustment.'
        }
        $observed = $next
    }
    throw 'UIA could not establish the exact text range endpoint.'
}

function Get-UiTextBounds {
    <# Return UIA TextPattern bounding rectangles for exact text in a named control. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$Text,
        [string]$ControlName = 'Agent Pane'
    )
    process {
        if (-not $App.Hwnd) { throw "Get-UiTextBounds needs `$App.Hwnd (launch via Start-Terminal)." }
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$App.Hwnd)
        if (-not $root -or $root.Current.ProcessId -ne $App.Pid) {
            throw 'The UIA text window does not belong to the test application.'
        }
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            $ControlName
        )
        $controls = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) |
            Where-Object { $_.Current.ClassName -eq 'TermControl' -and -not $_.Current.IsOffscreen }
        if (@($controls).Count -ne 1) { throw "Expected one visible UIA text control named '$ControlName'." }
        $pattern = $controls[0].GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
        $range = Find-ItExactTextRange -DocumentRange $pattern.DocumentRange -Text $Text
        @($range.GetBoundingRectangles())
    }
}

function Invoke-UiMouseDrag {
    <# Send a real mouse drag between absolute screen coordinates through Windows App CLI. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][int]$FromX,
        [Parameter(Mandatory)][int]$FromY,
        [Parameter(Mandatory)][int]$ToX,
        [Parameter(Mandatory)][int]$ToY,
        [switch]$Right,
        [int]$HoldMs = 50
    )
    process {
        if ($Right -or $FromX -ne $ToX -or $FromY -ne $ToY) {
            throw 'Native drag is disabled: safe release after foreign-surface loss is not guaranteed. Use the verified canonical context-menu helper.'
        }
        $peer = Get-ItOwnedPointerPeer -App $App -X $FromX -Y $FromY
        $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$App.Hwnd)
        $target = $peer
        $invoke = $null
        while ($target -and -not [Windows.Automation.Automation]::Compare($target, $root)) {
            if ($target.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$invoke)) { break }
            if ($target.Current.ControlType -eq [Windows.Automation.ControlType]::ListItem) { break }
            $target = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($target)
        }
        if (-not $invoke) { throw 'The exact owned same-point activation exposes no public InvokePattern; physical drag fallback is refused.' }
        [void](Get-ItOwnedPointerPeer -App $App -X $FromX -Y $FromY -ExpectedPeer $target)
        $invoke.Invoke()
        $App
    }
}

function Assert-ItPointerFacts {
    param([Parameter(Mandatory)][hashtable]$Facts)
    foreach ($key in @('Lease', 'RunReceipt', 'NativeRoot', 'Foreground', 'NativeHit', 'DeepHit',
        'StablePeer', 'VisiblePeer', 'Cursor', 'NoHeldInput', 'NoOverlay')) {
        if (-not $Facts[$key]) { throw "Owned pointer input refused: $key" }
    }
}

function Get-ItOwnedPointerPeer {
    param($App, [int]$X, [int]$Y, $ExpectedPeer, [int]$AllowedHeldButton = 0, $ExpectedCursor)
    Initialize-WtWin32Input
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $root = [IntPtr][long]$App.Hwnd
    $current = Get-Process -Id $App.Pid -ErrorAction Stop
    $records = @(Get-Content -LiteralPath $App.InputReceiptPath -ErrorAction Stop | ForEach-Object { $_ | ConvertFrom-Json })
    $point = [ItE2E.ItWtWin32Input+POINT]::new(); $point.X = $X; $point.Y = $Y
    $native = [ItE2E.ItWtWin32Input]::WindowFromPoint($point)
    $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
    $peer = [Windows.Automation.AutomationElement]::FromPoint([Windows.Point]::new($X, $Y))
    $window = [Windows.Automation.AutomationElement]::FromHandle($root)
    $deep = -not [Windows.Automation.Automation]::Compare($peer, $window)
    $stable = -not $ExpectedPeer
    $overlay = $false
    $ancestor = $peer
    while ($ancestor) {
        if ($ExpectedPeer -and [Windows.Automation.Automation]::Compare($ancestor, $ExpectedPeer)) { $stable = $true }
        if ($ancestor.Current.ClassName -match 'Popup|Flyout|ContentDialog|FreOverlay' -or
            $ancestor.Current.ControlType -in @([Windows.Automation.ControlType]::ToolTip, [Windows.Automation.ControlType]::Menu)) { $overlay = $true }
        if ([Windows.Automation.Automation]::Compare($ancestor, $window)) { break }
        $ancestor = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($ancestor)
    }
    $deep = $deep -and $ancestor -and [Windows.Automation.Automation]::Compare($ancestor, $window)
    $held = @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92) | Where-Object {
        $_ -ne $AllowedHeldButton -and [ItE2E.ItWtWin32Input]::IsKeyDown($_)
    }
    Assert-ItPointerFacts @{
        Lease = ($App.Launched -and $App.OwnedProcess -and -not $App.OwnedProcess.HasExited -and
            $App.OwnedProcess.Id -eq $current.Id -and $current.StartTime -eq $App.OwnedProcess.StartTime -and
            $current.Path -eq (Join-Path $App.InstallLocation 'WindowsTerminal.exe'))
        RunReceipt = (@($records | Where-Object { $_.pid -eq $App.Pid -and $_.path -eq $current.Path -and
            $_.run_token -ceq $App.InputRunToken -and ([datetimeoffset]$_.start_utc).UtcDateTime.Ticks -eq
                $current.StartTime.ToUniversalTime().Ticks }).Count -eq 1)
        NativeRoot = ([ItE2E.ItWtWin32Input]::IsWindow($root) -and
            [ItE2E.ItWtWin32Input]::GetAncestor($root, 2) -eq $root -and
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($root) -eq $App.Pid)
        Foreground = ([ItE2E.ItWtWin32Input]::GetForegroundWindow() -eq $root)
        NativeHit = ([ItE2E.ItWtWin32Input]::GetAncestor($native, 2) -eq $root -and
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($native) -eq $App.Pid)
        DeepHit = $deep; StablePeer = $stable
        VisiblePeer = ($peer.Current.ProcessId -eq $App.Pid -and -not $peer.Current.IsOffscreen -and
            $peer.Current.BoundingRectangle.Contains([Windows.Point]::new($X, $Y)))
        Cursor = (-not $ExpectedCursor -or ($cursor[0] -eq $ExpectedCursor[0] -and $cursor[1] -eq $ExpectedCursor[1]))
        NoHeldInput = (-not @($held).Count); NoOverlay = (-not $overlay)
    }
    $peer
}

function Send-ItPointerButton {
    param($App, [uint32]$Flag, [int]$X, [int]$Y, $Peer, [int]$AllowedHeldButton = 0)
    [void](Get-ItOwnedPointerPeer -App $App -X $X -Y $Y -ExpectedPeer $Peer `
        -AllowedHeldButton $AllowedHeldButton -ExpectedCursor @($X, $Y))
    throw 'Unpaired mouse-button injection is disabled; use the trusted paired canonical input path or UIA InvokePattern.'
}

function Invoke-ItOwnedPointer {
    param($App, [int]$FromX, [int]$FromY, [int]$ToX, [int]$ToY,
        [switch]$Right, [int]$HoldMs = 50, [int]$HoverMs = 0, $ExpectedPeer, [scriptblock]$DuringHover)
    if (-not $HoverMs) { throw 'Mouse-down drag injection is disabled; no foreign-surface mouse-up cleanup is attempted.' }
    Initialize-WtWin32Input
    $original = [ItE2E.ItWtWin32Input]::GetCursorPosition()
    $from = Get-ItOwnedPointerPeer -App $App -X $FromX -Y $FromY -ExpectedPeer $ExpectedPeer
    $to = Get-ItOwnedPointerPeer -App $App -X $ToX -Y $ToY
    $down = $false; $primary = $null; $button = if ($Right) { 2 } else { 1 }
    $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
    if ($dpi -eq [IntPtr]::Zero) { throw 'Owned pointer physical-coordinate context unavailable.' }
    try {
        [void](Get-ItOwnedPointerPeer -App $App -X $FromX -Y $FromY -ExpectedPeer $from -ExpectedCursor $original)
        [ItE2E.ItWtWin32Input]::SetCursorPos($FromX, $FromY) | Out-Null
        [void](Get-ItOwnedPointerPeer -App $App -X $FromX -Y $FromY -ExpectedPeer $from -ExpectedCursor @($FromX, $FromY))
        if ($HoverMs) {
            $clock = [Diagnostics.Stopwatch]::StartNew()
            while ($clock.ElapsedMilliseconds -lt $HoverMs) {
                Start-Sleep -Milliseconds 100
                [void](Get-ItOwnedPointerPeer -App $App -X $FromX -Y $FromY -ExpectedPeer $from -ExpectedCursor @($FromX, $FromY))
            }
            if ($DuringHover) { & $DuringHover }
        }
        else {
            Send-ItPointerButton -App $App -Flag $(if ($Right) { 8 } else { 2 }) -X $FromX -Y $FromY -Peer $from
            $down = $true
            Start-Sleep -Milliseconds $HoldMs
            [void](Get-ItOwnedPointerPeer -App $App -X $ToX -Y $ToY -ExpectedPeer $to -AllowedHeldButton $button -ExpectedCursor @($FromX, $FromY))
            [ItE2E.ItWtWin32Input]::SetCursorPos($ToX, $ToY) | Out-Null
            [void](Get-ItOwnedPointerPeer -App $App -X $ToX -Y $ToY -ExpectedPeer $to -AllowedHeldButton $button -ExpectedCursor @($ToX, $ToY))
            Send-ItPointerButton -App $App -Flag $(if ($Right) { 16 } else { 4 }) -X $ToX -Y $ToY -Peer $to -AllowedHeldButton $button
            $down = $false
        }
    }
    catch { $primary = $_; throw }
    finally {
        try {
            if ($down) {
                $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
                $releasePeer = if ($cursor[0] -eq $ToX -and $cursor[1] -eq $ToY) { $to } else { $from }
                Send-ItPointerButton -App $App -Flag $(if ($Right) { 16 } else { 4 }) `
                    -X $cursor[0] -Y $cursor[1] -Peer $releasePeer -AllowedHeldButton $button
            }
            $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
            [void](Get-ItOwnedPointerPeer -App $App -X $cursor[0] -Y $cursor[1] -ExpectedCursor $cursor)
            [ItE2E.ItWtWin32Input]::SetCursorPos($original[0], $original[1]) | Out-Null
        }
        catch {
            if ($primary) { throw [AggregateException]::new('Pointer action and owned cleanup both failed.',
                [Exception[]]@($primary.Exception, $_.Exception)) }
            throw
        }
        finally { [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) }
    }
}

function Invoke-ItOwnedHover {
    param($App, [string]$Selector, [ValidateRange(1, 10000)][int]$DwellMs = 1200, [scriptblock]$DuringHover)
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$App.Hwnd)
    $condition = [Windows.Automation.OrCondition]::new(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $Selector),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Selector))
    $peers = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
        $_.Current.ProcessId -eq $App.Pid -and -not $_.Current.IsOffscreen -and $_.Current.IsEnabled
    })
    if ($peers.Count -ne 1) { throw 'Hover requires one exact visible owned UIA peer; opaque selector fallback refused.' }
    $bounds = $peers[0].Current.BoundingRectangle
    $x = [int]($bounds.Left + $bounds.Width / 2); $y = [int]($bounds.Top + $bounds.Height / 2)
    Invoke-ItOwnedPointer -App $App -FromX $x -FromY $y -ToX $x -ToY $y -HoverMs $DwellMs -ExpectedPeer $peers[0] -DuringHover $DuringHover
}

function Set-UiValue {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Selector, [Parameter(Mandatory)][AllowEmptyString()][string]$Value)
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
