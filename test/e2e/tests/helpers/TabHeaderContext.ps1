function Assert-TestTabHeaderPointSafety {
    param([Parameter(Mandatory)][hashtable]$Guard)
    foreach ($name in @('ProtocolOwned', 'FullyVisible', 'InHeaderBand', 'OutsideActions', 'Stable',
        'LeaseOwned', 'RunLease', 'NativeOwned', 'ForegroundOwned', 'NoKnownOverlay')) {
        if (-not $Guard[$name]) { throw "Missing independent canonical header guard: $name" }
    }
    if ($Guard.ForbiddenHit -or (-not $Guard.DeepHit -and -not $Guard.OpaqueRoot)) {
        throw 'Canonical tab header point is covered or belongs to a pane.'
    }
}

function Test-KnownTabHeaderOverlayPeer {
    param($Peer, [string]$PalettePattern, [Windows.Point]$Point, $App, $Peers)
    $p = $Peer.Current
    $known = -not $p.IsOffscreen -and $p.BoundingRectangle.Width -gt 0 -and $p.BoundingRectangle.Height -gt 0 -and (
        $p.ControlType -in @([Windows.Automation.ControlType]::Menu, [Windows.Automation.ControlType]::MenuItem,
            [Windows.Automation.ControlType]::ToolTip) -or $p.ClassName -match 'Popup|Flyout|ContentDialog|FreOverlay' -or
        $p.AutomationId -in @('WelcomePage', 'SettingsPage', 'FreOverlayElement', 'SettingsNav', 'SearchTextBox', 'CommandPaletteElement') -or
        ($PalettePattern -and $p.Name -match $PalettePattern))
    if (-not $known) { return $false }
    if (-not $Point -or -not $App) { return $true }
    if ($p.ProcessId -ne $App.Pid) { return $true }
    # Settings, FRE and modal/palette surfaces remain authoritative global gates.
    if ($p.ClassName -match 'ContentDialog|FreOverlay' -or
        $p.AutomationId -in @('WelcomePage', 'SettingsPage', 'FreOverlayElement', 'SettingsNav', 'CommandPaletteElement') -or
        ($PalettePattern -and $p.Name -match $PalettePattern)) { return $true }
    if ($p.ClassName -ne 'Xaml_WindowedPopupClass') { return $p.BoundingRectangle.Contains($Point) }
    try {
        $hostHandle = [IntPtr][long]$p.NativeWindowHandle
        $np = [ItE2E.ItWtWin32Input+POINT]::new()
        $np.X = [int]$Point.X; $np.Y = [int]$Point.Y
        $hit = [ItE2E.ItWtWin32Input]::WindowFromPoint($np)
        $root = [IntPtr][long]$App.Hwnd
        if ($hostHandle -eq [IntPtr]::Zero -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($hostHandle) -ne $App.Pid -or
            $hit -ne $root -or [ItE2E.ItWtWin32Input]::GetAncestor($hit, 2) -ne $root -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($hit) -ne $App.Pid) { return $true }
        $children = @($Peer.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition))
        if ($children.Count -ne 1 -or $children[0].Current.ProcessId -ne $App.Pid -or
            $children[0].Current.ClassName -ne 'Popup' -or
            $children[0].Current.ControlType -ne [Windows.Automation.ControlType]::Window -or
            -not $children[0].Current.IsOffscreen -or -not $children[0].Current.BoundingRectangle.IsEmpty) { return $true }
        $childId = @($children[0].GetRuntimeId()) -join ','
        $hostId = @($Peer.GetRuntimeId()) -join ','
        $linked = 0
        foreach ($tooltip in @($Peers | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::ToolTip })) {
            if ($tooltip.Current.ProcessId -ne $App.Pid) { return $true }
            $ancestor = $tooltip
            $foundChild = $false
            $foundHost = $false
            for ($depth = 0; $ancestor -and $depth -lt 8; $depth++) {
                if ($ancestor.Current.ProcessId -ne $App.Pid) { break }
                $id = @($ancestor.GetRuntimeId()) -join ','
                if ($id -eq $childId) { $foundChild = $true }
                if ($id -eq $hostId -and $ancestor.Current.NativeWindowHandle -eq $p.NativeWindowHandle) {
                    $foundHost = $true
                    break
                }
                $ancestor = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($ancestor)
            }
            if ($foundHost) {
                if (-not $foundChild -or $tooltip.Current.IsOffscreen -or
                    $tooltip.Current.BoundingRectangle.IsEmpty -or $tooltip.Current.BoundingRectangle.Width -le 0 -or
                    $tooltip.Current.BoundingRectangle.Height -le 0 -or $tooltip.Current.BoundingRectangle.Contains($Point)) { return $true }
                $linked++
            }
        }
        if ($linked -eq 0) { return $true }
        $fresh = @($Peer.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition))
        if ($fresh.Count -ne 1 -or (@($fresh[0].GetRuntimeId()) -join ',') -ne $childId -or
            -not $fresh[0].Current.IsOffscreen -or -not $fresh[0].Current.BoundingRectangle.IsEmpty -or
            (@($Peer.GetRuntimeId()) -join ',') -ne $hostId -or $Peer.Current.NativeWindowHandle -ne $hostHandle.ToInt64()) { return $true }
        return $false
    } catch { return $true }
}

