#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PRs #1043/#1052: real menu -> layout rebuild -> canonical owner and Sidebar selection.
# Pre-fix capture-retry: two pinned singletons lost Sidebar selection; active-pane stayed E_FAIL.
# Pin glyphs are view-local: hidden in Horizontal, visible in Sidebar; #1052 behavior is unchanged.
# Existing protection: SidebarTabKeyboard and PaneProgress. No real provider or pin-policy edits.
BeforeDiscovery {
    $script:Ready = [bool]((Get-Command winapp -ErrorAction SilentlyContinue) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-AppxPackage | Where-Object Name -eq 'IntelligentTerminal'))
}

Describe 'Feature: pinned tab selection' -Tag @('Feature', 'PinnedTabSelection') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
        $script:app = $null
        $script:cursor = $null
        (Get-ItTestPackage) | Should -Be Dev
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply TerminalApp.dll and wta.exe hashes from the intended source build receipt.'
        }
        $script:target = Resolve-ItApp -Package Dev
        @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables) | Should -HaveCount 0
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Recover existing configuration backup first: $path"
            }
        }
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('pinned-selection-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence | Out-Null
        $marker = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $script:titles = @("Alpha-$marker", "Beta-$marker", "Gamma-$marker")
        $pwsh = (Get-Command pwsh).Source
        $profile = '{' + [guid]::NewGuid().ToString() + '}'
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $code = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\acp.log'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
        try {
            $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
                firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                'warning.confirmOnClose' = 'never'; autoErrorDetectionEnabled = $false
                acpAgent = 'custom:pinned-selection-fixture'
                acpCustomCommand = "`"$pwsh`" -NoProfile -EncodedCommand $encoded"
                defaultProfile = $profile
                profiles = @{ list = @(@{
                    guid = $profile; name = $script:titles[0]; tabTitle = $script:titles[0]
                    suppressApplicationTitle = $true; shellIntegrationEnabled = $false
                    commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                }) }
            }
        }
        catch {
            if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                throw "Launch failed with package processes remaining; backups retained: $_"
            }
            Restore-WtConfig -App $script:target
            throw
        }
        $script:app.Launched | Should -BeTrue
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $script:cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
        $loaded = @((Get-Process -Id $script:app.Pid).Modules | Where-Object ModuleName -eq 'TerminalApp.dll')
        $loaded | Should -HaveCount 1
        $loaded[0].FileName | Should -Be (Join-Path $script:target.InstallLocation 'TerminalApp.dll')
        @{
            pid = $script:app.Pid; source = $env:ITE2E_EXPECTED_SOURCE_REVISION
            appHash = $env:ITE2E_EXPECTED_APP_SHA256; wtaHash = $env:ITE2E_EXPECTED_WTA_SHA256
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json')

        function Get-OwnedContainer {
            param([string]$Id)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.ProcessId -eq $script:app.Pid
            })
        }
        function Get-HeaderText {
            param($Parent, [string]$Title)
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Title),
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text))
            @($Parent.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                $bounds = $_.Current.BoundingRectangle
                -not $_.Current.IsOffscreen -and $_.Current.ProcessId -eq $script:app.Pid -and
                $bounds.Width -gt 0 -and $bounds.Height -gt 0 -and $bounds.Height -lt 80 -and
                $Parent.Current.BoundingRectangle.Contains($bounds)
            })
        }
        function Get-CanonicalTabs {
            @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId) | ForEach-Object {
                $tab = $_
                $panes = @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId))
                $panes | Should -HaveCount 1 -Because 'all three tabs must remain terminal singletons'
                [pscustomobject]@{ Title = $tab.title; Tab = $tab.tab_id; Session = $panes[0].session_id }
            })
        }
        function Get-VisiblePinGlyphs {
            param($Parent)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::NameProperty, 'Pinned')
            @($Parent.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                $bounds = $_.Current.BoundingRectangle
                $_.Current.ClassName -eq 'FontIcon' -and $_.Current.ProcessId -eq $script:app.Pid -and
                -not $_.Current.IsOffscreen -and $bounds.Width -gt 0 -and $bounds.Height -gt 0 -and
                $Parent.Current.BoundingRectangle.Contains($bounds)
            })
        }
        function Assert-PinGeometry {
            param($Headers, [string]$Layout, [bool[]]$Pinned)
            $reference = $Headers[2]
            $reference.textHeight | Should -BeGreaterThan 0
            for ($index = 0; $index -lt $Headers.Count; $index++) {
                $delta = ($Headers[$index].titleOffset - $reference.titleOffset) / $reference.textHeight
                if ($Layout -eq 'vertical' -and $Pinned[$index]) {
                    $delta | Should -BeGreaterThan 0 -Because 'a pinned Sidebar header reserves extra leading space compared with the same-profile ordinary Gamma header'
                }
                else {
                    [math]::Abs($delta) | Should -BeLessThan 0.5 -Because 'Horizontal and ordinary Sidebar headers share the leading slot within half the rendered text height'
                }
            }
        }
        function Assert-PinPresentation {
            param([string]$Phase, [string]$Layout, [bool[]]$Pinned = @($true, $true, $false))
            $id = if ($Layout -eq 'vertical') { 'ItemsList' } else { 'TabView' }
            $containers = @(Get-OwnedContainer $id)
            $containers | Should -HaveCount 1
            $container = $containers[0]
            $type = if ($Layout -eq 'vertical') { [Windows.Automation.ControlType]::ListItem } else { [Windows.Automation.ControlType]::TabItem }
            $rows = @()
            foreach ($title in $script:titles) {
                $headers = @(Get-HeaderText -Parent $container -Title $title)
                $headers | Should -HaveCount 1
                $row = $headers[0]
                while ($row -and $row.Current.ControlType -ne $type -and
                    -not [Windows.Automation.Automation]::Compare($row, $container)) {
                    $row = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($row)
                }
                if (-not $row -or $row.Current.ControlType -ne $type) { throw "No owned $type header row for $title." }
                $row.Current.ProcessId | Should -Be $script:app.Pid
                $row.Current.IsOffscreen | Should -BeFalse
                $rows += $row
            }
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $windowBounds = $window.Current.BoundingRectangle
            $windowBounds.Height | Should -BeGreaterThan 0
            $observed = @()
            for ($index = 0; $index -lt $rows.Count; $index++) {
                $rows[$index].Current.ControlType | Should -Be $type -Because 'capture the native TabItem or Sidebar ListItem, including its leading glyphs'
                $rows[$index].Current.ClassName | Should -Not -Be 'TermControl'
                $rows[$index].Current.ProcessId | Should -Be $script:app.Pid
                $rows[$index].Current.IsOffscreen | Should -BeFalse
                $bounds = $rows[$index].Current.BoundingRectangle
                $bounds.Width | Should -BeGreaterThan 0
                $bounds.Height | Should -BeGreaterThan 0
                $bounds.Height | Should -BeLessThan ($windowBounds.Height / 4) -Because 'the header must stay small relative to the owned window at any DPI'
                $container.Current.BoundingRectangle.Contains($bounds) | Should -BeTrue
                $windowBounds.Contains($bounds) | Should -BeTrue
                $pins = @(Get-VisiblePinGlyphs -Parent $rows[$index])
                $text = @(Get-HeaderText -Parent $rows[$index] -Title $script:titles[$index])
                $text | Should -HaveCount 1
                $textBounds = $text[0].Current.BoundingRectangle
                $name = "$Phase-$index-header"
                $bitmap = [Drawing.Bitmap]::new([int][math]::Ceiling($bounds.Width), [int][math]::Ceiling($bounds.Height))
                $graphics = [Drawing.Graphics]::FromImage($bitmap)
                try {
                    $graphics.CopyFromScreen([int]$bounds.X, [int]$bounds.Y, 0, 0, $bitmap.Size)
                    $bitmap.Save((Join-Path $script:evidence "$name.png"), [Drawing.Imaging.ImageFormat]::Png)
                }
                finally { $graphics.Dispose(); $bitmap.Dispose() }
                $observed += @{
                    title = $script:titles[$index]; visiblePinPeersDiagnostic = $pins.Count
                    titleOffset = $textBounds.Left - $bounds.Left; textHeight = $textBounds.Height
                    automationName = $rows[$index].Current.Name
                    accessiblePinned = $rows[$index].Current.Name -match '(?:^|,\s*)Pinned(?:,|$)'
                    bounds = $bounds.ToString(); capture = "$name.png"; backend = 'Desktop compositor CopyFromScreen'
                }
            }
            @{
                layout = $Layout; headers = $observed
                oracle = 'Same-profile title-leading offsets measure reserved layout space; FontIcon peers are diagnostic only and may be absent in UWP.'
                renderedVisualVerdict = 'REQUIRES_VISUAL_REVIEW'
            } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence "$Phase-pins.json")
            for ($index = 0; $index -lt $observed.Count; $index++) {
                $observed[$index].accessiblePinned | Should -Be $Pinned[$index] -Because 'hiding the horizontal glyph must not remove the tab accessibility label'
            }
            Assert-PinGeometry -Headers $observed -Layout $Layout -Pinned $Pinned
            $observed
        }
        function Assert-CanonicalTabs {
            $tabs = @(Get-CanonicalTabs)
            $tabs | Should -HaveCount 3
            ($tabs.Title -join '|') | Should -BeExactly ($script:titles -join '|')
            ($tabs.Session -join '|') | Should -BeExactly ($script:sessions -join '|')
            $tabs | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'canonical.json')
        }
        function Open-OwnedTabMenu {
            param([string]$Title)
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $terminalCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ClassNameProperty, 'TermControl')
            $terminals = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $terminalCondition) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.ProcessId -eq $script:app.Pid
            })
            $terminals | Should -HaveCount 1
            $terminalBounds = $terminals[0].Current.BoundingRectangle
            $terminalBounds.Width | Should -BeGreaterThan 0
            $terminalBounds.Height | Should -BeGreaterThan 0
            # Leave the previous header so its tooltip cannot cover the next right-click.
            [ItE2E.ItWtWin32Input]::SetCursorPos(
                [int]($terminalBounds.X + $terminalBounds.Width / 2),
                [int]($terminalBounds.Y + $terminalBounds.Height / 2)) | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground -Repeat 2 | Out-Null
            Wait-UiElement -App $script:app -Selector PinTabMenuItem -Gone -TimeoutSec 5 | Out-Null
            $before = Get-ActivePane -App $script:app
            $layout = Get-WtSetting -App $script:app -Key tabLayout
            $id = if ($layout -eq 'vertical') { 'ItemsList' } else { 'TabView' }
            $parents = @(Get-OwnedContainer $id)
            $parents | Should -HaveCount 1
            $titleHeaders = @(Get-HeaderText -Parent $parents[0] -Title $Title)
            $titleHeaders | Should -HaveCount 1
            $tree = Get-UiTree -App $script:app -Selector $id -Depth 12
            $matches = @([regex]::Matches($tree, '(?m)^(?<indent>[ \t]*)(?<selector>lbl-textview-\S+|TextView) Text "' + [regex]::Escape($Title) + '"'))
            $matches.Count | Should -BeGreaterThan 0
            $depth = ($matches | ForEach-Object { $_.Groups['indent'].Length } | Measure-Object -Minimum).Minimum
            $matches = @($matches | Where-Object { $_.Groups['indent'].Length -eq $depth })
            $matches | Should -HaveCount 1 -Because 'right-click targets the exact parent header, never terminal text'
            Invoke-UiClick -App $script:app -Selector $matches[0].Groups['selector'].Value -Right | Out-Null
            @{
                title = $Title; targetBounds = $titleHeaders[0].Current.BoundingRectangle.ToString()
                cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
                foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
            } | ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $script:evidence 'menu-pointer.jsonl')
            try {
                Wait-UiElement -App $script:app -Selector PinTabMenuItem -TimeoutSec 10 | Out-Null
            }
            catch {
                Get-UiTree -App $script:app -Depth 16 |
                    Set-Content -LiteralPath (Join-Path $script:evidence "menu-$Title-failed.tree.txt")
                throw
            }
            $after = Get-ActivePane -App $script:app
            @{ title = $Title; beforeMenu = $before; afterMenu = $after } |
                ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath (Join-Path $script:evidence 'menus.jsonl')
            # Right-click may select its owner; the subsequent action must preserve that actual owner.
            [string]$after.session_id
        }
        function Assert-PinMenu {
            param([string]$Title, [bool]$Pinned)
            $owner = Open-OwnedTabMenu $Title
            $label = if ($Pinned) { 'Unpin tab' } else { 'Pin tab' }
            Wait-UiElement -App $script:app -Selector $label -TimeoutSec 10 | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Be $owner
        }
        function Assert-LayoutReady {
            param([string]$Layout)
            $script:requestedLayout = $Layout
            $script:phaseLog = Join-Path $script:evidence "$Layout-observations.jsonl"
            $script:unexpectedLayoutError = $null
            $predicate = {
                $observation = @{ utc = [datetime]::UtcNow; layout = $script:requestedLayout; ready = $false }
                try {
                    $observation.persistedLayout = Get-WtSetting -App $script:app -Key tabLayout
                    $observation.active = Get-ActivePane -App $script:app
                    $id = if ($script:requestedLayout -eq 'vertical') { 'ItemsList' } else { 'TabView' }
                    $parents = @(Get-OwnedContainer $id)
                    $observation.containers = $parents.Count
                    $headers = if ($parents.Count -eq 1) { @(Get-HeaderText -Parent $parents[0] -Title $script:titles[0]) } else { @() }
                    $observation.headers = $headers.Count
                    $selectionReady = $true
                    if ($script:requestedLayout -eq 'vertical') {
                        $selected = if ($parents.Count -eq 1) {
                            $pattern = $parents[0].GetCurrentPattern([Windows.Automation.SelectionPattern]::Pattern)
                            @($pattern.Current.GetSelection())
                        } else { @() }
                        $observation.selected = @($selected | ForEach-Object { $_.Current.Name })
                        $selectionReady = $selected.Count -eq 1 -and
                            $selected[0].Current.ControlType -eq [Windows.Automation.ControlType]::ListItem -and
                            -not $selected[0].Current.IsOffscreen -and
                            $selected[0].Current.ProcessId -eq $script:app.Pid -and
                            @(Get-HeaderText -Parent $selected[0] -Title $script:titles[0]).Count -eq 1
                    }
                    $focus = [Windows.Automation.AutomationElement]::FocusedElement
                    $observation.focusClass = $focus.Current.ClassName
                    $observation.focusPid = $focus.Current.ProcessId
                    $observation.ready = $observation.persistedLayout -eq $script:requestedLayout -and
                        $observation.active.session_id -eq $script:sessions[0] -and
                        $parents.Count -eq 1 -and $headers.Count -eq 1 -and $selectionReady -and
                        $observation.focusPid -eq $script:app.Pid -and $observation.focusClass -eq 'TermControl' -and
                        -not $script:unexpectedLayoutError
                }
                catch {
                    $observation.error = $_.Exception.Message
                    $observation.stack = $_.ScriptStackTrace
                    $observation.expectedTransition = $_.Exception.Message -match 'GetActivePane failed: 0x80004005'
                    $observation | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $script:phaseLog
                    if (-not $observation.expectedTransition) { $script:unexpectedLayoutError = $_.Exception.Message }
                    return $false
                }
                $observation | ConvertTo-Json -Depth 8 -Compress | Add-Content -LiteralPath $script:phaseLog
                [bool]$observation.ready
            }
            try {
                Wait-Until -TimeoutSec 10 -IntervalSec 0.25 -Because "$Layout restores Alpha's active pane, terminal focus and realized selection" -Condition $predicate | Out-Null
            }
            catch {
                Get-UiTree -App $script:app -Depth 12 | Set-Content -LiteralPath (Join-Path $script:evidence "$Layout-failed.tree.txt")
                @{ verdict = 'FAILED'; layout = $Layout; error = $_.Exception.Message; unexpectedError = $script:unexpectedLayoutError } |
                    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "$Layout-result.json")
                throw
            }
            Assert-CanonicalTabs
        }
    }

    AfterAll {
        try {
            if ($script:app) {
                Stop-Terminal -App $script:app -RestoreSettings $false
                @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables) | Should -HaveCount 0 -Because 'configuration restores only after the fixture package is inactive'
                Restore-WtConfig -App $script:target
            }
        }
        finally {
            if ($script:cursor) {
                [ItE2E.ItWtWin32Input]::SetCursorPos($script:cursor[0], $script:cursor[1]) | Should -BeTrue
            }
        }
    }

    It 'Pinned tabs preserve selection across layout changes' {
        (Test-WtWindowKeyFocusable -App $script:app) | Should -BeTrue -Because 'real context menus require an interactive desktop'
        $alpha = Get-ActivePane -App $script:app
        $beta = New-WtTab -App $script:app -Title $script:titles[1] -Cwd $script:evidence
        $gamma = New-WtTab -App $script:app -Title $script:titles[2] -Cwd $script:evidence
        $script:sessions = @($alpha.session_id, $beta.session_id, $gamma.session_id)
        Assert-CanonicalTabs
        Set-WtPaneFocus -App $script:app -SessionId $script:sessions[0]
        foreach ($title in $script:titles[0..1]) {
            $owner = Open-OwnedTabMenu $title
            Wait-UiElement -App $script:app -Selector 'Pin tab' | Out-Null
            Invoke-UiElement -App $script:app -Selector PinTabMenuItem | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Be $owner
            Assert-PinMenu -Title $title -Pinned $true
        }
        Assert-CanonicalTabs
        # Complete the primary owner/selection regression before any secondary visual oracle.
        foreach ($layout in @('horizontal', 'vertical')) {
            Set-WtPaneFocus -App $script:app -SessionId $script:sessions[0]
            (Open-OwnedTabMenu $script:titles[0]) | Should -Be $script:sessions[0]
            $label = if ($layout -eq 'horizontal') { 'Switch to horizontal tabs' } else { 'Switch to sidebar' }
            Invoke-UiElement -App $script:app -Selector $label | Out-Null
            # Never force focus after the switch: persistence alone is not readiness.
            Assert-LayoutReady $layout
            foreach ($title in $script:titles[0..1]) { Assert-PinMenu -Title $title -Pinned $true }
            Assert-PinMenu -Title $script:titles[2] -Pinned $false
            Assert-CanonicalTabs
        }
        @{
            ownerSelection = 'PASSED'; layouts = @('horizontal', 'vertical'); sessions = $script:sessions
            secondaryGeometry = 'NOT_YET_CHECKED'; renderedVisualVerdict = 'REQUIRES_VISUAL_REVIEW'
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence 'primary-layout.json')
        $baseline = @(Assert-PinPresentation -Phase 'sidebar-before' -Layout vertical)
        foreach ($layout in @('horizontal', 'vertical')) {
            Set-WtPaneFocus -App $script:app -SessionId $script:sessions[0]
            (Open-OwnedTabMenu $script:titles[0]) | Should -Be $script:sessions[0]
            $label = if ($layout -eq 'horizontal') { 'Switch to horizontal tabs' } else { 'Switch to sidebar' }
            Invoke-UiElement -App $script:app -Selector $label | Out-Null
            Assert-LayoutReady $layout
            Assert-PinPresentation -Phase "$layout-after" -Layout $layout | Out-Null
            foreach ($title in $script:titles[0..1]) { Assert-PinMenu -Title $title -Pinned $true }
            Assert-PinMenu -Title $script:titles[2] -Pinned $false
            Assert-CanonicalTabs
        }
        $owner = Open-OwnedTabMenu $script:titles[1]
        Wait-UiElement -App $script:app -Selector 'Unpin tab' | Out-Null
        Invoke-UiElement -App $script:app -Selector PinTabMenuItem | Out-Null
        (Get-ActivePane -App $script:app).session_id | Should -Be $owner
        Assert-PinMenu -Title $script:titles[0] -Pinned $true
        Assert-PinMenu -Title $script:titles[1] -Pinned $false
        Assert-PinMenu -Title $script:titles[2] -Pinned $false
        # #1052 retains first-ordinary placement, not original-position restoration.
        Assert-CanonicalTabs
        $unpinned = @(Assert-PinPresentation -Phase 'sidebar-unpin' -Layout vertical -Pinned @($true, $false, $false))
        ($baseline[1].titleOffset - $baseline[2].titleOffset) |
            Should -BeGreaterThan ($unpinned[1].titleOffset - $unpinned[2].titleOffset) -Because 'unpinning Beta removes its extra leading slot relative to the unchanged Gamma control'
        @{
            ownerSelection = 'PASSED'; matchedHeaderGeometry = 'PASSED'
            renderedVisualVerdict = 'REQUIRES_VISUAL_REVIEW'
            instruction = 'Review full-header compositor crops for actual Sidebar pins and Horizontal pin absence for the separate C373 MANUAL checklist item "Pinned tab glyphs render only in Sidebar"; a passing test credits C372 selection, identity, accessibility and geometry only, not this manual sign-off.'
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'acceptance.json')
    }
}
