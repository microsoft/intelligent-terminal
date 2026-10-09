#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #1070: selected Light headers previously had black provider glyphs beside white titles.
# Contract: native identity and independent header layout survive Horizontal -> Sidebar -> Horizontal.
# Render oracle: scoped compositor crops require independent glyph/title foreground review.
# No UIA foreground property, pixel verdict, hooks, authenticated CLI, or global theme changes.
# Existing Actions/Combined suites protect creation, grouping, reordering and History behavior.

Describe 'Feature: Sidebar provider appearance' -Tag @('Feature', 'SidebarProviderAppearance') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
        function Find-AppearanceElement {
            param($Parent, [string]$Value, [switch]$Name)
            $property = if ($Name) { [Windows.Automation.AutomationElement]::NameProperty } else {
                [Windows.Automation.AutomationElement]::AutomationIdProperty
            }
            @($Parent.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new($property, $Value)) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.ProcessId -eq $script:app.Pid
            })
        }
        function Get-AppearanceRoot {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $root
        }
        function Get-AppearanceRawPeers {
            param($Parent, [int]$Depth = 0)
            if ($Depth -ge 6) { return }
            $walker = [Windows.Automation.TreeWalker]::RawViewWalker
            $child = $walker.GetFirstChild($Parent)
            while ($child) {
                @{
                    depth = $Depth; name = $child.Current.Name; class = $child.Current.ClassName
                    automation_id = $child.Current.AutomationId; bounds = $child.Current.BoundingRectangle.ToString()
                    offscreen = $child.Current.IsOffscreen; runtime_id = @($child.GetRuntimeId())
                }
                Get-AppearanceRawPeers $child ($Depth + 1)
                $child = $walker.GetNextSibling($child)
            }
        }
        function Save-AppearanceHeaders {
            param([string]$Layout, [string]$Phase)
            $containerId = if ($Layout -eq 'vertical') { 'ItemsList' } else { 'TabView' }
            $container = @(Find-AppearanceElement (Get-AppearanceRoot) $containerId)
            $container | Should -HaveCount 1
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $windowBounds = (Get-AppearanceRoot).Current.BoundingRectangle
            [ItE2E.ItWtWin32Input]::SetCursorPos(
                [int]($windowBounds.Left + $windowBounds.Width / 2),
                [int]($windowBounds.Bottom - 80)) | Should -BeTrue
            $records = foreach ($native in $script:natives) {
                $texts = @(Find-AppearanceElement $container[0] $native.title -Name | Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text
                })
                $texts | Should -HaveCount 1
                $row = $texts[0]
                $type = if ($Layout -eq 'vertical') { [Windows.Automation.ControlType]::ListItem } else {
                    [Windows.Automation.ControlType]::TabItem
                }
                while ($row -and $row.Current.ControlType -ne $type) {
                    $row = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($row)
                }
                if (-not $row) { throw "Missing owned $type header for $($native.title)" }
                $bounds = $row.Current.BoundingRectangle
                $row.Current.ProcessId | Should -Be $script:app.Pid
                $row.Current.IsOffscreen | Should -BeFalse
                $bounds.Width | Should -BeGreaterThan 0
                $bounds.Height | Should -BeGreaterThan 0
                $bounds.Height | Should -BeLessThan ($windowBounds.Height / 4)
                $container[0].Current.BoundingRectangle.Contains($bounds) | Should -BeTrue
                $windowBounds.Contains($bounds) | Should -BeTrue
                Wait-Until -TimeoutSec 5 -Because 'header selection catches up with protocol focus' -Condition {
                    $row.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected -eq
                        ($native.session_id -eq $script:selected)
                } | Out-Null
                $selected = $row.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected
                $selected | Should -Be ($native.session_id -eq $script:selected)
                $capture = "$Phase-$($native.provider).png"
                $image = [Drawing.Bitmap]::new([int][Math]::Ceiling($bounds.Width), [int][Math]::Ceiling($bounds.Height))
                $graphics = [Drawing.Graphics]::FromImage($image)
                try {
                    $graphics.CopyFromScreen([int]$bounds.Left, [int]$bounds.Top, 0, 0, $image.Size)
                    $image.Save((Join-Path $script:evidence $capture), [Drawing.Imaging.ImageFormat]::Png)
                }
                finally { $graphics.Dispose(); $image.Dispose() }
                $textBounds = $texts[0].Current.BoundingRectangle
                @{
                    provider_id = $native.provider; pane_id = $native.session_id; native_process_pid = $native.pid
                    selected = $selected; title = $native.title; capture = $capture
                    row_runtime_id = @($row.GetRuntimeId()); title_runtime_id = @($texts[0].GetRuntimeId())
                    raw_peers_diagnostic_only = @(Get-AppearanceRawPeers $row)
                    row_bounds = $bounds.ToString(); title_bounds = $textBounds.ToString()
                    title_offset = $textBounds.Left - $bounds.Left; title_height = $textBounds.Height
                    expected_provider_mask_uri = "ms-appx:///AgentIcons/Masks/$($native.provider).png"
                    mask_uri_source = 'canonical protocol provider ID; URI is expected, not queried rendered state'
                }
            }
            @($records) | Should -HaveCount 2
            ($records[0].row_runtime_id -join ',') | Should -Not -Be ($records[1].row_runtime_id -join ',')
            [Math]::Abs($records[0].title_offset - $records[1].title_offset) |
                Should -BeLessThan ($records[0].title_height / 2) -Because 'provider-specific headers reserve the same clean leading slot'
            @{
                phase = $Phase; layout = $Layout; application_theme_setting = Get-WtSetting -App $script:app -Key theme
                configured_profile_tab_color = '#000000'; headers = @($records)
                backend = 'Desktop compositor CopyFromScreen; not RenderTargetBitmap'
                automated_verdict = 'metadata, selection, geometry and independent header peers only'
                rendered_foreground_verdict = 'REQUIRES_VISUAL_REVIEW'
                review = 'Compare actual glyph strokes with title foreground in each selected/unselected crop; ignore background/corners. Verify Light/Dark appearance and distinct Copilot/OpenCode silhouettes.'
            } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.json")
        }
        function Assert-AppearanceIdentity {
            $panes = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId) | ForEach-Object {
                Get-WtPanes -App $script:app -TabId ([string]$_.tab_id) -WindowId ([string]$script:app.WindowId)
            })
            foreach ($native in $script:natives) {
                $pane = @($panes | Where-Object session_id -EQ $native.session_id)
                $pane | Should -HaveCount 1
                $pane[0].native_agent_provider_id | Should -Be $native.provider
                (Get-WtPaneStatus -App $script:app -SessionId $native.session_id).pid | Should -Be $native.pid
                (Get-Process -Id $native.pid -ErrorAction Stop).HasExited | Should -BeFalse
            }
            $ordinary = @($panes | Where-Object session_id -EQ $script:ordinary)
            $ordinary | Should -HaveCount 1
            [string]$ordinary[0].native_agent_provider_id | Should -BeNullOrEmpty
            ConvertTo-Json -InputObject $panes -Depth 10 |
                Set-Content -LiteralPath (Join-Path $script:evidence 'protocol-panes.json')
        }
        function Switch-AppearanceLayout {
            param([string]$Layout)
            $id = if ($Layout -eq 'horizontal') { 'ItemsList' } else { 'TabView' }
            $tree = Get-UiTree -App $script:app -Selector $id -Depth 12
            $matches = @([regex]::Matches($tree, '(?m)^\s*(?<selector>lbl-textview-\S+|TextView) Text "' +
                [regex]::Escape($script:natives[0].title) + '"'))
            $matches | Should -HaveCount 1
            Invoke-UiClick -App $script:app -Selector $matches[0].Groups['selector'].Value -Right | Out-Null
            $label = if ($Layout -eq 'horizontal') { 'Switch to horizontal tabs' } else { 'Switch to sidebar' }
            Invoke-UiElement -App $script:app -Selector $label | Out-Null
            $newId = if ($Layout -eq 'horizontal') { 'TabView' } else { 'ItemsList' }
            Wait-Until -TimeoutSec 10 -Because "$Layout realizes its own headers" -Condition {
                (Get-WtSetting -App $script:app -Key tabLayout) -eq $Layout -and
                    @(Find-AppearanceElement (Get-AppearanceRoot) $newId).Count -eq 1
            } | Out-Null
            Assert-AppearanceIdentity
        }
    }

    BeforeEach {
        $script:app = $null
        $script:cursor = $null
        $env:ITE2E_PACKAGE | Should -Be Dev
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        $head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $head) { throw 'Cannot resolve test worktree revision.' }
        if (-not $env:ITE2E_SOURCE_COMMIT -or -not $env:ITE2E_SOURCE_COMMIT.StartsWith($head, [StringComparison]::Ordinal)) {
            throw 'Supply the exact frozen-source build receipt.'
        }
        $env:ITE2E_EXPECTED_APP_SHA256 | Should -Not -BeNullOrEmpty
        $env:ITE2E_EXPECTED_WTA_SHA256 | Should -Not -BeNullOrEmpty
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) { throw "Recover existing backup first: $path" }
        }
    }
    AfterEach {
        try {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
        finally {
            if ($script:cursor) { [void][ItE2E.ItWtWin32Input]::SetCursorPos($script:cursor[0], $script:cursor[1]) }
        }
    }

    It 'Native provider headers retain independent layout and render evidence in <Theme> theme' -ForEach @(
        @{ Theme = 'light' }, @{ Theme = 'dark' }
    ) {
        $run = [guid]::NewGuid().ToString('N')
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) "provider-appearance-$Theme-$run"
        New-Item -ItemType Directory -Path $script:evidence | Out-Null
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-InteractiveDelegate.ps1')).Path
        $log = Join-Path $script:evidence 'launch.jsonl'
        $profile = '{' + [guid]::NewGuid().ToString() + '}'
        try {
            $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                theme = $Theme; language = 'en-US'; tabLayout = 'horizontal'; startupActions = ''
                firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                autoErrorDetectionEnabled = $false; autoFixEnabled = $false; 'warning.confirmOnClose' = 'never'
                acpAgent = 'custom:appearance'; acpCustomCommand = "`"$pwsh`" -NoProfile -File `"$(Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')`" -LogPath `"$(Join-Path $script:evidence 'acp.log')`""
                defaultProfile = $profile
                profiles = @{ list = @(@{
                    guid = $profile; name = "UserDemo ordinary $run"; tabColor = '#000000'
                    tabTitle = "UserDemo ordinary $run"; suppressApplicationTitle = $true
                    shellIntegrationEnabled = $false; commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                }) }
            }
        }
        catch {
            if (-not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) { Restore-WtConfig -App $script:target }
            throw
        }
        $script:app.Launched | Should -BeTrue
        (Get-WtSetting -App $script:app -Key theme) | Should -Be $Theme
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true
        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $script:cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
        $loaded = @((Get-Process -Id $script:app.Pid).Modules | Where-Object ModuleName -EQ 'TerminalApp.dll')
        $loaded | Should -HaveCount 1
        $loaded[0].FileName | Should -Be (Join-Path $script:target.InstallLocation 'TerminalApp.dll')
        $script:ordinary = (Get-ActivePane -App $script:app).session_id
        $script:natives = foreach ($provider in @('copilot', 'opencode')) {
            $title = "UserDemo $provider $($run.Substring(0, 8))"
            $command = "`"$pwsh`" -NoLogo -NoProfile -File `"$fixture`" -LogPath `"$log`" -RunId $run"
            $created = Invoke-WtCli -App $script:app -Arguments @('new-tab', '-c', $command, '-d', $script:evidence, '-n', $title, '--agent-provider', $provider)
            Wait-Until -TimeoutSec 10 -Because 'owned fixture writes its native process receipt' -Condition {
                (Test-Path $log) -and @(Get-Content $log | ForEach-Object { $_ | ConvertFrom-Json } |
                    Where-Object pane_session_id -EQ $created.session_id).Count -eq 1
            } | Out-Null
            $receipt = @(Get-Content $log | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object pane_session_id -EQ $created.session_id)[0]
            $receipt.run_id | Should -Be $run
            $receipt.pid | Should -BeGreaterThan 0
            $receipt.command_line | Should -Match '\-NoProfile'
            $receipt.command_line | Should -Not -Match '\-Canonical|\-Resume'
            $probe = "probe-$($run.Substring(0, 8))"
            Send-WtInput -App $script:app -SessionId $created.session_id -Text "$probe`r"
            Wait-Until -TimeoutSec 10 -Because 'real fixture process responds through its own terminal buffer' -Condition {
                (Get-WtCapture -App $script:app -SessionId $created.session_id) -like "*ITE2E-DELEGATE-ALIVE * $probe*"
            } | Out-Null
            [pscustomobject]@{ provider = $provider; title = $title; session_id = $created.session_id; pid = [int]$receipt.pid }
        }
        ($script:natives.pid | Select-Object -Unique) | Should -HaveCount 2
        Assert-AppearanceIdentity
        foreach ($phase in @('horizontal-first', 'vertical', 'horizontal-restored')) {
            $layout = if ($phase -eq 'vertical') { 'vertical' } else { 'horizontal' }
            if ($phase -ne 'horizontal-first') { Switch-AppearanceLayout $layout }
            foreach ($native in $script:natives) {
                Set-WtPaneFocus -App $script:app -SessionId $native.session_id
                $script:selected = $native.session_id
                Save-AppearanceHeaders $layout "$phase-selected-$($native.provider)"
            }
            if ($layout -eq 'vertical') {
                @(Find-AppearanceElement (Get-AppearanceRoot) 'Tabs' -Name) | Should -HaveCount 1
                Invoke-UiClick -App $script:app -Selector VerticalTabsHeaderButton | Out-Null
                Wait-Until -TimeoutSec 5 -Because 'Agents view filters ordinary shell by real native metadata' -Condition {
                    @(Find-AppearanceElement (Get-AppearanceRoot) 'Agents' -Name).Count -eq 1
                } | Out-Null
                $list = @(Find-AppearanceElement (Get-AppearanceRoot) ItemsList)
                $list | Should -HaveCount 1
                $rows = @($list[0].FindAll([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,
                        [Windows.Automation.ControlType]::ListItem)) | Where-Object {
                    -not $_.Current.IsOffscreen -and
                    -not $_.FindFirst([Windows.Automation.TreeScope]::Descendants,
                        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton')) -and
                    -not $_.FindFirst([Windows.Automation.TreeScope]::Descendants,
                        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryProviderIcon'))
                })
                $canonical = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId)
                foreach ($native in $script:natives) {
                    $context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $native.session_id)
                    [string]$context.pane.session_id | Should -Be $native.session_id
                    $context.pane.pid | Should -Be $native.pid
                    @($canonical | Where-Object { $_.tab_id -eq $context.pane.tab_id -and $_.title -eq $native.title }) | Should -HaveCount 1
                    @($rows | Where-Object {
                        @(Find-AppearanceElement $_ $native.title -Name).Count -gt 0
                    }) | Should -HaveCount 1
                }
                $ordinaryContext = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $script:ordinary)
                $ordinaryTab = @($canonical | Where-Object tab_id -eq $ordinaryContext.pane.tab_id)
                $ordinaryTab | Should -HaveCount 1
                @($rows | Where-Object {
                    @(Find-AppearanceElement $_ $ordinaryTab[0].title -Name).Count -gt 0
                }) | Should -HaveCount 0 -Because 'ordinary shell must not become an Agent live row'
                foreach ($native in $script:natives) {
                    Set-WtPaneFocus -App $script:app -SessionId $native.session_id
                    $script:selected = $native.session_id
                    Save-AppearanceHeaders 'vertical' "vertical-agents-selected-$($native.provider)"
                }
                Invoke-UiClick -App $script:app -Selector VerticalTabsHeaderButton | Out-Null
                Wait-Until -TimeoutSec 5 -Because 'Tabs view restores before the horizontal round trip' -Condition {
                    @(Find-AppearanceElement (Get-AppearanceRoot) 'Tabs' -Name).Count -eq 1
                } | Out-Null
            }
            Assert-AppearanceIdentity
        }
        @{
            source_commit = $env:ITE2E_SOURCE_COMMIT; app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256
            wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256; app_pid = $script:app.Pid
            install_location = $script:target.InstallLocation; application_theme_setting = $Theme
            natives = $script:natives; ordinary_pane = $script:ordinary
            rendered_foreground_verdict = 'REQUIRES_VISUAL_REVIEW'
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence 'acceptance.json')
    }
}
