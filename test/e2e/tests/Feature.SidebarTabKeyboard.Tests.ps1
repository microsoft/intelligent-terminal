#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object Name -like '*IntelligentTerminal*') -and
        (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: Sidebar tab keyboard navigation' -Tag @('Feature', 'SidebarTabKeyboard') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes

        $package = Get-ItTestPackage
        $target = Resolve-ItApp -Package $package
        $startupSettings = @{
            language = 'en-US'
            tabLayout = 'vertical'
            sidebarShowRecentAgentSessions = $false
            actions = @()
            keybindings = @()
            startupActions = ''
            firstWindowPreference = 'defaultProfile'
            windowingBehavior = 'useNew'
            'warning.confirmOnClose' = 'never'
        }
        if ($target.PSObject.Properties['PrivateProtocolClsid']) {
            $profile = '{' + [guid]::NewGuid() + '}'
            $startupSettings.defaultProfile = $profile
            $startupSettings.profiles = @{
                defaults = @{ environment = @{
                    PATH = "$($target.InstallLocation);$env:PATH"
                    WTCLI_PATH = $target.WtcliPath
                    WT_WTCLI_PATH = $target.WtcliPath
                } }
                list = @(@{ guid = $profile; name = 'Sidebar keyboard shell'
                    commandline = 'pwsh.exe -NoLogo -NoProfile' })
            }
        }
        $script:app = $null
        $script:app = Start-Terminal -Package $package -PassFre $true -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'; sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'; sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings $startupSettings
        $script:app | Add-Member RequireOwnedForeground $true -Force

        function Get-SidebarElement {
            param([string]$AutomationId)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $AutomationId)
            $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        }

        function Get-VisibleTabRows {
            $list = Get-SidebarElement -AutomationId 'ItemsList'
            if (-not $list -or $list.Current.IsOffscreen) { throw 'The ordinary Sidebar tabs list is not visible.' }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            $historyHeader = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton')
            $found = $list.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
            @(
                for ($i = 0; $i -lt $found.Count; $i++) {
                    $row = $found[$i]
                    $bounds = $row.Current.BoundingRectangle
                    if (-not $row.Current.IsOffscreen -and $bounds.Width -gt 0 -and $bounds.Height -gt 0 -and
                        -not $row.FindFirst([Windows.Automation.TreeScope]::Descendants, $historyHeader)) {
                        $row
                    }
                }
            )
        }

        function Test-TabRowTitle {
            param($Row, [string]$Title)
            if ($Row.Current.Name -eq $Title) { return $true }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::NameProperty, $Title)
            [bool]$Row.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        }

        function Get-FocusedTabRowIndex {
            $focused = [Windows.Automation.AutomationElement]::FocusedElement
            if (-not $focused -or $focused.Current.ProcessId -ne $script:app.Pid) { return -1 }
            $walker = [Windows.Automation.TreeWalker]::ControlViewWalker
            $rows = @(Get-VisibleTabRows)
            for ($index = 0; $index -lt $rows.Count; $index++) {
                for ($element = $focused; $element; $element = $walker.GetParent($element)) {
                    if ([Windows.Automation.Automation]::Compare($element, $rows[$index])) { return $index }
                }
            }
            -1
        }

        function Assert-FocusedTabRow {
            param([int]$Index, [string]$SelectedTabId, [string]$Because)
            Test-Until -TimeoutSec 4 -IntervalSec 0.2 -Condition {
                (Get-FocusedTabRowIndex) -eq $Index -and
                [string](Get-ActivePane -App $script:app).tab_id -eq $SelectedTabId
            } | Out-Null
            $focused = [Windows.Automation.AutomationElement]::FocusedElement
            $observed = [pscustomobject]@{
                Because = $Because
                ExpectedRow = $Index
                FocusedRow = (Get-FocusedTabRowIndex)
                ExpectedTab = $SelectedTabId
                SelectedTab = [string](Get-ActivePane -App $script:app).tab_id
                FocusProcessId = if ($focused) { $focused.Current.ProcessId } else { 0 }
                FocusClass = if ($focused) { $focused.Current.ClassName } else { '' }
                FocusAutomationId = if ($focused) { $focused.Current.AutomationId } else { '' }
            }
            $observed | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidenceDir 'navigation-observations.jsonl') -Encoding utf8
            $observed.FocusProcessId |
                Should -Be $script:app.Pid -Because 'the owned Dev window must retain keyboard focus after physical input'
            $observed.SelectedTab |
                Should -Be $SelectedTabId -Because 'keyboard traversal must not activate another tab'
            $observed.FocusedRow |
                Should -Be $Index -Because $Because
        }

        function Assert-NavigationTab {
            param($Tab, [string]$Title, [string]$Draft)
            $observation = [pscustomobject]@{
                ExpectedTab = [string]$Tab.tab_id
                ExpectedPane = [string]$Tab.session_id
                ActualTab = ''
                ActualPane = ''
                FocusProcessId = 0
                FocusClass = ''
                Matched = $false
            }
            $matched = Test-Until -TimeoutSec 6 -IntervalSec 0.2 -Condition {
                $active = Get-ActivePane -App $script:app
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $observation.ActualTab = [string]$active.tab_id
                $observation.ActualPane = [string]$active.session_id
                $observation.FocusProcessId = if ($focused) { $focused.Current.ProcessId } else { 0 }
                $observation.FocusClass = if ($focused) { $focused.Current.ClassName } else { '' }
                if ([string]$active.tab_id -ne [string]$Tab.tab_id -or
                    [string]$active.session_id -ne [string]$Tab.session_id -or
                    -not $focused -or $focused.Current.ProcessId -ne $script:app.Pid -or
                    -not $focused.Current.HasKeyboardFocus -or $focused.Current.IsOffscreen -or
                    $focused.Current.ClassName -ne 'TermControl') {
                    return $false
                }
                if ($Draft) {
                    $pattern = $null
                    if (-not $focused.TryGetCurrentPattern([Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
                        return $false
                    }
                    $visibleText = (@($pattern.GetVisibleRanges() | ForEach-Object { $_.GetText(-1) }) -join '')
                    if (-not $visibleText.Contains($Draft) -or
                        -not (Get-WtCapture -App $script:app -SessionId $Tab.session_id -MaxLines 30).TrimEnd().EndsWith($Draft)) {
                        return $false
                    }
                }
                $true
            }
            $observation.Matched = $matched
            $observation | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidenceDir 'activation-observations.jsonl') -Encoding utf8
            if (-not $matched) {
                Save-UiScreenshot -App $script:app -Path (
                    Join-Path $script:evidenceDir "activation-failure-$([guid]::NewGuid().ToString('N')).png") | Out-Null
            }
            $matched | Should -BeTrue -Because 'navigation must activate the exact existing tab, focus its input and display its retained draft'
            if ($Title) {
                $rows = @(Get-VisibleTabRows | Where-Object { Test-TabRowTitle -Row $_ -Title $Title })
                $rows.Count | Should -Be 1
                $rows[0].GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected |
                    Should -BeTrue -Because 'the displayed row selection must agree with the active terminal'
            }
        }

        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidenceDir = Join-Path ([IO.Path]::GetFullPath($artifactRoot)) "sidebar-tab-keyboard-$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null
        Wait-UiElement -App $script:app -Selector VerticalTabsHeader | Out-Null
        $initialTabs = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId))
        if ($initialTabs.Count -ne 1) { throw "Expected one initial terminal tab, found $($initialTabs.Count)." }
        $script:initialPane = Get-ActivePane -App $script:app

        $script:marker = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $script:titleA = "focus-$script:marker-A"
        $script:titleB = "focus-$script:marker-B"
        $script:tabA = New-WtTab -App $script:app -Title $script:titleA -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
        $script:tabB = New-WtTab -App $script:app -Title $script:titleB -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
        $rows = @(Get-VisibleTabRows)
        if ($rows.Count -ne 3 -or
            -not (Test-TabRowTitle -Row $rows[1] -Title $script:titleA) -or
            -not (Test-TabRowTitle -Row $rows[2] -Title $script:titleB)) {
            throw 'The three expected ordinary Sidebar tab rows did not materialize in order.'
        }
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'Sidebar navigation keeps selected tabs in view' -Tag 'SidebarNavigationScroll' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        $before = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId))
        $selectedBefore = Get-ActivePane -App $script:app
        $firstTitle = [string]$before[0].title
        # UIA clips row rectangles to the viewport, so retain an unclipped row's height.
        $rowHeight = @(Get-VisibleTabRows)[0].Current.BoundingRectangle.Height
        $rowHeight | Should -BeGreaterThan 0
        $created = [Collections.Generic.List[object]]::new()
        $query = "viewport-$script:marker"
        $searchOpened = $false
        $groupSplit = $null
        $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
        $window.Current.ProcessId | Should -Be $script:app.Pid
        $windowPattern = $window.GetCurrentPattern([Windows.Automation.WindowPattern]::Pattern)
        $priorWindowState = $windowPattern.Current.WindowVisualState
        $windowPattern.SetWindowVisualState([Windows.Automation.WindowVisualState]::Normal)
        $bounds = [ItE2E.ItWtWin32Input+RECT]::new()
        $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        if ($dpi -eq [IntPtr]::Zero) { throw 'Physical window sizing context is unavailable.' }
        try {
            if (-not [ItE2E.ItWtWin32Input]::GetWindowRect([IntPtr][long]$script:app.Hwnd, [ref]$bounds)) {
                throw 'Cannot capture the owned window bounds.'
            }
            if (-not [ItE2E.ItWtWin32Input]::SetWindowPos([IntPtr][long]$script:app.Hwnd,
                [IntPtr]::Zero, $bounds.Left, $bounds.Top, $bounds.Right - $bounds.Left, 480, 0x14)) {
                throw 'Cannot size the owned window for an overflowing sidebar.'
            }
        }
        finally { [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) }

        function Get-NavigationViewportState {
            param([string]$Title)
            $list = Get-SidebarElement ItemsList
            $viewport = $list.Current.BoundingRectangle
            $visibleRows = @(Get-VisibleTabRows)
            $rows = @($visibleRows | Where-Object { Test-TabRowTitle -Row $_ -Title $Title })
            $row = if ($rows.Count -eq 1) { $rows[0] } else { $null }
            $rectangle = if ($row) { $row.Current.BoundingRectangle } else { $null }
            [pscustomobject]@{
                Title = $Title
                RowCount = $rows.Count
                Selected = [bool]($row -and $row.GetCurrentPattern(
                    [Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected)
                RowTop = if ($rectangle) { $rectangle.Top } else { $null }
                RowBottom = if ($rectangle) { $rectangle.Bottom } else { $null }
                ExpectedRowHeight = $rowHeight
                ViewportTop = $viewport.Top
                ViewportBottom = $viewport.Bottom
                ScrollPercent = $list.GetCurrentPattern(
                    [Windows.Automation.ScrollPattern]::Pattern).Current.VerticalScrollPercent
                VisibleRows = @($visibleRows | ForEach-Object {
                    [pscustomobject]@{ Name = $_.Current.Name
                        Selected = $_.GetCurrentPattern(
                            [Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected }
                })
                FullyVisible = [bool]($rectangle -and $rectangle.Height -ge $rowHeight - 1 -and
                    $rectangle.Top -ge $viewport.Top - 1 -and $rectangle.Bottom -le $viewport.Bottom + 1)
            }
        }
        function Assert-NavigationViewport {
            param($Tab, [string]$Title, [string]$Label, [string]$Draft)
            Assert-NavigationTab -Tab $Tab -Draft $Draft
            $stable = [pscustomobject]@{ Since = $null; Top = 0.0; Bottom = 0.0; Scroll = 0.0 }
            $visible = Test-Until -TimeoutSec 6 -IntervalSec 0.1 -Condition {
                $state = Get-NavigationViewportState $Title
                if (-not $state.Selected -or -not $state.FullyVisible) {
                    $stable.Since = $null
                    return $false
                }
                if (-not $stable.Since -or [Math]::Abs($state.RowTop - $stable.Top) -gt 1 -or
                    [Math]::Abs($state.RowBottom - $stable.Bottom) -gt 1 -or
                    [Math]::Abs($state.ScrollPercent - $stable.Scroll) -gt 0.1) {
                    $stable.Since = [datetime]::UtcNow
                }
                $stable.Top = $state.RowTop
                $stable.Bottom = $state.RowBottom
                $stable.Scroll = $state.ScrollPercent
                ([datetime]::UtcNow - $stable.Since).TotalMilliseconds -ge 500
            }
            Get-NavigationViewportState $Title | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidenceDir 'viewport-observations.jsonl') -Encoding utf8
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "$Label.png") | Out-Null
            $visible | Should -BeTrue -Because 'the correctly activated tab must also have its selected row fully inside the sidebar viewport'
            $settled = Get-NavigationViewportState $Title
            ($settled.Selected -and $settled.FullyVisible) |
                Should -BeTrue -Because 'the selected row must remain visible after layout and screenshot capture'
            Assert-NavigationTab -Tab $Tab -Draft $Draft
        }
        try {
            foreach ($index in 0..9) {
                $title = "$index-$query"
                $tab = New-WtTab -App $script:app -Title $title -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
                $tab | Add-Member TestTitle $title
                $created.Add($tab)
            }
            $last = $created[$created.Count - 1]
            $draft = "VIEWPORT_DRAFT_$script:marker"
            Send-WtInput -App $script:app -SessionId $last.session_id -Text $draft | Out-Null
            $scroll = (Get-SidebarElement ItemsList).GetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern)
            $scroll.Current.VerticallyScrollable | Should -BeTrue -Because 'the regression requires an actually overflowing viewport'
            Set-WtPaneFocus -App $script:app -SessionId $script:initialPane.session_id
            $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
            Wait-Until -TimeoutSec 5 -Because 'the sidebar is at its beginning before navigating offscreen' -Condition {
                $scroll.Current.VerticalScrollPercent -le 0.1
            } | Out-Null
            (Get-NavigationViewportState $last.TestTitle).FullyVisible | Should -BeFalse
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'viewport-before-wrap.png') | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $last -Title $last.TestTitle -Draft $draft -Label 'viewport-after-up'
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $script:initialPane -Title $firstTitle -Label 'viewport-after-down'
            $visibleOffset = $scroll.Current.VerticalScrollPercent
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $script:tabA -Title $script:titleA -Label 'viewport-visible-target'
            [Math]::Abs($scroll.Current.VerticalScrollPercent - $visibleOffset) | Should -BeLessOrEqual 0.1
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $script:initialPane -Title $firstTitle -Label 'viewport-visible-return'

            $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 100)
            Wait-Until -TimeoutSec 5 -Because 'manual sidebar scrolling reaches its end' -Condition {
                $scroll.Current.VerticalScrollPercent -ge 99.9
            } | Out-Null
            $manualOffset = $scroll.Current.VerticalScrollPercent
            Set-WtPaneFocus -App $script:app -SessionId $script:initialPane.session_id
            for ($sample = 0; $sample -lt 4; ++$sample) {
                Start-Sleep -Milliseconds 150
                [Math]::Abs($scroll.Current.VerticalScrollPercent - $manualOffset) | Should -BeLessOrEqual 0.1
            }
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $script:tabA -Title $script:titleA -Label 'viewport-after-nonwrapping-down'

            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            $searchOpened = $true
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $query | Out-Null
            Set-WtPaneFocus -App $script:app -SessionId $created[0].session_id
            $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
            Wait-Until -TimeoutSec 5 -Because 'the filtered viewport is at its beginning' -Condition {
                $scroll.Current.VerticalScrollPercent -le 0.1
            } | Out-Null
            (Get-NavigationViewportState $last.TestTitle).FullyVisible | Should -BeFalse
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $last -Title $last.TestTitle -Draft $draft -Label 'viewport-filtered-up'
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationViewport -Tab $created[0] -Title $created[0].TestTitle -Label 'viewport-filtered-down'
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $query

            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            $searchOpened = $false
            $group = $created[5]
            $groupSplit = Split-WtPane -App $script:app -SessionId $group.session_id -Direction right `
                -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            Set-WtPaneFocus -App $script:app -SessionId $group.session_id
            $groupTitle = [string](@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)) |
                Where-Object tab_id -eq $group.tab_id).title
            function Get-GroupRow {
                $condition = [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
                $rows = (Get-SidebarElement ItemsList).FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
                @($rows | Where-Object { Test-TabRowTitle -Row $_ -Title $groupTitle }) |
                    Select-Object -First 1
            }
            $groupRow = Get-GroupRow
            $groupRow | Should -Not -BeNullOrEmpty
            $groupRow.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern).ScrollIntoView()
            $paneCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'PaneActivateButton')
            Wait-Until -TimeoutSec 6 -Because 'the split tab must have two expanded pane rows' -Condition {
                (Get-GroupRow).FindAll([Windows.Automation.TreeScope]::Descendants, $paneCondition).Count -eq 2
            } | Out-Null
            $viewport = (Get-SidebarElement ItemsList).Current.BoundingRectangle
            $range = $viewport.Height * (100.0 / $scroll.Current.VerticalViewSize - 1.0)
            $targetTop = $viewport.Bottom - $rowHeight - 4.0
            $offset = $range * $scroll.Current.VerticalScrollPercent / 100.0 +
                (Get-GroupRow).Current.BoundingRectangle.Top - $targetTop
            $offset | Should -BeGreaterThan 0
            $offset | Should -BeLessThan $range
            $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 100.0 * $offset / $range)
            Set-WtPaneFocus -App $script:app -SessionId $created[4].session_id
            Wait-Until -TimeoutSec 6 -Because 'the split header must be visible while its children are clipped' -Condition {
                $row = Get-GroupRow
                $top = $row.Current.BoundingRectangle.Top
                $children = $row.FindAll([Windows.Automation.TreeScope]::Descendants, $paneCondition)
                $top -ge $viewport.Top - 1 -and $top + $rowHeight -le $viewport.Bottom + 1 -and
                    @($children | Where-Object { $_.Current.IsOffscreen }).Count -gt 0
            } | Out-Null
            $groupOffset = $scroll.Current.VerticalScrollPercent
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'viewport-split-before.png') | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $group
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'viewport-split-after.png') | Out-Null
            for ($sample = 0; $sample -lt 4; ++$sample) {
                Start-Sleep -Milliseconds 150
                [Math]::Abs($scroll.Current.VerticalScrollPercent - $groupOffset) |
                    Should -BeLessOrEqual 0.1 -Because 'clipped pane children must not scroll an already-visible tab header'
            }
            Set-WtPaneFocus -App $script:app -SessionId $created[4].session_id
            $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
            Wait-Until -TimeoutSec 5 -Because 'the split header is offscreen before navigating back to it' -Condition {
                $row = Get-GroupRow
                $scroll.Current.VerticalScrollPercent -le 0.1 -and $row.Current.IsOffscreen
            } | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $group
            Wait-Until -TimeoutSec 6 -Because 'an offscreen split header must still be revealed' -Condition {
                $row = Get-GroupRow
                $top = $row.Current.BoundingRectangle.Top
                -not $row.Current.IsOffscreen -and $top -ge $viewport.Top - 1 -and
                    $top + $rowHeight -le $viewport.Bottom + 1
            } | Out-Null
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'viewport-split-revealed.png') | Out-Null
        }
        finally {
            if ($searchOpened) { Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null }
            if ($groupSplit) { Close-WtPane -App $script:app -SessionId $groupSplit.session_id }
            for ($index = $created.Count - 1; $index -ge 0; --$index) {
                Close-WtPane -App $script:app -SessionId $created[$index].session_id
            }
            Set-WtPaneFocus -App $script:app -SessionId $selectedBefore.session_id
            $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
            if ($dpi -eq [IntPtr]::Zero) { throw 'Physical window restoration context is unavailable.' }
            try {
                if ([ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][long]$script:app.Hwnd) -ne $script:app.Pid -or
                    -not [ItE2E.ItWtWin32Input]::SetWindowPos([IntPtr][long]$script:app.Hwnd, [IntPtr]::Zero,
                        $bounds.Left, $bounds.Top, $bounds.Right - $bounds.Left, $bounds.Bottom - $bounds.Top, 0x14)) {
                    throw 'Cannot restore the owned window bounds.'
                }
            }
            finally { [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) }
            $windowPattern.SetWindowVisualState($priorWindowState)
        }
        (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',') |
            Should -BeExactly ($before.tab_id -join ',')
    }

    It 'Sidebar navigation hotkeys cycle through displayed tabs' -Tag 'SidebarNavigationHotkeys' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        $before = (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',')
        $draftA = "NAV_A_$script:marker"
        $draftB = "NAV_B_$script:marker"
        Send-WtInput -App $script:app -SessionId $script:tabA.session_id -Text $draftA | Out-Null
        Send-WtInput -App $script:app -SessionId $script:tabB.session_id -Text $draftB | Out-Null
        Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
        Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'cycle-before.png') | Out-Null

        Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
        Assert-NavigationTab -Tab $script:initialPane
        Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
        Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB
        Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
        Assert-NavigationTab -Tab $script:tabA -Title $script:titleA -Draft $draftA
        Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
        Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB

        Send-WtWindowKey -App $script:app -Vk 0x53 -Ctrl -Shift -RequireForeground | Out-Null
        (Test-Until -TimeoutSec 5 -Condition {
            [Windows.Automation.AutomationElement]::FocusedElement.Current.Name -eq 'Collapse sidebar'
        }) | Should -BeTrue
        Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
        Assert-NavigationTab -Tab $script:initialPane
        Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
        Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB

        Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        try {
            $query = "focus-$script:marker"
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $query | Out-Null
            (Test-Until -TimeoutSec 6 -Condition { @(Get-VisibleTabRows).Count -eq 2 }) | Should -BeTrue
            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $script:tabA -Title $script:titleA -Draft $draftA
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'cycle-filtered-wrap.png') | Out-Null

            @(Get-VisibleTabRows)[0].SetFocus()
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $script:tabA -Title $script:titleA -Draft $draftA
            (Get-SidebarElement -AutomationId 'SearchTextBox').SetFocus()
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $query

            Set-UiValue -App $script:app -Selector SearchTextBox -Value $script:titleA | Out-Null
            (Test-Until -TimeoutSec 6 -Condition { @(Get-VisibleTabRows).Count -eq 1 }) | Should -BeTrue
            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            foreach ($vk in @(0x28, 0x26)) {
                (Get-SidebarElement -AutomationId 'SearchTextBox').SetFocus()
                Send-WtWindowKey -App $script:app -Vk $vk -Ctrl -Alt -RequireForeground | Out-Null
                Assert-NavigationTab -Tab $script:tabA -Title $script:titleA -Draft $draftA
            }

            $emptyQuery = "navigation-no-match-$script:marker"
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $emptyQuery | Out-Null
            (Test-Until -TimeoutSec 6 -Condition { @(Get-VisibleTabRows).Count -eq 0 }) | Should -BeTrue
            Set-WtPaneFocus -App $script:app -SessionId $script:tabA.session_id
            foreach ($vk in @(0x28, 0x26)) {
                Send-WtWindowKey -App $script:app -Vk $vk -Ctrl -Alt -RequireForeground | Out-Null
                Assert-NavigationTab -Tab $script:tabA -Draft $draftA
            }
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $emptyQuery
        }
        finally {
            $search = Get-SidebarElement -AutomationId 'SearchTextBox'
            if ($search -and -not $search.Current.IsOffscreen) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            }
        }
        (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',') |
            Should -BeExactly $before -Because 'navigation must not create, close, reorder or replace tabs'
        Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
        Assert-NavigationTab -Tab $script:tabB -Title $script:titleB -Draft $draftB
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'cycle-after-round-trip.png') | Out-Null

        $createdPanes = [Collections.Generic.List[string]]::new()
        try {
            $visibleQuery = "nav-visible-$script:marker"
            $titleX = "$visibleQuery-agent"
            $titleGap = "$visibleQuery-ordinary"
            $titleY = "nav-other-$script:marker-agent"
            $cwd = [IO.Path]::GetPathRoot($env:SystemRoot)
            $agentX = Invoke-WtCli -App $script:app -Arguments @(
                'new-tab', '-c', 'pwsh.exe -NoLogo -NoProfile -NoExit', '-d', $cwd,
                '-n', $titleX, '-w', [string]$script:app.WindowId, '--agent-provider', 'copilot')
            $createdPanes.Add([string]$agentX.session_id)
            $gap = New-WtTab -App $script:app -Title $titleGap -Cwd $cwd -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            $createdPanes.Add([string]$gap.session_id)
            $agentY = Invoke-WtCli -App $script:app -Arguments @(
                'new-tab', '-c', 'pwsh.exe -NoLogo -NoProfile -NoExit', '-d', $cwd,
                '-n', $titleY, '-w', [string]$script:app.WindowId, '--agent-provider', 'copilot')
            $createdPanes.Add([string]$agentY.session_id)
            $split = Split-WtPane -App $script:app -SessionId $agentY.session_id -Direction right `
                -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            $createdPanes.Add([string]$split.session_id)
            $splitTarget = [pscustomobject]@{ tab_id = $agentY.tab_id; session_id = $split.session_id }
            $panes = @(Get-WtPanes -App $script:app -TabId $agentY.tab_id -WindowId ([string]$script:app.WindowId))
            $panes.Count | Should -Be 2
            @($panes | Where-Object session_id -eq $agentY.session_id)[0].native_agent_provider_id | Should -Be 'copilot'
            $paneIds = @($panes.session_id | Sort-Object)
            $paneProcesses = @{}
            foreach ($paneId in $paneIds) {
                $paneProcesses[$paneId] = (Get-WtPaneStatus -App $script:app -SessionId $paneId).pid
            }
            $draftX = "NAV_X_$script:marker"
            $draftSplit = "NAV_SPLIT_$script:marker"
            $draftGap = "NAV_GAP_$script:marker"
            Send-WtInput -App $script:app -SessionId $agentX.session_id -Text $draftX | Out-Null
            Send-WtInput -App $script:app -SessionId $split.session_id -Text $draftSplit | Out-Null
            Send-WtInput -App $script:app -SessionId $gap.session_id -Text $draftGap | Out-Null
            $order = (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',')

            Set-TestSidebarScope -App $script:app -AgentsOnly $true -Recent $false
            (Test-Until -TimeoutSec 8 -Condition { @(Get-VisibleTabRows).Count -eq 2 }) | Should -BeTrue
            Set-WtPaneFocus -App $script:app -SessionId $split.session_id
            Assert-NavigationTab -Tab $splitTarget -Title $titleY -Draft $draftSplit
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $agentX -Title $titleX -Draft $draftX
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $splitTarget -Title $titleY -Draft $draftSplit
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $agentX -Title $titleX -Draft $draftX
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $splitTarget -Title $titleY -Draft $draftSplit
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'cycle-agents-split.png') | Out-Null

            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $visibleQuery | Out-Null
            (Test-Until -TimeoutSec 8 -Condition { @(Get-VisibleTabRows).Count -eq 2 }) | Should -BeTrue
            Set-WtPaneFocus -App $script:app -SessionId $agentX.session_id
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $gap -Title $titleGap -Draft $draftGap
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $agentX -Title $titleX -Draft $draftX
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $visibleQuery
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            Set-WtPaneFocus -App $script:app -SessionId $split.session_id
            Send-WtWindowKey -App $script:app -Vk 0x28 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $agentX -Title $titleX -Draft $draftX
            Send-WtWindowKey -App $script:app -Vk 0x26 -Ctrl -Alt -RequireForeground | Out-Null
            Assert-NavigationTab -Tab $splitTarget -Title $titleY -Draft $draftSplit
            @((Get-WtPanes -App $script:app -TabId $agentY.tab_id -WindowId ([string]$script:app.WindowId)).session_id | Sort-Object) |
                Should -Be $paneIds
            foreach ($paneId in $paneIds) {
                (Get-WtPaneStatus -App $script:app -SessionId $paneId).pid | Should -Be $paneProcesses[$paneId]
            }
            (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',') |
                Should -BeExactly $order -Because 'agent-filtered navigation must not reorder or recreate owning tabs'
        }
        finally {
            $search = Get-SidebarElement -AutomationId 'SearchTextBox'
            if ($search -and -not $search.Current.IsOffscreen) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            }
            Set-TestSidebarScope -App $script:app -AgentsOnly $false -Recent $false
            for ($index = $createdPanes.Count - 1; $index -ge 0; $index--) {
                Close-WtPane -App $script:app -SessionId $createdPanes[$index]
            }
        }
        (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',') | Should -BeExactly $before

        try {
            Set-WtSetting -App $script:app -Key tabLayout -Value horizontal | Out-Null
            Wait-UiElement -App $script:app -Selector NewTabButton | Out-Null
            Test-UiElementExists -App $script:app -Selector SearchTabsButton -TimeoutSec 1 | Should -BeFalse
            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'cycle-horizontal-before.png') | Out-Null
            foreach ($vk in @(0x26, 0x28)) {
                Send-WtWindowKey -App $script:app -Vk $vk -Ctrl -Alt -RequireForeground | Out-Null
                for ($sample = 0; $sample -lt 3; $sample++) {
                    Start-Sleep -Milliseconds 200
                    Assert-NavigationTab -Tab $script:tabB
                }
            }
            Test-UiElementExists -App $script:app -Selector NewTabButton -TimeoutSec 2 | Should -BeTrue
            Test-UiElementExists -App $script:app -Selector SearchTabsButton -TimeoutSec 1 | Should -BeFalse
            (@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id -join ',') |
                Should -BeExactly $before
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'cycle-horizontal-after.png') | Out-Null
        }
        finally {
            Set-WtSetting -App $script:app -Key tabLayout -Value vertical | Out-Null
            Wait-UiElement -App $script:app -Selector VerticalTabsHeader | Out-Null
        }
    }

    It 'Sidebar tabs keyboard navigation stays in the list (unfiltered)' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
        $toolbarEnd = Get-SidebarElement -AutomationId 'FilterTabsButton'
        $toolbarEnd.Current.IsOffscreen | Should -BeFalse
        $toolbarEnd.SetFocus()
        $toolbarEnd.Current.HasKeyboardFocus | Should -BeTrue
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'unfiltered-before.png') | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'unfiltered-after-tab.png') | Out-Null
        $entryIndex = Get-FocusedTabRowIndex
        $entryIndex | Should -BeIn @(0, 1, 2) -Because 'Tab must enter a visible unfiltered Sidebar row'
        Assert-FocusedTabRow -Index $entryIndex -SelectedTabId ([string]$script:tabB.tab_id) -Because 'Tab enters the unfiltered Sidebar list without entering a terminal'
        for ($row = $entryIndex; $row -gt 0; $row--) {
            Send-WtWindowKey -App $script:app -Vk 0x26 -RequireForeground | Out-Null
            if ($row -eq $entryIndex) {
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'unfiltered-after-first-navigation.png') | Out-Null
            }
            Assert-FocusedTabRow -Index ($row - 1) -SelectedTabId ([string]$script:tabB.tab_id) -Because 'Up moves to a different unfiltered row without activating it'
        }
        for ($row = 0; $row -lt 2; $row++) {
            Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
            if ($entryIndex -eq 0 -and $row -eq 0) {
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'unfiltered-after-first-navigation.png') | Out-Null
            }
            Assert-FocusedTabRow -Index ($row + 1) -SelectedTabId ([string]$script:tabB.tab_id) -Because 'Down moves through unfiltered rows without activating them'
        }
        Send-WtWindowKey -App $script:app -Vk 0x26 -RequireForeground | Out-Null
        Assert-FocusedTabRow -Index 1 -SelectedTabId ([string]$script:tabB.tab_id) -Because 'Up returns to the unfiltered tab selected by Enter'
        Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
        (Test-Until -TimeoutSec 8 -Condition {
            [string](Get-ActivePane -App $script:app).tab_id -eq [string]$script:tabA.tab_id -and
            [Windows.Automation.AutomationElement]::FocusedElement.Current.ClassName -eq 'TermControl'
        }) | Should -BeTrue -Because 'Enter alone activates the focused tab and moves focus into its terminal'
    }

    It 'Sidebar tabs keyboard navigation stays in the list (filtered)' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        Set-WtPaneFocus -App $script:app -SessionId $script:tabA.session_id
        Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        Wait-UiElement -App $script:app -Selector SearchTextBox | Out-Null
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "focus-$script:marker" | Out-Null
        (Test-Until -TimeoutSec 8 -Condition {
            @(Get-VisibleTabRows).Count -eq 2
        }) | Should -BeTrue -Because 'search filters out the unrelated first tab'
        $rows = @(Get-VisibleTabRows)
        (Test-TabRowTitle -Row $rows[0] -Title $script:titleA) | Should -BeTrue
        (Test-TabRowTitle -Row $rows[1] -Title $script:titleB) | Should -BeTrue
        $search = Get-SidebarElement -AutomationId 'SearchTextBox'
        $search.SetFocus()
        $search.Current.HasKeyboardFocus | Should -BeTrue
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'filtered-before.png') | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        Assert-FocusedTabRow -Index 0 -SelectedTabId ([string]$script:tabA.tab_id) -Because 'Tab enters the filtered Sidebar results'
        Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'filtered-after-down.png') | Out-Null
        Assert-FocusedTabRow -Index 1 -SelectedTabId ([string]$script:tabA.tab_id) -Because 'Down reaches the next filtered result without entering its terminal'
        Send-WtWindowKey -App $script:app -Vk 0x26 -RequireForeground | Out-Null
        Assert-FocusedTabRow -Index 0 -SelectedTabId ([string]$script:tabA.tab_id) -Because 'Up returns to the first filtered result'
        Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
        Assert-FocusedTabRow -Index 1 -SelectedTabId ([string]$script:tabA.tab_id) -Because 'filtered rows remain navigable after revisiting them'
        Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
        (Test-Until -TimeoutSec 8 -Condition {
            [string](Get-ActivePane -App $script:app).tab_id -eq [string]$script:tabB.tab_id -and
            [Windows.Automation.AutomationElement]::FocusedElement.Current.ClassName -eq 'TermControl'
        }) | Should -BeTrue -Because 'Enter activates the focused filtered result'

        $tree = Get-UiTree -App $script:app -Selector ItemsList -Depth 8
        $pattern = '(?m)^\s*(?<Selector>lbl-textview-\S+|TextView) Text "' + [regex]::Escape($script:titleA) + '"'
        $matches = @([regex]::Matches($tree, $pattern))
        $matches.Count | Should -Be 1 -Because 'the pointer target must identify exactly one visible tab header'
        Invoke-UiClick -App $script:app -Selector $matches[0].Groups['Selector'].Value | Out-Null
        (Test-Until -TimeoutSec 8 -Condition {
            [string](Get-ActivePane -App $script:app).tab_id -eq [string]$script:tabA.tab_id
        }) | Should -BeTrue -Because 'ordinary pointer selection must still activate its tab'

        $search.SetFocus()
        $search.Current.HasKeyboardFocus | Should -BeTrue
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        Assert-FocusedTabRow -Index 0 -SelectedTabId ([string]$script:tabA.tab_id) -Because 'the filtered list remains keyboard-operable after pointer selection'
        Send-WtWindowKey -App $script:app -Vk 0x0D -Ctrl -RequireForeground | Out-Null
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'filtered-after-modified-enter.png') | Out-Null
        Assert-FocusedTabRow -Index 0 -SelectedTabId ([string]$script:tabA.tab_id) -Because 'Ctrl+Enter must not activate the already selected tab'
        Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
        $entered = Test-Until -TimeoutSec 5 -Condition {
            [string](Get-ActivePane -App $script:app).tab_id -eq [string]$script:tabA.tab_id -and
            [Windows.Automation.AutomationElement]::FocusedElement.Current.ClassName -eq 'TermControl'
        }
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'filtered-after-current-enter.png') | Out-Null
        $focused = [Windows.Automation.AutomationElement]::FocusedElement
        $observed = [pscustomobject]@{
            Because = 'Enter on the already selected filtered tab'
            FocusedRow = (Get-FocusedTabRowIndex)
            SelectedTab = [string](Get-ActivePane -App $script:app).tab_id
            FocusProcessId = if ($focused) { $focused.Current.ProcessId } else { 0 }
            FocusClass = if ($focused) { $focused.Current.ClassName } else { '' }
        }
        $observed | ConvertTo-Json -Compress |
            Add-Content -LiteralPath (Join-Path $script:evidenceDir 'navigation-observations.jsonl') -Encoding utf8
        $observed.FocusProcessId | Should -Be $script:app.Pid -Because 'the owned Dev window must retain keyboard focus'
        $observed.SelectedTab | Should -Be ([string]$script:tabA.tab_id) -Because 'Enter must keep the selected tab active'
        $entered | Should -BeTrue -Because 'Enter on the already selected row must also enter its terminal'
    }

    It 'Sidebar hotkey entry and Tab traversal return to the same shell' -Tag 'SidebarHotkeyTabJourney' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        $existingSearch = Get-SidebarElement -AutomationId 'SearchTextBox'
        if ($existingSearch -and -not $existingSearch.Current.IsOffscreen) {
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        }
        Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
        $sourceFocus = Wait-Until -TimeoutSec 5 -Condition {
            $focused = [Windows.Automation.AutomationElement]::FocusedElement
            if ($focused -and $focused.Current.ProcessId -eq $script:app.Pid -and
                $focused.Current.HasKeyboardFocus -and $focused.Current.ClassName -eq 'TermControl') { $focused }
        }
        $sourceFocus | Should -Not -BeNullOrEmpty
        $tabId = [string](Get-ActivePane -App $script:app).tab_id
        $draft = "SIDEBAR_HOTKEY_DRAFT_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
        Send-WtInput -App $script:app -SessionId $script:tabB.session_id -Text $draft | Out-Null
        Wait-Until -TimeoutSec 8 -Because 'the original shell to display its unsent draft' -Condition {
            (Get-WtCapture -App $script:app -SessionId $script:tabB.session_id -MaxLines 30).TrimEnd().EndsWith($draft)
        } | Out-Null

        foreach ($fromCollapsed in @($false, $true)) {
            Send-WtWindowKey -App $script:app -Vk 0x53 -Ctrl -Shift -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $focused -and $focused.Current.ProcessId -eq $script:app.Pid -and
                    $focused.Current.Name -eq 'Collapse sidebar' -and $focused.Current.HasKeyboardFocus
            }) | Should -BeTrue -Because 'Ctrl+Shift+S must focus the upper-left sidebar toggle'
            $search = Get-SidebarElement -AutomationId 'SearchTextBox'
            [bool]($search -and -not $search.Current.IsOffscreen) |
                Should -BeFalse -Because 'sidebar entry must not open search'

            Send-WtWindowKey -App $script:app -Vk 0x46 -Ctrl -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition {
                (Get-SidebarElement -AutomationId 'SearchTextBox').Current.HasKeyboardFocus
            }) | Should -BeTrue -Because 'Ctrl+F from the sidebar toggle must explicitly enter search'
            Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { (Get-FocusedTabRowIndex) -ge 0 }) |
                Should -BeTrue -Because 'Tab from explicitly opened search retains its existing list navigation'
            $row = Get-FocusedTabRowIndex
            Assert-FocusedTabRow -Index $row -SelectedTabId $tabId -Because 'Tab must not activate a terminal after hotkey entry'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "hotkey-tab-row-$fromCollapsed.png") | Out-Null

            Send-WtWindowKey -App $script:app -Vk 0x53 -Ctrl -Shift -RequireForeground | Out-Null
            Wait-UiElement -App $script:app -Selector CompactNewTabButton | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $sourceFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'the hotkey must return from a Sidebar tab row to the original shell'
            (Get-WtCapture -App $script:app -SessionId $script:tabB.session_id -MaxLines 30).TrimEnd() |
                Should -Match ([regex]::Escape($draft) + '$') -Because 'the unsent shell draft must survive both hotkey transitions'
        }
    }

    It 'Global search shortcut opens sidebar search without replacing the query' -Tag 'SidebarSearchHotkey' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        foreach ($fromCollapsed in @($false, $true)) {
            $expand = Get-UiElement -App $script:app -Selector 'Expand sidebar'
            if ($expand -and -not $expand.isOffscreen) {
                Invoke-UiElement -App $script:app -Selector 'Expand sidebar' | Out-Null
            }
            $search = Get-SidebarElement -AutomationId 'SearchTextBox'
            if ($search -and -not $search.Current.IsOffscreen) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            }
            if ($fromCollapsed) {
                Invoke-UiElement -App $script:app -Selector 'Collapse sidebar' | Out-Null
                Wait-UiElement -App $script:app -Selector CompactNewTabButton | Out-Null
            }
            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            $tabId = [string](Get-ActivePane -App $script:app).tab_id
            Send-WtWindowKey -App $script:app -Vk 0x46 -Ctrl -RequireForeground | Out-Null
            $opened = Test-Until -TimeoutSec 6 -Condition {
                $search = Get-SidebarElement -AutomationId 'SearchTextBox'
                $search -and -not $search.Current.IsOffscreen -and $search.Current.HasKeyboardFocus
            }
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "after-search-shortcut-$fromCollapsed.png") | Out-Null
            $opened | Should -BeTrue -Because 'Ctrl+F from terminal input must open and focus sidebar search'

            $query = "focus-$script:marker"
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $query | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x24 -RequireForeground | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x27 -RequireForeground | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x27 -Shift -RequireForeground | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x46 -Ctrl -RequireForeground | Out-Null
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $query
            Send-WtWindowKey -App $script:app -Vk 0x08 -RequireForeground | Out-Null
            $editedQuery = $query.Remove(1, 1)
            (Get-UiValue -App $script:app -Selector SearchTextBox) |
                Should -BeExactly $editedQuery -Because 'Ctrl+F must preserve the existing one-character selection, not select all'

            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            Send-WtWindowKey -App $script:app -Vk 0x46 -Ctrl -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition {
                (Get-SidebarElement -AutomationId 'SearchTextBox').Current.HasKeyboardFocus
            }) | Should -BeTrue
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $editedQuery
            [string](Get-ActivePane -App $script:app).tab_id | Should -Be $tabId
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "global-search-$fromCollapsed.png") | Out-Null

            Send-WtWindowKey -App $script:app -Vk 0x53 -Ctrl -Shift -RequireForeground | Out-Null
            Wait-UiElement -App $script:app -Selector CompactNewTabButton | Out-Null
            (Test-Until -TimeoutSec 5 -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $focused -and $focused.Current.ProcessId -eq $script:app.Pid -and $focused.Current.ClassName -eq 'TermControl'
            }) | Should -BeTrue -Because 'Ctrl+Shift+S retains collapse and return-to-input behavior after explicit search entry'
        }
    }

    It 'Sidebar New Tab shortcut works from empty list focus' -Tag 'SidebarNewTabShortcut' {
        $script:app.Launched | Should -BeTrue
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the owned window cannot take foreground for physical keyboard input'
            return
        }
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'new-tab-before-setup.png') | Out-Null
        $expand = Get-UiElement -App $script:app -Selector 'Expand sidebar'
        if ($expand -and -not $expand.isOffscreen) {
            Invoke-UiElement -App $script:app -Selector 'Expand sidebar' | Out-Null
        }
        Wait-Until -TimeoutSec 5 -Because 'the sidebar must finish expanding before resetting search' -Condition {
            $header = Get-SidebarElement -AutomationId 'VerticalTabsHeader'
            $header -and -not $header.Current.IsOffscreen
        } | Out-Null
        $searchToggle = (Get-SidebarElement -AutomationId 'SearchTabsButton').GetCurrentPattern(
            [Windows.Automation.TogglePattern]::Pattern)
        if ($searchToggle.Current.ToggleState -eq [Windows.Automation.ToggleState]::On) {
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        }
        Wait-Until -TimeoutSec 5 -Because 'search must be closed independently of the previous case' -Condition {
            $search = Get-SidebarElement -AutomationId 'SearchTextBox'
            -not $search -or $search.Current.IsOffscreen
        } | Out-Null
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'new-tab-ready.png') | Out-Null

        $baseline = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count
        foreach ($origin in @('blank-first', 'blank-repeat', 'row', 'terminal')) {
            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            if ($origin -like 'blank-*') {
                $bounds = (Get-SidebarElement -AutomationId 'ItemsList').Current.BoundingRectangle
                $rowBottom = (@(Get-VisibleTabRows) | ForEach-Object { $_.Current.BoundingRectangle.Bottom } |
                    Measure-Object -Maximum).Maximum
                $y = [int]($bounds.Top + $bounds.Height / 2)
                $y | Should -BeGreaterThan ($rowBottom + 8) -Because 'the list center must be below every tab row'
                $y | Should -BeLessThan ($bounds.Bottom - 8) -Because 'the pointer must hit genuine blank list space, not a tab row'
                Invoke-UiClick -App $script:app -Selector ItemsList | Out-Null
            }
            elseif ($origin -eq 'row') {
                @(Get-VisibleTabRows)[1].SetFocus()
                (Get-FocusedTabRowIndex) | Should -Be 1
            }
            else {
                [Windows.Automation.AutomationElement]::FocusedElement.Current.ClassName | Should -Be 'TermControl'
            }

            Send-WtWindowKey -App $script:app -Vk 0x54 -Ctrl -Shift -RequireForeground | Out-Null
            $created = $null
            try {
                (Test-Until -TimeoutSec 8 -Condition {
                    @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count -eq ($baseline + 1)
                }) | Should -BeTrue -Because "Ctrl+Shift+T from $origin must create exactly one new tab"
                $created = Get-ActivePane -App $script:app
                $created.session_id | Should -Not -Be $script:tabB.session_id
            }
            finally {
                if ($created) { Close-WtPane -App $script:app -SessionId $created.session_id }
            }
            @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count | Should -Be $baseline
        }

        Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        Wait-Until -TimeoutSec 5 -Because 'search must be visible before keyboard editing' -Condition {
            $search = Get-SidebarElement -AutomationId 'SearchTextBox'
            $search -and -not $search.Current.IsOffscreen
        } | Out-Null
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "focus-$script:marker" | Out-Null
        $search = Get-SidebarElement -AutomationId 'SearchTextBox'
        Invoke-UiClick -App $script:app -Selector SearchTextBox | Out-Null
        Wait-Until -TimeoutSec 5 -Because 'the owned search field must have keyboard focus' -Condition {
            $focused = [Windows.Automation.AutomationElement]::FocusedElement
            $focused -and $focused.Current.ProcessId -eq $script:app.Pid -and
                [Windows.Automation.Automation]::Compare($focused, $search)
        } | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x41 -Ctrl -RequireForeground | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x2E -RequireForeground | Out-Null
        Wait-Until -TimeoutSec 5 -Because 'the search field must process Ctrl+A/Delete' -Condition {
            $search.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -eq ''
        } | Out-Null
        $search.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value |
            Should -BeNullOrEmpty -Because 'handled Ctrl+A/Delete must still edit search text, not the terminal'
        @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count | Should -Be $baseline

        $query = "no-matching-tab-$script:marker"
        Set-UiValue -App $script:app -Selector SearchTextBox -Value $query | Out-Null
        (Test-Until -TimeoutSec 5 -Condition { @(Get-VisibleTabRows).Count -eq 0 }) | Should -BeTrue
        Invoke-UiClick -App $script:app -Selector ItemsList | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x54 -Ctrl -Shift -RequireForeground | Out-Null
        $created = $null
        try {
            (Test-Until -TimeoutSec 8 -Condition {
                @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count -eq ($baseline + 1)
            }) | Should -BeTrue -Because 'blank list shortcuts must also work when there is no visible row to focus'
            $created = Get-ActivePane -App $script:app
            $search.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value | Should -Be $query
        }
        finally {
            if ($created) { Close-WtPane -App $script:app -SessionId $created.session_id }
        }
        @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count | Should -Be $baseline
        Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
    }
}
