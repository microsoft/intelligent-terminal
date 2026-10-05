function Invoke-TestAgentInputClick {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$App,
        [Parameter(Mandatory)][string]$Anchor,
        [ValidateRange(0, 4096)][int]$Column = 0
    )

    $window = [IntPtr][int64]$App.Hwnd
    if ([ItE2E.ItWtWin32Input]::GetWindowProcessId($window) -ne $App.Pid -or
        [ItE2E.ItWtWin32Input]::GetForegroundWindow() -ne $window) {
        throw 'Physical click target lost its process identity or foreground.'
    }
    foreach ($key in 0x01, 0x10, 0x11, 0x12, 0x5B, 0x5C) {
        if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) {
            throw 'Physical click requires released mouse and modifier keys.'
        }
    }
    if ($Anchor -notmatch '^[\x20-\x7E]+$') {
        throw 'The geometry anchor must be printable ASCII; the target may follow it.'
    }
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, 'Agent Pane')
    $controls = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) |
        Where-Object { $_.Current.ClassName -eq 'TermControl' -and -not $_.Current.IsOffscreen }
    if (@($controls).Count -ne 1) { throw 'Expected one visible test-owned Agent Pane.' }
    $pattern = $controls[0].GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $range = $pattern.DocumentRange.FindText($Anchor, $false, $false)
    if ($range) {
        # FindText supplies the source position; bound its range to exactly the ASCII anchor.
        $end = [System.Windows.Automation.Text.TextPatternRangeEndpoint]::End
        $start = [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start
        $range.MoveEndpointByRange($end, $range, $start)
        $moved = $range.MoveEndpointByUnit($end, [System.Windows.Automation.Text.TextUnit]::Character, $Anchor.Length)
        if ($moved -ne $Anchor.Length) { throw 'UIA could not bound the complete anchor.' }
    }
    if (-not $range -or $range.GetText(-1) -cne $Anchor) {
        $observed = if ($range) { $range.GetText(-1) } else { '<no match>' }
        throw "UIA anchor mismatch: expected '$Anchor'; range '$observed'; document contains anchor=$($pattern.DocumentRange.GetText(-1).Contains($Anchor))."
    }
    $rectangles = @($range.GetBoundingRectangles())
    if ($rectangles.Count -ne 1 -or $rectangles[0].Width -le 0 -or $rectangles[0].Height -le 0) {
        throw 'The unique visible text anchor must occupy one UIA rectangle.'
    }
    $rect = $rectangles[0]
    $cellWidth = $rect.Width / $Anchor.Length
    $x = [int][Math]::Floor($rect.Left + (($Column + 0.5) * $cellWidth))
    $y = [int][Math]::Floor($rect.Top + ($rect.Height / 2))
    if (-not $controls[0].Current.BoundingRectangle.Contains([double]$x, [double]$y)) {
        throw 'Calculated click point is outside the visible test-owned Agent Pane bounds.'
    }
    $hit = [System.Windows.Automation.AutomationElement]::FromPoint(
        [System.Windows.Point]::new($x, $y))
    if ($hit.Current.ProcessId -ne $App.Pid) {
        throw 'Another process covers the calculated click target.'
    }

    $downMouse = [ItE2E.ItWtWin32Input+MOUSEINPUT]::new()
    $downMouse.dwFlags = 0x0002
    $downUnion = [ItE2E.ItWtWin32Input+INPUTUNION]::new()
    $downUnion.mouse = $downMouse
    $down = [ItE2E.ItWtWin32Input+INPUT]::new()
    $down.data = $downUnion
    $upMouse = [ItE2E.ItWtWin32Input+MOUSEINPUT]::new()
    $upMouse.dwFlags = 0x0004
    $upUnion = [ItE2E.ItWtWin32Input+INPUTUNION]::new()
    $upUnion.mouse = $upMouse
    $up = [ItE2E.ItWtWin32Input+INPUT]::new()
    $up.data = $upUnion
    $size = [Runtime.InteropServices.Marshal]::SizeOf($down)
    $previous = [ItE2E.ItWtWin32Input]::GetCursorPosition()
    try {
        if (-not [ItE2E.ItWtWin32Input]::SetCursorPos($x, $y)) {
            throw 'Could not position the physical mouse at the input cell.'
        }
        $sent = [ItE2E.ItWtWin32Input]::SendInput(
            2, [ItE2E.ItWtWin32Input+INPUT[]]@($down, $up), $size)
        if ($sent -eq 1) {
            if ([ItE2E.ItWtWin32Input]::SendInput(1, [ItE2E.ItWtWin32Input+INPUT[]]@($up), $size) -ne 1) {
                throw 'Could not release the mouse button pressed by this test.'
            }
        }
        if ($sent -ne 2) { throw 'The complete physical down/up click was not delivered.' }
        if ([ItE2E.ItWtWin32Input]::GetForegroundWindow() -ne $window -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($window) -ne $App.Pid) {
            throw 'Physical click lost its target window.'
        }
    }
    finally {
        if (-not [ItE2E.ItWtWin32Input]::SetCursorPos($previous[0], $previous[1])) {
            throw 'Could not restore the original mouse position.'
        }
    }
    [pscustomobject]@{ X = $x; Y = $y; CellWidth = $cellWidth; Anchor = $Anchor; Column = $Column }
}
