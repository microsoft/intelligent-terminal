function Enable-TestWindowEnglishKeyboardLayout {
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App)

    if (-not ('ItE2E.TestWindowKeyboardLayout' -as [type])) {
        Add-Type -Namespace ItE2E -Name TestWindowKeyboardLayout -MemberDefinition @'
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")] public static extern IntPtr GetKeyboardLayout(uint threadId);
    [DllImport("user32.dll")] public static extern int GetKeyboardLayoutList(int count, [Out] IntPtr[] layouts);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
'@
    }

    $window = [IntPtr][int64]$App.Hwnd
    [uint32]$windowPid = 0
    $thread = [ItE2E.TestWindowKeyboardLayout]::GetWindowThreadProcessId($window, [ref]$windowPid)
    if ($windowPid -ne $App.Pid) { throw 'Keyboard-layout target does not belong to the test window.' }

    $previousLayout = [ItE2E.TestWindowKeyboardLayout]::GetKeyboardLayout($thread)
    $layoutCount = [ItE2E.TestWindowKeyboardLayout]::GetKeyboardLayoutList(0, $null)
    $layouts = [IntPtr[]]::new($layoutCount)
    [void][ItE2E.TestWindowKeyboardLayout]::GetKeyboardLayoutList($layoutCount, $layouts)
    $englishLayouts = @($layouts | Where-Object { $_.ToInt64() -eq 0x04090409 } | Select-Object -First 1)
    if ($englishLayouts.Count -ne 1) {
        throw 'Physical letter-key assertions require an already loaded English (US) keyboard layout.'
    }

    $testLayout = $englishLayouts[0]
    $changed = $previousLayout -ne $testLayout
    $context = [pscustomobject]@{
        Window = $window
        Pid = [uint32]$windowPid
        Thread = [uint32]$thread
        PreviousLayout = $previousLayout
        TestLayout = $testLayout
        Changed = $false
    }
    if ($changed) {
        $activated = $false
        try {
            if (-not [ItE2E.TestWindowKeyboardLayout]::PostMessage($window, 0x0050, [IntPtr]::Zero, $testLayout)) {
                throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
            }
            $context.Changed = $true
            Wait-Until -TimeoutSec 5 -Because 'the test window to activate its deterministic keyboard layout' -Condition ({
                [ItE2E.TestWindowKeyboardLayout]::GetKeyboardLayout($thread) -eq $testLayout
            }.GetNewClosure()) | Out-Null
            $activated = $true
        }
        finally {
            if (-not $activated -and $context.Changed) {
                Restore-TestWindowKeyboardLayout -App $App -Context $context
            }
        }
    }

    $context
}

function Restore-TestWindowKeyboardLayout {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$App,
        [Parameter(Mandatory)]$Context
    )

    if (-not $Context.Changed) { return }

    [uint32]$windowPid = 0
    $thread = [ItE2E.TestWindowKeyboardLayout]::GetWindowThreadProcessId($Context.Window, [ref]$windowPid)
    if ($thread -ne $Context.Thread -or $windowPid -ne $App.Pid -or $windowPid -ne $Context.Pid) {
        throw 'Keyboard-layout restoration target no longer belongs to the test window.'
    }
    if (-not [ItE2E.TestWindowKeyboardLayout]::PostMessage($Context.Window, 0x0050, [IntPtr]::Zero, $Context.PreviousLayout)) {
        throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
    }
    Wait-Until -TimeoutSec 5 -Because 'the test window to restore its original keyboard layout' -Condition ({
        [ItE2E.TestWindowKeyboardLayout]::GetKeyboardLayout($Context.Thread) -eq $Context.PreviousLayout
    }.GetNewClosure()) | Out-Null
}
