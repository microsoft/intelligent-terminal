#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Six timestamp fixtures follow the configured UI language: LONG numeric
# minutes/hours/days, then UTC long dates at seven days and older.
# Trigger/boundary: listing-capable ACP fixture -> deployed WTA -> native XAML.
# Oracle: source timestamps + capture interval -> Windows ICU; actual UIA.
# Control: unique Demo search excludes private history; exact three text leaves,
# no ownership action or Content-view decorative icon. The suite submits no
# hooks or prompts; the fixture supplies only the tested rows. Normal read-only
# native-provider startup initialize/session-list calls may still occur.
# Existing protection: CombinedAgentsSidebar C387 and formatter unit tests.

Describe 'Feature: Sidebar compact relative time' -Tag @('Feature', 'SidebarRelativeTime') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TestTerminalCleanup.ps1')
        . (Join-Path $PSScriptRoot '..\fixtures\SidebarRelativeTimeOracle.ps1')
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'Select Dev explicitly for PR #1070.' }
        $script:target = Resolve-ItApp -Package Dev
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'Dev must be inactive; this suite will not close existing windows.'
        }
        foreach ($pair in @(@('WtaPath', 'ITE2E_EXPECTED_WTA_SHA256'), @('App', 'ITE2E_EXPECTED_APP_SHA256'))) {
            $expected = [Environment]::GetEnvironmentVariable($pair[1])
            if (-not $expected) { throw "Required exact-source receipt: $($pair[1])" }
            $path = if ($pair[0] -eq 'App') { Join-Path $script:target.InstallLocation 'TerminalApp.dll' } else { $script:target.WtaPath }
            (Get-FileHash -LiteralPath $path).Hash | Should -Be $expected
        }
        $head = git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD
        if (-not $env:ITE2E_SOURCE_COMMIT -or -not $env:ITE2E_SOURCE_COMMIT.StartsWith($head, [StringComparison]::Ordinal)) {
            throw 'Source receipt must identify the current worktree HEAD.'
        }
        $script:deadline = [DateTimeOffset]::UtcNow.AddSeconds(300)
        function Get-AgeParts($Element, [switch]$Content) {
            $walker = if ($Content) { [Windows.Automation.TreeWalker]::ContentViewWalker } else { [Windows.Automation.TreeWalker]::RawViewWalker }
            for ($child = $walker.GetFirstChild($Element); $child; $child = $walker.GetNextSibling($child)) {
                $child
                Get-AgeParts $child -Content:$Content
            }
        }
        function Get-AgeElement([string]$Id) {
            if ([DateTimeOffset]::UtcNow -gt $script:deadline) { throw 'Compact age suite exceeded 300 seconds.' }
            $window = [Windows.Automation.AutomationElement]::FromHandle([intptr][long]$script:app.Hwnd)
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $Id))
        }
        function Get-AgeRows {
            $list = Get-AgeElement ItemsList
            if ($list -and -not $list.Current.IsOffscreen) {
                @($list.FindAll([Windows.Automation.TreeScope]::Children,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,
                        [Windows.Automation.ControlType]::ListItem)))
            }
        }
    }

    It 'Sidebar compact ages localize six units and mirror readable rows (<Locale>)' -ForEach @(
        @{ Locale = 'en-US'; Rtl = $false }, @{ Locale = 'ar-SA'; Rtl = $true }
    ) {
        $script:app = $null
        $launchStarted = $null
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('sidebar-relative-time-' + $Locale + '-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $evidence -Force | Out-Null
        $marker = 'Demo-' + [guid]::NewGuid().ToString('N')
        $provider = 'custom:compact-age-fixture'
        $providerName = $provider
        $now = [DateTimeOffset]::UtcNow
        $units = @('minute', 'hour', 'day', 'week', 'month', 'year')
        $times = @($now.AddSeconds(-130), $now.AddHours(-3).AddMinutes(-2), $now.AddDays(-6),
            $now.AddDays(-7), $now.AddMonths(-2).AddDays(-1), $now.AddYears(-1).AddDays(-1))
        $history = @(for ($i = 0; $i -lt 6; $i++) {
            @{ sessionId = "$marker-$i"; title = "$marker $($units[$i])"; cwd = $evidence
                updatedAt = $times[$i].UtcDateTime.ToString("yyyy-MM-dd'T'HH:mm:ss.fff'Z'") }
        })
        $historyPath = Join-Path $evidence 'history.json'
        @{ sessions = $history } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $historyPath -Encoding utf8
        $fixtureLog = Join-Path $evidence 'acp.log'
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $code = "& '$($fixture.Replace("'", "''"))' -LogPath '$($fixtureLog.Replace("'", "''"))' -HistoryPath '$($historyPath.Replace("'", "''"))'"
        $command = 'pwsh -NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
        $hashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) { throw "Unrecovered backup: $path" }
            $hashes[$path] = if (Test-Path $path) { (Get-FileHash $path).Hash } else { $null }
        }
        try {
            if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) { throw 'Dev became active.' }
            $profileId = '{' + [guid]::NewGuid().ToString() + '}'
            $launchStarted = Get-Date
            $script:app = Start-Terminal -Package Dev -PassFre $true -TimeoutSec 60 -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = $Locale; tabLayout = 'vertical'; tabLayoutVerticalWidth = 600
                startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                acpAgent = $provider; acpCustomCommand = $command; acpModel = ''
                autoFixEnabled = $false; autoErrorDetectionEnabled = $false; defaultProfile = $profileId
                profiles = @{ list = @(@{ guid = $profileId; name = 'Demo compact ages'
                    commandline = '"' + (Get-Command pwsh).Source + '" -NoLogo -NoProfile -NoExit'
                    startingDirectory = $evidence; acpAgent = $provider; acpCustomCommand = $command }) }
            }
            $script:app.Launched | Should -BeTrue
            $script:app | Add-Member RequireOwnedForeground $true
            [ItE2E.ItWtWin32Input]::ShowWindow([intptr][long]$script:app.Hwnd, 3) | Out-Null
            Open-AgentPane -App $script:app -TimeoutSec 30 | Out-Null
            Wait-AgentReady -App $script:app -TimeoutSec 30 | Should -BeTrue
            Stop-AgentPane -App $script:app | Out-Null
            Set-TestSidebarScope -App $script:app -Recent $true
            Wait-Until -TimeoutSec 30 -Condition { $list = Get-AgeElement HistoryHeaderButton; $list -and -not $list.Current.IsOffscreen } | Out-Null
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $marker | Out-Null
            $header = Get-AgeElement HistoryHeaderButton
            $header.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Current.ExpandCollapseState |
                Should -Be ([Windows.Automation.ExpandCollapseState]::Expanded)
            Wait-Until -TimeoutSec 30 -Condition {
                $names = @(Get-AgeRows | ForEach-Object { Get-AgeParts $_ } | ForEach-Object { $_.Current.Name })
                $missing = @(foreach ($entry in $history) {
                    if (@($names | Where-Object { $_ -eq $entry.title }).Count -ne 1) { $entry }
                })
                $missing.Count -eq 0
            } | Out-Null
            (Get-Content $fixtureLog -Raw) | Should -Match '\|list\|'
            (Get-Content $fixtureLog -Raw) | Should -Not -Match '\|prompt\|'
            $pipe = (Get-Content (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
            $snapshot = Invoke-Wta -App $script:app -TimeoutSec 10 -Arguments @('sessions', 'list', '--master', $pipe, '--json', '--include-status')
            $owned = @($snapshot.sessions | Where-Object { $_.session_id -in $history.sessionId })
            $owned.Count | Should -Be 6
            $owned | ConvertTo-Json -Depth 12 | Set-Content (Join-Path $evidence 'source.json') -Encoding utf8
            $beforeCapture = [DateTimeOffset]::UtcNow
            $receipts = @(foreach ($i in 0..5) {
                $row = @(Get-AgeRows | Where-Object {
                    @(Get-AgeParts $_ | Where-Object { $_.Current.Name -eq $history[$i].title }).Count -gt 0
                })
                $row.Count | Should -Be 1
                $parts = @(Get-AgeParts $row[0])
                $content = @(Get-AgeParts $row[0] -Content)
                @($parts | ForEach-Object {
                    @{ name = $_.Current.Name; id = $_.Current.AutomationId; class = $_.Current.ClassName; type = $_.Current.ControlType.ProgrammaticName
                        bounds = $_.Current.BoundingRectangle.ToString(); offscreen = $_.Current.IsOffscreen
                        runtimeId = @($_.GetRuntimeId()); contentElement = $_.Current.IsContentElement }
                }) | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $evidence "row-$i-raw.json") -Encoding utf8
                $leaves = @($content | Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    -not [string]::IsNullOrEmpty($_.Current.Name) -and
                    -not @(Get-AgeParts $_ -Content | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text }).Count
                })
                $leaves.Count | Should -Be 3 -Because 'all named Content-view text leaves must be title, cwd and age/date, including offscreen or zero-size peers'
                foreach ($leaf in $leaves) {
                    $leaf.Current.IsOffscreen | Should -BeFalse
                    $leaf.Current.BoundingRectangle.Width | Should -BeGreaterThan 0
                    $leaf.Current.BoundingRectangle.Height | Should -BeGreaterThan 0
                }
                $title = @($leaves | Where-Object { $_.Current.Name -eq $history[$i].title })
                $cwdText = @($leaves | Where-Object { $_.Current.Name -eq $evidence })
                $title.Count | Should -Be 1; $cwdText.Count | Should -Be 1
                $row[0].Current.IsKeyboardFocusable | Should -BeTrue
                $row[0].Current.Name | Should -Be "$($history[$i].title) $([char]0xB7) $providerName"
                $subtitle = @($leaves | Where-Object { $_ -notin @($title[0], $cwdText[0]) })
                $subtitle.Count | Should -Be 1
                $captured = [DateTimeOffset]::UtcNow
                $source = [DateTimeOffset]::Parse($history[$i].updatedAt)
                $sourceRow = @($owned | Where-Object { $_.session_id -eq $history[$i].sessionId })
                $sourceRow.Count | Should -Be 1
                $sourceRow[0].last_activity_at_ms | Should -Be $source.ToUnixTimeMilliseconds()
                $sourceRow[0].provider_id | Should -Be $provider
                $sourceRow[0].status | Should -Be 'Historical'
                $sourceRow[0].pane_session_id | Should -BeNullOrEmpty
                $counts = @([ItSidebarAgeOracle]::Count($units[$i], $source, $beforeCapture),
                    [ItSidebarAgeOracle]::Count($units[$i], $source, $captured)) | Select-Object -Unique
                foreach ($count in $counts) {
                    $count | Should -BeIn $(if ($i -eq 0) { @(2, 3) } else { @(@(2, 3, 6, 1, 2, 1)[$i]) })
                }
                $expected = if ($i -lt 3) {
                    @($counts | ForEach-Object { [ItSidebarAgeOracle]::Format($Locale, $units[$i], $_) })
                } else { @([ItSidebarAgeOracle]::CalendarDate($Locale, $source)) }
                $subtitle[0].Current.Name | Should -BeIn $expected
                if ($Rtl) {
                    $english = if ($i -lt 3) { @($counts | ForEach-Object { [ItSidebarAgeOracle]::Format('en-US', $units[$i], $_) }) }
                    else { @([ItSidebarAgeOracle]::CalendarDate('en-US', $source)) }
                    $subtitle[0].Current.Name | Should -Not -BeIn $english
                }
                $icon = @($parts | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' -and $_.Current.BoundingRectangle.Width -gt 0 })
                $icon.Count | Should -Be 1; $icon[0].Current.Name | Should -Be $providerName
                # A custom provider's Raw-only SymbolIcon has a TextBlock glyph;
                # it is decoration, not a fourth semantic metadata field.
                $allowedRawTextIds = @($leaves; @(Get-AgeParts $icon[0])) |
                    ForEach-Object { $_.GetRuntimeId() -join ',' }
                foreach ($rawText in @($parts | Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                    $_.Current.BoundingRectangle.Height -gt 0 -and
                    -not @(Get-AgeParts $_ | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text }).Count
                })) {
                    ($rawText.GetRuntimeId() -join ',') | Should -BeIn $allowedRawTextIds -Because 'only the leading decorative icon subtree may add a Raw-only text leaf'
                }
                @($content | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count | Should -Be 0
                @($content | Where-Object { $_.Current.Name -eq $providerName -and
                    -not @(Get-AgeParts $_ -Content | Where-Object { $_.Current.Name -eq $providerName }).Count }).Count | Should -Be 0
                @($parts | Where-Object { $_.Current.AutomationId -eq 'HistoryOwnershipButton' -and -not $_.Current.IsOffscreen }).Count | Should -Be 0
                $rb = $row[0].Current.BoundingRectangle; $vb = (Get-AgeElement ItemsList).Current.BoundingRectangle
                $wb = [Windows.Automation.AutomationElement]::FromHandle([intptr][long]$script:app.Hwnd).Current.BoundingRectangle
                $tb = $title[0].Current.BoundingRectangle; $sb = $subtitle[0].Current.BoundingRectangle
                $cb = $cwdText[0].Current.BoundingRectangle; $ib = $icon[0].Current.BoundingRectangle
                foreach ($b in @($rb, $tb, $sb, $cb, $ib)) {
                    $b.Width | Should -BeGreaterThan 0; $b.Height | Should -BeGreaterThan 0
                    $b.Left | Should -BeGreaterOrEqual $vb.Left; $b.Right | Should -BeLessOrEqual $vb.Right
                    $b.Top | Should -BeGreaterOrEqual $vb.Top; $b.Bottom | Should -BeLessOrEqual $vb.Bottom
                    $b.Left | Should -BeGreaterOrEqual $wb.Left; $b.Right | Should -BeLessOrEqual $wb.Right
                    $b.Top | Should -BeGreaterOrEqual $wb.Top; $b.Bottom | Should -BeLessOrEqual $wb.Bottom
                    $b.Left | Should -BeGreaterOrEqual $rb.Left; $b.Right | Should -BeLessOrEqual $rb.Right
                    $b.Top | Should -BeGreaterOrEqual $rb.Top; $b.Bottom | Should -BeLessOrEqual $rb.Bottom
                }
                $tb.Bottom | Should -BeLessOrEqual $cb.Top
                $cb.Bottom | Should -BeLessOrEqual $sb.Top
                [math]::Abs(($ib.Top + $ib.Bottom - $rb.Top - $rb.Bottom) / 2) | Should -BeLessOrEqual 2
                [math]::Abs($ib.Width - $ib.Height) | Should -BeLessOrEqual 1
                if ($Rtl) {
                    $ib.Left | Should -BeGreaterThan $tb.Right
                    [math]::Abs($tb.Right - $sb.Right) | Should -BeLessOrEqual 1
                    [math]::Abs($tb.Right - $cb.Right) | Should -BeLessOrEqual 1
                } else {
                    $ib.Right | Should -BeLessThan $tb.Left
                    [math]::Abs($tb.Left - $sb.Left) | Should -BeLessOrEqual 1
                    [math]::Abs($tb.Left - $cb.Left) | Should -BeLessOrEqual 1
                }
                @{ unit = $units[$i]; source = $history[$i]; beforeCaptureTime = $beforeCapture.ToString('o')
                    captureTime = $captured.ToString('o'); counts = @($counts); expected = $expected
                    rendered = $subtitle[0].Current.Name; row = $rb.ToString(); viewport = $vb.ToString()
                    title = $tb.ToString(); subtitle = $sb.ToString(); cwd = $cb.ToString(); icon = $ib.ToString()
                    containerProviderName = $row[0].Current.Name; contentProviderCount = 0; locale = $Locale; rtl = $Rtl }
            })
            $receipts | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $evidence 'rows.json') -Encoding utf8
            $widthReceipts = @()
            foreach ($width in @(320, 600)) {
                $beforeWidth = (Get-AgeElement ItemsList).Current.BoundingRectangle.Width
                Set-WtSetting -App $script:app -Key tabLayoutVerticalWidth -Value $width | Out-Null
                Wait-Until -TimeoutSec 10 -Because 'actual mixed viewport responds to the narrow/normal Sidebar width' -Condition {
                    $actual = (Get-AgeElement ItemsList).Current.BoundingRectangle.Width
                    if ($width -eq 320) { $actual -lt $beforeWidth - 100 }
                    else { $actual -gt $beforeWidth + 100 }
                } | Out-Null
                $viewport = (Get-AgeElement ItemsList).Current.BoundingRectangle
                $heading = Get-AgeElement HistoryHeaderButton
                $heading.Current.Name | Should -BeIn @(Get-WtReswTextValues -Key 'VerticalTabsSessionHistoryHeader.Text')
                $viewport.Contains($heading.Current.BoundingRectangle) | Should -BeTrue
                foreach ($entry in $history) {
                    $row = @(Get-AgeRows | Where-Object {
                        @(Get-AgeParts $_ | Where-Object { $_.Current.Name -eq $entry.title }).Count -gt 0
                    })
                    $row | Should -HaveCount 1
                    $row[0].GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern).ScrollIntoView()
                    $row = @(Get-AgeRows | Where-Object {
                        @(Get-AgeParts $_ | Where-Object { $_.Current.Name -eq $entry.title }).Count -gt 0
                    })[0]
                    $row.SetFocus()
                    [Windows.Automation.Automation]::Compare($row, [Windows.Automation.AutomationElement]::FocusedElement) | Should -BeTrue
                    $viewport.Contains($row.Current.BoundingRectangle) | Should -BeTrue
                    $parts = @(Get-AgeParts $row)
                    $icon = @($parts | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' })[0]
                    $title = @($parts | Where-Object { $_.Current.Name -eq $entry.title -and
                        $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text })[0]
                    $title.Current.BoundingRectangle.Height | Should -BeGreaterThan 0
                    if ($Rtl) { $icon.Current.BoundingRectangle.Left | Should -BeGreaterThan $title.Current.BoundingRectangle.Right }
                    else { $icon.Current.BoundingRectangle.Right | Should -BeLessThan $title.Current.BoundingRectangle.Left }
                    $widthReceipts += @{ requested = $width; viewport = $viewport.ToString(); rtl = $Rtl
                        heading = $heading.Current.BoundingRectangle.ToString(); row = $row.Current.BoundingRectangle.ToString()
                        title = $title.Current.BoundingRectangle.ToString(); icon = $icon.Current.BoundingRectangle.ToString()
                        focus_runtime_id = @($row.GetRuntimeId()) }
                }
            }
            $widthReceipts | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $evidence 'narrow-normal-rtl-geometry.json')
            Save-UiScreenshot -App $script:app -Path (Join-Path $evidence 'viewport.png') | Out-Null
            Test-Path (Join-Path $evidence 'viewport.png') | Should -BeTrue
            (Get-Content $fixtureLog -Raw) | Should -Not -Match '\|prompt\|'
        } finally {
            Stop-TestTerminal -App $script:app -Target $script:target -LaunchStarted $launchStarted
            foreach ($path in $hashes.Keys) {
                $actual = if (Test-Path $path) { (Get-FileHash $path).Hash } else { $null }
                $actual | Should -Be $hashes[$path] -Because 'locale and user state restore byte-for-byte'
            }
        }
    }
}
