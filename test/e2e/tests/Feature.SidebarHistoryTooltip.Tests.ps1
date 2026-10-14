#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
param([switch]$RtlOnly)
# Contract: real title/icon/blank-padding hover -> product ToolTip peer containing
# the full title, provider, cwd and currently rendered age, without adding a
# provider text line to the row. ACP listing -> WTA -> native ItemsList -> popup.
# Refresh changes the same session's title; replacement changes cwd/age. Distant rows and
# replacement sessions must never inherit another row's tooltip.
# Negative controls: hover leaves the active shell unchanged; filtering excludes
# other fixture rows. Existing CombinedAgentsSidebar protects native resume,
# live status/window ownership and Keep Running; SidebarRelativeTime protects
# locale age formatting; SidebarTooltipPlacement protects popup placement.
# Run this SAME oracle against pre/post-fix private builds: a missing tip fails,
# never skips. No baseline switch weakens the fixed-build contract.

Describe 'Feature: Sidebar history hover' -Tag @('Feature', 'SidebarHistoryTooltip') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TestTerminalCleanup.ps1')
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, WindowsBase, System.Windows.Forms -ErrorAction Stop
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $script:titleTipScreenshot = $false
        $script:missingTipScreenshot = $false
        $script:package = Get-ItTestPackage
        if ($script:package -notmatch '^IntelligentTerminal\.Worktree\.[A-Za-z0-9]+_[A-Za-z0-9]+$') {
            throw 'Pin the explicitly selected private Dev package family, not Dev/Store/Auto.'
        }
        $script:target = Resolve-ItApp -Package $script:package
        Assert-WtPackageInactive -App $script:target
        foreach ($pair in @(@('TerminalApp.dll', $env:ITE2E_EXPECTED_APP_SHA256), @('wta.exe', $env:ITE2E_EXPECTED_WTA_SHA256))) {
            if ($pair[1] -notmatch '^[0-9a-fA-F]{64}$' -or
                (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation $pair[0])).Hash -ne $pair[1]) {
                throw "Missing or mismatched exact-source receipt: $($pair[0])"
            }
        }
        $head = git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD
        if ($LASTEXITCODE -ne 0 -or $env:ITE2E_SOURCE_COMMIT -ne $head) { throw 'Source receipt must equal the current feature worktree HEAD.' }

        function Get-HoverParts($Element, [switch]$Content) {
            $walker = if ($Content) { [Windows.Automation.TreeWalker]::ContentViewWalker } else { [Windows.Automation.TreeWalker]::RawViewWalker }
            for ($child = $walker.GetFirstChild($Element); $child; $child = $walker.GetNextSibling($child)) {
                $child
                Get-HoverParts $child -Content:$Content
            }
        }
        function Get-HoverElement([string]$Id) {
            if ($script:clock.Elapsed.TotalSeconds -gt 240) { throw 'History hover fixture exceeded 240 seconds.' }
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            if ($root.Current.ProcessId -ne $script:app.Pid) { throw 'Window ownership changed.' }
            $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $Id))
        }
        function Get-HoverRows {
            $list = Get-HoverElement ItemsList
            if (-not $list -or $list.Current.IsOffscreen) { throw 'Missing visible owned ItemsList.' }
            @($list.FindAll([Windows.Automation.TreeScope]::Children,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [Windows.Automation.ControlType]::ListItem)))
        }
        function Get-HoverRow([string]$Title) {
            if ($script:rowAmbiguity) { throw $script:rowAmbiguity }
            $matches = @(Get-HoverRows | Where-Object {
                @(Get-HoverParts $_ | Where-Object { $_.Current.Name -ceq $Title -and
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text }).Count -eq 1
            })
            if ($matches.Count -gt 1) {
                $script:rowAmbiguity = "Ambiguous realized history row: $Title"
                throw $script:rowAmbiguity
            }
            if ($matches.Count -eq 0) { return $null }
            $matches[0]
        }
        function Save-HoverBuildReceipt([string]$Stage, [string]$Failure = '') {
            $script:buildReceipt.stage = $Stage
            $script:buildReceipt.recordedAt = [DateTimeOffset]::UtcNow.ToString('o')
            if ($Failure) { $script:buildReceipt.failure = $Failure }
            # Keep the last observed live paths even after teardown.
            if ($script:app -and -not $script:app.OwnedProcess.HasExited) {
                $script:buildReceipt.host = @{ pid = $script:app.Pid; hwnd = [string]$script:app.Hwnd
                    path = $script:app.OwnedProcess.Path; startTime = $script:app.OwnedProcess.StartTime.ToUniversalTime().ToString('o') }
                $script:buildReceipt.loadedApp = @($script:app.OwnedProcess.Modules | Where-Object ModuleName -eq 'TerminalApp.dll' |
                    ForEach-Object { @{ path = $_.FileName; sha256 = (Get-FileHash -LiteralPath $_.FileName).Hash } })
                $script:buildReceipt.observedWta = @(Get-WtProcessesForApp -App $script:app -IncludePackageExecutables |
                    Where-Object { $_.Path -and [IO.Path]::GetFileName($_.Path) -ieq 'wta.exe' } |
                    ForEach-Object { @{ pid = $_.Id; path = $_.Path; sha256 = (Get-FileHash -LiteralPath $_.Path).Hash
                        startTime = $_.StartTime.ToUniversalTime().ToString('o') } })
            }
            $script:buildReceipt | ConvertTo-Json -Depth 10 |
                Set-Content -LiteralPath (Join-Path $script:evidence 'build-receipt.json') -Encoding utf8
        }
        function Save-HoverScreenshot([string]$Kind, $Tip, $Row, [Windows.Point]$Point) {
            Assert-HoverLease $Point $Row
            $frame = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd).Current.BoundingRectangle
            if ($Tip -and -not $frame.Contains($Tip.Current.BoundingRectangle)) {
                throw 'Tooltip extends outside HWND screenshot crop; do not claim incomplete visual proof.'
            }
            $path = Join-Path $script:evidence "$Kind.png"
            Save-UiScreenshot -App $script:app -Path $path -CaptureScreen | Out-Null
            if (-not (Test-Path -LiteralPath $path) -or (Get-Item -LiteralPath $path).Length -eq 0) {
                throw "Authentic $Kind screenshot capture failed."
            }
            if ($Tip -and ($Tip.Current.IsOffscreen -or -not $frame.Contains($Tip.Current.BoundingRectangle))) {
                throw 'Tooltip disappeared or left the crop during screenshot capture; visual proof is not accepted.'
            }
            @{ kind = $Kind; path = $path; capturedAt = [DateTimeOffset]::UtcNow.ToString('o')
                hwnd = [string]$script:app.Hwnd; frame = $frame.ToString()
                popup = if ($Tip) { $Tip.Current.BoundingRectangle.ToString() } else { $null }
                title = $Row.Current.Name; point = "$($Point.X),$($Point.Y)" } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "$Kind.json") -Encoding utf8
        }
        function Get-HoverTips {
            $owned = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$script:app.Pid)
            foreach ($window in [Windows.Automation.AutomationElement]::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $owned)) {
                foreach ($tip in $window.FindAll([Windows.Automation.TreeScope]::Subtree,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,
                        [Windows.Automation.ControlType]::ToolTip))) {
                    if ($tip.Current.ProcessId -ne $script:app.Pid) { throw 'Foreign tooltip peer.' }
                    if (-not $tip.Current.IsOffscreen -and $tip.Current.BoundingRectangle.Width -gt 0 -and
                        $tip.Current.BoundingRectangle.Height -gt 0) { $tip }
                }
            }
        }
        function Assert-HoverLease([Windows.Point]$Point, $Row) {
            $process = Get-Process -Id $script:app.Pid -ErrorAction Stop
            if (-not $script:app.Launched -or $script:app.OwnedProcess.HasExited -or
                $process.StartTime -ne $script:app.OwnedProcess.StartTime -or $process.Path -ne $script:app.OwnedProcess.Path) {
                throw 'Stale creation-proven process lease.'
            }
            if (-not [ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup([ItE2E.ItWtWin32Input]::GetForegroundWindow(),
                [IntPtr][long]$script:app.Hwnd, $script:app.Pid)) { throw 'Foreign foreground; pointer injection refused.' }
            if (-not $Row.Current.BoundingRectangle.Contains($Point) -or
                ($Row.Current.ControlType -eq [Windows.Automation.ControlType]::ListItem -and
                    -not (Get-HoverElement ItemsList).Current.BoundingRectangle.Contains($Row.Current.BoundingRectangle))) {
                throw 'Clipped row or unsafe pointer target.'
            }
            $native = [ItE2E.ItWtWin32Input+POINT]::new(); $native.X = $Point.X; $native.Y = $Point.Y
            $hit = [ItE2E.ItWtWin32Input]::WindowFromPoint($native)
            if ([ItE2E.ItWtWin32Input]::GetAncestor($hit, 2) -ne [IntPtr][long]$script:app.Hwnd -or
                [ItE2E.ItWtWin32Input]::GetWindowProcessId($hit) -ne $script:app.Pid) { throw 'Pointer target is covered or foreign.' }
            foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
                if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Held user input; pointer injection refused.' }
            }
        }
        function Move-HoverPointer([Windows.Point]$Point, $Row) {
            Assert-HoverLease $Point $Row
            $screen = [Windows.Forms.SystemInformation]::VirtualScreen
            $mouse = [ItE2E.ItWtWin32Input+MOUSEINPUT]::new()
            $mouse.dx = [int][Math]::Round(($Point.X - $screen.Left) * 65535 / ($screen.Width - 1))
            $mouse.dy = [int][Math]::Round(($Point.Y - $screen.Top) * 65535 / ($screen.Height - 1))
            $mouse.dwFlags = 0xC001 # MOVE | VIRTUALDESK | ABSOLUTE
            $data = [ItE2E.ItWtWin32Input+INPUTUNION]::new(); $data.mouse = $mouse
            $input = [ItE2E.ItWtWin32Input+INPUT]::new(); $input.data = $data
            if ([ItE2E.ItWtWin32Input]::SendInput(1, @($input),
                [Runtime.InteropServices.Marshal]::SizeOf($input)) -ne 1) { throw 'Physical hover SendInput failed.' }
            $script:pointerMoved = $true
        }
        function Clear-HoverTip {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $bounds = $root.Current.BoundingRectangle
            $point = [Windows.Point]::new([int]($bounds.Left + $bounds.Width / 2), [int]($bounds.Top + 100))
            Move-HoverPointer $point $root
            Wait-Until -TimeoutSec 4 -Because 'actual popup closes after pointer departure' -Condition { @(Get-HoverTips).Count -eq 0 } | Out-Null
        }
        function Assert-HistoryHover($Entry, [string]$Area, [string[]]$Forbidden = @()) {
            $script:buildReceipt.lastHover = @{ title = $Entry.title; area = $Area; language = $script:language }
            Clear-HoverTip
            $row = Get-HoverRow $Entry.title
            if (-not $row) { throw "Missing intended history row before hover: $($Entry.title)" }
            $row.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern).ScrollIntoView()
            $row = Wait-Until -TimeoutSec 5 -Condition {
                $candidate = Get-HoverRow $Entry.title
                if ($candidate -and -not $candidate.Current.IsOffscreen -and
                    (Get-HoverElement ItemsList).Current.BoundingRectangle.Contains($candidate.Current.BoundingRectangle)) { $candidate }
            }
            $parts = @(Get-HoverParts $row)
            $leaves = @(Get-HoverParts $row -Content | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and $_.Current.Name -and
                -not @(Get-HoverParts $_ -Content | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text }).Count
            })
            $leaves | Should -HaveCount 3 -Because 'visible detail remains title, cwd and age, not provider'
            $title = @($leaves | Where-Object { $_.Current.Name -ceq $Entry.title })
            $cwd = @($leaves | Where-Object { $_.Current.Name -ceq $Entry.cwd })
            if ($title.Count -ne 1 -or $cwd.Count -ne 1) {
                throw "History row metadata mismatch on $Area. Expected title='$($Entry.title)' cwd='$($Entry.cwd)'; visible lines=$($leaves.Current.Name | ConvertTo-Json -Compress)."
            }
            $title | Should -HaveCount 1; $cwd | Should -HaveCount 1
            $age = @($leaves | Where-Object { $_ -notin @($title[0], $cwd[0]) })
            $age | Should -HaveCount 1
            @($leaves | Where-Object { $_.Current.Name -ceq $script:provider }) | Should -HaveCount 0
            foreach ($leaf in $leaves) { $leaf.Current.IsOffscreen | Should -BeFalse }
            $icon = @($parts | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' })
            $icon | Should -HaveCount 1
            $box = switch ($Area) {
                Title { $title[0].Current.BoundingRectangle }
                Icon { $icon[0].Current.BoundingRectangle }
                Padding { $row.Current.BoundingRectangle }
            }
            $point = if ($Area -eq 'Padding') {
                $leading = if ($script:language -eq 'ar-SA') { $box.Right - 12 } else { $box.Left + 12 }
                [Windows.Point]::new([int]$leading, [int]($box.Top + $box.Height / 2))
            } else { [Windows.Point]::new([int]($box.Left + $box.Width / 2), [int]($box.Top + $box.Height / 2)) }
            if ($Area -eq 'Padding' -and @($parts | Where-Object {
                $_.Current.ControlType -in @([Windows.Automation.ControlType]::Text, [Windows.Automation.ControlType]::Button) -and
                $_.Current.BoundingRectangle.Contains($point)
            }).Count) { throw 'Blank-padding control is not blank; do not substitute title hover.' }
            Assert-HoverLease $point $row
            # XAML Islands' UIA FromPoint can return only the host HWND.
            # Native hit testing above rejects overlays; realized geometry pins the row.
            $rowHits = @(Get-HoverRows | Where-Object { -not $_.Current.IsOffscreen -and
                $_.Current.BoundingRectangle.Contains($point) })
            $rowHits | Should -HaveCount 1 -Because 'the physical point must identify exactly one realized history row'
            [Windows.Automation.Automation]::Compare($rowHits[0], $row) | Should -BeTrue -Because 'realized geometry must resolve the requested row identity'
            $runtime = $row.GetRuntimeId() -join ','
            Move-HoverPointer $point $row
            try {
                $tip = Wait-Until -TimeoutSec 4 -IntervalSec 0.1 -Because "visible product tooltip on $Area" -Condition {
                    $tips = @(Get-HoverTips)
                    if ($tips.Count -gt 1) { throw 'Ambiguous visible product tooltip.' }
                    if ($tips.Count -eq 1) { $tips[0] }
                }
            } catch {
                $hoverFailure = $_
                if (-not $script:missingTipScreenshot -and @(Get-HoverTips).Count -eq 0) {
                    Save-HoverScreenshot missing-tooltip $null $row $point
                    $script:missingTipScreenshot = $true
                }
                throw $hoverFailure
            }
            $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
            $cursor[0] | Should -Be $point.X; $cursor[1] | Should -Be $point.Y
            ((Get-HoverRow $Entry.title).GetRuntimeId() -join ',') | Should -BeExactly $runtime
            $tipBounds = $tip.Current.BoundingRectangle
            $rowBounds = $row.Current.BoundingRectangle
            if ($script:language -eq 'ar-SA') {
                $tipBounds.Right | Should -BeLessOrEqual ($rowBounds.Left + 8) -Because 'RTL history tooltip prefers the terminal-content side'
            } else {
                $tipBounds.Left | Should -BeGreaterOrEqual ($rowBounds.Right - 8) -Because 'LTR history tooltip prefers the terminal-content side'
            }
            foreach ($neighbor in @(Get-HoverRows)) {
                if ([Windows.Automation.Automation]::Compare($neighbor, $row) -or $neighbor.Current.IsOffscreen) { continue }
                $intersection = [Windows.Rect]::Intersect($tipBounds, $neighbor.Current.BoundingRectangle)
                $intersection.IsEmpty | Should -BeTrue -Because 'the tooltip must leave neighboring history rows unobstructed'
            }
            $tipText = @(Get-HoverParts $tip -Content | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and $_.Current.Name -and
                -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                $_.Current.BoundingRectangle.Height -gt 0 -and
                -not @(Get-HoverParts $_ -Content | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text }).Count
            })
            # A bound multiline ToolTip exposes separate TextBlock peers, not
            # necessarily one aggregate accessible Name on the popup itself.
            $text = if ($tipText.Count) { ($tipText | ForEach-Object { $_.Current.Name }) -join "`n" } else { $tip.Current.Name }
            $text = $text.Replace("`r`n", "`n")
            foreach ($expected in @($Entry.title, $script:provider, $Entry.cwd, $age[0].Current.Name)) {
                $text | Should -Match ([regex]::Escape($expected)) -Because 'assert the visible ToolTip, not row HelpText or XAML source'
            }
            ($text -split "`n").Count | Should -BeGreaterOrEqual 3
            foreach ($stale in $Forbidden) { $text | Should -Not -Match ([regex]::Escape($stale)) }
            if ($Area -eq 'Title' -and -not $script:titleTipScreenshot) {
                Save-HoverScreenshot title-hover $tip $row $point
                $script:titleTipScreenshot = $true
            }
            $active = Get-ActivePane -App $script:app
            $active.session_id | Should -Be $script:shell.session_id
            $active.tab_id | Should -Be $script:shell.tab_id
            $active.window_id | Should -Be $script:shell.window_id
            [pscustomobject]@{ title = $Entry.title; cwd = $Entry.cwd; provider = $script:provider; area = $Area
                age = $age[0].Current.Name; tooltip = $text; runtime = $runtime; point = "$($point.X),$($point.Y)" }
        }
    }

    It 'History hover exposes full metadata and survives refresh and row reuse (<Provider>, <Language>)' -ForEach (@(
        @{ Provider = 'custom:history-hover-alpha'; Language = 'en-US' },
        @{ Provider = 'custom:history-hover-beta'; Language = 'ar-SA' }
    ) | Where-Object { -not $RtlOnly -or $_.Language -eq 'ar-SA' }) {
        $script:app = $null; $launch = $null; $originalCursor = $null; $primary = $null
        $script:provider = $Provider
        $script:language = $Language
        $script:pointerMoved = $false
        $script:rowAmbiguity = $null
        $script:clock = [Diagnostics.Stopwatch]::StartNew()
        $marker = 'HistoryHover-' + [guid]::NewGuid().ToString('N')
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) $marker
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $historyPath = Join-Path $script:evidence 'history.json'
        $log = Join-Path $script:evidence 'acp.log'
        $now = [DateTimeOffset]::UtcNow
        $history = @(for ($i = 0; $i -lt 36; $i++) {
            @{ sessionId = "$marker-$i"; title = "$marker row-$i " + ('long complete title text ' * 8) + "END-$i"
                cwd = $script:evidence; updatedAt = $now.AddMinutes(-3 - $i).UtcDateTime.ToString('o') }
        })
        @{ sessions = $history } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $historyPath -Encoding utf8
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $code = "& '$($fixture.Replace("'", "''"))' -LogPath '$($log.Replace("'", "''"))' -HistoryPath '$($historyPath.Replace("'", "''"))'"
        $command = 'pwsh -NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
        $receipts = [Collections.Generic.List[object]]::new()
        $script:buildReceipt = [ordered]@{
            package = $script:package; installLocation = $script:target.InstallLocation
            sourceCommit = (git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD)
            sourceReceipt = $env:ITE2E_SOURCE_COMMIT; provider = $Provider; language = $Language
            expectedAppSha256 = $env:ITE2E_EXPECTED_APP_SHA256; expectedWtaSha256 = $env:ITE2E_EXPECTED_WTA_SHA256
            installedApp = @{ path = (Join-Path $script:target.InstallLocation 'TerminalApp.dll')
                sha256 = (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash }
            installedWta = @{ path = $script:target.WtaPath; sha256 = (Get-FileHash -LiteralPath $script:target.WtaPath).Hash }
        }
        Save-HoverBuildReceipt before-launch
        $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        if ($dpi -eq [IntPtr]::Zero) { throw 'Physical DPI context unavailable.' }
        try {
            Assert-WtPackageInactive -App $script:target
            $profile = '{' + [guid]::NewGuid() + '}'
            $launch = Get-Date
            $script:app = Start-Terminal -Package $script:package -PassFre $true -TimeoutSec 45 -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = $Language; tabLayout = 'vertical'; tabLayoutVerticalWidth = 320
                startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                acpAgent = $Provider; acpCustomCommand = $command; acpModel = ''
                autoFixEnabled = $false; autoErrorDetectionEnabled = $false; defaultProfile = $profile
                profiles = @{ list = @(@{ guid = $profile; name = 'History hover shell'
                    commandline = 'cmd.exe /d /k "prompt $G"'; startingDirectory = $script:evidence
                    shellIntegrationEnabled = $false; acpAgent = $Provider; acpCustomCommand = $command }) }
            }
            $workArea = [Windows.Forms.Screen]::PrimaryScreen.WorkingArea
            if (-not [ItE2E.ItWtWin32Input]::SetWindowPos([IntPtr][long]$script:app.Hwnd, [IntPtr]::Zero,
                $workArea.Left + 10, $workArea.Top + 10, $workArea.Width - 20, $workArea.Height - 20, 4)) {
                throw 'Cannot position the owned fixture window within the interactive screen.'
            }
            Save-HoverBuildReceipt launched
            $loaded = @($script:app.OwnedProcess.Modules | Where-Object ModuleName -eq 'TerminalApp.dll')
            $loaded | Should -HaveCount 1
            $loaded[0].FileName | Should -Be (Join-Path $script:target.InstallLocation 'TerminalApp.dll')
            (Get-FileHash -LiteralPath $loaded[0].FileName).Hash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
            $originalCursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
            $script:app | Add-Member RequireOwnedForeground $true
            $script:shell = Get-ActivePane -App $script:app
            Open-AgentPane -App $script:app -TimeoutSec 30 | Out-Null
            Wait-Until -TimeoutSec 30 -Because 'the configured ACP fixture actually services history listing' -Condition {
                (Test-Path -LiteralPath $log) -and (Get-Content -LiteralPath $log -Raw).Contains('|list|')
            } | Out-Null
            Save-HoverBuildReceipt agent-ready
            $script:buildReceipt.observedWta.Count | Should -BeGreaterThan 0
            foreach ($wta in $script:buildReceipt.observedWta) {
                $wta.path | Should -Be $script:target.WtaPath
                $wta.sha256 | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
            }
            Stop-AgentPane -App $script:app | Out-Null
            Set-TestSidebarScope -App $script:app -Recent $true
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            Set-UiValue -App $script:app -Selector SearchTextBox -Value "$marker row-0 " | Out-Null
            Wait-Until -TimeoutSec 30 -Condition { Get-HoverRow $history[0].title } | Out-Null
            if (-not (Set-WtWindowForeground -App $script:app -Attempts 2)) { throw 'Cannot acquire owned foreground.' }
            foreach ($area in @('Title', 'Icon', 'Padding')) { $receipts.Add((Assert-HistoryHover $history[0] $area)) }
            Clear-HoverTip
            $focusedRow = Get-HoverRow $history[0].title
            $focusedRow.SetFocus()
            $focusedRow.Current.HasKeyboardFocus | Should -BeTrue -Because 'UIA focus must reach the intended history row'
            $receipts.Add((Assert-HistoryHover $history[0] Title))
            Clear-HoverTip
            Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'focus returns to the original shell without activating history' -Condition {
                -not (Get-HoverRow $history[0].title).Current.HasKeyboardFocus -and
                    (Get-ActivePane -App $script:app).session_id -eq $script:shell.session_id
            } | Out-Null
            @(Get-HoverRows | Where-Object { @(Get-HoverParts $_ | Where-Object { $_.Current.Name -ceq $history[1].title }).Count }) |
                Should -HaveCount 0 -Because 'search filters out other fixture rows'
            $pipe = (Get-Content (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
            $oldTitle = $history[0].title
            $history[0].title = "$marker row-0 REFRESHED " + ('changed long complete title ' * 8) + 'NEW-END'
            @{ sessions = $history } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $historyPath -Encoding utf8
            Invoke-Wta -App $script:app -TimeoutSec 20 -Arguments @('sessions', 'refresh', '--master', $pipe, '--json') | Out-Null
            Wait-Until -TimeoutSec 20 -Condition { Get-HoverRow $history[0].title } | Out-Null
            $receipts.Add((Assert-HistoryHover $history[0] Padding @($oldTitle)))
            Clear-HoverTip
            foreach ($index in @(35, 1, 0)) {
                Set-UiValue -App $script:app -Selector SearchTextBox -Value "$marker row-$index " | Out-Null
                Wait-Until -TimeoutSec 10 -Condition { Get-HoverRow $history[$index].title } | Out-Null
                $receipts.Add((Assert-HistoryHover $history[$index] Icon @($history[($index + 1) % 36].title)))
            }
            Clear-HoverTip
            $removedTitle = $history[0].title
            $replacementCwd = Join-Path $script:evidence 'replacement-workspace'
            New-Item -ItemType Directory -Path $replacementCwd | Out-Null
            $history[0] = @{ sessionId = "$marker-replacement"; title = "$marker replacement " +
                    ('replacement complete title ' * 8) + 'REPLACEMENT-END'
                cwd = $replacementCwd; updatedAt = $now.AddDays(-2).UtcDateTime.ToString('o') }
            @{ sessions = $history } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $historyPath -Encoding utf8
            Invoke-Wta -App $script:app -TimeoutSec 20 -Arguments @('sessions', 'refresh', '--master', $pipe, '--json') | Out-Null
            Set-UiValue -App $script:app -Selector SearchTextBox -Value "$marker replacement " | Out-Null
            Wait-Until -TimeoutSec 20 -Condition { Get-HoverRow $history[0].title } | Out-Null
            $receipts.Add((Assert-HistoryHover $history[0] Title @($oldTitle, $removedTitle)))
            $receipts[-1].age | Should -Not -Be $receipts[0].age
            Clear-HoverTip
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $marker | Out-Null
            $scroll = (Get-HoverElement ItemsList).GetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern)
            $scroll.Current.VerticallyScrollable | Should -BeTrue -Because 'the fixture actually exceeds the realized viewport'
            foreach ($amount in @([Windows.Automation.ScrollAmount]::LargeIncrement, [Windows.Automation.ScrollAmount]::LargeDecrement)) {
                $beforeScroll = $scroll.Current.VerticalScrollPercent
                for ($step = 0; $step -lt 8; $step++) { $scroll.Scroll([Windows.Automation.ScrollAmount]::NoAmount, $amount) }
                Wait-Until -TimeoutSec 5 -Because 'the actual ItemsList scroll position changes' -Condition {
                    $scroll.Current.VerticalScrollPercent -ne $beforeScroll
                } | Out-Null
                $visible = @(Get-HoverRows | Where-Object { -not $_.Current.IsOffscreen -and
                    (Get-HoverElement ItemsList).Current.BoundingRectangle.Contains($_.Current.BoundingRectangle) })
                $entry = @($history | Where-Object {
                    $entryTitle = $_.title
                    @($visible | Where-Object { @(Get-HoverParts $_ | Where-Object { $_.Current.Name -ceq $entryTitle }).Count }).Count
                }) | Select-Object -First 1
                if (-not $entry) { throw 'No fully visible fixture row after real scrolling.' }
                $receipts.Add((Assert-HistoryHover $entry Padding @($oldTitle)))
                Clear-HoverTip
            }
            $source = Invoke-Wta -App $script:app -TimeoutSec 10 -Arguments @('sessions', 'list', '--master', $pipe, '--json', '--include-status')
            $owned = @($source.sessions | Where-Object session_id -in $history.sessionId)
            $owned | Should -HaveCount 36
            @($owned | Where-Object provider_id -ne $Provider) | Should -HaveCount 0
            (Get-Content -LiteralPath $log -Raw) | Should -Match '\|list\|'
            (Get-Content -LiteralPath $log -Raw) | Should -Not -Match '\|prompt\|'
            $receipts | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $script:evidence 'hover-receipts.json') -Encoding utf8
        } catch { $primary = $_ }
        finally {
            $errors = [Collections.Generic.List[Exception]]::new()
            try { Save-HoverBuildReceipt before-cleanup $(if ($primary) { $primary.ToString() + "`n" + $primary.ScriptStackTrace } else { '' }) }
            catch { $errors.Add($_.Exception) }
            try {
                if ($script:app -and $originalCursor -and $script:pointerMoved) {
                    $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
                    $point = [ItE2E.ItWtWin32Input+POINT]::new(); $point.X = $cursor[0]; $point.Y = $cursor[1]
                    if (-not [ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup([ItE2E.ItWtWin32Input]::WindowFromPoint($point),
                        [IntPtr][long]$script:app.Hwnd, $script:app.Pid)) { throw 'Cursor recovery refused from foreign surface.' }
                    if (-not [ItE2E.ItWtWin32Input]::SetCursorPos($originalCursor[0], $originalCursor[1])) { throw 'Cursor recovery failed.' }
                }
            } catch { $errors.Add($_.Exception) }
            try { Stop-TestTerminal -App $script:app -Target $script:target -LaunchStarted $launch } catch { $errors.Add($_.Exception) }
            if ([ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi) -eq [IntPtr]::Zero) { $errors.Add([Exception]::new('DPI recovery failed.')) }
            if ($errors.Count) {
                if ($primary) { $errors.Insert(0, $primary.Exception) }
                throw [AggregateException]::new('History hover action/cleanup failed; retain recovery evidence.', $errors.ToArray())
            }
        }
        if ($primary) { throw $primary }
    }
}
