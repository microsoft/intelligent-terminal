#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

# Real shell -> packaged wtcli/COM -> master -> mixed Sidebar rows. No ACP prompt
# or settings writes. Raw icon bounds are optional evidence, not glyph proof.
Describe 'Feature: Sidebar row alignment' -Tag @('Feature', 'SidebarRowAlignment') {
    BeforeAll {
        function Resolve-AlignmentMarker {
            param([AllowNull()][AllowEmptyString()][string]$Value)
            if ([string]::IsNullOrEmpty($Value)) {
                return 'row-align-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
            }
            if ($Value -cnotmatch '\A[A-Za-z0-9][A-Za-z0-9-]{7,63}\z') {
                throw 'ITE2E_ALIGNMENT_MARKER must be a unique run-scoped 8-64 character alphanumeric/hyphen identifier.'
            }
            $Value
        }
        $script:marker = Resolve-AlignmentMarker $env:ITE2E_ALIGNMENT_MARKER
        $script:alignmentFlowDirection = if ($env:ITE2E_ALIGNMENT_FLOW_DIRECTION) {
            $env:ITE2E_ALIGNMENT_FLOW_DIRECTION
        } else { 'LTR' }
        if ($script:alignmentFlowDirection -cnotin @('LTR', 'RTL')) {
            throw 'ITE2E_ALIGNMENT_FLOW_DIRECTION must match the tested layout: LTR or RTL.'
        }
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\SidebarSessionCleanup.ps1')
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:app = $null
        $script:ownsConfigBackup = $false
        $script:runtimeBackedUp = $false
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'Explicit Dev package selection is required.' }
        $script:target = Resolve-ItApp -Package Dev
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'Close Dev before running alignment; existing processes are never adopted.'
        }
        $headLines = @(& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse --verify HEAD)
        if ($headLines.Count -ne 1 -or $LASTEXITCODE -ne 0 -or
            $headLines[0] -notmatch '\A[0-9a-fA-F]{40}\z') {
            throw 'Cannot resolve candidate HEAD for exact-source build provenance.'
        }
        $head = $headLines[0]
        if (-not $env:ITE2E_SOURCE_COMMIT -or
            $env:ITE2E_SOURCE_COMMIT -notmatch ('^' + [regex]::Escape($head) + '(?:$|[-+ ])')) {
            throw 'ITE2E_SOURCE_COMMIT must identify HEAD from the exact-source build receipt.'
        }
        foreach ($pair in @(
            @((Join-Path $script:target.InstallLocation 'TerminalApp.dll'), $env:ITE2E_EXPECTED_APP_SHA256),
            @($script:target.WtaPath, $env:ITE2E_EXPECTED_WTA_SHA256)
        )) {
            if ($pair[1] -notmatch '^[0-9a-fA-F]{64}$') { throw 'Exact App/WTA SHA256 build receipts are required.' }
            (Get-FileHash -LiteralPath $pair[0]).Hash | Should -Be $pair[1]
        }
        Get-WtSetting -App $script:target -Key tabLayout | Should -Be 'vertical'
        Get-FreCompleted -App $script:target | Should -BeTrue
        $script:settingsHash = (Get-FileHash -LiteralPath $script:target.SettingsPath).Hash
        $script:stateHash = (Get-FileHash -LiteralPath $script:target.StatePath).Hash
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Existing recovery backup must be recovered first: $path"
            }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) $script:marker
        if (Test-Path -LiteralPath $script:evidence -ErrorAction Stop) {
            throw 'Alignment evidence already exists; use a distinct artifact root for each matched capture.'
        }
        New-Item -ItemType Directory -Path $script:evidence -ErrorAction Stop | Out-Null
        $script:runtimePath = Join-Path $script:target.LocalStateDir 'IntelligentTerminal'
        $script:runtimeBackup = Join-Path $script:evidence 'original-runtime'
        $script:runtimeExisted = Test-Path -LiteralPath $script:runtimePath -ErrorAction Stop
        $script:runtimeHashes = @{}
        if ($script:runtimeExisted) {
            foreach ($file in Get-ChildItem -LiteralPath $script:runtimePath -File -Recurse -Force -ErrorAction Stop) {
                $relative = [IO.Path]::GetRelativePath($script:runtimePath, $file.FullName)
                $script:runtimeHashes[$relative] = (Get-FileHash -LiteralPath $file.FullName -ErrorAction Stop).Hash
            }
            Copy-Item -LiteralPath $script:runtimePath -Destination $script:runtimeBackup -Recurse -ErrorAction Stop
            foreach ($relative in $script:runtimeHashes.Keys) {
                (Get-FileHash -LiteralPath (Join-Path $script:runtimeBackup $relative) -ErrorAction Stop).Hash |
                    Should -Be $script:runtimeHashes[$relative]
            }
        }
        @{ settings = $script:settingsHash; state = $script:stateHash; runtime = $script:runtimeHashes } |
            ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence 'original-hashes.json') -ErrorAction Stop
        $script:runtimeBackedUp = $true

        if (-not ('ItE2E.RowAlignmentActivation' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ItE2E {
    public static class RowAlignmentActivation {
        [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
        [ComImport, Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D"),
         InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IActivationManager {
            [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
                [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
        }
        [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
        private class ActivationManager { }
        public static void Launch(string appId, string arguments) {
            var manager = (IActivationManager)new ActivationManager();
            try {
                uint processId;
                Marshal.ThrowExceptionForHR(manager.ActivateApplication(appId, arguments, 0, out processId));
            } finally { Marshal.ReleaseComObject(manager); }
        }
    }
}
'@
        }
        $script:liveTitle = "$script:marker-live"
        $script:historyTitle = "$script:marker-history"
        $arguments = '-w new new-tab --title "' + $script:liveTitle + '" --startingDirectory "' +
            $script:evidence + '" pwsh -NoLogo -NoProfile -NoExit'
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'Dev opened during preparation; refusing launch.'
        }
        Backup-WtConfig -App $script:target
        $script:ownsConfigBackup = $true
        (Get-FileHash -LiteralPath "$($script:target.SettingsPath).e2ebak").Hash | Should -Be $script:settingsHash
        (Get-FileHash -LiteralPath "$($script:target.StatePath).e2ebak").Hash | Should -Be $script:stateHash
        [ItE2E.RowAlignmentActivation]::Launch($script:target.AppUserModelId, $arguments)
        $window = Wait-Until -TimeoutSec 40 -Because 'the explicitly activated fixture window appears' -Condition {
            Get-WtWindowHwnds -App $script:target | Where-Object {
                $_.title -eq $script:liveTitle -and (Get-Process -Id $_.pid).Path -eq $script:target.WindowsTerminal
            } | Select-Object -First 1
        }
        $script:app = $script:target.PSObject.Copy()
        $script:app.Hwnd = $window.hwnd
        $script:app.Pid = $window.pid
        # HWND ownership is not shared-process termination authority.
        $script:app | Add-Member -NotePropertyName Launched -NotePropertyValue $false
        $script:app | Add-Member -NotePropertyName OwnedPaneIds -NotePropertyValue ([Collections.Generic.List[string]]::new())
        Resolve-WtComClsid -App $script:app | Out-Null
        Assert-SidebarWindowOwnership -App $script:app
        $tabs = @(foreach ($protocolWindow in Get-WtWindows -App $script:app) {
            Get-WtTabs -App $script:app -WindowId $protocolWindow.window_id |
                Where-Object title -EQ $script:liveTitle
        })
        $tabs.Count | Should -Be 1
        $panes = @(Get-WtPanes -App $script:app -TabId $tabs[0].tab_id -WindowId $tabs[0].window_id |
            Where-Object { $_.window_id -eq $tabs[0].window_id -and -not $_.is_agent_pane })
        $panes.Count | Should -Be 1
        $script:pane = $panes[0].session_id
        $script:app | Add-Member -NotePropertyName WindowId -NotePropertyValue $panes[0].window_id
        $script:app.OwnedPaneIds.Add($script:pane)
        $script:pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw -ErrorAction Stop).Trim()
        $script:liveId = "$script:marker-current"
        $script:historyId = "$script:marker-ended"
        $script:hookFixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Emit-SidebarSessionHooks.ps1')).Path

        function Send-AlignmentHooks {
            param([object[]]$Events)
            $token = [guid]::NewGuid().ToString('N')
            $inputPath = Join-Path $script:evidence "$token.json"
            $receiptPath = Join-Path $script:evidence "$token.receipt.json"
            ConvertTo-Json -InputObject $Events -Depth 8 | Set-Content -LiteralPath $inputPath -ErrorAction Stop
            $quote = { param($Text) "'" + $Text.Replace("'", "''") + "'" }
            $command = "& $(& $quote $script:hookFixture) -InputPath $(& $quote $inputPath)" +
                " -ReceiptPath $(& $quote $receiptPath) -WtcliPath $(& $quote $script:app.WtcliPath)"
            Send-WtInput -App $script:app -SessionId $script:pane -Text $command
            Send-WtKeys -App $script:app -SessionId $script:pane -Keys Enter
            Wait-Until -TimeoutSec 45 -Because 'shell hooks complete through the packaged COM bridge' -Condition {
                Test-Path -LiteralPath $receiptPath
            } | Out-Null
            $receipt = Get-Content -LiteralPath $receiptPath -Raw -ErrorAction Stop | ConvertFrom-Json
            $receipt.events | Should -Be $Events.Count
            $receipt.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $script:pane.Trim('{}').ToLowerInvariant()
        }
        function Read-AlignmentSessions {
            $result = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $script:pipe, '--origin', 'shell', '--json', '--include-status') -Raw
            if ($result.ExitCode -ne 0) { throw $result.StdErr }
            @(($result.StdOut | ConvertFrom-Json -Depth 32).sessions |
                Where-Object session_id -In @($script:liveId, $script:historyId))
        }
        function Get-AlignmentElement {
            param([string]$Id)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $Id))
        }
        function Get-AlignmentRawParts {
            param($Element)
            $walker = [Windows.Automation.TreeWalker]::RawViewWalker
            $child = $walker.GetFirstChild($Element)
            while ($child) {
                $child
                Get-AlignmentRawParts $child
                $child = $walker.GetNextSibling($child)
            }
        }
        function Get-AlignmentRows {
            $list = Get-AlignmentElement ItemsList
            if (-not $list -or $list.Current.IsOffscreen) { throw 'Visible mixed ItemsList is required.' }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            @($list.FindAll([Windows.Automation.TreeScope]::Children, $condition))
        }
        function Get-AlignmentTitle {
            param($Row, [string]$Title)
            @(Get-AlignmentRawParts $Row | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    $_.Current.Name -eq $Title -and -not $_.Current.IsOffscreen -and
                    $_.Current.BoundingRectangle.Width -gt 0
            })
        }
        function Get-AlignmentRect {
            param($Element)
            $r = $Element.Current.BoundingRectangle
            @{ left = $r.Left; top = $r.Top; right = $r.Right; bottom = $r.Bottom; width = $r.Width; height = $r.Height }
        }
        function Get-AlignmentLeadingEdge {
            param($Rectangle, [ValidateSet('LTR', 'RTL')][string]$FlowDirection)
            if ($FlowDirection -eq 'RTL') { $Rectangle.Right } else { $Rectangle.Left }
        }
        function Assert-AlignmentDelta {
            param([double]$First, [double]$Second, [double]$Scale)
            if ($Scale -le 0) { throw 'A valid physical-pixel/DIP scale is required.' }
            [Math]::Abs($First - $Second) / $Scale | Should -BeLessOrEqual 1
        }
        $events = @(
            @{ event = 'agent.session.start'; payload = @{ session_id = $script:historyId; cwd = (Join-Path $script:evidence $script:historyTitle) } }
            @{ event = 'agent.session.end'; payload = @{ session_id = $script:historyId; reason = 'user_exit' } }
            @{ event = 'agent.session.start'; payload = @{ session_id = $script:liveId; cwd = (Join-Path $script:evidence $script:liveTitle) } }
            @{ event = 'agent.stop'; payload = @{ session_id = $script:liveId } }
        )
        Send-AlignmentHooks $events
        Wait-Until -TimeoutSec 20 -Because 'master contains exactly one ended and one idle same-provider shell fixture' -Condition {
            $sessions = @(Read-AlignmentSessions)
            $sessions.Count -eq 2 -and @($sessions | Where-Object {
                $_.session_id -eq $script:historyId -and $_.status -in @('Ended', 'Historical') -and $_.provider_id -eq 'copilot'
            }).Count -eq 1 -and @($sessions | Where-Object {
                $_.session_id -eq $script:liveId -and $_.status -eq 'Idle' -and $_.provider_id -eq 'copilot'
            }).Count -eq 1
        } | Out-Null
        $toggle = (Get-AlignmentElement SearchTabsButton).GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
        if ($toggle.Current.ToggleState -ne [Windows.Automation.ToggleState]::On) {
            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
        }
        Wait-UiElement -App $script:app -Selector SearchTextBox -TimeoutSec 15 | Out-Null
        Set-UiValue -App $script:app -Selector SearchTextBox -Value $script:marker | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'shared search reveals the owned live and recent titles regardless of scope' -Condition {
            $rows = @(Get-AlignmentRows)
            @($rows | Where-Object { @(Get-AlignmentTitle $_ $script:liveTitle).Count -eq 1 }).Count -eq 1 -and
                @($rows | Where-Object { @(Get-AlignmentTitle $_ $script:historyTitle).Count -eq 1 }).Count -eq 1
        } | Out-Null
        $loadedApp = @((Get-Process -Id $script:app.Pid).Modules | Where-Object ModuleName -EQ TerminalApp.dll)
        $loadedApp.Count | Should -Be 1
        $loadedApp[0].FileName | Should -Be (Join-Path $script:target.InstallLocation 'TerminalApp.dll')
        (Get-FileHash -LiteralPath $loadedApp[0].FileName).Hash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        @{
            source_commit = $env:ITE2E_SOURCE_COMMIT; head = $head; package = $script:app.Package
            version = $script:app.Version; app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256
            wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256; pane_id = $script:pane
            sessions = @(Read-AlignmentSessions)
            loaded_app = @($loadedApp | Select-Object FileName)
        } | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json') -ErrorAction Stop
    }

    AfterAll {
        $cleanupFailure = $null
        try {
            Invoke-SidebarSessionCleanup -App $script:app -Target $script:target `
                -OwnsConfigBackup $script:ownsConfigBackup -SettingsHash $script:settingsHash `
                -StateHash $script:stateHash -Evidence $script:evidence
        }
        catch { $cleanupFailure = $_ }
        # The existing primitive writes this receipt only after owned-pane closure
        # and package inactivity. Screenshot failure must not prevent safe recovery.
        $runtimeRecoveryFailure = $null
        if ($script:ownsConfigBackup -and $script:runtimeBackedUp) {
            try {
                if (-not $script:evidence -or
                    -not (Test-Path -LiteralPath (Join-Path $script:evidence 'cleanup.json') -ErrorAction Stop)) {
                    throw 'Runtime recovery receipt is missing; the snapshot is retained.'
                }
                $recovery = Get-Content -LiteralPath (Join-Path $script:evidence 'cleanup.json') -Raw -ErrorAction Stop |
                    ConvertFrom-Json -NoEnumerate -ErrorAction Stop
                if ($recovery -isnot [pscustomobject]) {
                    throw 'Runtime recovery receipt is not an object; the snapshot is retained.'
                }
                $settingsPreserved = $recovery.PSObject.Properties['settings_preserved']
                $statePreserved = $recovery.PSObject.Properties['state_preserved']
                if (-not $settingsPreserved -or -not $statePreserved -or
                    $settingsPreserved.Value -isnot [bool] -or $statePreserved.Value -isnot [bool] -or
                    -not $settingsPreserved.Value -or -not $statePreserved.Value) {
                    throw 'Runtime recovery requires boolean successful preservation flags; the snapshot is retained.'
                }
                if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                    throw 'Dev active: runtime recovery snapshot retained.'
                }
                foreach ($relative in $script:runtimeHashes.Keys) {
                    (Get-FileHash -LiteralPath (Join-Path $script:runtimeBackup $relative) -ErrorAction Stop).Hash |
                        Should -Be $script:runtimeHashes[$relative]
                }
                if (Test-Path -LiteralPath $script:runtimePath -ErrorAction Stop) { Remove-Item -LiteralPath $script:runtimePath -Recurse -Force -ErrorAction Stop }
                if ($script:runtimeExisted) {
                    Copy-Item -LiteralPath $script:runtimeBackup -Destination $script:runtimePath -Recurse -ErrorAction Stop
                    foreach ($relative in $script:runtimeHashes.Keys) {
                        (Get-FileHash -LiteralPath (Join-Path $script:runtimePath $relative) -ErrorAction Stop).Hash |
                            Should -Be $script:runtimeHashes[$relative]
                    }
                }
                @{ restored = $true; hashes = $script:runtimeHashes } | ConvertTo-Json -Depth 8 |
                    Set-Content -LiteralPath (Join-Path $script:evidence 'runtime-cleanup.json') -ErrorAction Stop
            }
            catch { $runtimeRecoveryFailure = $_ }
        }
        if ($cleanupFailure -and $runtimeRecoveryFailure) {
            throw [AggregateException]::new('Original cleanup failure and runtime recovery failure; snapshot retained.',
                [Exception[]]@($cleanupFailure.Exception, $runtimeRecoveryFailure.Exception))
        }
        if ($cleanupFailure) { throw $cleanupFailure }
        if ($runtimeRecoveryFailure) { throw $runtimeRecoveryFailure }
    }

    It 'Sidebar live and recent titles align' {
        Assert-SidebarWindowOwnership -App $script:app
        Get-UiValue -App $script:app -Selector SearchTextBox -ValuePattern | Should -Be $script:marker
        $list = Get-AlignmentElement ItemsList
        $rows = @(Get-AlignmentRows)
        $live = @($rows | Where-Object { @(Get-AlignmentTitle $_ $script:liveTitle).Count -eq 1 })[0]
        $recent = @($rows | Where-Object { @(Get-AlignmentTitle $_ $script:historyTitle).Count -eq 1 })[0]
        $liveTitle = @(Get-AlignmentTitle $live $script:liveTitle)[0]
        $recentTitle = @(Get-AlignmentTitle $recent $script:historyTitle)[0]
        $scale = [ItE2E.RowAlignmentActivation]::GetDpiForWindow([IntPtr]([long]$script:app.Hwnd)) / 96.0
        $scale | Should -BeGreaterThan 0
        $viewport = $list.Current.BoundingRectangle
        $liveLeading = Get-AlignmentLeadingEdge $liveTitle.Current.BoundingRectangle $script:alignmentFlowDirection
        $recentLeading = Get-AlignmentLeadingEdge $recentTitle.Current.BoundingRectangle $script:alignmentFlowDirection
        foreach ($row in @($live, $recent)) {
            $r = $row.Current.BoundingRectangle
            $row.Current.IsOffscreen | Should -BeFalse
            $r.Width | Should -BeGreaterThan 0
            $r.Height | Should -BeGreaterThan 0
            $r.Top | Should -BeGreaterOrEqual $viewport.Top
            $r.Bottom | Should -BeLessOrEqual $viewport.Bottom
        }
        $parts = @(Get-AlignmentRawParts $recent)
        $metadata = @($parts | Where-Object {
            $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                $_.Current.Name -and $_.Current.Name -ne $script:historyTitle -and
                $_.Current.BoundingRectangle.Top -ge ($recentTitle.Current.BoundingRectangle.Bottom - $scale)
        } | Sort-Object {
            if ($script:alignmentFlowDirection -eq 'RTL') { -$_.Current.BoundingRectangle.Right }
            else { $_.Current.BoundingRectangle.Left }
        })
        $metadata.Count | Should -BeGreaterThan 0
        $metadataLeading = Get-AlignmentLeadingEdge $metadata[0].Current.BoundingRectangle $script:alignmentFlowDirection
        $historyIcon = @($parts | Where-Object {
            $_.Current.AutomationId -eq 'HistoryProviderIcon' -and -not $_.Current.IsOffscreen -and
                $_.Current.BoundingRectangle.Width -gt 0
        })
        $liveIcon = @(Get-AlignmentRawParts $live | Where-Object {
            $_.Current.AutomationId -eq 'TabIconPresenter' -and -not $_.Current.IsOffscreen -and
                $_.Current.BoundingRectangle.Width -gt 0
        })
        $iconsVerified = $liveIcon.Count -eq 1 -and $historyIcon.Count -eq 1
        $receipt = @{
            fixture_marker = $script:marker; live_title_text = $script:liveTitle; recent_title_text = $script:historyTitle
            source_commit = $env:ITE2E_SOURCE_COMMIT; app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256
            wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256
            coordinate_space = 'physical screen; offsets share ItemsList origin'; dip_scale = $scale
            flow_direction = $script:alignmentFlowDirection
            items_list = Get-AlignmentRect $list; live_row = Get-AlignmentRect $live; recent_row = Get-AlignmentRect $recent
            live_title = Get-AlignmentRect $liveTitle; recent_title = Get-AlignmentRect $recentTitle
            history_metadata = Get-AlignmentRect $metadata[0]
            title_delta_dip = ($liveLeading - $recentLeading) / $scale
            metadata_delta_dip = ($metadataLeading - $recentLeading) / $scale
            icon_geometry_verified = $iconsVerified
            icon_limitation = 'Raw provider control bounds are not compositor glyph proof; absent peers require native transforms and independent visual sign-off.'
            compositor_signoff = 'pending independent review of alignment.png and selected.png'
        }
        if ($iconsVerified) {
            $receipt.live_icon = Get-AlignmentRect $liveIcon[0]
            $receipt.history_icon = Get-AlignmentRect $historyIcon[0]
            $a = $liveIcon[0].Current.BoundingRectangle
            $b = $historyIcon[0].Current.BoundingRectangle
            $receipt.icon_center_delta_dip = (($a.Left + $a.Width / 2) - ($b.Left + $b.Width / 2)) / $scale
        }
        $receipt | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence 'geometry.json') -ErrorAction Stop
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'alignment.png') | Out-Null
        # Use one screen origin, not each row's local indentation (which hides the regression).
        Assert-AlignmentDelta $liveLeading $recentLeading $scale
        Assert-AlignmentDelta $metadataLeading $recentLeading $scale
        if ($iconsVerified) {
            foreach ($icon in @($liveIcon[0], $historyIcon[0])) {
                $icon.Current.BoundingRectangle.Width / $scale | Should -BeGreaterOrEqual 15
                $icon.Current.BoundingRectangle.Width / $scale | Should -BeLessOrEqual 17
            }
            $a = $liveIcon[0].Current.BoundingRectangle
            $b = $historyIcon[0].Current.BoundingRectangle
            Assert-AlignmentDelta ($a.Left + $a.Width / 2) ($b.Left + $b.Width / 2) $scale
        }
        else { Write-Warning $receipt.icon_limitation }
        $active = (Get-ActivePane -App $script:app).session_id
        $active | Should -Be $script:pane
        $recent.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
        # History interaction deliberately preserves the canonical live-tab selection.
        $recent.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected | Should -BeFalse
        $live.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected | Should -BeTrue
        (Get-ActivePane -App $script:app).session_id | Should -Be $active
        $selected = $recent.Current.BoundingRectangle
        Assert-AlignmentDelta $selected.Left $receipt.recent_row.left $scale
        Assert-AlignmentDelta $selected.Right $receipt.recent_row.right $scale
        Get-AlignmentRect $recent | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'selected-bounds.json') -ErrorAction Stop
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'selected.png') | Out-Null
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-unmatched" | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'a nonmatching query excludes both real owned fixtures' -Condition {
            @((Get-AlignmentRows) | Where-Object {
                @(Get-AlignmentTitle $_ $script:liveTitle).Count -or @(Get-AlignmentTitle $_ $script:historyTitle).Count
            }).Count -eq 0
        } | Out-Null
        Set-UiValue -App $script:app -Selector SearchTextBox -Value $script:marker | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'restoring shared search recovers both owned rows' -Condition {
            $rows = @(Get-AlignmentRows)
            @($rows | Where-Object { @(Get-AlignmentTitle $_ $script:liveTitle).Count -eq 1 }).Count -eq 1 -and
                @($rows | Where-Object { @(Get-AlignmentTitle $_ $script:historyTitle).Count -eq 1 }).Count -eq 1
        } | Out-Null
        Get-UiValue -App $script:app -Selector SearchTextBox -ValuePattern | Should -Be $script:marker
        (Get-FileHash -LiteralPath $script:target.SettingsPath).Hash | Should -Be $script:settingsHash
    }
}
