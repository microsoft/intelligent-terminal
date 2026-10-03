#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
function Initialize-CombinedCleanupNative {
    if ('ItE2ECombinedCleanup.Native' -as [type]) { return }
    Add-Type @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace ItE2ECombinedCleanup {
    [ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager {
        [PreserveSig]
        int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
            [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
    }
    public static class Native {
        delegate bool EnumWindowCallback(IntPtr hwnd, IntPtr parameter);
        [DllImport("user32.dll", SetLastError=true)]
        static extern bool EnumWindows(EnumWindowCallback callback, IntPtr parameter);
        [DllImport("user32.dll")]
        static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll", SetLastError=true)]
        static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
        public static uint[] VisibleProcessIds() {
            var ids = new HashSet<uint>();
            int error = 0;
            bool failed = false;
            if (!EnumWindows((hwnd, parameter) => {
                if (IsWindowVisible(hwnd)) {
                    uint pid;
                    if (GetWindowThreadProcessId(hwnd, out pid) == 0) {
                        failed = true;
                        error = Marshal.GetLastWin32Error();
                    } else
                        ids.Add(pid);
                }
                return true;
            }, IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error());
            if (failed) throw new Win32Exception(error);
            var result = new uint[ids.Count];
            ids.CopyTo(result);
            return result;
        }
        public static uint Activate(string appId) {
            var instance = Activator.CreateInstance(Type.GetTypeFromCLSID(
                new Guid("45ba127d-10a8-46ea-8ab7-56ea9078943c"), true));
            try {
                uint pid;
                Marshal.ThrowExceptionForHR(((IApplicationActivationManager)instance)
                    .ActivateApplication(appId,
                        "-w new new-tab --title ite2e-combined-cleanup cmd.exe /c exit", 0, out pid));
                return pid;
            } finally {
                Marshal.FinalReleaseComObject(instance);
            }
        }
    }
}
'@
}

function Get-CombinedVisibleProcessIds {
    Initialize-CombinedCleanupNative
    [ItE2ECombinedCleanup.Native]::VisibleProcessIds()
}

function Start-CombinedCleanupTab {
    param([Parameter(Mandatory)][string]$AppUserModelId)
    $initializer = (Get-Command Initialize-CombinedCleanupNative).Definition
    $launch = "function Initialize-CombinedCleanupNative { $initializer }; Initialize-CombinedCleanupNative; " +
        "[ItE2ECombinedCleanup.Native]::Activate('$($AppUserModelId.Replace("'", "''"))')"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($launch))
    $result = Invoke-Native -FilePath (Get-Command pwsh -ErrorAction Stop).Source `
        -Arguments @('-NoProfile', '-EncodedCommand', $encoded) -TimeoutSec 15
    if ($result.TimedOut -or $result.ExitCode -ne 0) {
        throw "Combined cleanup task-tab activation failed (timeout=$($result.TimedOut)): $($result.StdErr)"
    }
    [uint32]::Parse($result.StdOut.Trim())
}

function Invoke-CombinedHeadlessRecovery {
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)]$Target, [bool]$InitiallyInactive)
    if (-not $InitiallyInactive -or -not $App.Launched -or -not $App.OwnedProcess -or
        $App.OwnedProcess.Id -ne $App.Pid -or -not $App.OwnedProcess.HasExited -or
        $App.Package -ne $Target.Package -or -not $Target.AppUserModelId -or
        $App.AppUserModelId -ne $Target.AppUserModelId -or -not $Target.WindowsTerminal) {
        throw 'Combined headless recovery requires initial inactivity and a confirmed owned Dev host exit.'
    }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $expectedPath = [IO.Path]::GetFullPath($Target.WindowsTerminal)
    $remaining = @(Get-WtProcessesForApp -App $Target -IncludePackageExecutables)
    if (-not $remaining.Count) { return }
    $validate = {
        $visible = @(Get-CombinedVisibleProcessIds)
        foreach ($process in $remaining) {
            if ($clock.Elapsed.TotalSeconds -ge 12) { throw 'Combined headless identity checks exceeded their time bound.' }
            $null = $process.Handle
            if ($process.HasExited -or $process.MainWindowHandle -ne 0 -or $process.Id -in $visible -or
                -not $process.Path -or [IO.Path]::GetFullPath($process.Path) -ne $expectedPath) {
                throw 'Combined recovery refuses visible, changed, or non-host package processes.'
            }
            $snapshot = Get-CimInstance Win32_Process -Filter "ProcessId=$($process.Id)" -OperationTimeoutSec 5 -ErrorAction Stop
            $ticks = $process.StartTime.ToUniversalTime().Ticks
            if (-not $snapshot -or $snapshot.ProcessId -ne $process.Id -or -not $snapshot.CreationDate -or
                -not $snapshot.ExecutablePath -or $snapshot.Name -ine 'WindowsTerminal.exe' -or
                [IO.Path]::GetFullPath($snapshot.ExecutablePath) -ne $expectedPath -or
                $snapshot.CreationDate.ToUniversalTime().Ticks -ne ($ticks - $ticks % 10) -or
                $process.StartTime -le $App.OwnedProcess.StartTime -or
                $snapshot.CommandLine -notmatch '(?i)(?:^"[^"]+"|^\S+)\s+-Embedding\s*$') {
                throw 'Combined recovery requires an exact, identity-bound headless Dev COM server.'
            }
        }
    }
    & $validate
    $current = @(Get-WtProcessesForApp -App $Target -IncludePackageExecutables)
    if (-not $current.Count) { return }
    if ($current.Count -ne $remaining.Count -or @($current | Where-Object {
        $currentId = $_.Id
        $captured = $remaining | Where-Object Id -eq $currentId | Select-Object -First 1
        -not $captured -or $captured.StartTime -ne $_.StartTime
    }).Count) { throw 'Dev membership changed before the bounded cleanup activation.' }
    & $validate
    if ($clock.Elapsed.TotalSeconds -ge 12) { throw 'Combined headless verification exceeded its pre-activation time bound.' }
    # A new self-exiting task tab supplies normal GUI lifetime without adopting or killing these hosts.
    $activatedPid = Start-CombinedCleanupTab -AppUserModelId $Target.AppUserModelId
    $waitSeconds = [Math]::Min(30, [Math]::Max(0, 57 - $clock.Elapsed.TotalSeconds))
    if ($waitSeconds -le 0 -or -not (Test-Until -TimeoutSec $waitSeconds -IntervalSec 0.25 -Condition {
        -not @(Get-WtProcessesForApp -App $Target -IncludePackageExecutables).Count
    })) { throw 'Combined cleanup task tab did not quiesce Dev within 30 seconds; backups retained.' }
    Start-Sleep -Seconds 3
    if (@(Get-WtProcessesForApp -App $Target -IncludePackageExecutables).Count -or $clock.Elapsed.TotalSeconds -gt 60) {
        throw 'Dev reactivated or bounded recovery expired; backups retained.'
    }
    [pscustomobject]@{ headless_ids = @($remaining.Id); activated_pid = $activatedPid; package_process_count = 0 }
}
}

BeforeDiscovery {
    if ($env:ITE2E_COMBINED_RETENTION_STATUS -and $env:ITE2E_COMBINED_RETENTION_STATUS -notin @('Idle', 'Working')) {
        throw 'ITE2E_COMBINED_RETENTION_STATUS must be Idle or Working when supplied.'
    }
}

Describe 'Feature: combined Agents sidebar' -Tag @('Feature', 'CombinedAgentsSidebar') {
    BeforeAll {
        $script:app = $null
        $script:ownsConfig = $false
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        if ($env:ITE2E_PACKAGE -ne 'Dev') { throw 'Combined sidebar validation requires ITE2E_PACKAGE=Dev.' }
        $script:target = Resolve-ItApp -Package Dev
        $script:initialProcessCheckAt = [DateTimeOffset]::UtcNow.ToString('o')
        $script:initialProcesses = @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables)
        if ($script:initialProcesses.Count) {
            throw 'Refusing to adopt or close an existing Dev process.'
        }
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply TerminalApp.dll and WTA SHA-256 values from the exact feature build receipt.'
        }
        $expectedHead = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $expectedHead) { throw 'Cannot determine the source revision for build provenance.' }
        if (-not $env:ITE2E_SOURCE_COMMIT -or -not $env:ITE2E_SOURCE_COMMIT.StartsWith($expectedHead, [StringComparison]::Ordinal)) {
            throw "Supply source provenance from the feature build receipt rooted at $expectedHead."
        }
        $appHash = (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash
        $wtaHash = (Get-FileHash -LiteralPath $script:target.WtaPath).Hash
        $appHash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        $wtaHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Existing configuration backup requires recovery: $path"
            }
            $script:originalHashes[$path] = if (Test-Path -LiteralPath $path) {
                (Get-FileHash -LiteralPath $path).Hash
            } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('combined-sidebar-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        foreach ($path in $script:originalHashes.Keys) {
            if (Test-Path -LiteralPath $path) {
                Copy-Item -LiteralPath $path -Destination (Join-Path $script:evidence ('original-' + [IO.Path]::GetFileName($path)))
            }
        }
        @{
            package = $script:target.Package; version = $script:target.Version
            install_location = $script:target.InstallLocation; source_commit = $env:ITE2E_SOURCE_COMMIT
            app_sha256 = $appHash; wta_sha256 = $wtaHash
            original_configuration_hashes = $script:originalHashes
            initial_process_check_at = $script:initialProcessCheckAt
            initial_package_process_ids = @($script:initialProcesses.Id)
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package-before-launch.json')
        $script:historyPath = Join-Path $script:evidence 'history.json'
        $script:fixtureLog = Join-Path $script:evidence 'fixture.log'
        $script:releasePromptPath = Join-Path $script:evidence 'release-prompt'
        $script:heldPromptMarker = $null
        $script:marker = 'combined-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
        $script:history = @(foreach ($i in 0..23) {
            @{
                sessionId = "$script:marker-history-$i"
                title = "$script:marker-history-$('{0:D2}' -f $i)"
                cwd = $script:evidence
                updatedAt = [DateTimeOffset]::UtcNow.AddMinutes(-$i).ToString('o')
            }
        })
        @{ sessions = $script:history } | ConvertTo-Json -Depth 6 |
            Set-Content -LiteralPath $script:historyPath -Encoding utf8
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))' -HistoryPath '$($script:historyPath.Replace("'", "''"))' -ReleasePromptPath '$($script:releasePromptPath.Replace("'", "''"))'"
        $command = 'pwsh -NoProfile -EncodedCommand ' +
            [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))

        function Get-CombinedElement {
            param([string]$Id)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        }
        function Get-CombinedRows {
            param([string]$Id)
            $list = Get-CombinedElement $Id
            if (-not $list -or $list.Current.IsOffscreen) { throw "Missing visible list: $Id" }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            @($list.FindAll([Windows.Automation.TreeScope]::Children, $condition))
        }
        function Get-CombinedRowText {
            param($Row)
            @($Row.Current.Name; @($Row.FindAll(
                [Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)) |
                ForEach-Object { $_.Current.Name }) -join ' '
        }
        function Get-CombinedRawChildren {
            param($Element)
            $walker = [Windows.Automation.TreeWalker]::RawViewWalker
            $child = $walker.GetFirstChild($Element)
            while ($child) {
                $child
                Get-CombinedRawChildren $child
                $child = $walker.GetNextSibling($child)
            }
        }
        function Get-CombinedVisiblePart {
            param($Element, [string]$Id)
            $parts = @(Get-CombinedRawChildren $Element | Where-Object {
                $_.Current.AutomationId -eq $Id -and -not $_.Current.IsOffscreen -and
                    $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0
            })
            $parts.Count | Should -Be 1 -Because "the rendered $Id must have one real UIA peer, including Raw view"
            $parts[0]
        }
        function Assert-CombinedHeaderCue {
            param([string]$Name)
            $away = (Get-CombinedElement ItemsList).Current.BoundingRectangle
            if (-not [ItE2E.ItWtWin32Input]::SetCursorPos(
                [int]($away.Left + $away.Width / 2), [int]($away.Top + $away.Height / 2))) {
                throw 'Cannot move the pointer away from the header to prove its persistent non-hover cue.'
            }
            Start-Sleep -Milliseconds 250
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "header-cue-$Name.png") | Out-Null
            Get-UiTree -App $script:app -Selector VerticalTabsHeaderButton -Depth 6 |
                Set-Content -LiteralPath (Join-Path $script:evidence "header-cue-$Name.tree.txt")
            $button = Get-CombinedElement VerticalTabsHeaderButton
            $button.Current.IsOffscreen | Should -BeFalse
            $button.Current.IsEnabled | Should -BeTrue
            $label = Get-CombinedVisiblePart $button VerticalTabsHeader
            $label.Current.Name | Should -Be $Name
            $bounds = $button.Current.BoundingRectangle
            $textBounds = $label.Current.BoundingRectangle
            $bounds.Width | Should -BeGreaterThan 0
            $bounds.Height | Should -BeGreaterThan 0
            $textBounds.Left | Should -BeGreaterOrEqual $bounds.Left
            $textBounds.Right | Should -BeLessOrEqual $bounds.Right
            $textBounds.Top | Should -BeGreaterOrEqual $bounds.Top
            $textBounds.Bottom | Should -BeLessOrEqual $bounds.Bottom
            # Decorative PathIcon visuals need not expose a UIA peer; pixels require screenshot review.
            @{
                name = $Name; button = $bounds.ToString()
                label = $textBounds.ToString()
                screenshot = "header-cue-$Name.png"; visual_cue_review = 'pending'
            } | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidence 'header-cue.jsonl')
        }
        function Assert-CombinedHistoryMetadata {
            param([string]$Title, [string]$Status, [string]$Provider, [switch]$OtherWindow)
            $metadataPhase = if ($OtherWindow) { "metadata-other-window-$Status" } else { "metadata-$($Status ?? 'Historical')" }
            Save-CombinedActionEvidence $metadataPhase -Screenshot
            $rows = @(Get-CombinedRows HistoryList)
            $rows.Count | Should -Be 1
            $row = $rows[0]
            $parts = @(Get-CombinedRawChildren $row)
            $visible = @($parts | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Height -gt 0 -and
                    $_.Current.BoundingRectangle.Width -gt 0
            })
            $textLeaves = @($visible | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    -not @(Get-CombinedRawChildren $_ | Where-Object {
                        $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                            -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                            $_.Current.BoundingRectangle.Height -gt 0
                    }).Count
            })
            # Highlighted text exposes leaf Text peers, not the wrapper's XAML name.
            $titles = @($textLeaves | Where-Object { $_.Current.Name.Contains($Title) })
            $titles.Count | Should -Be 1 -Because 'the history row must expose one unambiguous visible title'
            $titlePart = $titles[0]
            $times = @($textLeaves | Where-Object { $_.Current.Name -match '^(just now|\d+ (minute|hour|day)s? ago)$' })
            $times.Count | Should -Be 1 -Because 'the history row must expose one unambiguous visible relative time'
            $time = $times[0]
            $icon = Get-CombinedVisiblePart $row HistoryProviderIcon
            (Get-CombinedRowText $row) | Should -Match ([regex]::Escape($Title))
            $icon.Current.Name | Should -Be $Provider -Because 'icon-only provider identity remains accessible'
            $timeText = $time.Current.Name
            $timeText | Should -Match '^(just now|\d+ (minute|hour|day)s? ago)$'
            $timeText | Should -Not -Match ('Historical|Ended|' + [regex]::Escape($Provider))
            @($visible | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    $_.Current.Name -eq $Provider
            }).Count | Should -Be 0 -Because 'provider must not be repeated as metadata text'
            $titleBounds = $titlePart.Current.BoundingRectangle
            $timeBounds = $time.Current.BoundingRectangle
            $iconBounds = $icon.Current.BoundingRectangle
            $titleBounds.Bottom | Should -BeLessOrEqual $timeBounds.Top
            $titleBounds.Bottom | Should -BeLessOrEqual $iconBounds.Top
            $rowBounds = $row.Current.BoundingRectangle
            $titleBounds.Left | Should -BeLessOrEqual ($timeBounds.Left + 1)
            $iconBounds.Left | Should -BeGreaterOrEqual $rowBounds.Left
            $iconBounds.Right | Should -BeLessOrEqual $rowBounds.Right
            $iconBounds.Top | Should -BeGreaterOrEqual $rowBounds.Top
            $iconBounds.Bottom | Should -BeLessOrEqual $rowBounds.Bottom
            $timeBounds.Right | Should -BeLessOrEqual $iconBounds.Left
            $iconBounds.Top | Should -BeLessThan $timeBounds.Bottom
            $iconBounds.Bottom | Should -BeGreaterThan $timeBounds.Top
            if ($Status) {
                $statusLabel = if ($Status -eq 'Working') { 'Active' } else { $Status }
                if ($OtherWindow) { $statusLabel += ' · another window' }
                $statuses = @($textLeaves | Where-Object { $_.Current.Name -eq $statusLabel })
                $statuses.Count | Should -Be 1 -Because 'the history row must expose one unambiguous meaningful status'
                $statusPart = $statuses[0]
                $statusPart.Current.Name | Should -Be $statusLabel
                if ($OtherWindow) {
                    @($textLeaves | Where-Object { $_.Current.Name }).Count | Should -Be 3 -Because 'title plus time and combined status must not add a fourth metadata field'
                }
                $statusBounds = $statusPart.Current.BoundingRectangle
                $statusBounds.Left | Should -BeGreaterOrEqual $timeBounds.Right
                $statusBounds.Right | Should -BeLessOrEqual $iconBounds.Left
                $statusBounds.Top | Should -BeLessThan $iconBounds.Bottom
                $statusBounds.Bottom | Should -BeGreaterThan $iconBounds.Top
            } else {
                @($textLeaves | Where-Object {
                    $_.Current.Name -match '^(Historical|Ended|Idle|Active|Working|Attention|Error)$'
                }).Count | Should -Be 0 -Because 'redundant historical status is not rendered'
            }
            @{
                title = $Title; status = $Status; provider = $icon.Current.Name
                rendered_status = if ($Status) { $statusPart.Current.Name } else { $null }
                other_window = [bool]$OtherWindow
                title_bounds = $titleBounds.ToString(); time_bounds = $timeBounds.ToString()
                status_bounds = if ($Status) { $statusBounds.ToString() } else { $null }
                icon_bounds = $iconBounds.ToString()
            } | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidence 'history-metadata.jsonl')
        }
        function Get-CombinedScroll {
            param([string]$Id)
            $list = Get-CombinedElement $Id
            $pattern = $null
            if ($list.TryGetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern, [ref]$pattern)) { return $pattern }
            foreach ($child in @($list.FindAll(
                [Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition))) {
                if ($child.TryGetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern, [ref]$pattern)) { return $pattern }
            }
            throw "No real UIA ScrollPattern in $Id."
        }
        function Set-CombinedView {
            param([bool]$Agents)
            $list = Get-CombinedElement HistoryList
            $active = $list -and -not $list.Current.IsOffscreen
            if ([bool]$active -ne $Agents) {
                Invoke-UiClick -App $script:app -Selector VerticalTabsHeaderButton | Out-Null
            }
            Wait-Until -TimeoutSec 10 -Because 'the requested sidebar view renders' -Condition {
                $list = Get-CombinedElement HistoryList
                $header = Get-CombinedElement VerticalTabsHeader
                $expected = if ($Agents) { 'Agents' } else { 'Tabs' }
                [bool]($list -and -not $list.Current.IsOffscreen) -eq $Agents -and
                    $header -and $header.Current.Name -eq $expected
            } | Out-Null
        }
        function Assert-CombinedSearchState {
            param([bool]$Active)
            Wait-Until -TimeoutSec 5 -Because 'search visibility finishes the requested transition' -Condition {
                $search = Get-CombinedElement SearchTextBox
                [bool]($search -and -not $search.Current.IsOffscreen -and
                    $search.Current.BoundingRectangle.Height -gt 0) -eq $Active
            } | Out-Null
            $button = Get-CombinedElement SearchTabsButton
            $button | Should -Not -BeNullOrEmpty
            $toggle = $button.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
            $expected = if ($Active) { [Windows.Automation.ToggleState]::On } else { [Windows.Automation.ToggleState]::Off }
            $toggle.Current.ToggleState | Should -Be $expected
            $search = Get-CombinedElement SearchTextBox
            [bool]($search -and -not $search.Current.IsOffscreen -and
                $search.Current.BoundingRectangle.Height -gt 0) | Should -Be $Active
        }
        function Open-CombinedSearch {
            $toggle = (Get-CombinedElement SearchTabsButton).GetCurrentPattern(
                [Windows.Automation.TogglePattern]::Pattern)
            if ($toggle.Current.ToggleState -eq [Windows.Automation.ToggleState]::Off) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            }
            Assert-CombinedSearchState $true
        }
        function Set-CombinedQuery {
            param([string]$Value)
            Open-CombinedSearch
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $Value | Out-Null
        }
        function Assert-CombinedBounds {
            $upper = (Get-CombinedElement ItemsList).Current.BoundingRectangle
            $lower = (Get-CombinedElement HistoryList).Current.BoundingRectangle
            $divider = (Get-CombinedElement HistorySplitter).Current.BoundingRectangle
            $upper.Height | Should -BeGreaterThan 40
            $lower.Height | Should -BeGreaterThan 20
            $upper.Bottom | Should -BeLessOrEqual ($divider.Top + 2)
            $lower.Top | Should -BeGreaterOrEqual $divider.Bottom
            @{
                upper = @{ x = $upper.X; y = $upper.Y; width = $upper.Width; height = $upper.Height }
                lower = @{ x = $lower.X; y = $lower.Y; width = $lower.Width; height = $lower.Height }
                divider_y = $divider.Y
            } | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidence 'bounds.jsonl')
        }
        function Get-CombinedSnapshot {
            $before = @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables)
            $started = [DateTimeOffset]::UtcNow.ToString('o')
            $stopError = $null
            try {
                Invoke-Wta -App $script:app -TimeoutSec 10 -Arguments @(
                    'sessions', 'list', '--master', $script:pipe, '--json', '--include-status')
            }
            finally {
                @{
                    phase = 'master-snapshot'; started_at = $started; finished_at = [DateTimeOffset]::UtcNow.ToString('o')
                    process_ids_before = @($before.Id)
                    processes_after = @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables |
                        Select-Object Id, Path, StartTime)
                } | ConvertTo-Json -Depth 4 -Compress |
                    Add-Content -LiteralPath (Join-Path $script:evidence 'process-observations.jsonl')
            }
        }
        function Get-CombinedAttachedTabCount {
            @((Get-WtWindows -App $script:app) | Where-Object {
                [string]$_.window_id -eq [string]$script:app.WindowId
            })[0].tab_count
        }
        function Save-CombinedActionEvidence {
            param([string]$Phase, [switch]$Screenshot)
            $focused = [Windows.Automation.AutomationElement]::FocusedElement
            @{
                phase = $Phase; at = [DateTimeOffset]::UtcNow.ToString('o')
                owned_sessions = @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -like "$script:marker-*" -or $_.session_id -like 'chat-fixture-*'
                })
                history_rows = @(Get-CombinedRows HistoryList | ForEach-Object { Get-CombinedRowText $_ })
                upper_rows = @(Get-CombinedRows ItemsList | ForEach-Object { Get-CombinedRowText $_ })
                attached_tabs = Get-CombinedAttachedTabCount
                search = Get-UiValue -App $script:app -Selector SearchTextBox
                header = (Get-CombinedElement VerticalTabsHeader).Current.Name
                active_pane = Get-ActivePane -App $script:app
                helpers = @(Get-AgentPaneSessions -App $script:app)
                focus = if ($focused) { @{
                    automation_id = $focused.Current.AutomationId; class = $focused.Current.ClassName
                    process_id = $focused.Current.ProcessId; name = $focused.Current.Name
                } }
            } | ConvertTo-Json -Depth 12 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.json")
            Get-UiTree -App $script:app -Selector HistoryList -Depth 6 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.tree.txt")
            if ($Screenshot) {
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "$Phase.png") | Out-Null
            }
        }
        function Invoke-CombinedHistoryRow {
            $rows = @(Get-CombinedRows HistoryList)
            $rows.Count | Should -Be 1 -Because 'a history action must have exactly one filtered target'
            $bounds = $rows[0].Current.BoundingRectangle
            $rows[0].Current.IsOffscreen | Should -BeFalse
            $bounds.Height | Should -BeGreaterThan 0
            $x = [int]($bounds.X + $bounds.Width / 2)
            $y = [int]($bounds.Y + $bounds.Height / 2)
            Invoke-UiMouseDrag -App $script:app -FromX $x -FromY $y -ToX $x -ToY $y -HoldMs 50 | Out-Null
        }
        function Invoke-CombinedTabContext {
            param([string]$Title)
            $tree = Get-UiTree -App $script:app -Selector ItemsList -Depth 8
            $pattern = '(?m)^\s*(?<Selector>lbl-textview-\S+|TextView) Text "' + [regex]::Escape($Title) + '"'
            $matches = @([regex]::Matches($tree, $pattern))
            $matches.Count | Should -Be 1 -Because 'context actions must target exactly one Sidebar title, not terminal text'
            Invoke-UiClick -App $script:app -Selector $matches[0].Groups['Selector'].Value -Right | Out-Null
        }
        function Invoke-CombinedNativeHook {
            param($Tab, [string]$SessionId, [string]$Cwd, [string]$Status, [switch]$ToolOnly)
            New-Item -ItemType Directory -Path $Cwd -Force | Out-Null
            $path = Join-Path $script:evidence ("hook-$SessionId.json")
            @{ session_id = $SessionId; cwd = $Cwd; tool_name = 'edit' } |
                ConvertTo-Json -Compress | Set-Content -LiteralPath $path -Encoding utf8
            $events = @()
            if (-not $ToolOnly) { $events += 'agent.session.start' }
            if ($Status -eq 'Working') { $events += 'agent.tool.starting' }
            foreach ($event in $events) {
                @{
                    at = [DateTimeOffset]::UtcNow.ToString('o'); event = $event
                    session_id = $SessionId; shell_pane_id = $Tab.session_id; provider = 'copilot'
                } | ConvertTo-Json -Compress |
                    Add-Content -LiteralPath (Join-Path $script:evidence 'hook-sequence.jsonl')
                $command = "Get-Content -Raw -LiteralPath '$($path.Replace("'", "''"))' | & '$($script:app.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event $event"
                Invoke-RunCommand -App $script:app -SessionId $Tab.session_id -Command $command -SettleSec 5 | Out-Null
            }
        }
        $script:ownsConfig = $true
        $script:app = Start-Terminal -Package Dev -PassFre $true -TimeoutSec 60 -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; tabLayoutVerticalWidth = 320
            startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            acpAgent = 'custom:combined-sidebar-fixture'; acpCustomCommand = $command; acpModel = ''
            autoFixEnabled = $false; actions = @(); keybindings = @(); 'warning.confirmOnClose' = 'never'
        }
        $script:app.Launched | Should -BeTrue -Because 'only a test-owned Dev window may be driven'
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true
        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue -Because 'desktop ownership is required'
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        (Get-AgentPaneSession -App $script:app).AcpSessionId | Should -Match '^chat-fixture-'
        Set-CombinedView $true
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Set-CombinedView $false
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'startup-header.png') | Out-Null
        $survival = [Diagnostics.Stopwatch]::StartNew()
        while ($survival.Elapsed.TotalSeconds -lt 30) {
            Get-Process -Id $script:app.Pid -ErrorAction Stop | Out-Null
            Start-Sleep -Milliseconds 500
        }
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        @{
            pid = $script:app.Pid; hwnd = $script:app.Hwnd; observed_seconds = $survival.Elapsed.TotalSeconds
            header = 'Tabs'; agents_round_trip = $true
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'startup-survival.json')
        $script:tabs = @(foreach ($i in 0..7) {
            $existingPaneIds = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
            $tab = New-WtTab -App $script:app -Title "$script:marker-open-$('{0:D2}' -f $i)" -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            $pane = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $existingPaneIds -TimeoutSec 40
            Wait-AgentReady -App $script:app -PaneSessionId $pane.PaneSessionId -TimeoutSec 40 | Should -BeTrue
            $pane.AcpSessionId | Should -Match '^chat-fixture-'
            $tab | Add-Member -NotePropertyName FixtureSession -NotePropertyValue $pane
            $tab
        })
        $script:pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
        Wait-Until -TimeoutSec 90 -Because 'the real master imports deterministic ACP history' -Condition {
            $snapshot = Invoke-Wta -App $script:app -TimeoutSec 10 -Arguments @(
                'sessions', 'list', '--master', $script:pipe, '--json', '--include-status')
            @($snapshot.sessions | Where-Object session_id -eq $script:history[0].sessionId).Count -eq 1
        } | Out-Null
        @{
            package = $script:app.Package; version = $script:app.Version; pid = $script:app.Pid
            install_location = $script:app.InstallLocation; source_commit = $env:ITE2E_SOURCE_COMMIT
            app_sha256 = $appHash; wta_sha256 = $wtaHash
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json')
    }

    BeforeEach {
        Set-CombinedView $true
        $searchToggle = (Get-CombinedElement SearchTabsButton).GetCurrentPattern(
            [Windows.Automation.TogglePattern]::Pattern)
        if ($searchToggle.Current.ToggleState -eq [Windows.Automation.ToggleState]::On) {
            Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        }
        Assert-CombinedSearchState $false
        foreach ($id in @('ItemsList', 'HistoryList')) {
            (Get-CombinedScroll $id).SetScrollPercent(
                [Windows.Automation.ScrollPattern]::NoScroll, 0)
        }
    }

    AfterEach {
        if ($script:heldPromptMarker) {
            $held = '|held|' + $script:heldPromptMarker
            $released = '|released|' + $script:heldPromptMarker
            $log = Get-Content -LiteralPath $script:fixtureLog -Raw
            if ($log.Contains($held) -and -not $log.Contains($released)) {
                [IO.File]::WriteAllText($script:releasePromptPath, 'release')
                Wait-Until -TimeoutSec 10 -Because 'an aborted case releases only its owned held fixture turn' -Condition {
                    (Get-Content -LiteralPath $script:fixtureLog -Raw).Contains($released)
                } | Out-Null
            }
            $script:heldPromptMarker = $null
        }
    }

    AfterAll {
        $cleanupError = $null
        try {
            if ($script:app) {
                $before = @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables)
                $started = [DateTimeOffset]::UtcNow.ToString('o')
                try {
                    Stop-Terminal -App $script:app -RestoreSettings $false
                    if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                        $recovery = Invoke-CombinedHeadlessRecovery -App $script:app -Target $script:target `
                            -InitiallyInactive ([bool]($script:initialProcessCheckAt -and $script:initialProcesses.Count -eq 0))
                        $recovery | ConvertTo-Json |
                            Set-Content -LiteralPath (Join-Path $script:evidence 'headless-recovery.json')
                    }
                }
                catch { $stopError = $_; throw }
                finally {
                    try {
                        @{
                            phase = 'owned-terminal-stop'; owned_pid = $script:app.Pid
                            started_at = $started; finished_at = [DateTimeOffset]::UtcNow.ToString('o')
                            process_ids_before = @($before.Id)
                            processes_after = @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables |
                                Select-Object Id, Path, StartTime)
                        } | ConvertTo-Json -Depth 4 -Compress |
                            Add-Content -LiteralPath (Join-Path $script:evidence 'process-observations.jsonl')
                    }
                    catch {
                        Write-ItLog -Level ERROR -Message "Combined stop observation failed: $_"
                        if (-not $stopError) { throw }
                    }
                }
            }
        }
        catch { $cleanupError = $_; throw }
        finally {
            try {
                if ($script:ownsConfig) {
                    if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                        throw 'Dev remains active; retaining backups instead of touching unowned processes or racing configuration writes.'
                    }
                    Restore-WtConfig -App $script:target
                    foreach ($path in $script:originalHashes.Keys) {
                        $hash = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                        $hash | Should -Be $script:originalHashes[$path] -Because 'restore original configuration bytes'
                    }
                }
            }
            catch {
                Write-ItLog -Level ERROR -Message "Combined suite restoration failed; backups retained: $_"
                if (-not $cleanupError) { throw }
            }
        }
    }

    It 'Combined sidebar header switches Tabs and Agents' {
        Assert-CombinedSearchState $false
        Set-CombinedView $false
        Assert-CombinedSearchState $false
        Assert-CombinedHeaderCue Tabs
        Set-CombinedView $true
        Assert-CombinedHeaderCue Agents
        Assert-CombinedSearchState $false
        Assert-CombinedBounds
        foreach ($id in @('TabHistoryButton', 'HistoryCloseButton', 'HistorySearchTextBox')) {
            Get-CombinedElement $id | Should -BeNullOrEmpty -Because 'retired independent history controls must not remain'
        }
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        @((Get-CombinedRows ItemsList) | Where-Object {
            (Get-CombinedRowText $_).Contains("$script:marker-open-")
        }).Count | Should -BeGreaterThan 0
        @((Get-CombinedRows HistoryList) | Where-Object {
            (Get-CombinedRowText $_).Contains("$script:marker-history-")
        }).Count | Should -BeGreaterThan 0
        Set-CombinedView $false
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Assert-CombinedSearchState $false
        Set-CombinedView $true
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Assert-CombinedSearchState $false
        Assert-CombinedBounds
    }

    It 'History metadata shows time useful status and trailing agent icon' {
        $history = $script:history[0]
        Set-CombinedQuery $history.title
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows HistoryList).Count -eq 1 } | Out-Null
        $historical = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $history.sessionId)
        $historical.Count | Should -Be 1
        $historical[0].status | Should -BeIn @('Historical', 'Ended')
        Assert-CombinedHistoryMetadata -Title $history.title -Provider 'custom:combined-sidebar-fixture'
        foreach ($query in @('custom:combined-sidebar-fixture', 'combined-sidebar-fixture')) {
            Set-CombinedQuery $query
            Wait-Until -TimeoutSec 10 -Because 'icon-only history still matches canonical and display provider aliases' -Condition {
                @((Get-CombinedRows HistoryList) | Where-Object {
                    (Get-CombinedRowText $_).Contains($history.title)
                }).Count -eq 1
            } | Out-Null
        }
        foreach ($status in @('Idle', 'Working')) {
            $index = if ($status -eq 'Idle') { 6 } else { 7 }
            $tab = $script:tabs[$index]
            $baseline = Get-AgentPaneSession -App $script:app -PaneSessionId $tab.FixtureSession.PaneSessionId
            $shellPid = (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid
            $nativeId = "$script:marker-metadata-$status"
            $title = "$script:marker-metadata-title-$status"
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
            if ($status -eq 'Working') {
                Remove-Item -LiteralPath $script:releasePromptPath -ErrorAction SilentlyContinue
                $holdMarker = 'SCROLL_TURN_00_' + [guid]::NewGuid().ToString('N')
                $script:heldPromptMarker = $holdMarker
                Send-AgentPrompt -App $script:app -PaneSessionId $baseline.PaneSessionId -Text "$holdMarker HOLD_FOR_RELEASE" | Out-Null
                Assert-AgentPaneText -App $script:app -PaneSessionId $baseline.PaneSessionId -Pattern "PENDING_$holdMarker" -TimeoutSec 15
            }
            Invoke-CombinedNativeHook -Tab $tab -SessionId $nativeId -Cwd (Join-Path $script:evidence $title) -Status $status
            Wait-Until -TimeoutSec 15 -Because 'the native root is bound before detaching its real tab' -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $nativeId -and $_.provider_id -eq 'copilot' -and $_.status -eq $status -and
                        ([string]$_.pane_session_id).Trim('{}') -eq ([string]$tab.session_id).Trim('{}')
                }).Count -eq 1
            } | Out-Null
            Set-CombinedView $false
            Set-CombinedQuery ''
            $count = Get-CombinedAttachedTabCount
            try {
                Invoke-CombinedTabContext "$script:marker-open-$('{0:D2}' -f $index)"
                Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
                Invoke-CombinedTabContext "$script:marker-open-$('{0:D2}' -f $index)"
                Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
                Wait-Until -TimeoutSec 10 -Condition { (Get-CombinedAttachedTabCount) -eq $count - 1 } | Out-Null
                Set-CombinedView $true
                Set-CombinedQuery $title
                Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows HistoryList).Count -eq 1 } | Out-Null
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $nativeId -and $_.provider_id -eq 'copilot' -and $_.status -eq $status
                }).Count | Should -Be 1 -Because 'metadata assertions require the actual unattached live status'
                Assert-CombinedHistoryMetadata -Title $title -Status $status -Provider Copilot
                Set-CombinedQuery copilot
                Wait-Until -TimeoutSec 10 -Because 'live provider search still finds the detached identity' -Condition {
                    @((Get-CombinedRows HistoryList) | Where-Object {
                        (Get-CombinedRowText $_).Contains($title)
                    }).Count -eq 1
                } | Out-Null
            }
            finally {
                Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
                Wait-Until -TimeoutSec 20 -Condition { (Get-CombinedAttachedTabCount) -eq $count } | Out-Null
                if ($status -eq 'Working') {
                    [IO.File]::WriteAllText($script:releasePromptPath, 'release')
                    Assert-AgentPaneText -App $script:app -PaneSessionId $baseline.PaneSessionId -Pattern "ACK_$holdMarker" -TimeoutSec 15
                }
            }
            (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid | Should -Be $shellPid
            $current = Get-AgentPaneSession -App $script:app -PaneSessionId $baseline.PaneSessionId
            $current.AcpSessionId | Should -Be $baseline.AcpSessionId
            $current.HelperProcessId | Should -Be $baseline.HelperProcessId
        }
    }

    It 'History shows live status and ownership across windows' {
        $sourceApp = $script:app
        $sourceWindow = [string]$sourceApp.WindowId
        $sourceHwnds = @(Get-WtWindowHwnds -App $sourceApp | Where-Object pid -eq $sourceApp.Pid).hwnd
        $windowsBefore = @(Get-WtWindows -App $sourceApp).window_id
        $folder = Join-Path $script:evidence "$script:marker-other-window-native"
        New-Item -ItemType Directory -Path $folder | Out-Null
        $shim = Join-Path $folder 'copilot.exe'
        $launchLog = Join-Path $folder 'launch.jsonl'
        $sid = [guid]::NewGuid().ToString()
        $tab = $null
        $nativeProcess = $null
        $oldActions = (Get-WtSettingsObject -App $sourceApp).actions
        function Invoke-C388Move {
            param($App, [string]$Action)
            Send-WtWindowKey -App $App -Vk 0x50 -Ctrl -Shift -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 8 -Condition { Test-CommandPaletteOpen -App $App } | Out-Null
            Set-UiValue -App $App -Selector '_searchBox' -Value $Action | Out-Null
            $result = & (Get-Module ItE2E) {
                param($Target, $Name)
                Invoke-WinAppUi -App $Target -UiArgs @('invoke', $Name)
            } $App $Action
            $result.ExitCode | Should -Be 0
        }
        function Send-C388Hook {
            param([string]$Event)
            $json = @{ session_id = $sid; cwd = $folder; tool_name = 'edit' } | ConvertTo-Json -Compress
            $code = "'$($json.Replace("'", "''"))' | & '$($sourceApp.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event $Event; exit `$LASTEXITCODE"
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
            $result = Invoke-Native -FilePath (Get-Command pwsh.exe).Source -Arguments @('-NoProfile', '-EncodedCommand', $encoded) `
                -Environment @{ WT_SESSION = $tab.session_id; WT_COM_CLSID = $sourceApp.ComClsid } -TimeoutSec 10
            $result.ExitCode | Should -Be 0
        }
        try {
            # The same native, no-quota fixture used by AgentsModeActions runs inside the real pane.
            $pwsh = (Get-Command pwsh.exe).Source
            $config = @{
                ITE2E_SHIM_PWSH = $pwsh
                ITE2E_SHIM_FIXTURE = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-InteractiveDelegate.ps1')).Path
                ITE2E_SHIM_LOG = $launchLog; ITE2E_SHIM_RUN = $sid; ITE2E_SHIM_WTCLI = $sourceApp.WtcliPath
            }
            $header = Join-Path $folder 'config.h'
            @($config.Keys | ForEach-Object { "#define $_ LR`"ite2e($($config[$_]))ite2e`"" }) |
                Set-Content -LiteralPath $header -Encoding ascii
            $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
            $vs = Invoke-Native -FilePath $vswhere -Arguments @('-latest', '-products', '*',
                '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath')
            $vs.ExitCode | Should -Be 0
            $vcvars = Join-Path $vs.StdOut.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
            $nativeSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-CopilotDelegate.cpp')).Path
            $build = "call `"$vcvars`" >nul && cl /nologo /EHsc /std:c++17 /FI`"$header`" `"$nativeSource`" /Fe:`"$shim`" /Fo:`"$folder\copilot.obj`" /link /INCREMENTAL:NO"
            $buildScript = "& `$env:ComSpec /d /c '$($build.Replace("'", "''"))'; exit `$LASTEXITCODE"
            $buildEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($buildScript))
            (Invoke-Native -FilePath $pwsh -Arguments @('-NoProfile', '-EncodedCommand', $buildEncoded) `
                -WorkingDirectory $folder -TimeoutSec 60).ExitCode | Should -Be 0
            Set-CombinedView $false
            $tab = New-WtTab -App $sourceApp -Command "`"$shim`" --session-id $sid" -Cwd $folder -Title "$script:marker-other-window"
            Wait-Until -TimeoutSec 20 -Condition { Test-Path -LiteralPath $launchLog } | Out-Null
            $launches = @(Get-Content -LiteralPath $launchLog | ForEach-Object { $_ | ConvertFrom-Json })
            $launches.Count | Should -Be 1
            $launches[0].session_id | Should -Be $sid
            $launches[0].pane_session_id | Should -Be $tab.session_id
            $nativeProcess = Get-Process -Id $launches[0].native_pid -ErrorAction Stop
            $nativeProcess.Path | Should -Be $shim
            Wait-Until -TimeoutSec 20 -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object { $_.session_id -eq $sid -and $_.owner_window_id }).Count -eq 1
            } | Out-Null
            $sourceOwner = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)[0].owner_window_id
            [uint64]$sourceOwner | Should -BeGreaterThan 0
            [string]$sourceOwner | Should -Be $sourceWindow
            Set-WtSetting -App $sourceApp -Key actions -Value (@($oldActions) + @(
                @{ name = 'ITE2E C388 return owner'; command = @{ action = 'moveTab'; window = $sourceWindow } }
            )) | Out-Null
            Set-WtPaneFocus -App $sourceApp -SessionId $tab.session_id
            Invoke-C388Move -App $sourceApp -Action 'Move tab to a new window'
            $foreignWindow = Wait-Until -TimeoutSec 20 -Condition {
                $created = @(Get-WtWindows -App $sourceApp | Where-Object window_id -NotIn $windowsBefore)
                if ($created.Count -eq 1) { [string]$created[0].window_id }
            }
            $foreignHwnd = Wait-Until -TimeoutSec 15 -Condition {
                $created = @(Get-WtWindowHwnds -App $sourceApp | Where-Object {
                    $_.pid -eq $sourceApp.Pid -and $_.hwnd -notin $sourceHwnds
                })
                if ($created.Count -eq 1) { $created[0].hwnd }
            }
            $foreignApp = $sourceApp.PSObject.Copy()
            $foreignApp.WindowId = $foreignWindow
            $foreignApp.Hwnd = $foreignHwnd
            $context = Invoke-WtCli -App $foreignApp -Arguments @('get-pane-context', '--target', $tab.session_id)
            [string]$context.pane.session_id | Should -Be $tab.session_id
            [string]$context.pane.window_id | Should -Be $foreignWindow
            Wait-Until -TimeoutSec 20 -Condition {
                $row = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)
                $row.Count -eq 1 -and $row[0].owner_window_id -and $row[0].owner_window_id -ne $sourceOwner
            } | Out-Null
            $foreignOwner = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)[0].owner_window_id
            [string]$foreignOwner | Should -Be $foreignWindow
            foreach ($state in @(@{ Status = 'Working'; Event = 'agent.tool.starting' },
                @{ Status = 'Idle'; Event = 'agent.session.start' })) {
                Send-C388Hook $state.Event
                Wait-Until -TimeoutSec 20 -Condition {
                    @((Get-CombinedSnapshot).sessions | Where-Object {
                        $_.session_id -eq $sid -and $_.status -eq $state.Status -and $_.owner_window_id -eq $foreignOwner
                    }).Count -eq 1
                } | Out-Null
                $script:app = $sourceApp
                Set-CombinedView $true
                Set-CombinedQuery (Split-Path $folder -Leaf)
                Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows HistoryList).Count -eq 1 } | Out-Null
                Assert-CombinedHistoryMetadata -Title (Split-Path $folder -Leaf) -Status $state.Status -Provider Copilot -OtherWindow
                Save-CombinedActionEvidence "other-window-$($state.Status)" -Screenshot
                @{
                    session_id = $sid; pane_session_id = $tab.session_id
                    source_window_id = $sourceWindow; owner_window_id = $foreignWindow
                    owner_context = $context
                    session = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)
                } | ConvertTo-Json -Depth 10 |
                    Set-Content -LiteralPath (Join-Path $script:evidence "other-window-$($state.Status)-owner.json")
                $script:app = $foreignApp
                Set-CombinedView $true
                Set-CombinedQuery (Split-Path $folder -Leaf)
                try {
                    Wait-Until -TimeoutSec 15 -Because 'the owner view finishes excluding its represented identity' -Condition {
                        @(Get-CombinedRows HistoryList).Count -eq 0
                    } | Out-Null
                }
                finally {
                    Save-CombinedActionEvidence "owner-window-$($state.Status)" -Screenshot
                }
                @(Get-CombinedRows HistoryList).Count | Should -Be 0 -Because 'the owner window excludes its represented session'
            }
            $script:app = $sourceApp
            $windowCount = @(Get-WtWindows -App $sourceApp).Count
            $tabCount = @(Get-WtWindows -App $sourceApp | ForEach-Object { Get-WtTabs -App $sourceApp -WindowId $_.window_id }).Count
            Invoke-CombinedHistoryRow
            Wait-Until -TimeoutSec 15 -Condition {
                $active = Get-ActivePane -App $sourceApp
                $active.session_id -eq $tab.session_id -and [string]$active.window_id -eq $foreignWindow
            } | Out-Null
            @(Get-WtWindows -App $sourceApp).Count | Should -Be $windowCount
            @(Get-WtWindows -App $sourceApp | ForEach-Object { Get-WtTabs -App $sourceApp -WindowId $_.window_id }).Count | Should -Be $tabCount
            @(Get-Content -LiteralPath $launchLog).Count | Should -Be 1
            $nativeProcess.HasExited | Should -BeFalse
            $script:app = $foreignApp
            Invoke-C388Move -App $foreignApp -Action 'ITE2E C388 return owner'
            $script:app = $sourceApp
            Wait-Until -TimeoutSec 20 -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $sid -and $_.owner_window_id -eq $sourceOwner
                }).Count -eq 1 -and @(Get-WtWindows -App $sourceApp).window_id -notcontains $foreignWindow
            } | Out-Null
            Set-CombinedView $false
            Set-CombinedQuery ''
            Invoke-CombinedTabContext "$script:marker-other-window"
            Invoke-UiElement -App $sourceApp -Selector KeepTabRunningMenuItem | Out-Null
            Invoke-CombinedTabContext "$script:marker-other-window"
            Invoke-UiElement -App $sourceApp -Selector 'Close tab' | Out-Null
            Set-CombinedView $true
            Set-CombinedQuery (Split-Path $folder -Leaf)
            Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows HistoryList).Count -eq 1 } | Out-Null
            Assert-CombinedHistoryMetadata -Title (Split-Path $folder -Leaf) -Status Idle -Provider Copilot
            (Get-CombinedRowText (Get-CombinedRows HistoryList)[0]) | Should -Not -Match 'another window'
            Invoke-CombinedHistoryRow
            Set-WtPaneFocus -App $sourceApp -SessionId $tab.session_id
            Send-WtInput -App $sourceApp -SessionId $tab.session_id -Text "exit`n"
            Wait-Until -TimeoutSec 20 -Condition { $nativeProcess.HasExited } | Out-Null
            Wait-Until -TimeoutSec 30 -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $sid -and $_.status -in @('Ended', 'Historical') -and -not $_.owner_window_id
                }).Count -eq 1
            } | Out-Null
        }
        finally {
            $script:app = $sourceApp
            if ($tab -and $nativeProcess -and -not $nativeProcess.HasExited) {
                Close-WtPane -App $sourceApp -SessionId $tab.session_id
                Wait-Until -TimeoutSec 15 -Condition { $nativeProcess.HasExited } | Out-Null
            }
            Set-WtSetting -App $sourceApp -Key actions -Value @($oldActions) | Out-Null
            if (-not $nativeProcess -or $nativeProcess.HasExited) {
                foreach ($name in @('copilot.exe', 'copilot.obj')) {
                    $path = Join-Path $folder $name
                    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
                }
            }
        }
    }

    It 'Combined sidebar search filters both sections and preserves the query' {
        Assert-CombinedSearchState $false
        Open-CombinedSearch
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Set-CombinedQuery "$script:marker-open-00"
        Wait-Until -TimeoutSec 10 -Because 'one open row and no unrelated historical rows remain' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 1 -and @(Get-CombinedRows HistoryList).Count -eq 0
        } | Out-Null
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Set-CombinedView $false
        Assert-CombinedSearchState $true
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be "$script:marker-open-00"
        Set-CombinedView $true
        Assert-CombinedSearchState $true
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be "$script:marker-open-00"
        Set-CombinedQuery "$script:marker-history-00"
        Wait-Until -TimeoutSec 10 -Because 'history-only search leaves Agents active' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 0 -and @(Get-CombinedRows HistoryList).Count -eq 1
        } | Out-Null
        foreach ($close in @($false, $true)) {
            if ($close) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
                $closeClock = [Diagnostics.Stopwatch]::StartNew()
                $closeSamples = @(foreach ($seconds in @(0.25, 1.0)) {
                    $remaining = $seconds - $closeClock.Elapsed.TotalSeconds
                    if ($remaining -gt 0) { Start-Sleep -Milliseconds ([int]($remaining * 1000)) }
                    $box = Get-CombinedElement SearchTextBox
                    $toggle = (Get-CombinedElement SearchTabsButton).GetCurrentPattern(
                        [Windows.Automation.TogglePattern]::Pattern)
                    $value = if ($box) { $box.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern) } else { $null }
                    @{
                        elapsed_seconds = $closeClock.Elapsed.TotalSeconds
                        toggle = $toggle.Current.ToggleState.ToString()
                        textbox_present = [bool]$box
                        query = if ($value) { $value.Current.Value } else { $null }
                        offscreen = if ($box) { $box.Current.IsOffscreen } else { $null }
                        height = if ($box) { $box.Current.BoundingRectangle.Height } else { $null }
                        upper_count = @(Get-CombinedRows ItemsList).Count
                        history_count = @(Get-CombinedRows HistoryList).Count
                    }
                })
                $closeSamples | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'search-close-samples.json')
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'search-close-after-1s.png') | Out-Null
                Get-UiTree -App $script:app -Depth 8 |
                    Set-Content -LiteralPath (Join-Path $script:evidence 'search-close-after-1s.tree.txt')
                Wait-Until -TimeoutSec 5 -Because 'search close animation settles with its textbox hidden' -Condition {
                    $box = Get-CombinedElement SearchTextBox
                    -not ($box -and -not $box.Current.IsOffscreen -and $box.Current.BoundingRectangle.Height -gt 0)
                } | Out-Null
                Assert-CombinedSearchState $false
            } else {
                Set-CombinedQuery ''
            }
            Wait-Until -TimeoutSec 10 -Because 'clear or close restores both real lists' -Condition {
                @(Get-CombinedRows ItemsList).Count -gt 1 -and @(Get-CombinedRows HistoryList).Count -gt 1
            } | Out-Null
            (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
            if (-not $close) {
                Set-CombinedQuery 'no-match-combined-sidebar'
                Wait-Until -TimeoutSec 10 -Condition {
                    @(Get-CombinedRows ItemsList).Count -eq 0 -and @(Get-CombinedRows HistoryList).Count -eq 0
                } | Out-Null
            }
        }
        Set-CombinedView $false
        Assert-CombinedSearchState $false
        Set-CombinedView $true
        Assert-CombinedSearchState $false
    }

    It 'Combined sidebar sections scroll independently' {
        $upper = Get-CombinedScroll ItemsList
        $lower = Get-CombinedScroll HistoryList
        $upper.Current.VerticallyScrollable | Should -BeTrue
        $lower.Current.VerticallyScrollable | Should -BeTrue
        $lowerBefore = $lower.Current.VerticalScrollPercent
        $upper.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 75)
        Wait-Until -TimeoutSec 5 -Condition { $upper.Current.VerticalScrollPercent -gt 50 } | Out-Null
        $lower.Current.VerticalScrollPercent | Should -Be $lowerBefore
        $upperBefore = $upper.Current.VerticalScrollPercent
        $lower.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 75)
        Wait-Until -TimeoutSec 5 -Condition { $lower.Current.VerticalScrollPercent -gt 50 } | Out-Null
        $upper.Current.VerticalScrollPercent | Should -Be $upperBefore
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'independent-scroll.png') | Out-Null
    }

    It 'Combined sidebar divider supports pointer and keyboard resizing' {
        $divider = Get-CombinedElement HistorySplitter
        $before = $divider.Current.BoundingRectangle
        Invoke-UiMouseDrag -App $script:app -FromX ([int]($before.X + $before.Width / 2)) -FromY ([int]($before.Y + 4)) `
            -ToX ([int]($before.X + $before.Width / 2)) -ToY ([int]($before.Y + 44)) -HoldMs 150 | Out-Null
        Wait-Until -TimeoutSec 5 -Condition {
            (Get-CombinedElement HistorySplitter).Current.BoundingRectangle.Y -gt $before.Y + 10
        } | Out-Null
        Assert-CombinedBounds
        $divider.SetFocus()
        $divider.Current.HasKeyboardFocus | Should -BeTrue
        $beforeKey = $divider.Current.BoundingRectangle.Y
        Send-WtWindowKey -App $script:app -Vk 0x26 -RequireForeground | Out-Null
        Wait-Until -TimeoutSec 5 -Condition { $divider.Current.BoundingRectangle.Y -lt $beforeKey - 5 } | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
        Wait-Until -TimeoutSec 5 -Condition {
            [Math]::Abs($divider.Current.BoundingRectangle.Y - $beforeKey) -lt 3
        } | Out-Null
        Assert-CombinedBounds
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'divider-resized.png') | Out-Null
    }

    It 'Combined sidebar keeps both sections usable after window resizing' {
        $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
        $transform = $window.GetCurrentPattern([Windows.Automation.TransformPattern]::Pattern)
        $transform.Current.CanResize | Should -BeTrue
        $original = $window.Current.BoundingRectangle
        try {
            $transform.Resize($original.Width, 640)
            Wait-Until -TimeoutSec 10 -Because 'the actual window shrinks' -Condition {
                $window.Current.BoundingRectangle.Height -lt $original.Height - 20 -and
                    (Get-CombinedElement HistoryList).Current.BoundingRectangle.Height -gt 20
            } | Out-Null
            Assert-CombinedBounds
            $small = (Get-CombinedElement HistorySplitter).Current.BoundingRectangle.Y
            $transform.Resize($original.Width, [Math]::Max(760, $original.Height))
            Wait-Until -TimeoutSec 10 -Because 'the actual window grows and lays out both sections' -Condition {
                $window.Current.BoundingRectangle.Height -ge 740 -and
                    (Get-CombinedElement HistorySplitter).Current.BoundingRectangle.Y -gt $small
            } | Out-Null
            Assert-CombinedBounds
        }
        finally {
            $transform.Resize($original.Width, $original.Height)
            Wait-Until -TimeoutSec 10 -Condition {
                (Get-CombinedElement HistoryList).Current.BoundingRectangle.Height -gt 20
            } | Out-Null
        }
    }

    It 'Combined sidebar excludes represented Idle sessions by identity' {
        $session = $script:tabs[0].FixtureSession
        $nativeId = "$script:marker-represented-idle"
        Invoke-CombinedNativeHook -Tab $script:tabs[0] -SessionId $nativeId `
            -Cwd (Join-Path $script:evidence "$script:marker-represented") -Status Idle
        Wait-Until -TimeoutSec 15 -Because 'the actual connected ACP session is Idle in master state' -Condition {
            @((Get-CombinedSnapshot).sessions | Where-Object {
                $_.session_id -eq $session.AcpSessionId -and $_.status -eq 'Idle'
            }).Count -eq 1
        } | Out-Null
        Set-CombinedQuery "$script:marker-open-00"
        Wait-Until -TimeoutSec 10 -Because 'the unique represented tab is rendered in the upper list' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 1
        } | Out-Null
        (Get-CombinedRowText (Get-CombinedRows ItemsList)[0]) |
            Should -Match ([regex]::Escape("$script:marker-open-00"))
        $current = Get-AgentPaneSession -App $script:app -PaneSessionId $session.PaneSessionId
        $current.AcpSessionId | Should -Be $session.AcpSessionId
        $current.HelperProcessId | Should -Be $session.HelperProcessId
        Set-CombinedQuery "$script:marker-represented"
        Wait-Until -TimeoutSec 10 -Because 'open Idle agents remain above, not duplicated into History' -Condition {
            @(Get-CombinedRows HistoryList).Count -eq 0 -and
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $nativeId -and $_.provider_id -eq 'copilot' -and $_.status -eq 'Idle'
                }).Count -eq 1
        } | Out-Null
        $script:history[1].title = "$script:marker-open-00"
        @{ sessions = $script:history } | ConvertTo-Json -Depth 6 |
            Set-Content -LiteralPath $script:historyPath -Encoding utf8
        Wait-Until -TimeoutSec 20 -Because 'master imports an unrelated history identity with the same title as the open tab' -Condition {
            @((Get-CombinedSnapshot).sessions | Where-Object {
                $_.session_id -eq $script:history[1].sessionId -and $_.title -eq "$script:marker-open-00"
            }).Count -eq 1
        } | Out-Null
        Set-CombinedQuery "$script:marker-open-00"
        Wait-Until -TimeoutSec 15 -Because 'same-title different-identity history remains alongside the unique represented upper row' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 1 -and @(Get-CombinedRows HistoryList).Count -eq 1
        } | Out-Null
        Set-CombinedQuery "$script:marker-history-00"
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows HistoryList).Count -eq 1 } | Out-Null
        (Get-CombinedRowText (Get-CombinedRows HistoryList)[0]) |
            Should -Match ([regex]::Escape("$script:marker-history-00")) -Because 'unrepresented history must not be suppressed'
    }

    It 'Combined sidebar retains unattached Idle and Working sessions (<Status>)' -ForEach (@(
        @{ Status = 'Idle'; Index = 0 }, @{ Status = 'Working'; Index = 1 }
    ) | Where-Object { -not $env:ITE2E_COMBINED_RETENTION_STATUS -or $_.Status -eq $env:ITE2E_COMBINED_RETENTION_STATUS }) {
        $tab = $script:tabs[$Index]
        $session = $tab.FixtureSession
        $nativeId = "$script:marker-kept-$Status"
        $nativeTitle = "$script:marker-native-$Status"
        Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
        $shellPid = (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid
        $marker = 'SCROLL_TURN_00_' + [guid]::NewGuid().ToString('N')
        if ($Status -eq 'Working') {
            Remove-Item -LiteralPath $script:releasePromptPath -ErrorAction SilentlyContinue
            $script:heldPromptMarker = $marker
            Invoke-CombinedNativeHook -Tab $tab -SessionId $nativeId -Cwd (Join-Path $script:evidence $nativeTitle) -Status Idle
            @{
                at = [DateTimeOffset]::UtcNow.ToString('o'); event = 'fixture-prompt'
                session_id = $session.AcpSessionId; helper_pane_id = $session.PaneSessionId; marker = $marker
            } | ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $script:evidence 'hook-sequence.jsonl')
            Send-AgentPrompt -App $script:app -PaneSessionId $session.PaneSessionId -Text "$marker HOLD_FOR_RELEASE" | Out-Null
            Assert-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -Pattern "PENDING_$marker" -TimeoutSec 15
        }
        Invoke-CombinedNativeHook -Tab $tab -SessionId $nativeId -Cwd (Join-Path $script:evidence $nativeTitle) `
            -Status $Status -ToolOnly:($Status -eq 'Working')
        Wait-Until -TimeoutSec 15 -Because "master confirms $Status before detachment" -Condition {
            @((Get-CombinedSnapshot).sessions | Where-Object {
                $_.session_id -eq $nativeId -and $_.provider_id -eq 'copilot' -and $_.status -eq $Status
            }).Count -eq 1
        } | Out-Null
        Set-CombinedView $true
        Set-CombinedQuery $nativeTitle
        Wait-Until -TimeoutSec 15 -Because 'the explicitly started root identity is represented before detachment' -Condition {
            @(Get-CombinedRows HistoryList).Count -eq 0
        } | Out-Null
        Save-CombinedActionEvidence "before-detach-$Status"
        Set-CombinedView $false
        Set-CombinedQuery ''
        $title = "$script:marker-open-$('{0:D2}' -f $Index)"
        $beforeCount = Get-CombinedAttachedTabCount
        try {
            Invoke-CombinedTabContext $title
            Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
            Invoke-CombinedTabContext $title
            Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'the real kept tab becomes unattached' -Condition {
                (Get-CombinedAttachedTabCount) -eq $beforeCount - 1
            } | Out-Null
            (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid | Should -Be $shellPid
            Set-CombinedView $true
            Set-CombinedQuery $nativeTitle
            try {
                Wait-Until -TimeoutSec 15 -Because "unattached $Status remains actionable in real History" -Condition {
                    @(Get-CombinedRows HistoryList).Count -eq 1
                } | Out-Null
            }
            catch {
                Save-CombinedActionEvidence "unattached-$Status" -Screenshot
                throw
            }
            @((Get-CombinedSnapshot).sessions | Where-Object {
                $_.session_id -eq $nativeId -and $_.provider_id -eq 'copilot' -and $_.status -eq $Status
            }).Count | Should -Be 1
            Save-CombinedActionEvidence "detached-$Status"
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
            Wait-Until -TimeoutSec 20 -Because 'ordinary focus-pane reattaches the exact original shell tab' -Condition {
                [string](Get-ActivePane -App $script:app).session_id -eq [string]$tab.session_id -and
                    (Get-CombinedAttachedTabCount) -eq $beforeCount
            } | Out-Null
            (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid | Should -Be $shellPid
            $current = Get-AgentPaneSession -App $script:app -PaneSessionId $session.PaneSessionId
            $current.HelperProcessId | Should -Be $session.HelperProcessId
            $current.AcpSessionId | Should -Be $session.AcpSessionId
            Set-CombinedView $true
            Set-CombinedQuery $nativeTitle
            try {
                Wait-Until -TimeoutSec 10 -Because 'the reattached identity is represented above rather than duplicated in History' -Condition {
                    @(Get-CombinedRows HistoryList).Count -eq 0
                } | Out-Null
            }
            catch {
                Save-CombinedActionEvidence "reattached-$Status" -Screenshot
                throw
            }
        }
        finally {
            if ($Status -eq 'Working') {
                [IO.File]::WriteAllText($script:releasePromptPath, 'release')
            }
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
        }
        if ($Status -eq 'Working') {
            Open-AgentPane -App $script:app | Out-Null
            Assert-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -Pattern "ACK_$marker" -TimeoutSec 15
            (Get-Content -LiteralPath $script:fixtureLog -Raw) |
                Should -Not -Match ('\|cancel\|' + [regex]::Escape($session.AcpSessionId))
        }
    }

    It 'Combined sidebar reports unavailable history providers without creating a tab' {
        $history = $script:history[0]
        Set-CombinedQuery $history.title
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows HistoryList).Count -eq 1 } | Out-Null
        $beforeTabs = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId))
        Invoke-CombinedHistoryRow
        Wait-Until -TimeoutSec 30 -Because 'the real history action surfaces its unsupported-provider error' -Condition {
            $message = Get-CombinedElement HistoryMessage
            $message -and -not $message.Current.IsOffscreen -and
                $message.Current.Name -match 'provider is unavailable'
        } | Out-Null
        @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count | Should -Be $beforeTabs.Count
        @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $history.sessionId).Count | Should -Be 1
        @(Get-CombinedRows HistoryList).Count | Should -Be 1 -Because 'an unavailable provider must not consume the session'
        (Get-Content -LiteralPath $script:fixtureLog -Raw) |
            Should -Not -Match ('\|load\|[^|]+\|' + [regex]::Escape($history.sessionId))
    }

    It 'Combined sidebar history action restores a legitimate live session' {
        $tab = $script:tabs[2]
        $sid = 'combined-action-' + [guid]::NewGuid().ToString('N')
        $path = Join-Path $script:evidence 'action-hook.json'
        @{ session_id = $sid; cwd = $script:evidence; tool_name = 'edit' } |
            ConvertTo-Json -Compress | Set-Content -LiteralPath $path -Encoding utf8
        Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
        $command = "Get-Content -Raw -LiteralPath '$($path.Replace("'", "''"))' | & '$($script:app.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event agent.tool.starting"
        Invoke-RunCommand -App $script:app -SessionId $tab.session_id -Command $command -SettleSec 5 | Out-Null
        Wait-Until -TimeoutSec 20 -Because 'a real terminal hook binds the known-provider session to the owned shell' -Condition {
            @((Get-CombinedSnapshot).sessions | Where-Object {
                $_.session_id -eq $sid -and $_.provider_id -eq 'copilot' -and
                    ([string]$_.pane_session_id).Trim('{}') -eq ([string]$tab.session_id).Trim('{}') -and $_.status -eq 'Working'
            }).Count -eq 1
        } | Out-Null
        $shellPid = (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid
        Set-CombinedView $false
        Set-CombinedQuery ''
        $title = "$script:marker-open-02"
        $before = Get-CombinedAttachedTabCount
        try {
            Invoke-CombinedTabContext $title
            Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
            Invoke-CombinedTabContext $title
            Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
            Wait-Until -TimeoutSec 10 -Condition {
                (Get-CombinedAttachedTabCount) -eq $before - 1
            } | Out-Null
            Set-CombinedView $true
            Set-CombinedQuery (Split-Path $script:evidence -Leaf)
            try {
                Wait-Until -TimeoutSec 15 -Because 'the detached known-provider row is available for a legitimate History action' -Condition {
                    @(Get-CombinedRows HistoryList).Count -eq 1
                } | Out-Null
            }
            catch {
                Save-CombinedActionEvidence 'known-provider-action' -Screenshot
                throw
            }
            Invoke-CombinedHistoryRow
            Wait-Until -TimeoutSec 20 -Because 'the actual History click restores the original tab through master and COM' -Condition {
                [string](Get-ActivePane -App $script:app).session_id -eq [string]$tab.session_id -and
                    (Get-CombinedAttachedTabCount) -eq $before
            } | Out-Null
            (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid | Should -Be $shellPid
            $current = Get-AgentPaneSession -App $script:app -PaneSessionId $tab.FixtureSession.PaneSessionId
            $current.AcpSessionId | Should -Be $tab.FixtureSession.AcpSessionId
            $current.HelperProcessId | Should -Be $tab.FixtureSession.HelperProcessId
        }
        finally {
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
        }
    }
}