function Get-TestTabHeaderOverlayReceipt {
    param($Peer, [int]$OwnedPid)
    $p = $Peer.Current
    if ($p.ProcessId -ne $OwnedPid) { return }
    @{
        ControlType = $p.ControlType.ProgrammaticName; AutomationId = $p.AutomationId
        Class = $p.ClassName; IsOffscreen = $p.IsOffscreen; Bounds = $p.BoundingRectangle.ToString()
    }
}

function Get-TestTabHeaderFinalReasons {
    param([hashtable]$Facts)
    @($Facts.Keys | Where-Object { $Facts[$_] -is [bool] -and -not $Facts[$_] } | Sort-Object)
}

function Close-TestOwnedTabFlyout {
    param([Parameter(Mandatory)]$App, [Windows.Point]$HeaderPoint)
    $runToken = $env:ITE2E_RUN_TOKEN
    $receiptPath = $env:ITE2E_OWNED_PROCESS_RECEIPT
    $checkLease = {
        $current = Get-Process -Id $App.Pid -ErrorAction Stop
        if (-not $App.Launched -or -not $App.OwnedProcess -or $App.OwnedProcess.HasExited -or
            $App.OwnedProcess.Id -ne $App.Pid -or $current.StartTime -ne $App.OwnedProcess.StartTime -or
            $current.Path -ne (Join-Path $App.InstallLocation 'WindowsTerminal.exe')) {
            throw 'Tab flyout dismissal requires the original owned process lease.'
        }
        if (-not $runToken -or -not $receiptPath -or $env:ITE2E_RUN_TOKEN -cne $runToken -or
            $env:ITE2E_OWNED_PROCESS_RECEIPT -cne $receiptPath -or -not (Test-Path -LiteralPath $receiptPath)) {
            throw 'Tab flyout dismissal requires its captured run receipt.'
        }
        $records = @(Get-Content -LiteralPath $receiptPath | ForEach-Object { $_ | ConvertFrom-Json })
        if (@($records | Where-Object { $_.pid -eq $App.Pid -and $_.path -eq $current.Path -and
            $_.run_token -ceq $runToken -and ([datetimeoffset]$_.start_utc).UtcDateTime.Ticks -eq
            $current.StartTime.ToUniversalTime().Ticks }).Count -ne 1) {
            throw 'Tab flyout dismissal run/process identity is ambiguous or stale.'
        }
    }
    & $checkLease
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    & (Get-Module ItE2E) { Initialize-WtWin32Input }
    $root = [IntPtr][long]$App.Hwnd
    if (-not [ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup(
        [ItE2E.ItWtWin32Input]::GetForegroundWindow(), $root, [uint32]$App.Pid)) {
        throw 'Tab flyout dismissal refuses a foreign foreground window.'
    }
    $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
    if ($dpi -eq [IntPtr]::Zero) { throw 'Tab flyout dismissal coordinate context unavailable.' }
    try {
        $window = [Windows.Automation.AutomationElement]::FromHandle($root)
        if ($window.Current.ProcessId -ne $App.Pid) { throw 'Tab flyout dismissal window changed ownership.' }
        # Leave the previous header without clicking a pane or changing shell focus.
        $box = $window.Current.BoundingRectangle
        $point = [ItE2E.ItWtWin32Input+POINT]::new()
        $point.X = [int]($box.X + $box.Width / 2); $point.Y = [int]($box.Y + $box.Height / 2)
        $hit = [ItE2E.ItWtWin32Input]::WindowFromPoint($point)
        if ([ItE2E.ItWtWin32Input]::GetAncestor($hit, 2) -ne $root -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($hit) -ne $App.Pid) {
            throw 'Tab flyout cursor-away point is covered or foreign.'
        }
        foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
            if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Tab flyout dismissal refuses held input.' }
        }
        if (-not [ItE2E.ItWtWin32Input]::SetCursorPos($point.X, $point.Y)) { throw 'Tab flyout cursor-away placement failed.' }
        $checkEscapeState = {
            & $checkLease
            $cursor = [ItE2E.ItWtWin32Input+POINT]::new()
            if ([IntPtr][long]$App.Hwnd -ne $root -or
                [ItE2E.ItWtWin32Input]::GetForegroundWindow() -ne $root -or
                [ItE2E.ItWtWin32Input]::GetAncestor($root, 2) -ne $root -or
                [ItE2E.ItWtWin32Input]::GetWindowProcessId($root) -ne $App.Pid -or
                -not [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$cursor) -or
                $cursor.X -ne $point.X -or $cursor.Y -ne $point.Y) {
                throw 'Tab flyout Escape refuses changed foreground, root or cursor.'
            }
            $hit = [ItE2E.ItWtWin32Input]::WindowFromPoint($cursor)
            if ([ItE2E.ItWtWin32Input]::GetAncestor($hit, 2) -ne $root -or
                [ItE2E.ItWtWin32Input]::GetWindowProcessId($hit) -ne $App.Pid) {
                throw 'Tab flyout Escape cursor point is covered or foreign.'
            }
            foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 27, 91, 92)) {
                if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Tab flyout Escape refuses held input.' }
            }
        }
        $deliverEscape = {
            [ItE2E.ItWtWin32Input]::keybd_event([byte]0x1B, 0, 0, [UIntPtr]::Zero)
            [ItE2E.ItWtWin32Input]::keybd_event([byte]0x1B, 0, 0x2, [UIntPtr]::Zero)
        }
        foreach ($escape in 1..2) {
            & $checkEscapeState
            & $deliverEscape
        }
        $menuGone = $false
        Wait-UiElement -App $App -Selector KeepTabRunningMenuItem -Gone -TimeoutSec 5 | Out-Null
        $menuGone = $true
        $palettePattern = Get-WtReswTextRegex -Key CommandPaletteControlName
        if (-not $palettePattern) { $palettePattern = '(?i)Command palette' }
        $settle = @{ Clear = $null; Matched = [Collections.Generic.List[object]]::new() }
        try {
            Wait-Until -TimeoutSec 5 -Because 'the previous owned context flyout and tooltip are absent' -Condition {
                $peers = $window.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)
                $matched = @($peers | Where-Object { Test-KnownTabHeaderOverlayPeer $_ $palettePattern $HeaderPoint $App $peers })
                $settle.Clear = $matched.Count -eq 0
                $settle.Matched.Clear()
                foreach ($peer in $matched) {
                    try {
                        $item = Get-TestTabHeaderOverlayReceipt $peer $App.Pid
                        if ($item) {
                            $settle.Matched.Add($item)
                        }
                    } catch { Write-Warning 'Dismissal overlay metadata unavailable; predicate result is unchanged.' }
                }
                $settle.Clear
            } | Out-Null
        }
        catch {
            $timeoutError = $_
            try {
                $cursor = [ItE2E.ItWtWin32Input+POINT]::new()
                $cursorAvailable = [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$cursor)
                $hit = [ItE2E.ItWtWin32Input]::WindowFromPoint($point)
                $evidence = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\..\artifacts' }
                New-Item -ItemType Directory -Path $evidence -Force | Out-Null
                @{
                    stage = 'owned-flyout-settle-timeout'; pid = $App.Pid; hwnd = $root.ToInt64()
                    MenuGone = $menuGone; RemainingPredicate = 'KnownOverlayAbsent'; PredicateClear = $settle.Clear
                    MatchedOverlays = @($settle.Matched.ToArray())
                    OwnedPoint = @{ X = $point.X; Y = $point.Y }
                    CursorAvailable = $cursorAvailable
                    CursorAtOwnedPoint = $cursorAvailable -and $cursor.X -eq $point.X -and $cursor.Y -eq $point.Y
                    NativePointRoot = [ItE2E.ItWtWin32Input]::GetAncestor($hit, 2).ToInt64()
                    NativePointPid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($hit)
                    Foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
                    ExtraInputDelivered = $false
                } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (
                    Join-Path $evidence ('tab-header-dismiss-timeout-' + [guid]::NewGuid().ToString('N') + '.json'))
            } catch { Write-Warning 'Dismissal timeout diagnostic unavailable; original timeout is unchanged.' }
            throw $timeoutError
        }
    }
    finally { [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) }
}

