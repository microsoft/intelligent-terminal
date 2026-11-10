#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = Test-WinAppAvailable
}

Describe 'Feature: Sidebar tooltip placement' -Tag @('Feature', 'SidebarTooltipPlacement') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\TestTerminalCleanup.ps1')
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase -ErrorAction Stop
        if (-not ('Windows.Rect' -as [type])) { throw 'This PowerShell process lacks Windows desktop UIA types; run bootstrap and the desktop-type probe before launch.' }
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
    }

    It 'Sidebar tooltips leave adjacent rows selectable (<Language>)' -TestCases @(
        @{ Language = 'en-US' }
        @{ Language = 'ar-SA' }
    ) {
        param($Language)
        $package = Get-ItTestPackage
        if ($package -in @('Dev', 'Store', 'Auto') -or $package -notmatch '.+_.+') { throw 'Select an explicit private package family name.' }
        $target = Resolve-ItApp -Package $package
        Assert-WtPackageInactive -App $target
        foreach ($pair in @(@('TerminalApp.dll', $env:ITE2E_EXPECTED_APP_SHA256), @('wta.exe', $env:ITE2E_EXPECTED_WTA_SHA256))) {
            if ($pair[1] -notmatch '^[0-9a-fA-F]{64}$' -or
                (Get-FileHash -LiteralPath (Join-Path $target.InstallLocation $pair[0])).Hash -ne $pair[1]) {
                throw "Missing or mismatched exact-source build hash: $($pair[0])"
            }
        }
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $app = $null; $launch = $null; $hover = $null; $originalCursor = $null; $primary = $null
        $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        if ($dpi -eq [IntPtr]::Zero) { throw 'Physical coordinate context unavailable.' }
        $token = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $titles = @("Alpha-$token", "Bravo-$token", "Charlie-$token")
        $profiles = @(0..2 | ForEach-Object { @{
            guid = "{$([guid]::NewGuid())}"; tabTitle = $titles[$_]
            name = if ($_ -eq 1) { "Terminal profile`nFixture line two`nFixture line three`nFixture line four: Bravo" + $(if ($Language -eq 'ar-SA') { "`nسطر عربي" }) } else { $titles[$_] }
            commandline = 'cmd.exe /d /k "prompt $G"'; suppressApplicationTitle = $true; shellIntegrationEnabled = $false
        } })
        function Get-HeaderRows([string]$Layout) {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$app.Hwnd)
            if ($root.Current.ProcessId -ne $app.Pid) { throw 'Window ownership changed.' }
            $id = if ($Layout -eq 'vertical') { 'ItemsList' } else { 'TabView' }
            $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $id))
            if (-not $list -or $list.Current.IsOffscreen) { throw "Missing visible $id." }
            if ($Layout -eq 'vertical') {
                $rail = $list.Current.BoundingRectangle; $frame = $root.Current.BoundingRectangle
                ($rail.Left + $rail.Width / 2 -gt $frame.Left + $frame.Width / 2) |
                    Should -Be ($Language -eq 'ar-SA') -Because 'the actual Sidebar rail must occupy the locale-appropriate physical side'
            }
            $kind = if ($Layout -eq 'vertical') { [Windows.Automation.ControlType]::ListItem } else { [Windows.Automation.ControlType]::TabItem }
            $peers = @($list.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, $kind)))
            foreach ($title in $titles) {
                $matches = @($peers | Where-Object { $_.Current.ProcessId -eq $app.Pid -and $_.Current.Name -ceq $title -and -not $_.Current.IsOffscreen })
                if ($matches.Count -ne 1) { throw "Ambiguous visible header: $title" }
                $peer = $matches[0]; $band = $peer.Current.BoundingRectangle
                $buttons = @($peer.FindAll([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Button)) |
                    Where-Object { $_.Current.ProcessId -eq $app.Pid -and -not $_.Current.IsOffscreen })
                if ($band.IsEmpty -or $band.Width -le 0 -or $band.Height -le 0 -or
                    -not $list.Current.BoundingRectangle.Contains($band) -or -not $root.Current.BoundingRectangle.Contains($band)) { throw 'Clipped header.' }
                [pscustomobject]@{ Peer = $peer; Band = $band; Buttons = $buttons; Runtime = ($peer.GetRuntimeId() -join ',')
                    Point = [Windows.Point]::new([int]($band.Left + $band.Width / 2), [int]($band.Top + $band.Height / 2)) }
            }
        }
        function Assert-HeaderInput($row, [switch]$Cursor) {
            if ($clock.Elapsed.TotalSeconds -ge 90) { throw 'Fixture exceeded its 90-second action budget.' }
            $process = Get-Process -Id $app.Pid -ErrorAction Stop
            if (-not $app.Launched -or $app.OwnedProcess.HasExited -or $process.StartTime -ne $app.OwnedProcess.StartTime -or
                $process.Path -ne $app.OwnedProcess.Path -or ($row.Peer.GetRuntimeId() -join ',') -ne $row.Runtime) { throw 'Stale creation/header lease.' }
            if (-not [ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup([ItE2E.ItWtWin32Input]::GetForegroundWindow(), [IntPtr][long]$app.Hwnd, $app.Pid)) { throw 'Header foreground is foreign.' }
            $points = @($row.Point)
            if ($Cursor) {
                $position = [ItE2E.ItWtWin32Input]::GetCursorPosition()
                $points += [Windows.Point]::new($position[0], $position[1])
            }
            foreach ($location in $points) {
                if (-not $row.Band.Contains($location) -or @($row.Buttons | Where-Object { $_.Current.BoundingRectangle.Contains($location) }).Count) { throw 'Header point is outside its activation band or intersects an action.' }
                $point = [ItE2E.ItWtWin32Input+POINT]::new(); $point.X = $location.X; $point.Y = $location.Y
                $hit = [ItE2E.ItWtWin32Input]::WindowFromPoint($point)
                if ([ItE2E.ItWtWin32Input]::GetAncestor($hit, 2) -ne [IntPtr][long]$app.Hwnd -or
                    [ItE2E.ItWtWin32Input]::GetWindowProcessId($hit) -ne $app.Pid) { throw 'Actual or expected header point is covered or foreign.' }
            }
            foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
                if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Held user input; no injection authorized.' }
            }
        }
        function Get-HeaderSelector($row, [string]$Layout) {
            $search = Invoke-WinAppUi -App $app -UiArgs @('search', $row.Peer.Current.Name, '--json') -TimeoutSec 15
            if ($search.TimedOut -or $search.ExitCode -ne 0) { throw "Header search failed: $($search.StdErr)" }
            $json = $search.StdOut | ConvertFrom-Json -ErrorAction Stop
            $kind = if ($Layout -eq 'vertical') { 'ListItem' } else { 'TabItem' }
            $matches = @($json.matches | Where-Object { $_.name -ceq $row.Peer.Current.Name -and
                $_.type -eq $kind -and $_.isEnabled -and -not $_.isOffscreen -and $_.width -gt 0 -and $_.height -gt 0 })
            if ($matches.Count -ne 1) { throw 'No unique winapp semantic header.' }
            $match = $matches[0]; $bounds = $row.Peer.Current.BoundingRectangle
            if ($match.x -ne $bounds.Left -or $match.y -ne $bounds.Top -or $match.width -ne $bounds.Width -or $match.height -ne $bounds.Height) { throw 'Winapp header geometry differs from current UIA geometry.' }
            $selector = @($match.slug, $match.Slug, $match.selector | Where-Object { $_ } | Select-Object -Unique)
            if ($selector.Count -ne 1) { throw 'No unambiguous semantic header selector.' }
            $selector[0]
        }
        function Get-OwnedTooltips {
            $condition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$app.Pid)
            foreach ($window in [Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $condition)) {
                if ($window.Current.ProcessId -ne $app.Pid) { throw 'Popup root changed ownership.' }
                foreach ($tip in $window.FindAll([Windows.Automation.TreeScope]::Subtree,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ToolTip))) {
                    if ($tip.Current.ProcessId -ne $app.Pid) { throw 'Foreign tooltip.' }
                    if (-not $tip.Current.IsOffscreen -and $tip.Current.BoundingRectangle.Width -gt 0 -and $tip.Current.BoundingRectangle.Height -gt 0) { $tip }
                }
            }
        }
        try {
            $launch = Get-Date
            $app = Start-Terminal -Package $package -TimeoutSec 30 -PassFre $true -State @{
                featureRolloutId = '{00000000-0000-0000-0000-000000000001}'; sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'; sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = $Language; tabLayout = 'horizontal'; startupActions = ''; firstWindowPreference = 'defaultProfile'
                windowingBehavior = 'useNew'; 'warning.confirmOnClose' = 'never'; autoErrorDetectionEnabled = $false; autoFixEnabled = $false
                defaultProfile = $profiles[0].guid; profiles = @{ list = $profiles }
            }
            $loaded = @($app.OwnedProcess.Modules | Where-Object ModuleName -eq 'TerminalApp.dll')
            if ($loaded.Count -ne 1 -or $loaded[0].FileName -ne (Join-Path $target.InstallLocation 'TerminalApp.dll') -or
                (Get-FileHash -LiteralPath $loaded[0].FileName).Hash -ne $env:ITE2E_EXPECTED_APP_SHA256) { throw 'Wrong loaded App module.' }
            $originalCursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
            $identities = @((Get-ActivePane -App $app))
            foreach ($index in @(1, 2)) {
                $identities += Invoke-WtCli -App $app -Arguments @('new-tab', '-p', $profiles[$index].guid, '-n', $titles[$index], '-w', [string]$app.WindowId)
            }
            $identities = @($identities | ForEach-Object { (Invoke-WtCli -App $app -Arguments @('get-pane-context', '--target', $_.session_id)).pane })
            $expectedText = $null
            foreach ($layout in @('horizontal', 'vertical')) {
                Set-WtSetting -App $app -Key tabLayout -Value $layout | Out-Null
                Wait-UiElement -App $app -Selector $(if ($layout -eq 'vertical') { 'ItemsList' } else { 'TabView' }) -TimeoutSec 10 | Out-Null
                Set-WtPaneFocus -App $app -SessionId $identities[1].session_id | Out-Null
                if (-not (Set-WtWindowForeground -App $app -Attempts 2)) { throw 'Cannot acquire owned foreground.' }
                $rows = @(Get-HeaderRows $layout); $owner = $rows[1]
                $selector = Get-HeaderSelector $owner $layout
                $box = $owner.Peer.Current.BoundingRectangle
                if ([int]($box.Left + $box.Width / 2) -ne $owner.Point.X -or [int]($box.Top + $box.Height / 2) -ne $owner.Point.Y) { throw 'Semantic center differs from the safe activation-band center.' }
                if (@(Get-OwnedTooltips).Count) { throw 'Tooltip already open before controlled hover.' }
                Assert-HeaderInput $owner
                $start = [Diagnostics.ProcessStartInfo]::new((Get-Command winapp -ErrorAction Stop).Source)
                $start.UseShellExecute = $false; $start.CreateNoWindow = $true
                $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
                $start.Environment['WINAPP_CLI_TELEMETRY_OPTOUT'] = '1'
                foreach ($argument in @('ui', 'hover', $selector, '-w', [string]$app.Hwnd, '--dwell-time', '5000', '--json')) { $start.ArgumentList.Add($argument) }
                $hover = [Diagnostics.Process]::Start($start)
                $stdout = $hover.StandardOutput.ReadToEndAsync(); $stderr = $hover.StandardError.ReadToEndAsync()
                $tip = $null; $poll = [Diagnostics.Stopwatch]::StartNew()
                while (-not $tip -and $poll.Elapsed.TotalSeconds -lt 4) {
                    if ($hover.HasExited) { throw 'Hover ended before observation.' }
                    $found = @(Get-OwnedTooltips)
                    if ($found.Count -gt 1) { throw 'Ambiguous owned tooltip.' }
                    if ($found.Count -eq 1) { Assert-HeaderInput $owner -Cursor; $tip = $found[0] }
                    else { Assert-HeaderInput $owner; Start-Sleep -Milliseconds 100 }
                }
                if (-not $tip) { throw 'No actual tooltip appeared within four seconds of genuine hover.' }
                $fresh = @(Get-HeaderRows $layout)
                ($fresh.Runtime -join '|') | Should -BeExactly ($rows.Runtime -join '|')
                (($fresh | ForEach-Object { $_.Band.ToString() }) -join '|') | Should -BeExactly (($rows | ForEach-Object { $_.Band.ToString() }) -join '|')
                $text = $tip.Current.Name.Replace("`r`n", "`n")
                $text | Should -BeExactly $owner.Peer.Current.HelpText.Replace("`r`n", "`n")
                $text | Should -Match ([regex]::Escape($profiles[1].name))
                if ($layout -eq 'horizontal') { $expectedText = $text }
                else {
                    $text | Should -BeExactly $expectedText
                    foreach ($neighbor in @(0, 2)) {
                        $intersection = [Windows.Rect]::Intersect($tip.Current.BoundingRectangle, $fresh[$neighbor].Band)
                        $area = if ($intersection.IsEmpty) { 0 } else { $intersection.Width * $intersection.Height }
                        $area | Should -Be 0 -Because 'both adjacent activation bands must remain unobstructed'
                    }
                }
                $alpha = Get-HeaderSelector $fresh[0] $layout
                if ($hover.HasExited) { throw 'Hover ended before physical neighbor selection.' }
                Assert-HeaderInput $fresh[0]
                Invoke-Native -FilePath $start.FileName -Arguments @('ui', 'click', $alpha, '-w', [string]$app.Hwnd, '--json') -TimeoutSec 15 |
                    ForEach-Object { if ($_.TimedOut -or $_.ExitCode -ne 0) { throw "Physical Alpha click failed: $($_.StdErr)" } }
                Wait-Until -TimeoutSec 5 -Because 'physical Alpha selection preserves original identity' -Condition {
                    $active = Get-ActivePane -App $app
                    $active.session_id -eq $identities[0].session_id -and $active.tab_id -eq $identities[0].tab_id -and $active.window_id -eq $identities[0].window_id
                } | Out-Null
                if (-not (Wait-ItProcessDeadline -Process $hover -TimeoutSec 7) -or $hover.ExitCode -ne 0) { throw 'Genuine hover child failed or exceeded deadline.' }
                if (-not $stdout.Wait(1000) -or -not $stderr.Wait(1000)) { throw 'Hover output drain timed out.' }
                $hover.Dispose(); $hover = $null
                $poll.Restart()
                while (@(Get-OwnedTooltips).Count -and $poll.Elapsed.TotalSeconds -lt 3) { Start-Sleep -Milliseconds 100 }
                if (@(Get-OwnedTooltips).Count) { throw 'Owned tooltip remained open after hover and neighbor selection.' }
                $tabs = @(Get-WtTabs -App $app -WindowId $app.WindowId)
                ($tabs.tab_id -join '|') | Should -BeExactly ($identities.tab_id -join '|')
                foreach ($identity in $identities) {
                    @((Get-WtPanes -App $app -TabId $identity.tab_id -WindowId $app.WindowId)).session_id | Should -BeExactly $identity.session_id
                }
            }
        }
        catch { $primary = $_ }
        finally {
            $cleanup = [Collections.Generic.List[Exception]]::new()
            try {
                if ($hover) {
                    try { if (-not (Wait-ItProcessDeadline -Process $hover -TimeoutSec 7)) { $hover.Kill(); throw 'Owned hover child required timeout termination.' } }
                    finally { $hover.Dispose() }
                }
                if ($app -and $originalCursor) {
                    $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
                    $point = [ItE2E.ItWtWin32Input+POINT]::new(); $point.X = $cursor[0]; $point.Y = $cursor[1]
                    if (-not [ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup([ItE2E.ItWtWin32Input]::WindowFromPoint($point), [IntPtr][long]$app.Hwnd, $app.Pid)) { throw 'Cursor recovery refused on foreign surface.' }
                    if (-not [ItE2E.ItWtWin32Input]::SetCursorPos($originalCursor[0], $originalCursor[1])) { throw 'Cursor recovery failed.' }
                }
            } catch { $cleanup.Add($_.Exception) }
            try { Stop-TestTerminal -App $app -Target $target -LaunchStarted $launch } catch { $cleanup.Add($_.Exception) }
            if ([ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) -eq [IntPtr]::Zero) { $cleanup.Add([Exception]::new('DPI recovery failed.')) }
            if ($cleanup.Count) {
                if ($primary) { $cleanup.Insert(0, $primary.Exception) }
                throw [AggregateException]::new('Tooltip action/cleanup failed.', $cleanup.ToArray())
            }
        }
        if ($primary) { throw $primary }
    }
}
