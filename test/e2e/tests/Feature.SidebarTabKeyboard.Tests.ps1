#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object Name -like '*IntelligentTerminal*') -and
        (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: Sidebar tab keyboard navigation' -Tag @('Feature', 'SidebarTabKeyboard') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes

        $script:app = $null
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'
            tabLayout = 'vertical'
            startupActions = ''
            firstWindowPreference = 'defaultProfile'
            windowingBehavior = 'useNew'
            'warning.confirmOnClose' = 'never'
        }

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
            $found = $list.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
            @(
                for ($i = 0; $i -lt $found.Count; $i++) {
                    $row = $found[$i]
                    $bounds = $row.Current.BoundingRectangle
                    if (-not $row.Current.IsOffscreen -and $bounds.Width -gt 0 -and $bounds.Height -gt 0) {
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

        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidenceDir = Join-Path ([IO.Path]::GetFullPath($artifactRoot)) "sidebar-tab-keyboard-$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null
        Wait-UiElement -App $script:app -Selector VerticalTabsHeader | Out-Null
        $initialTabs = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId))
        if ($initialTabs.Count -ne 1) { throw "Expected one initial terminal tab, found $($initialTabs.Count)." }

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
                $search = Get-SidebarElement -AutomationId 'SearchTextBox'
                $search -and -not $search.Current.IsOffscreen -and $search.Current.HasKeyboardFocus
            }) | Should -BeTrue -Because 'Ctrl+Shift+S must enter the Sidebar search box'

            Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { (Get-FocusedTabRowIndex) -ge 0 }) |
                Should -BeTrue -Because 'Tab from hotkey-opened search must enter a Sidebar tab row'
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
}