function Invoke-TestTabHeaderContextMenu {
    param(
        [Parameter(Mandatory)]$App,
        [Parameter(Mandatory)][string]$PaneSessionId,
        [Parameter(Mandatory)][string]$Title
    )
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    & (Get-Module ItE2E) { Initialize-WtWin32Input }
    $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
    if ($dpi -eq [IntPtr]::Zero) { throw 'Physical tab header coordinate context unavailable.' }
    $runToken = $env:ITE2E_RUN_TOKEN
    $originalCursor = [ItE2E.ItWtWin32Input+POINT]::new()
    $cursorMoved = $false
    $clickDelivered = $false
    try {
        $context = Invoke-WtCli -App $App -Arguments @('get-pane-context', '--target', $PaneSessionId)
        if ([string]$context.pane.session_id -ne $PaneSessionId -or
            [string]$context.pane.window_id -ne [string]$App.WindowId) {
            throw 'Tab context target is not the requested pane in the owned window.'
        }
        $tabs = @(Get-WtTabs -App $App -WindowId $App.WindowId | Where-Object {
            [string]$_.tab_id -eq [string]$context.pane.tab_id -and $_.title -eq $Title
        })
        if ($tabs.Count -ne 1 -or
            @(Get-WtTabs -App $App -WindowId $App.WindowId | Where-Object title -eq $Title).Count -ne 1) {
            throw 'Tab context requires one canonical tab with the exact controlled title.'
        }
        $leaseOwned = {
            $current = Get-Process -Id $App.Pid -ErrorAction Stop
            $App.Launched -and $App.OwnedProcess -and -not $App.OwnedProcess.HasExited -and
                $App.OwnedProcess.Id -eq $App.Pid -and $current.StartTime -eq $App.OwnedProcess.StartTime -and
                $current.Path -eq (Join-Path $App.InstallLocation 'WindowsTerminal.exe')
        }
        if (-not (& $leaseOwned)) { throw 'Header context requires the original owned process before acquiring foreground.' }
        if (-not (Set-WtWindowForeground -App $App)) { throw 'Owned tab window cannot take foreground.' }
        $walker = [Windows.Automation.TreeWalker]::RawViewWalker
        $resolve = {
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$App.Hwnd)
            if ($window.Current.ProcessId -ne $App.Pid) { throw 'Tab header window changed ownership.' }
            $vertical = (Get-WtSetting -App $App -Key tabLayout) -eq 'vertical'
            $id = if ($vertical) { 'ItemsList' } else { 'TabView' }
            $type = if ($vertical) { [Windows.Automation.ControlType]::ListItem } else { [Windows.Automation.ControlType]::TabItem }
            $container = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $id))
            if (-not $container -or $container.Current.IsOffscreen) { throw "Missing visible canonical tab container: $id" }
            $texts = $container.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.AndCondition]::new(
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Title),
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)))
            $headers = @(
                foreach ($text in $texts) {
                    if ($text.Current.ProcessId -ne $App.Pid) { continue }
                    $ancestor = $walker.GetParent($text)
                    while ($ancestor -and -not [Windows.Automation.Automation]::Compare($ancestor, $container)) {
                        # Pane titles are valid duplicates, but their Button owns a pane flyout.
                        if ($ancestor.Current.AutomationId -eq 'PaneActivateButton') { break }
                        if ($ancestor.Current.ControlType -eq $type) {
                            if ($ancestor.Current.ProcessId -eq $App.Pid) {
                                [pscustomobject]@{ Text = $text; Row = $ancestor; Container = $container; Window = $window; Vertical = $vertical }
                            }
                            break
                        }
                        $ancestor = $walker.GetParent($ancestor)
                    }
                }
            )
            if ($headers.Count -ne 1) { throw "Expected one canonical tab header for '$Title', found $($headers.Count)." }
            $headers[0]
        }
        $header = & $resolve
        $scroll = $null
        if ($header.Row.TryGetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern, [ref]$scroll)) {
            ([Windows.Automation.ScrollItemPattern]$scroll).ScrollIntoView()
        }
        # Scrolling can recycle peers. Resolve the live header again before physical input.
        $header = & $resolve
        $bounds = $header.Text.Current.BoundingRectangle
        $point = [Windows.Point]::new([int]($bounds.X + $bounds.Width / 2), [int]($bounds.Y + $bounds.Height / 2))
        $geometry = {
            param($h)
            $parts = @($h.Row.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition) |
                Where-Object { $_.Current.ProcessId -eq $App.Pid -and -not $_.Current.IsOffscreen -and
                    $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0 })
            $band = $h.Row.Current.BoundingRectangle
            if ($h.Vertical) {
                $anchors = @($parts | Where-Object { $_.Current.AutomationId -eq 'TabGroupToggleButton' })
                if (-not $anchors.Count) { $anchors = @($parts | Where-Object { $_.Current.AutomationId -eq 'TabCloseButton' }) }
                if ($anchors.Count -ne 1) { throw 'Canonical top header band is ambiguous or unavailable.' }
                $anchor = $anchors[0].Current.BoundingRectangle
                $band = [Windows.Rect]::new($band.Left, $anchor.Top, $band.Width, $anchor.Height)
            }
            $actions = @($parts | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::Button } |
                ForEach-Object { $_.Current.BoundingRectangle })
            @{
                FullyVisible = -not $h.Text.Current.IsOffscreen -and -not $h.Row.Current.IsOffscreen -and
                    $bounds.Width -gt 0 -and $bounds.Height -gt 0 -and
                    $bounds.Contains($point) -and $h.Container.Current.BoundingRectangle.Contains($bounds) -and
                    $h.Row.Current.BoundingRectangle.Contains($bounds)
                InHeaderBand = $band.Contains($point)
                OutsideActions = @($actions | Where-Object { $_.Contains($point) }).Count -eq 0
                Band = $band
                ToggleBounds = if ($h.Vertical) { $anchor.ToString() } else { $null }
                Fingerprint = (@(($h.Text.GetRuntimeId() -join ','); ($h.Row.GetRuntimeId() -join ',');
                    $h.Text.Current.BoundingRectangle.ToString(); $h.Row.Current.BoundingRectangle.ToString();
                    $h.Container.Current.BoundingRectangle.ToString(); $band.ToString()) +
                    @($actions | ForEach-Object ToString | Sort-Object)) -join '|'
            }
        }
        $first = & $geometry $header
        $header = & $resolve
        $second = & $geometry $header
        $hit = [Windows.Automation.AutomationElement]::FromPoint($point)
        $pointHit = $hit
        $ownedHit = $false
        $forbiddenHit = $pointHit.Current.ProcessId -ne $App.Pid
        $hitChain = @()
        while ($hit) {
            $hitChain += @{
                name = if ($hit.Current.ProcessId -eq $App.Pid) { $hit.Current.Name } else { $null }
                id = if ($hit.Current.ProcessId -eq $App.Pid) { $hit.Current.AutomationId } else { $null }
                class = $hit.Current.ClassName
                type = $hit.Current.ControlType.ProgrammaticName; pid = $hit.Current.ProcessId
                runtime_id = @($hit.GetRuntimeId()); bounds = $hit.Current.BoundingRectangle.ToString()
            }
            if ($hit.Current.ProcessId -ne $App.Pid) { break }
            if ($hit.Current.AutomationId -eq 'PaneActivateButton' -or
                $hit.Current.ControlType -eq [Windows.Automation.ControlType]::Button) { $forbiddenHit = $true; break }
            if ([Windows.Automation.Automation]::Compare($hit, $header.Row)) { $ownedHit = $true; break }
            $hit = $walker.GetParent($hit)
        }
        $nativePoint = [ItE2E.ItWtWin32Input+POINT]::new()
        $nativePoint.X = [int]$point.X
        $nativePoint.Y = [int]$point.Y
        $nativeHit = [ItE2E.ItWtWin32Input]::WindowFromPoint($nativePoint)
        $root = [IntPtr][long]$App.Hwnd
        # This host's point providers can be opaque. Root ownership alone is not
        # sufficient; the independent fixture guards below do not prove universal occlusion.
        $opaqueRoot = $pointHit.Current.ControlType -eq [Windows.Automation.ControlType]::Window -and
            $pointHit.Current.ClassName -eq 'CASCADIA_HOSTING_WINDOW_CLASS' -and
            [Windows.Automation.Automation]::Compare($pointHit, $header.Window)
        $palettePattern = Get-WtReswTextRegex -Key CommandPaletteControlName
        if (-not $palettePattern) { $palettePattern = '(?i)Command palette' }
        $overlayMatches = [Collections.Generic.List[object]]::new()
        $overlayClear = {
            $overlayMatches.Clear()
            $peers = @($header.Window.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition))
            $matched = @($peers | Where-Object { Test-KnownTabHeaderOverlayPeer $_ $palettePattern $point $App $peers })
            foreach ($peer in $matched) {
                try {
                    $item = Get-TestTabHeaderOverlayReceipt $peer $App.Pid
                    if ($item) { $overlayMatches.Add($item) }
                } catch { Write-Warning 'Matched-overlay diagnostic unavailable; overlay rejection is unchanged.' }
            }
            if ($matched.Count) { return $false }
            $others = [Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$App.Pid))
            $matched = @($others | Where-Object { -not [Windows.Automation.Automation]::Compare($_, $header.Window) -and
                -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Contains($point) -and
                ($_.Current.ClassName -ne 'Xaml_WindowedPopupClass' -or
                    (Test-KnownTabHeaderOverlayPeer $_ $palettePattern $point $App $peers)) })
            foreach ($peer in $matched) {
                try {
                    $item = Get-TestTabHeaderOverlayReceipt $peer $App.Pid
                    if ($item) { $overlayMatches.Add($item) }
                } catch { Write-Warning 'Matched-overlay diagnostic unavailable; overlay rejection is unchanged.' }
            }
            $matched.Count -eq 0
        }
        $runLease = $false
        if ($runToken -and $env:ITE2E_OWNED_PROCESS_RECEIPT -and (Test-Path -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT)) {
            $records = @(Get-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT | ForEach-Object { $_ | ConvertFrom-Json })
            $runLease = @($records | Where-Object { $_.pid -eq $App.Pid -and $_.run_token -ceq $runToken -and
                $_.path -eq $App.OwnedProcess.Path -and
                ([datetimeoffset]$_.start_utc).UtcDateTime.Ticks -eq $App.OwnedProcess.StartTime.ToUniversalTime().Ticks }).Count -eq 1
        }
        $guard = @{
            ProtocolOwned = $true; FullyVisible = $first.FullyVisible -and $second.FullyVisible
            InHeaderBand = $first.InHeaderBand -and $second.InHeaderBand
            OutsideActions = $first.OutsideActions -and $second.OutsideActions
            Stable = $first.Fingerprint -ceq $second.Fingerprint
            LeaseOwned = & $leaseOwned; RunLease = $runLease
            NativeOwned = [ItE2E.ItWtWin32Input]::GetAncestor($nativeHit, 2) -eq $root -and
                [ItE2E.ItWtWin32Input]::GetWindowProcessId($nativeHit) -eq $App.Pid
            ForegroundOwned = [ItE2E.ItWtWin32Input]::GetForegroundWindow() -eq $root -and
                [ItE2E.ItWtWin32Input]::GetAncestor($root, 2) -eq $root -and
                [ItE2E.ItWtWin32Input]::GetWindowProcessId($root) -eq $App.Pid
            NoKnownOverlay = & $overlayClear; DeepHit = $ownedHit; OpaqueRoot = $opaqueRoot; ForbiddenHit = $forbiddenHit
        }
        $evidence = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\..\artifacts' }
        New-Item -ItemType Directory -Path $evidence -Force | Out-Null
        $receipt = @{
            title = $Title; pane_id = $PaneSessionId; tab_id = $context.pane.tab_id; window_id = $App.WindowId
            pid = $App.Pid; hwnd = $App.Hwnd; previous_dpi_context = $dpi.ToInt64(); query_dpi_context = -4
            header_runtime_id = @($header.Text.GetRuntimeId()); header_bounds = $bounds.ToString()
            row_runtime_id = @($header.Row.GetRuntimeId()); row_bounds = $header.Row.Current.BoundingRectangle.ToString()
            viewport_bounds = $header.Container.Current.BoundingRectangle.ToString()
            point = @{ x = $point.X; y = $point.Y }; owned_hit = $ownedHit; hit_chain = $hitChain
            independent_guards = $guard; top_header_band = $second.Band.ToString()
            toggle_bounds = $second.ToggleBounds
            matched_known_overlays = @($overlayMatches.ToArray())
            native_hit_hwnd = $nativeHit.ToInt64(); native_hit_root = [ItE2E.ItWtWin32Input]::GetAncestor($nativeHit, 2).ToInt64()
            native_hit_pid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($nativeHit)
            foreground_hwnd = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
        }
        $receiptPath = Join-Path $evidence ('tab-header-context-' + [guid]::NewGuid().ToString('N') + '.json')
        $receipt | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $receiptPath
        Assert-TestTabHeaderPointSafety $guard
        $down = [ItE2E.ItWtWin32Input+INPUT]::new()
        $up = [ItE2E.ItWtWin32Input+INPUT]::new()
        $mouse = [ItE2E.ItWtWin32Input+MOUSEINPUT]::new()
        $data = [ItE2E.ItWtWin32Input+INPUTUNION]::new()
        $mouse.dwFlags = 0x0008; $data.mouse = $mouse; $down.data = $data
        $mouse.dwFlags = 0x0010; $data.mouse = $mouse; $up.data = $data
        $inputs = [ItE2E.ItWtWin32Input+INPUT[]]@($down, $up)
        $inputSize = [Runtime.InteropServices.Marshal]::SizeOf($down)
        foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
            if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Header context input refuses held input.' }
        }
        if (-not [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$originalCursor) -or
            -not [ItE2E.ItWtWin32Input]::SetCursorPos($nativePoint.X, $nativePoint.Y)) {
            throw 'Original cursor capture or header pointer placement failed.'
        }
        $cursorMoved = $true
        $header = & $resolve
        $final = & $geometry $header
        $finalOverlayClear = $null
        $finalHitSame = $null
        if (-not $final.FullyVisible -or -not $final.InHeaderBand -or -not $final.OutsideActions -or
            $final.Fingerprint -cne $second.Fingerprint -or -not ($finalOverlayClear = & $overlayClear) -or
            -not ($finalHitSame = [Windows.Automation.Automation]::Compare(
                [Windows.Automation.AutomationElement]::FromPoint($point), $pointHit))) {
            try {
            $facts = @{
                FullyVisible = $final.FullyVisible; InHeaderBand = $final.InHeaderBand; OutsideActions = $final.OutsideActions
                StableGeometry = $final.Fingerprint -ceq $second.Fingerprint
                NoKnownOverlay = $finalOverlayClear; SameHitPeer = $finalHitSame
            }
            $cursor = [ItE2E.ItWtWin32Input+POINT]::new()
            $cursorAvailable = [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$cursor)
            $nativeHit = [ItE2E.ItWtWin32Input]::WindowFromPoint($nativePoint)
            $finalNativeRoot = [ItE2E.ItWtWin32Input]::GetAncestor($nativeHit, 2).ToInt64()
            $finalNativePid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($nativeHit)
            $finalForeground = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
            $receipt.final_rejection = @{
                false_clauses = @(Get-TestTabHeaderFinalReasons $facts); clause_facts = $facts
                pre_title_bounds = $bounds.ToString(); final_title_bounds = $header.Text.Current.BoundingRectangle.ToString()
                pre_header_band = $second.Band.ToString(); final_header_band = $final.Band.ToString()
                pre_toggle_bounds = $second.ToggleBounds; final_toggle_bounds = $final.ToggleBounds
                pre_viewport_bounds = $receipt.viewport_bounds
                final_viewport_bounds = $header.Container.Current.BoundingRectangle.ToString()
                matched_known_overlays = if ($null -ne $finalOverlayClear) { @($overlayMatches.ToArray()) } else { @() }
                native_root = $finalNativeRoot; native_pid = $finalNativePid; foreground = $finalForeground
                native_root_owned = $finalNativeRoot -eq $root.ToInt64()
                native_pid_owned = $finalNativePid -eq $App.Pid; foreground_owned = $finalForeground -eq $root.ToInt64()
                cursor_available = $cursorAvailable
                cursor_at_point = $cursorAvailable -and $cursor.X -eq $nativePoint.X -and $cursor.Y -eq $nativePoint.Y
                held_input = @(@(1, 2, 4, 5, 6, 16, 17, 18, 91, 92) | Where-Object { [ItE2E.ItWtWin32Input]::IsKeyDown($_) }).Count -gt 0
                right_input_delivered = $false
            }
            $receipt | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $receiptPath
            } catch { Write-Warning 'Final-recheck diagnostic unavailable; original guard rejection is unchanged.' }
            throw 'Canonical tab header geometry, hit peer or known overlay changed before physical input.'
        }
        $cursor = [ItE2E.ItWtWin32Input+POINT]::new()
        if (-not (& $leaseOwned) -or $env:ITE2E_RUN_TOKEN -cne $runToken -or
            -not [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$cursor) -or $cursor.X -ne $nativePoint.X -or $cursor.Y -ne $nativePoint.Y -or
            [ItE2E.ItWtWin32Input]::GetForegroundWindow() -ne $root) {
            throw 'Header input requires the original run/process lease, unchanged cursor and foreground.'
        }
        $nativeHit = [ItE2E.ItWtWin32Input]::WindowFromPoint($cursor)
        if ([ItE2E.ItWtWin32Input]::GetAncestor($nativeHit, 2) -ne $root -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId($nativeHit) -ne $App.Pid) {
            throw 'Header input point is covered or outside the owned root.'
        }
        foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
            if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Header context input refuses held input before delivery.' }
        }
        if ([ItE2E.ItWtWin32Input]::SendInput(2, $inputs, $inputSize) -ne 2) { throw 'Paired header right-click input was not delivered.' }
        $clickDelivered = $true
        $App | Add-Member -NotePropertyName LastCanonicalHeaderPoint -NotePropertyValue $point -Force
    }
    finally {
        try {
            if ($cursorMoved -and -not $clickDelivered) {
                $held = @(1, 2, 4, 5, 6) | Where-Object { [ItE2E.ItWtWin32Input]::IsKeyDown($_) }
                if ($held) { Write-Warning 'Cursor restoration refused while a mouse button is held.' }
                elseif (-not [ItE2E.ItWtWin32Input]::SetCursorPos($originalCursor.X, $originalCursor.Y)) {
                    Write-Warning 'Original cursor position could not be restored after header input failure.'
                }
            }
        } finally { [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) }
    }
}
