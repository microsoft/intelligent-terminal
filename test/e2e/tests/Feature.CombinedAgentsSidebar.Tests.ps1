#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
function Invoke-CombinedCheckedCleanup {
    param($PrimaryFailure, [Parameter(Mandatory)][scriptblock]$Action)
    try { & $Action }
    catch {
        if ($PrimaryFailure) {
            throw [AggregateException]::new('Original test failure and separate cleanup failure.',
                [Exception[]]@($PrimaryFailure.Exception, $_.Exception))
        }
        throw
    }
}
$script:runtimeStatePath = Join-Path $script:target.LocalStateDir 'IntelligentTerminal'
$script:runtimeStateBackup = Join-Path $script:evidence 'original-runtime-state'
$script:runtimeStateExisted = Test-Path -LiteralPath $script:runtimeStatePath
$script:runtimeStateHashes = @{}
if ($script:runtimeStateExisted) {
    foreach ($file in @(Get-ChildItem -LiteralPath $script:runtimeStatePath -File -Recurse -Force)) {
        $relative = [IO.Path]::GetRelativePath($script:runtimeStatePath, $file.FullName)
        $script:runtimeStateHashes[$relative] = (Get-FileHash -LiteralPath $file.FullName).Hash
    }
    Copy-Item -LiteralPath $script:runtimeStatePath -Destination $script:runtimeStateBackup -Recurse -Force
}

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
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TabHeaderContext.ps1')
        Initialize-TestSidebarExpansionEvents
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
            param([ValidateSet('Live', 'Recent')][string]$Kind)
            $list = Get-CombinedElement ItemsList
            if (-not $list -or $list.Current.IsOffscreen) { throw 'Missing visible mixed ItemsList.' }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            @($list.FindAll([Windows.Automation.TreeScope]::Children, $condition) | Where-Object {
                $parts = @(Get-CombinedRawChildren $_)
                $recent = @($parts | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count -gt 0
                $heading = @($parts | Where-Object { $_.Current.AutomationId -eq 'HistoryHeaderButton' }).Count -gt 0
                -not $heading -and ($recent -eq ($Kind -eq 'Recent'))
            })
        }
        function Get-CombinedRowText {
            param($Row)
            @($Row.Current.Name; @($Row.FindAll(
                [Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)) |
                ForEach-Object { $_.Current.Name }) -join ' '
        }
        function Get-CombinedRawChildren {
            param($Element, [switch]$ContentView)
            $walker = if ($ContentView) { [Windows.Automation.TreeWalker]::ContentViewWalker } else { [Windows.Automation.TreeWalker]::RawViewWalker }
            $child = $walker.GetFirstChild($Element)
            while ($child) {
                $child
                Get-CombinedRawChildren $child -ContentView:$ContentView
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
            $label = Get-CombinedElement VerticalTabsHeader
            $label.Current.Name | Should -Be 'Tabs'
            $label.Current.ControlType | Should -Be ([Windows.Automation.ControlType]::Text)
            $label.Current.IsOffscreen | Should -BeFalse
            $label.Current.IsKeyboardFocusable | Should -BeFalse
            foreach ($patternId in @([Windows.Automation.InvokePattern]::Pattern,
                [Windows.Automation.TogglePattern]::Pattern)) {
                $pattern = $null
                $label.TryGetCurrentPattern($patternId, [ref]$pattern) | Should -BeFalse
            }
            Get-CombinedElement VerticalTabsHeaderButton | Should -BeNullOrEmpty
        }
        function Assert-CombinedHistoryMetadata {
            param([string]$Title, [string]$Status, [string]$Provider, [switch]$OtherWindow)
            $metadataPhase = if ($OtherWindow) { "metadata-other-window-$Status" } else { "metadata-$($Status ?? 'Historical')" }
            Save-CombinedActionEvidence $metadataPhase -Screenshot
            $rows = @(Get-CombinedRows Recent)
            $rows.Count | Should -Be 1
            $row = $rows[0]
            $parts = @(Get-CombinedRawChildren $row -ContentView)
            $textLeaves = @($parts | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    -not [string]::IsNullOrEmpty($_.Current.Name) -and
                    -not @(Get-CombinedRawChildren $_ -ContentView | Where-Object {
                        $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text
                    }).Count
            })
            $textLeaves.Count | Should -Be $(if ($Status) { 4 } else { 3 }) -Because 'every named Content-view text leaf counts, including hidden or zero-size semantic peers'
            foreach ($leaf in $textLeaves) {
                $leaf.Current.IsOffscreen | Should -BeFalse
                $leaf.Current.BoundingRectangle.Width | Should -BeGreaterThan 0
                $leaf.Current.BoundingRectangle.Height | Should -BeGreaterThan 0
            }
            # Highlighted text exposes leaf Text peers, not the wrapper's XAML name.
            $titles = @($textLeaves | Where-Object { $_.Current.Name.Contains($Title) })
            $titles.Count | Should -Be 1 -Because 'the history row must expose one unambiguous visible title'
            $titlePart = $titles[0]
            if (-not ('ItSidebarAgeOracle' -as [type])) {
                . (Join-Path $PSScriptRoot '..\fixtures\SidebarRelativeTimeOracle.ps1')
            }
            $beforeQuery = [DateTimeOffset]::UtcNow
            $sources = @((Get-CombinedSnapshot).sessions | Where-Object {
                $_.title -eq $Title -and $_.provider_id -eq $Provider
            })
            $sources.Count | Should -Be 1 -Because 'the displayed fixture row must have one timestamp source'
            $sourceTime = [DateTimeOffset]::FromUnixTimeMilliseconds([long]$sources[0].last_activity_at_ms)
            $afterQuery = [DateTimeOffset]::UtcNow
            $expectedTimes = @(foreach ($capture in @($beforeQuery, $afterQuery)) {
                ($capture - $sourceTime).TotalSeconds | Should -BeLessThan 3600 -Because 'these metadata fixtures exercise recent ages, not the separate six-unit locale matrix'
                if (($capture - $sourceTime).TotalSeconds -lt 60) { 'just now' }
                else { [ItSidebarAgeOracle]::Format('en-US', 'minute', [ItSidebarAgeOracle]::Count('minute', $sourceTime, $capture)) }
            }) | Select-Object -Unique
            $times = @($textLeaves | Where-Object { $_.Current.Name -in $expectedTimes })
            $times.Count | Should -Be 1 -Because 'the history row must expose one unambiguous visible relative time'
            $time = $times[0]
            $icon = Get-CombinedVisiblePart $row HistoryProviderIcon
            (Get-CombinedRowText $row) | Should -Match ([regex]::Escape($Title))
            $icon.Current.Name | Should -Be $Provider -Because 'the leading provider control retains its accessible identity'
            $timeText = $time.Current.Name
            $timeText | Should -BeIn $expectedTimes
            $timeText | Should -Not -Match ('Historical|Ended|' + [regex]::Escape($Provider))
            $providers = @($textLeaves | Where-Object { $_.Current.Name -eq $Provider })
            $providers.Count | Should -Be 1 -Because 'metadata must expose one visible provider display name'
            $providerPart = $providers[0]
            $contentParts = @(Get-CombinedRawChildren $row -ContentView)
            @($contentParts | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count |
                Should -Be 0 -Because 'the decorative provider icon must not duplicate the provider text in Content view'
            $providerLeaves = @($contentParts | Where-Object {
                $_.Current.Name -eq $Provider -and
                    -not @(Get-CombinedRawChildren $_ -ContentView | Where-Object { $_.Current.Name -eq $Provider }).Count
            })
            $providerLeaves.Count | Should -Be 1 -Because 'Content view descendants expose the provider once, excluding the row aggregate name'
            $titleBounds = $titlePart.Current.BoundingRectangle
            $timeBounds = $time.Current.BoundingRectangle
            $iconBounds = $icon.Current.BoundingRectangle
            $providerBounds = $providerPart.Current.BoundingRectangle
            $titleBounds.Bottom | Should -BeLessOrEqual $timeBounds.Top
            $titleBounds.Bottom | Should -BeLessOrEqual $providerBounds.Top
            $rowBounds = $row.Current.BoundingRectangle
            $providerBounds.Right | Should -BeLessOrEqual $rowBounds.Right -Because 'provider text must trim inside the available row rather than clip the ownership action'
            [math]::Abs($titleBounds.Left - $timeBounds.Left) | Should -BeLessOrEqual 1
            $iconBounds.Right | Should -BeLessThan $titleBounds.Left
            $iconBounds.Left | Should -BeGreaterOrEqual $rowBounds.Left
            $iconBounds.Right | Should -BeLessOrEqual $rowBounds.Right
            $iconBounds.Top | Should -BeGreaterOrEqual $rowBounds.Top
            $iconBounds.Bottom | Should -BeLessOrEqual $rowBounds.Bottom
            [math]::Abs(($iconBounds.Top + $iconBounds.Bottom) / 2 - ($rowBounds.Top + $rowBounds.Bottom) / 2) |
                Should -BeLessOrEqual 2 -Because 'the 16px leading icon is centered across both row lines'
            $dpiScale = $iconBounds.Width / 16
            [math]::Abs($iconBounds.Height - $iconBounds.Width) | Should -BeLessOrEqual 1
            $dpiScale | Should -BeGreaterOrEqual 1
            $timeBounds.Right | Should -BeLessOrEqual $providerBounds.Left
            $providerBounds.Top | Should -BeLessThan $timeBounds.Bottom
            $providerBounds.Bottom | Should -BeGreaterThan $timeBounds.Top
            if ($Status) {
                $statusLabel = if ($Status -eq 'Working') { 'Active' } else { $Status }
                $statuses = @($textLeaves | Where-Object { $_.Current.Name -eq $statusLabel })
                $statuses.Count | Should -Be 1 -Because 'the history row must expose one unambiguous meaningful status'
                $statusPart = $statuses[0]
                $statusPart.Current.Name | Should -Be $statusLabel
                $statusBounds = $statusPart.Current.BoundingRectangle
                $statusBounds.Left | Should -BeGreaterOrEqual $timeBounds.Right
                $statusBounds.Right | Should -BeLessOrEqual $providerBounds.Left
                $statusBounds.Top | Should -BeLessThan $providerBounds.Bottom
                $statusBounds.Bottom | Should -BeGreaterThan $providerBounds.Top
            } else {
                @($textLeaves | Where-Object {
                    $_.Current.Name -match '^(Historical|Ended|Idle|Active|Working|Attention|Error)$'
                }).Count | Should -Be 0 -Because 'redundant historical status is not rendered'
                Assert-CombinedOwnershipButton None
            }
            @{
                title = $Title; status = $Status; provider = $providerPart.Current.Name
                rendered_status = if ($Status) { $statusPart.Current.Name } else { $null }
                other_window = [bool]$OtherWindow
                title_bounds = $titleBounds.ToString(); time_bounds = $timeBounds.ToString()
                status_bounds = if ($Status) { $statusBounds.ToString() } else { $null }
                icon_bounds = $iconBounds.ToString()
                provider_bounds = $providerBounds.ToString(); row_bounds = $rowBounds.ToString()
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
            function Assert-CombinedOwnershipButton {
                param([ValidateSet('Background', 'OtherWindow', 'None')][string]$Kind)
                $rows = @(Get-CombinedRows Recent)
                $rows.Count | Should -Be 1
                $buttons = @(Get-CombinedRawChildren $rows[0] | Where-Object {
                    $_.Current.AutomationId -eq 'HistoryOwnershipButton' -and -not $_.Current.IsOffscreen -and
                        $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0
                })
                if ($Kind -eq 'None') {
                    $buttons.Count | Should -Be 0 -Because 'unknown and historical ownership must not display an actionable location'
                    return
                }
                $buttons.Count | Should -Be 1
                $expected = if ($Kind -eq 'Background') { 'Restore background tab' } else { 'Switch to other window' }
                $buttons[0].Current.Name | Should -Be $expected
                $buttons[0].Current.IsEnabled | Should -BeTrue
                $providerIcon = Get-CombinedVisiblePart $rows[0] HistoryProviderIcon
                $providers = @(Get-CombinedRawChildren $rows[0] | Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                        $_.Current.Name -eq $providerIcon.Current.Name -and -not $_.Current.IsOffscreen -and
                        $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0 -and
                        -not @(Get-CombinedRawChildren $_ | Where-Object {
                            $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                                -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                                $_.Current.BoundingRectangle.Height -gt 0
                        }).Count
                })
                $providers.Count | Should -Be 1 -Because 'ownership follows the visible provider display name'
                $bounds = $buttons[0].Current.BoundingRectangle
                $bounds.Left | Should -BeGreaterOrEqual $providers[0].Current.BoundingRectangle.Right
                $bounds.Right | Should -BeLessOrEqual $rows[0].Current.BoundingRectangle.Right
                Save-CombinedActionEvidence "ownership-$Kind" -Screenshot
                $buttons[0]
            }
            function New-CombinedCliFixture {
                param([string]$Purpose, [string]$SessionId = '', [string]$ResumeSession = '')
                $sid = if ($ResumeSession) { $ResumeSession } elseif ($SessionId) { $SessionId } else { [guid]::NewGuid().ToString() }
                $folder = Join-Path $script:evidence "$script:marker-$Purpose"
                New-Item -ItemType Directory -Path $folder -Force | Out-Null
                $shim = Join-Path $folder 'copilot.exe'
                if (Test-Path -LiteralPath $shim) { throw 'Owned native fixture output already exists; never overwrite an executable.' }
                $log = Join-Path $folder 'launch.jsonl'
                $pwsh = (Get-Command pwsh.exe).Source
                $config = @{
                    ITE2E_SHIM_PWSH = $pwsh
                    ITE2E_SHIM_FIXTURE = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-InteractiveDelegate.ps1')).Path
                    ITE2E_SHIM_LOG = $log; ITE2E_SHIM_RUN = $sid; ITE2E_SHIM_WTCLI = $script:app.WtcliPath
                }
                if ($ResumeSession) { $config.ITE2E_SHIM_RESUME_SESSION = $ResumeSession }
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
                $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($buildScript))
                (Invoke-Native -FilePath $pwsh -Arguments @('-NoProfile', '-EncodedCommand', $encoded) `
                    -WorkingDirectory $folder -TimeoutSec 60).ExitCode | Should -Be 0
                [pscustomobject]@{ SessionId = $sid; Folder = $folder; Shim = $shim; Log = $log }
            }
            function Send-CombinedCliHook {
                param($Fixture, [string]$PaneSessionId, [string]$Event)
                $json = @{ session_id = $Fixture.SessionId; cwd = $Fixture.Folder; tool_name = 'edit' } | ConvertTo-Json -Compress
                $code = "'$($json.Replace("'", "''"))' | & '$($script:app.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event $Event; exit `$LASTEXITCODE"
                $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
                (Invoke-Native -FilePath (Get-Command pwsh.exe).Source -Arguments @('-NoProfile', '-EncodedCommand', $encoded) `
                    -Environment @{ WT_SESSION = $PaneSessionId; WT_COM_CLSID = $script:app.ComClsid } -TimeoutSec 10).ExitCode | Should -Be 0
            }
            function Register-CombinedUnboundSession {
                param($Fixture)
                $name = $script:pipe -replace '^\\\\\.\\pipe\\', ''
                $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', $name, [IO.Pipes.PipeDirection]::InOut,
                    [IO.Pipes.PipeOptions]::Asynchronous)
                $reader = $null
                $writer = $null
                try {
                    $pipe.Connect(5000)
                    $reader = [IO.StreamReader]::new($pipe, [Text.UTF8Encoding]::new($false), $false, 1024, $true)
                    $writer = [IO.StreamWriter]::new($pipe, [Text.UTF8Encoding]::new($false), 1024, $true)
                    $writer.AutoFlush = $true
                    $settings = Get-WtSettingsObject -App $script:app
                    $requests = @(
                        @{ jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{
                            protocolVersion = 1; clientCapabilities = @{}
                            clientInfo = @{ name = 'ite2e-owned-unbound-fixture'; version = '1' }
                            _meta = @{ wta = @{ agent_id = $settings.acpAgent; agent_cmd = $settings.acpCustomCommand } }
                        } },
                        @{ jsonrpc = '2.0'; id = 2; method = '_intellterm.wta/session_hook'; params = @{
                            kind = 'SessionStarted'; key = $Fixture.SessionId; cli_source = 'Copilot'
                            pane_session_id = ''; cwd = $Fixture.Folder; title = (Split-Path $Fixture.Folder -Leaf)
                        } }
                    )
                    foreach ($request in $requests) {
                        $writer.WriteLine(($request | ConvertTo-Json -Depth 8 -Compress))
                        $clock = [Diagnostics.Stopwatch]::StartNew()
                        do {
                            $read = $reader.ReadLineAsync()
                            if (-not $read.Wait(5000)) { throw 'Owned master RPC response timed out.' }
                            $line = $read.GetAwaiter().GetResult()
                            if (-not $line) { throw 'Master closed the owned fixture RPC channel.' }
                            $response = $line | ConvertFrom-Json
                            if ($clock.Elapsed.TotalSeconds -gt 10) { throw 'Owned master RPC response exceeded its bound.' }
                        } while ($response.id -ne $request.id)
                        if ($response.error) { throw "Master rejected fixture admission: $($response.error | ConvertTo-Json -Compress -Depth 6)" }
                        $response | ConvertTo-Json -Depth 8 |
                            Set-Content (Join-Path $Fixture.Folder "master-rpc-$($request.id)-response.json")
                    }
                }
                finally {
                    if ($writer) { $writer.Dispose() }
                    if ($reader) { $reader.Dispose() }
                    $pipe.Dispose()
                }
            }
        function Get-CombinedFilterState {
            Invoke-UiClick -App $script:app -Selector FilterTabsButton | Out-Null
            try {
                $state = @{}
                foreach ($id in @('AgentsOnlyFilterMenuItem', 'RecentAgentSessionsFilterMenuItem')) {
                    $item = Wait-Until -TimeoutSec 30 -Because "$id is visible in the owned filter flyout" -Condition {
                        $peer = Get-CombinedElement $id
                        if ($peer -and -not $peer.Current.IsOffscreen) { $peer }
                    }
                    $item.Current.ProcessId | Should -Be $script:app.Pid
                    $item.Current.ControlType | Should -Be ([Windows.Automation.ControlType]::MenuItem)
                    $state[$id] = $item.GetCurrentPattern(
                        [Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq
                        [Windows.Automation.ToggleState]::On
                }
                $state
            }
            finally { Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null }
        }
        function Set-CombinedFilters {
            param([bool]$AgentsOnly, [bool]$Recent)
            foreach ($entry in @(
                @{ Id = 'AgentsOnlyFilterMenuItem'; Value = $AgentsOnly },
                @{ Id = 'RecentAgentSessionsFilterMenuItem'; Value = $Recent }
            )) {
                $state = Get-CombinedFilterState
                if ($state[$entry.Id] -ne $entry.Value) {
                    Invoke-UiClick -App $script:app -Selector FilterTabsButton | Out-Null
                    $item = Get-CombinedElement $entry.Id
                    $item.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
                }
            }
            $state = Get-CombinedFilterState
            $state.AgentsOnlyFilterMenuItem | Should -Be $AgentsOnly
            $state.RecentAgentSessionsFilterMenuItem | Should -Be $Recent
            Assert-CombinedHeaderCue Tabs
        }
        function Set-CombinedView {
            param([bool]$Agents)
            # Preserve the old tests' two fixture scopes, not the retired header navigation.
            Set-CombinedFilters -AgentsOnly $Agents -Recent $Agents
        }
        function Assert-CombinedScopeRows {
            param([string[]]$Expected)
            if ($script:scopeClock -and $script:scopeClock.Elapsed.TotalSeconds -ge 300) {
                throw 'Independent scope/search smoke exceeded its 300-second bound.'
            }
            $found = @{}
            $scroll = Get-CombinedScroll ItemsList
            $percent = 0.0
            $clock = [Diagnostics.Stopwatch]::StartNew()
            do {
                if ($clock.Elapsed.TotalSeconds -ge 30) { throw 'Visible ItemsList sweep exceeded 30 seconds.' }
                if ($scroll.Current.VerticallyScrollable) {
                    $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, $percent)
                    Start-Sleep -Milliseconds 100
                }
                $list = Get-CombinedElement ItemsList
                $viewport = $list.Current.BoundingRectangle
                foreach ($kind in @('Live', 'Recent')) {
                    foreach ($row in @(Get-CombinedRows $kind)) {
                        if ($row.Current.IsOffscreen -or -not $viewport.IntersectsWith($row.Current.BoundingRectangle)) { continue }
                        $row.Current.ProcessId | Should -Be $script:app.Pid
                        $leaves = @(Get-CombinedRawChildren $row -ContentView | Where-Object {
                            -not $_.Current.IsOffscreen -and $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text
                        })
                        $text = $row.Current.Name + (@($leaves | ForEach-Object { $_.Current.Name }) -join '')
                        foreach ($id in $script:scopeTitles.Keys) {
                            if ($text.Contains($script:scopeTitles[$id])) {
                                if (-not $found.ContainsKey($id)) { $found[$id] = [Collections.Generic.HashSet[string]]::new() }
                                [void]$found[$id].Add(($row.GetRuntimeId() -join ','))
                            }
                        }
                    }
                }
                if (-not $scroll.Current.VerticallyScrollable -or $percent -ge 100) { break }
                $percent = [Math]::Min(100, $percent + [Math]::Max(1, $scroll.Current.VerticalViewSize / 2))
            } while ($true)
            @($found.Keys | Sort-Object) | Should -Be @($Expected | Sort-Object)
            foreach ($id in $found.Keys) { $found[$id].Count | Should -Be 1 -Because "$id has one rendered qualified row, not a retained or duplicate peer" }
            if ($scroll.Current.VerticallyScrollable) {
                $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
            }
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
            $viewport = (Get-CombinedElement ItemsList).Current.BoundingRectangle
            $viewport.Height | Should -BeGreaterThan 40
            Get-CombinedElement HistorySplitter | Should -BeNullOrEmpty -Because 'the section separator is visible but not resizable'
            Get-CombinedElement HistoryList | Should -BeNullOrEmpty -Because 'no independent history viewport may remain'
            $scrollPeers = @(Get-CombinedRawChildren (Get-CombinedElement ItemsList) | Where-Object {
                $pattern = $null
                $_.TryGetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern, [ref]$pattern)
            })
            $scrollPeers.Count | Should -BeLessOrEqual 1 -Because 'mixed rows share one scrolling surface'
            @{
                viewport = $viewport.ToString(); scroll_peers = $scrollPeers.Count
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
            param([string]$Phase, [switch]$Screenshot, [string]$SessionId = '')
            $focused = [Windows.Automation.AutomationElement]::FocusedElement
            $historyVisible = [bool](Get-CombinedElement HistoryHeaderButton)
            @{
                phase = $Phase; at = [DateTimeOffset]::UtcNow.ToString('o')
                owned_sessions = @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -like "$script:marker-*" -or $_.session_id -like 'chat-fixture-*' -or
                        ($SessionId -and $_.session_id -eq $SessionId)
                })
                history_visible = [bool]$historyVisible
                history_rows = if ($historyVisible) { @(Get-CombinedRows Recent | ForEach-Object { Get-CombinedRowText $_ }) } else { @() }
                upper_rows = @(Get-CombinedRows Live | ForEach-Object { Get-CombinedRowText $_ })
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
            $treeSelector = 'ItemsList'
            Get-UiTree -App $script:app -Selector $treeSelector -Depth 6 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.tree.txt")
            if ($Screenshot) {
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "$Phase.png") | Out-Null
            }
        }
        function Invoke-CombinedHistoryRow {
            param([string]$Title, [string]$SessionId, [string]$PaneId = '', [string]$Status = '')
            $isReady = {
                param($Row, $List)
                $rectangle = $Row.Current.BoundingRectangle
                $viewport = $List.Current.BoundingRectangle
                -not $Row.Current.IsOffscreen -and -not $List.Current.IsOffscreen -and
                    $rectangle.Width -gt 0 -and $rectangle.Height -gt 0 -and
                    $viewport.Width -gt 0 -and $viewport.Height -gt 0 -and
                    $rectangle.Left -ge $viewport.Left -and $rectangle.Right -le $viewport.Right -and
                    $rectangle.Top -ge $viewport.Top -and $rectangle.Bottom -le $viewport.Bottom
            }
            $getTarget = {
                (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be $Title
                $rows = @(Get-CombinedRows Recent)
                $rows.Count | Should -Be 1 -Because 'a history action must have exactly one filtered target'
                $rows[0].Current.ProcessId | Should -Be $script:app.Pid
                @((Get-CombinedRawChildren $rows[0]) | Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and $_.Current.Name -eq $Title
                }).Count | Should -BeGreaterThan 0 -Because 'the controlled title identifies the intended row independently of age and status'
                $rows[0]
            }
            try {
                $sessions = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $SessionId)
                $sessions.Count | Should -Be 1
                if ($PaneId) {
                    ([string]$sessions[0].pane_session_id).Trim('{}') | Should -Be $PaneId.Trim('{}')
                }
                if ($Status) { $sessions[0].status | Should -Be $Status }
                $row = & $getTarget
                if (-not (& $isReady $row (Get-CombinedElement ItemsList))) {
                    $pattern = $null
                    if (-not $row.TryGetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern, [ref]$pattern)) {
                        $virtualized = $null
                        if (-not $row.TryGetCurrentPattern([Windows.Automation.VirtualizedItemPattern]::Pattern, [ref]$virtualized)) {
                            throw 'The intended History row exposes neither ScrollItemPattern nor VirtualizedItemPattern.'
                        }
                        ([Windows.Automation.VirtualizedItemPattern]$virtualized).Realize()
                        $row = & $getTarget
                        $pattern = $row.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern)
                    }
                    ([Windows.Automation.ScrollItemPattern]$pattern).ScrollIntoView()
                }
                $row = Wait-Until -TimeoutSec 5 -Because 'the same intended History row finishes realization and layout' -Condition {
                    $fresh = & $getTarget
                    if (& $isReady $fresh (Get-CombinedElement ItemsList)) {
                        $fresh
                    }
                }
                $row = & $getTarget
                (& $isReady $row (Get-CombinedElement ItemsList)) | Should -BeTrue -Because 'the fresh exact target must fit fully inside the shared viewport immediately before input'
                $bounds = $row.Current.BoundingRectangle
                $x = [int]($bounds.X + $bounds.Width / 2)
                $y = [int]($bounds.Y + $bounds.Height / 2)
                Invoke-UiMouseDrag -App $script:app -FromX $x -FromY $y -ToX $x -ToY $y -HoldMs 50 | Out-Null
            }
            catch {
                $original = $_
                try {
                    $list = Get-CombinedElement ItemsList
                    if (-not $list) { throw 'History viewport is unavailable during failure diagnostics.' }
                    $scroll = $null
                    $hasScroll = $list.TryGetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern, [ref]$scroll)
                    @{
                        title = $Title; session_id = $SessionId; pane_id = $PaneId; status = $Status
                        query = Get-UiValue -App $script:app -Selector SearchTextBox
                        viewport = $list.Current.BoundingRectangle.ToString()
                        scroll = if ($hasScroll) { ([Windows.Automation.ScrollPattern]$scroll).Current.VerticalScrollPercent }
                        rows = @(Get-CombinedRows Recent | ForEach-Object {
                            @{ name = $_.Current.Name; offscreen = $_.Current.IsOffscreen
                                bounds = $_.Current.BoundingRectangle.ToString(); process_id = $_.Current.ProcessId }
                        })
                    } | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $script:evidence 'history-action-target-failure.json')
                }
                catch {
                    $diagnosticError = $_
                    try {
                        @{ original_error = $original.ToString(); diagnostic_error = $diagnosticError.ToString() } |
                            ConvertTo-Json | Set-Content (Join-Path $script:evidence 'history-action-diagnostic-error.json')
                    }
                    catch { Write-Warning "History diagnostics failed: $diagnosticError; recording failed: $_" }
                }
                throw $original
            }
        }
        function Invoke-CombinedTabContext {
            param([string]$Title)
            $tabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId | Where-Object title -eq $Title)
            $tabs.Count | Should -Be 1 -Because 'the exact title must resolve one canonical tab in the owned window'
            $panes = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tabs[0].tab_id)
            $panes.Count | Should -BeGreaterThan 0
            $context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $panes[0].session_id)
            [string]$context.pane.tab_id | Should -Be ([string]$tabs[0].tab_id)
            [string]$context.pane.window_id | Should -Be ([string]$script:app.WindowId)
            # Header realization intentionally closes History. Resolve the visible projection
            # afresh; do not reopen Agents or reset the search contract to reuse an old peer.
            $rows = @(Get-CombinedRows Live | Where-Object {
                -not $_.Current.IsOffscreen -and @(Get-CombinedRawChildren $_ | Where-Object {
                    $_.Current.Name -eq $Title -and -not $_.Current.IsOffscreen
                }).Count -gt 0
            })
            $rows.Count | Should -Be 1
            $titles = @(Get-CombinedRawChildren $rows[0] | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                    $_.Current.Name -eq $Title -and -not $_.Current.IsOffscreen
            })
            $titles.Count | Should -Be 1
            $bounds = $titles[0].Current.BoundingRectangle
            Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $panes[0].session_id -Title $Title
        }
        function Invoke-CombinedOwnedGroupContext {
            param($Tab, [string]$Title)
            $tabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId)
            $owned = @($tabs | Where-Object tab_id -eq $Tab.tab_id)
            @{
                tab_id = $Tab.tab_id; pane_id = $Tab.session_id; window_id = $script:app.WindowId
                header = (Get-CombinedElement VerticalTabsHeader).Current.Name
                owned_tabs = $owned; foreground_hwnd = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
                rows = @(Get-CombinedRows Live | ForEach-Object {
                    @{ text = Get-CombinedRowText $_; runtime_id = @($_.GetRuntimeId())
                        offscreen = $_.Current.IsOffscreen; bounds = $_.Current.BoundingRectangle.ToString() }
                })
            } | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $script:evidence 'background-owned-group-before-assert.json')
            $owned.Count | Should -Be 1
            $owned[0].title | Should -Be $Title
            @($tabs | Where-Object title -eq $Title).Count | Should -Be 1
            $panes = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $Tab.tab_id)
            $panes.Count | Should -Be 2
            @($panes | Where-Object session_id -eq $Tab.session_id).Count | Should -Be 1
            $context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $Tab.session_id)
            [string]$context.pane.tab_id | Should -Be ([string]$Tab.tab_id)
            [string]$context.pane.window_id | Should -Be ([string]$script:app.WindowId)
            $groups = @(Get-CombinedRows Live | Where-Object {
                @((Get-CombinedRawChildren $_) | Where-Object {
                    $_.Current.AutomationId -eq 'TabGroupToggleButton' -and -not $_.Current.IsOffscreen
                }).Count -eq 1 -and (Get-CombinedRowText $_).Contains($Title)
            })
            $groups.Count | Should -Be 1 -Because 'the owned two-pane tab has one canonical group header, not a pane title'
            $toggle = @(Get-CombinedRawChildren $groups[0] | Where-Object {
                $_.Current.AutomationId -eq 'TabGroupToggleButton' -and -not $_.Current.IsOffscreen
            })[0].Current.BoundingRectangle
            $titles = @(Get-CombinedRawChildren $groups[0] | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and $_.Current.Name -eq $Title -and
                    -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Height -gt 0 -and
                    ($_.Current.BoundingRectangle.Top + $_.Current.BoundingRectangle.Height / 2) -ge $toggle.Top -and
                    ($_.Current.BoundingRectangle.Top + $_.Current.BoundingRectangle.Height / 2) -le $toggle.Bottom
            })
            $titles.Count | Should -Be 1 -Because 'only the group title shares the canonical group toggle row'
            $bounds = $titles[0].Current.BoundingRectangle
            $viewport = (Get-CombinedElement ItemsList).Current.BoundingRectangle
            $point = [Windows.Point]::new([int]($bounds.X + $bounds.Width / 2), [int]($bounds.Y + $bounds.Height / 2))
            $hit = [Windows.Automation.AutomationElement]::FromPoint($point)
            $nativePoint = [ItE2E.ItWtWin32Input+POINT]::new()
            $nativePoint.X = [int]$point.X
            $nativePoint.Y = [int]$point.Y
            $nativeHit = [ItE2E.ItWtWin32Input]::WindowFromPoint($nativePoint)
            $ancestor = $hit
            $ownedHit = $false
            while ($ancestor) {
                if ([Windows.Automation.Automation]::Compare($ancestor, $groups[0])) {
                    $ownedHit = $true
                    break
                }
                $ancestor = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($ancestor)
            }
            @{
                tab_id = $Tab.tab_id; shell_pane_id = $Tab.session_id; window_id = $script:app.WindowId
                group_runtime_id = @($groups[0].GetRuntimeId()); header_bounds = $bounds.ToString()
                toggle_bounds = $toggle.ToString(); viewport_bounds = $viewport.ToString()
                hit_name = $hit.Current.Name; hit_runtime_id = @($hit.GetRuntimeId())
                point = @{ x = $point.X; y = $point.Y }; owned_hit = $ownedHit
                viewport_contains_point = $viewport.Contains($point)
                hit_process_id = $hit.Current.ProcessId
                win32_hit_hwnd = $nativeHit.ToInt64()
                win32_hit_root = [ItE2E.ItWtWin32Input]::GetAncestor($nativeHit, 2).ToInt64()
                win32_hit_pid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($nativeHit)
                foreground_hwnd = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
                raw_group = @(Get-CombinedRawChildren $groups[0] | ForEach-Object {
                    @{ name = $_.Current.Name; id = $_.Current.AutomationId
                        offscreen = $_.Current.IsOffscreen; bounds = $_.Current.BoundingRectangle.ToString() }
                })
                scroll_percent = (Get-CombinedScroll ItemsList).Current.VerticalScrollPercent
            } | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $script:evidence 'background-owned-group-context.json')
            $viewport.Contains($point) | Should -BeTrue -Because 'a realized UIA title can still be clipped outside the list viewport'
            $bounds.Contains($point) | Should -BeTrue
            $groups[0].Current.ProcessId | Should -Be $script:app.Pid
            $titles[0].Current.ProcessId | Should -Be $script:app.Pid
            # The flyout belongs to TabHeaderGrid, not its ListViewItem container.
            # Use the exact header title, as the progress/pinned fixtures do; never
            # click the expanded group center, which can fall on a child pane.
            Set-WtWindowForeground -App $script:app -Attempts 3 -DelayMs 150 | Should -BeTrue
            $down = [ItE2E.ItWtWin32Input+INPUT]::new()
            $up = [ItE2E.ItWtWin32Input+INPUT]::new()
            $mouse = [ItE2E.ItWtWin32Input+MOUSEINPUT]::new()
            $data = [ItE2E.ItWtWin32Input+INPUTUNION]::new()
            $mouse.dwFlags = 0x0008
            $data.mouse = $mouse
            $down.data = $data
            $mouse.dwFlags = 0x0010
            $data.mouse = $mouse
            $up.data = $data
            $inputs = [ItE2E.ItWtWin32Input+INPUT[]]@($down, $up)
            $dpi = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
            if ($dpi -eq [IntPtr]::Zero) { throw 'Physical header coordinate context unavailable.' }
            $originalCursor = [ItE2E.ItWtWin32Input+POINT]::new()
            $cursorMoved = $false
            $clickDelivered = $false
            try {
                $current = Get-Process -Id $script:app.Pid -ErrorAction Stop
                if (-not $script:app.Launched -or -not $script:app.OwnedProcess -or
                    $script:app.OwnedProcess.HasExited -or $script:app.OwnedProcess.Id -ne $current.Id -or
                    $current.StartTime -ne $script:app.OwnedProcess.StartTime -or
                    $current.Path -ne (Join-Path $script:app.InstallLocation 'WindowsTerminal.exe')) {
                    throw 'Header context input requires the original owned process lease.'
                }
                foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
                    if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Header context input refuses held input.' }
                }
                if (-not [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$originalCursor)) {
                    throw 'Original cursor position unavailable before header context input.'
                }
                [ItE2E.ItWtWin32Input]::SetCursorPos($nativePoint.X, $nativePoint.Y) | Should -BeTrue
                $cursorMoved = $true
                if ($titles[0].Current.IsOffscreen -or $groups[0].Current.IsOffscreen -or
                    $titles[0].Current.BoundingRectangle -ne $bounds -or
                    -not (Get-CombinedElement ItemsList).Current.BoundingRectangle.Contains($point)) {
                    throw 'Canonical header geometry changed before context input.'
                }
                $root = [IntPtr][long]$script:app.Hwnd
                $cursor = [ItE2E.ItWtWin32Input+POINT]::new()
                if (-not [ItE2E.ItWtWin32Input]::GetCursorPos([ref]$cursor) -or
                    $cursor.X -ne $nativePoint.X -or $cursor.Y -ne $nativePoint.Y -or
                    [ItE2E.ItWtWin32Input]::GetForegroundWindow() -ne $root -or
                    [ItE2E.ItWtWin32Input]::GetAncestor($root, 2) -ne $root -or
                    [ItE2E.ItWtWin32Input]::GetWindowProcessId($root) -ne $script:app.Pid) {
                    throw 'Header context input requires the unchanged cursor and owned foreground root.'
                }
                $nativeHit = [ItE2E.ItWtWin32Input]::WindowFromPoint($cursor)
                if ([ItE2E.ItWtWin32Input]::GetAncestor($nativeHit, 2) -ne $root -or
                    [ItE2E.ItWtWin32Input]::GetWindowProcessId($nativeHit) -ne $script:app.Pid) {
                    throw 'Header context input point is covered or outside the owned root.'
                }
                foreach ($key in @(1, 2, 4, 5, 6, 16, 17, 18, 91, 92)) {
                    if ([ItE2E.ItWtWin32Input]::IsKeyDown($key)) { throw 'Header context input refuses held input before delivery.' }
                }
                if ([ItE2E.ItWtWin32Input]::SendInput(2, $inputs,
                    [Runtime.InteropServices.Marshal]::SizeOf($down)) -ne 2) {
                    throw 'Paired header right-click input was not delivered.'
                }
                $clickDelivered = $true
            }
            finally {
                try {
                    if ($cursorMoved -and -not $clickDelivered) {
                        $mouseHeld = @(1, 2, 4, 5, 6) | Where-Object { [ItE2E.ItWtWin32Input]::IsKeyDown($_) }
                        if ($mouseHeld) {
                            Write-Warning 'Cursor restoration refused while a mouse button is held.'
                        } elseif (-not [ItE2E.ItWtWin32Input]::SetCursorPos($originalCursor.X, $originalCursor.Y)) {
                            Write-Warning 'Original cursor position could not be restored after header input failure.'
                        }
                    }
                } finally {
                    [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($dpi)
                }
            }
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
        $startupState = @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        }
        $startupSettings = @{
            language = 'en-US'; tabLayout = 'vertical'; tabLayoutVerticalWidth = 320
            startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            acpAgent = 'custom:combined-sidebar-fixture'; acpCustomCommand = $command; acpModel = ''
            autoFixEnabled = $false; actions = @(); keybindings = @(); 'warning.confirmOnClose' = 'never'
        }
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'Dev started during preparation; refusing configuration mutation or adoption.'
        }
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Configuration markers changed during preparation; never replay them: $path"
            }
        }
        Backup-WtConfig -App $script:target
        $script:ownsConfig = $true
        Clear-WtConfig -App $script:target
        foreach ($key in $startupState.Keys) {
            Set-WtState -App $script:target -Key $key -Value $startupState[$key] | Out-Null
        }
        $script:app = Start-Terminal -Package Dev -PassFre $true -TimeoutSec 60 `
            -Backup $false -CleanSettings $false -Settings $startupSettings
        $script:app.Launched | Should -BeTrue -Because 'only a test-owned Dev window may be driven'
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true
        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue -Because 'desktop ownership is required'
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        (Get-AgentPaneSession -App $script:app).AcpSessionId | Should -Match '^chat-fixture-'
        Set-CombinedView $true
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        (Get-CombinedElement HistoryHeader).Current.Name | Should -Be 'Recent agent sessions'
        Set-CombinedView $false
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'startup-header.png') | Out-Null
        $survival = [Diagnostics.Stopwatch]::StartNew()
        $survivalSeconds = if ($env:ITE2E_HISTORY_INDICATORS_ONLY -eq '1') { 0 } else { 30 }
        while ($survival.Elapsed.TotalSeconds -lt $survivalSeconds) {
            Get-Process -Id $script:app.Pid -ErrorAction Stop | Out-Null
            Start-Sleep -Milliseconds 500
        }
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        @{
            pid = $script:app.Pid; hwnd = $script:app.Hwnd; observed_seconds = $survival.Elapsed.TotalSeconds
            header = 'Tabs'; agents_round_trip = $true
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'startup-survival.json')
        $seedIndices = if ($env:ITE2E_HISTORY_INDICATORS_ONLY -eq '1') { @() } else { 0..7 }
        $script:tabs = @(foreach ($i in $seedIndices) {
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
        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue -Because 'the owned interactive desktop is a prerequisite, not a product oracle'
        Set-CombinedView $true
        $searchToggle = (Get-CombinedElement SearchTabsButton).GetCurrentPattern(
            [Windows.Automation.TogglePattern]::Pattern)
        if ($searchToggle.Current.ToggleState -eq [Windows.Automation.ToggleState]::On) {
            Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        }
        Assert-CombinedSearchState $false
        Wait-Until -TimeoutSec 5 -Because 'search close layout has settled before measuring the lists' -Condition {
            $box = Get-CombinedElement SearchTextBox
            -not ($box -and -not $box.Current.IsOffscreen -and $box.Current.BoundingRectangle.Height -gt 0)
        } | Out-Null
        $scrollLists = if ($env:ITE2E_HISTORY_INDICATORS_ONLY -eq '1') { @() } else { @('ItemsList') }
        foreach ($id in $scrollLists) {
            $scroll = Get-CombinedScroll $id
            if ($scroll.Current.VerticallyScrollable) {
                $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
            }
            $heading = Get-CombinedElement HistoryHeaderButton
            if ($heading) {
                $heading.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
            }
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
        $script:scopeClock = $null
        $cleanupError = $null
        try {
            if ($script:app) {
                if ($script:heldPromptMarker) {
                    [IO.File]::WriteAllText($script:releasePromptPath, 'release')
                    $released = '|released|' + $script:heldPromptMarker
                    Wait-Until -TimeoutSec 10 -Because 'release the recorded held turn before stopping its fixture host' -Condition {
                        (Get-Content -LiteralPath $script:fixtureLog -Raw).Contains($released)
                    } | Out-Null
                    $script:heldPromptMarker = $null
                }
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
                    if ($script:runtimeStateExisted) {
                        foreach ($relative in $script:runtimeStateHashes.Keys) {
                            (Get-FileHash -LiteralPath (Join-Path $script:runtimeStateBackup $relative)).Hash |
                                Should -Be $script:runtimeStateHashes[$relative] -Because 'only this run fresh hash-backed snapshot may be restored'
                        }
                    }
                    if (Test-Path -LiteralPath $script:runtimeStatePath) {
                        Remove-Item -LiteralPath $script:runtimeStatePath -Recurse -Force
                    }
                    if ($script:runtimeStateExisted) {
                        Copy-Item -LiteralPath $script:runtimeStateBackup -Destination $script:runtimeStatePath -Recurse -Force
                        foreach ($relative in $script:runtimeStateHashes.Keys) {
                            (Get-FileHash -LiteralPath (Join-Path $script:runtimeStatePath $relative)).Hash |
                                Should -Be $script:runtimeStateHashes[$relative]
                        }
                    }
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

    Context 'Independent scope filters and global search' {
        BeforeAll {
            $script:scopeClock = [Diagnostics.Stopwatch]::StartNew()
            $script:scopeQuery = "scope-$script:marker"
            $script:scopeTitles = @{
                ordinary = "$script:scopeQuery ordinary copilot"
                agent = "$script:scopeQuery live Copilot"
                history = "$script:scopeQuery saved conversation"
                nonmatch = $script:history[1].title
            }
            $script:scopeOriginalHistoryTitle = $script:history[0].title
            $script:scopeTabs = [Collections.Generic.List[object]]::new()
            Set-WtPaneFocus -App $script:app -SessionId $script:tabs[0].session_id
            foreach ($kind in @('ordinary', 'agent')) {
                $arguments = @('new-tab', '-c', 'cmd.exe /d /k', '-d', $script:evidence, '-n', $script:scopeTitles[$kind])
                if ($kind -eq 'agent') { $arguments += @('--agent-provider', 'copilot') }
                $tab = Invoke-WtCli -App $script:app -Arguments $arguments -TimeoutSec 30
                $script:scopeTabs.Add($tab)
                $owned = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id |
                    Where-Object session_id -eq $tab.session_id)
                $owned | Should -HaveCount 1
                [string]$owned[0].native_agent_provider_id | Should -Be $(if ($kind -eq 'agent') { 'copilot' } else { '' })
            }
            $script:history[0].title = $script:scopeTitles.history
            @{ sessions = $script:history } | ConvertTo-Json -Depth 6 |
                Set-Content -LiteralPath $script:historyPath -Encoding utf8
            Invoke-Wta -App $script:app -TimeoutSec 30 -Arguments @('sessions', 'refresh', '--master', $script:pipe, '--json') | Out-Null
            Wait-Until -TimeoutSec 30 -Because 'the same ACP history fixture reaches the native projection' -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $script:history[0].sessionId -and $_.title -eq $script:scopeTitles.history
                }).Count -eq 1
            } | Out-Null
        }
        AfterAll {
            $script:scopeClock = $null
            foreach ($tab in @($script:scopeTabs)) {
                $owned = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id |
                    Where-Object session_id -eq $tab.session_id)
                $owned | Should -HaveCount 1
                Invoke-WtCli -App $script:app -Arguments @('kill-pane', '-t', $tab.session_id) | Out-Null
            }
            $script:history[0].title = $script:scopeOriginalHistoryTitle
            @{ sessions = $script:history } | ConvertTo-Json -Depth 6 |
                Set-Content -LiteralPath $script:historyPath -Encoding utf8
            Invoke-Wta -App $script:app -TimeoutSec 30 -Arguments @('sessions', 'refresh', '--master', $script:pipe, '--json') | Out-Null
        }
        It 'Sidebar scope filters are independent (<Combination>)' -ForEach @(
            @{ Combination = '00'; Agents = $false; Recent = $false; Expected = @('ordinary', 'agent') },
            @{ Combination = '10'; Agents = $true; Recent = $false; Expected = @('agent') },
            @{ Combination = '01'; Agents = $false; Recent = $true; Expected = @('ordinary', 'agent', 'history', 'nonmatch') },
            @{ Combination = '11'; Agents = $true; Recent = $true; Expected = @('agent', 'history', 'nonmatch') }
        ) {
            Set-CombinedFilters -AgentsOnly $Agents -Recent $Recent
            Assert-CombinedScopeRows $Expected
            Open-CombinedSearch
            Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be ''
            Assert-CombinedScopeRows $Expected
            Assert-CombinedHeaderCue Tabs
        }
        It 'Sidebar search bypasses both scope filters' {
            foreach ($agents in @($false, $true)) {
                foreach ($recent in @($false, $true)) {
                    Set-CombinedFilters -AgentsOnly $agents -Recent $recent
                    Set-CombinedQuery $script:scopeQuery
                    Assert-CombinedScopeRows @('ordinary', 'agent', 'history')
                    $state = Get-CombinedFilterState
                    $state.AgentsOnlyFilterMenuItem | Should -Be $agents
                    $state.RecentAgentSessionsFilterMenuItem | Should -Be $recent
                    Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be $script:scopeQuery
                    Assert-CombinedHeaderCue Tabs
                }
            }
        }
        It 'Sidebar history appears only for recent scope or search' {
            foreach ($agents in @($false, $true)) {
                Set-CombinedQuery ''
                Set-CombinedFilters -AgentsOnly $agents -Recent $false
                $expected = if ($agents) { @('agent') } else { @('ordinary', 'agent') }
                Assert-CombinedScopeRows $expected
                Set-CombinedQuery $script:scopeTitles.history
                Assert-CombinedScopeRows @('history')
                (Get-CombinedFilterState).RecentAgentSessionsFilterMenuItem | Should -BeFalse
                Set-CombinedQuery ''
                Assert-CombinedScopeRows $expected
            }
        }
        It 'Clearing sidebar search restores selected scope' {
            foreach ($agents in @($false, $true)) {
                foreach ($recent in @($false, $true)) {
                    $expected = @('agent')
                    if (-not $agents) { $expected += 'ordinary' }
                    if ($recent) { $expected += @('history', 'nonmatch') }
                    Set-CombinedFilters -AgentsOnly $agents -Recent $recent
                    foreach ($clearButton in @($false, $true)) {
                        Set-CombinedQuery $script:scopeQuery
                        Assert-CombinedScopeRows @('ordinary', 'agent', 'history')
                        if ($clearButton) {
                            $box = Get-CombinedElement SearchTextBox
                            $clear = @($box.FindAll([Windows.Automation.TreeScope]::Descendants,
                                [Windows.Automation.PropertyCondition]::new(
                                    [Windows.Automation.AutomationElement]::ControlTypeProperty,
                                    [Windows.Automation.ControlType]::Button)) | Where-Object { -not $_.Current.IsOffscreen })
                            $clear | Should -HaveCount 1 -Because 'the standard TextBox exposes its one visible clear button'
                            $clear[0].GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
                        } else { Set-CombinedQuery '' }
                        Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be ''
                        Assert-CombinedScopeRows $expected
                        $state = Get-CombinedFilterState
                        $state.AgentsOnlyFilterMenuItem | Should -Be $agents
                        $state.RecentAgentSessionsFilterMenuItem | Should -Be $recent
                    }
                }
            }
        }
        It 'Closing sidebar search restores selected scope' {
            foreach ($agents in @($false, $true)) {
                foreach ($recent in @($false, $true)) {
                    Set-CombinedFilters -AgentsOnly $agents -Recent $recent
                    Set-CombinedQuery $script:scopeQuery
                    Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
                    Assert-CombinedSearchState $false
                    $expected = @('agent')
                    if (-not $agents) { $expected += 'ordinary' }
                    if ($recent) { $expected += @('history', 'nonmatch') }
                    Assert-CombinedScopeRows $expected
                    Open-CombinedSearch
                    Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be ''
                    Assert-CombinedScopeRows $expected
                    $state = Get-CombinedFilterState
                    $state.AgentsOnlyFilterMenuItem | Should -Be $agents
                    $state.RecentAgentSessionsFilterMenuItem | Should -Be $recent
                }
            }
        }
        It 'Sidebar scope choices persist during global search' {
            Set-CombinedQuery $script:scopeQuery
            foreach ($pair in @(@($false, $false), @($true, $false), @($true, $true),
                @($false, $true), @($false, $false), @($true, $true))) {
                Set-CombinedFilters -AgentsOnly $pair[0] -Recent $pair[1]
                Assert-CombinedScopeRows @('ordinary', 'agent', 'history')
                Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be $script:scopeQuery
            }
            Set-CombinedQuery ''
            Assert-CombinedScopeRows @('agent', 'history', 'nonmatch')
        }
        It 'Sidebar search forces history expanded and restores its collapsed preference' {
            Set-CombinedQuery ''
            Set-CombinedFilters -AgentsOnly $false -Recent $true
            $header = Get-CombinedElement HistoryHeaderButton
            $pattern = $header.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
            $pattern.Collapse()
            Assert-CombinedScopeRows @('ordinary', 'agent')
            Set-CombinedQuery $script:scopeQuery
            (Get-CombinedElement HistoryHeaderButton).Current.IsEnabled | Should -BeFalse
            (Get-CombinedElement HistoryHeaderButton).GetCurrentPattern(
                [Windows.Automation.ExpandCollapsePattern]::Pattern).Current.ExpandCollapseState |
                Should -Be ([Windows.Automation.ExpandCollapseState]::Expanded)
            Assert-CombinedScopeRows @('ordinary', 'agent', 'history')
            Set-CombinedQuery ''
            (Get-CombinedElement HistoryHeaderButton).Current.IsEnabled | Should -BeTrue
            (Get-CombinedElement HistoryHeaderButton).GetCurrentPattern(
                [Windows.Automation.ExpandCollapsePattern]::Pattern).Current.ExpandCollapseState |
                Should -Be ([Windows.Automation.ExpandCollapseState]::Collapsed)
            Assert-CombinedScopeRows @('ordinary', 'agent')
            (Get-CombinedElement HistoryHeaderButton).GetCurrentPattern(
                [Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        }
        It 'Sidebar scope menus expose standard checked states and preserve keyboard focus' {
            if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
                Set-ItResult -Skipped -Because 'External desktop restrictions prevent the harness owned-foreground physical-input gate'
                return
            }
            Set-CombinedFilters -AgentsOnly $false -Recent $false
            Set-CombinedQuery $script:scopeQuery
            $capture = @(foreach ($tab in $script:scopeTabs) { Get-WtCapture -App $script:app -SessionId $tab.session_id })
            foreach ($entry in @(
                @{ Id = 'AgentsOnlyFilterMenuItem'; Name = 'Agents only'; Key = 0x47 },
                @{ Id = 'RecentAgentSessionsFilterMenuItem'; Name = 'Recent agent sessions'; Key = 0x52 }
            )) {
                Send-WtWindowKey -App $script:app -Vk $entry.Key -Ctrl -Shift -RequireForeground | Out-Null
                (Get-CombinedFilterState)[$entry.Id] | Should -BeTrue
                Assert-CombinedScopeRows @('ordinary', 'agent', 'history')
                (Get-CombinedElement SearchTextBox).SetFocus()
                Send-WtWindowKey -App $script:app -Vk 0x09 -Shift -RequireForeground | Out-Null
                (Get-CombinedElement FilterTabsButton).Current.HasKeyboardFocus | Should -BeTrue
                Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
                if ($entry.Id -eq 'RecentAgentSessionsFilterMenuItem') {
                    Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
                }
                Wait-Until -TimeoutSec 30 -Because 'physical menu navigation focuses the intended standard item' -Condition {
                    $focused = [Windows.Automation.AutomationElement]::FocusedElement
                    $focused -and $focused.Current.ProcessId -eq $script:app.Pid -and
                        $focused.Current.AutomationId -eq $entry.Id
                } | Out-Null
                $item = Get-CombinedElement $entry.Id
                $item.Current.Name | Should -Be $entry.Name
                $item.Current.ControlType | Should -Be ([Windows.Automation.ControlType]::MenuItem)
                "$($item.Current.AcceleratorKey) $($item.Current.HelpText)" |
                    Should -Match ('(?i)Ctrl.*Shift.*' + [char]$entry.Key)
                $item.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState |
                    Should -Be ([Windows.Automation.ToggleState]::On)
                $item.Current.HasKeyboardFocus | Should -BeTrue
                $activationKey = if ($entry.Id -eq 'AgentsOnlyFilterMenuItem') { 0x20 } else { 0x0D }
                Send-WtWindowKey -App $script:app -Vk $activationKey -RequireForeground | Out-Null
                (Get-CombinedFilterState)[$entry.Id] | Should -BeFalse
                (Get-CombinedElement FilterTabsButton).Current.HasKeyboardFocus | Should -BeTrue
                Assert-CombinedScopeRows @('ordinary', 'agent', 'history')
            }
            Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be $script:scopeQuery
            @(foreach ($tab in $script:scopeTabs) { Get-WtCapture -App $script:app -SessionId $tab.session_id }) | Should -Be $capture
        }
    }

    It 'Sidebar heading is static accessible text' {
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
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        @((Get-CombinedRows Live) | Where-Object {
            (Get-CombinedRowText $_).Contains("$script:marker-open-")
        }).Count | Should -BeGreaterThan 0
        (Get-CombinedScroll ItemsList).SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 100)
        Wait-Until -TimeoutSec 5 -Condition { @(Get-CombinedRows Recent).Count -gt 0 } | Out-Null
        @((Get-CombinedRows Recent) | Where-Object {
            (Get-CombinedRowText $_).Contains("$script:marker-history-")
        }).Count | Should -BeGreaterThan 0
        Set-CombinedView $false
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Assert-CombinedSearchState $false
        Set-CombinedView $true
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Assert-CombinedSearchState $false
        Assert-CombinedBounds
    }

    It 'History metadata shows time useful status and agent name with a leading icon' {
        $history = $script:history[0]
        Set-CombinedQuery $history.title
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
        $historical = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $history.sessionId)
        $historical.Count | Should -Be 1
        $historical[0].status | Should -BeIn @('Historical', 'Ended')
        Assert-CombinedHistoryMetadata -Title $history.title -Provider 'custom:combined-sidebar-fixture'
        foreach ($query in @('custom:combined-sidebar-fixture', 'combined-sidebar-fixture')) {
            Set-CombinedQuery $query
            Wait-Until -TimeoutSec 10 -Because 'history still matches canonical and display provider aliases' -Condition {
                @((Get-CombinedRows Recent) | Where-Object {
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
            $primaryFailure = $null
            try {
                Invoke-CombinedTabContext "$script:marker-open-$('{0:D2}' -f $index)"
                Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
                Invoke-CombinedTabContext "$script:marker-open-$('{0:D2}' -f $index)"
                Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
                Wait-Until -TimeoutSec 10 -Condition { (Get-CombinedAttachedTabCount) -eq $count - 1 } | Out-Null
                Set-CombinedView $true
                Set-CombinedQuery $title
                Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $nativeId -and $_.provider_id -eq 'copilot' -and $_.status -eq $status
                }).Count | Should -Be 1 -Because 'metadata assertions require the actual unattached live status'
                Assert-CombinedHistoryMetadata -Title $title -Status $status -Provider Copilot
                Set-CombinedQuery copilot
                Wait-Until -TimeoutSec 10 -Because 'live provider search still finds the detached identity' -Condition {
                    @((Get-CombinedRows Recent) | Where-Object {
                        (Get-CombinedRowText $_).Contains($title)
                    }).Count -eq 1
                } | Out-Null
            }
            catch { $primaryFailure = $_; throw }
            finally {
                Invoke-CombinedCheckedCleanup -PrimaryFailure $primaryFailure -Action {
                if ($status -eq 'Working') {
                    [IO.File]::WriteAllText($script:releasePromptPath, 'release')
                }
                Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
                Wait-Until -TimeoutSec 20 -Condition { (Get-CombinedAttachedTabCount) -eq $count } | Out-Null
                if ($status -eq 'Working') {
                    [IO.File]::WriteAllText($script:releasePromptPath, 'release')
                    Assert-AgentPaneText -App $script:app -PaneSessionId $baseline.PaneSessionId -Pattern "ACK_$holdMarker" -TimeoutSec 15
                }
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
        $primaryFailure = $null
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
            $cli = New-CombinedCliFixture 'other-window-native' -SessionId $sid
            $folder = $cli.Folder
            $shim = $cli.Shim
            $launchLog = $cli.Log
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
            [ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][long]$foreignHwnd) | Should -Be $sourceApp.Pid
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
                @{ Status = 'Idle'; Event = 'agent.stop' })) {
                Send-C388Hook $state.Event
                Wait-Until -TimeoutSec 20 -Condition {
                    @((Get-CombinedSnapshot).sessions | Where-Object {
                        $_.session_id -eq $sid -and $_.status -eq $state.Status -and $_.owner_window_id -eq $foreignOwner
                    }).Count -eq 1
                } | Out-Null
                $script:app = $sourceApp
                Set-CombinedView $true
                Set-CombinedQuery (Split-Path $folder -Leaf)
                Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
                Assert-CombinedHistoryMetadata -Title (Split-Path $folder -Leaf) -Status $state.Status -Provider Copilot -OtherWindow
                Assert-CombinedOwnershipButton OtherWindow | Out-Null
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
                        @(Get-CombinedRows Recent).Count -eq 0
                    } | Out-Null
                }
                finally {
                    Save-CombinedActionEvidence "owner-window-$($state.Status)" -Screenshot
                }
                @(Get-CombinedRows Recent).Count | Should -Be 0 -Because 'the owner window excludes its represented session'
            }
            $script:app = $sourceApp
            $windowCount = @(Get-WtWindows -App $sourceApp).Count
            $tabCount = @(Get-WtWindows -App $sourceApp | ForEach-Object { Get-WtTabs -App $sourceApp -WindowId $_.window_id }).Count
            $historyRows = @(Get-CombinedRows Recent)
            $historyRows.Count | Should -Be 1
            Set-WtWindowForeground -App $sourceApp -Attempts 3 -DelayMs 150 | Should -BeTrue
            $ownershipButton = Assert-CombinedOwnershipButton OtherWindow
            $buttonBounds = $ownershipButton.Current.BoundingRectangle
            Invoke-UiMouseDrag -App $sourceApp -FromX ([int]($buttonBounds.X + $buttonBounds.Width / 2)) `
                -FromY ([int]($buttonBounds.Y + $buttonBounds.Height / 2)) `
                -ToX ([int]($buttonBounds.X + $buttonBounds.Width / 2)) `
                -ToY ([int]($buttonBounds.Y + $buttonBounds.Height / 2)) -HoldMs 50 | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'the location button switches to the existing owner window' -Condition {
                $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
                $foreground.ToInt64() -eq [long]$foreignHwnd -and
                    [ItE2E.ItWtWin32Input]::GetWindowProcessId($foreground) -eq $sourceApp.Pid
            } | Out-Null
            Set-WtWindowForeground -App $sourceApp -Attempts 3 -DelayMs 150 | Should -BeTrue
            $historyRows = @(Get-CombinedRows Recent)
            $historyRows.Count | Should -Be 1
            $historyRows[0].SetFocus()
            $historyRows[0].Current.HasKeyboardFocus | Should -BeTrue
            try {
                Send-WtWindowKey -App $sourceApp -Vk 0x0D -RequireForeground | Out-Null
                Wait-Until -TimeoutSec 15 -Because 'physical History Enter focuses the existing owner window and native pane' -Condition {
                    $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
                    if ($foreground.ToInt64() -ne [long]$foreignHwnd) { return $false }
                    if ([ItE2E.ItWtWin32Input]::GetWindowProcessId($foreground) -ne $sourceApp.Pid) { return $false }
                    $active = Invoke-WtCli -App $sourceApp -Arguments @('get-pane-context', '--target', $tab.session_id)
                    $active.pane.session_id -eq $tab.session_id -and $active.pane.is_active -and
                        [string]$active.pane.window_id -eq $foreignWindow -and $active.pane.pid -eq $context.pane.pid
                } | Out-Null
            }
            finally {
                $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
                @{
                    trigger = 'physical Enter'; source_hwnd = $sourceApp.Hwnd; expected_owner_hwnd = $foreignHwnd
                    actual_foreground_hwnd = $foreground.ToInt64()
                    foreground_pid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($foreground)
                    expected_pid = $sourceApp.Pid; session_id = $sid; pane_session_id = $tab.session_id
                    context = Invoke-WtCli -App $sourceApp -Arguments @('get-pane-context', '--target', $tab.session_id)
                } | ConvertTo-Json -Depth 10 |
                    Set-Content -LiteralPath (Join-Path $script:evidence 'other-window-physical-enter.json')
            }
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
            Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
            Assert-CombinedHistoryMetadata -Title (Split-Path $folder -Leaf) -Status Idle -Provider Copilot
            (Get-CombinedRowText (Get-CombinedRows Recent)[0]) | Should -Not -Match 'another window'
            Invoke-CombinedHistoryRow -Title (Split-Path $folder -Leaf) -SessionId $sid -PaneId $tab.session_id -Status Idle
            Set-WtPaneFocus -App $sourceApp -SessionId $tab.session_id
            Send-WtInput -App $sourceApp -SessionId $tab.session_id -Text 'exit'
            Send-WtKeys -App $sourceApp -SessionId $tab.session_id -Keys @('Enter')
            Wait-Until -TimeoutSec 20 -Condition { $nativeProcess.HasExited } | Out-Null
            Wait-Until -TimeoutSec 30 -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $sid -and $_.status -in @('Ended', 'Historical') -and -not $_.owner_window_id
                }).Count -eq 1
            } | Out-Null
        }
        catch { $primaryFailure = $_; throw }
        finally {
            Invoke-CombinedCheckedCleanup -PrimaryFailure $primaryFailure -Action {
            $script:app = $sourceApp
            if ($tab -and $nativeProcess -and -not $nativeProcess.HasExited) {
                $currentNative = Get-Process -Id $nativeProcess.Id -ErrorAction Stop
                if ($currentNative.StartTime -ne $nativeProcess.StartTime -or $currentNative.Path -ne $shim) {
                    throw 'Cleanup refuses a changed native process/start-time/executable lease.'
                }
                Close-WtPane -App $sourceApp -SessionId $tab.session_id
                Wait-Until -TimeoutSec 15 -Condition { $nativeProcess.HasExited } | Out-Null
            }
            # Configuration backup restoration belongs to AfterAll, after package quiescence.
            if ($tab -and -not $nativeProcess) {
                throw 'Retaining fixture binaries: the launched native process lease was not captured.'
            }
            if (-not $nativeProcess -or $nativeProcess.HasExited) {
                foreach ($name in @('copilot.exe', 'copilot.obj')) {
                    $path = Join-Path $folder $name
                    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
                }
            }
            }
        }
    }

    It 'History background indicator restores the whole original tab (<Status>)' -ForEach @(
        @{ Status = 'Idle' }, @{ Status = 'Working' }
    ) {
        $fixture = New-CombinedCliFixture "background-$Status"
        $tab = $null
        $native = $null
        $primaryFailure = $null
        try {
            Set-CombinedView $false
            $tab = New-WtTab -App $script:app -Command "`"$($fixture.Shim)`" --session-id $($fixture.SessionId)" `
                -Cwd $fixture.Folder -Title "$script:marker-background-$Status"
            Wait-Until -TimeoutSec 20 -Condition { Test-Path $fixture.Log } | Out-Null
            $launch = Get-Content $fixture.Log | Select-Object -First 1 | ConvertFrom-Json
            $native = Get-Process -Id $launch.native_pid -ErrorAction Stop
            $native.Path | Should -Be $fixture.Shim
            $split = Split-WtPane -App $script:app -SessionId $tab.session_id -Direction right `
                -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            $panes = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id)
            $panes.Count | Should -Be 2
            $identities = @($panes | ForEach-Object {
                @{ sid = $_.session_id; pid = (Get-WtPaneStatus -App $script:app -SessionId $_.session_id).pid }
            })
            if ($Status -eq 'Working') {
                Send-CombinedCliHook -Fixture $fixture -PaneSessionId $tab.session_id -Event 'agent.tool.starting'
            }
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
            $count = Get-CombinedAttachedTabCount
            Invoke-CombinedOwnedGroupContext -Tab $tab -Title "$script:marker-background-$Status"
            Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
            Invoke-CombinedOwnedGroupContext -Tab $tab -Title "$script:marker-background-$Status"
            Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
            Wait-Until -TimeoutSec 10 -Condition { (Get-CombinedAttachedTabCount) -eq $count - 1 } | Out-Null
            Set-CombinedView $true
            Set-CombinedQuery (Split-Path $fixture.Folder -Leaf)
            Wait-Until -TimeoutSec 15 -Condition {
                @(Get-CombinedRows Recent).Count -eq 1 -and
                    @((Get-CombinedSnapshot).sessions | Where-Object {
                        $_.session_id -eq $fixture.SessionId -and $_.status -eq $Status -and $_.background_tab -eq $true
                    }).Count -eq 1
            } | Out-Null
            $button = Assert-CombinedOwnershipButton Background
            $beforeCollapse = Get-ActivePane -App $script:app
            $button.SetFocus()
            $button.Current.HasKeyboardFocus | Should -BeTrue
            $heading = Get-CombinedElement HistoryHeaderButton
            $collapse = $heading.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
            try {
                $collapse.Collapse()
                Wait-Until -TimeoutSec 5 -Condition {
                    $focused = [Windows.Automation.AutomationElement]::FocusedElement
                    $focused -and -not $focused.Current.IsOffscreen -and
                        [Windows.Automation.Automation]::Compare($focused, (Get-CombinedElement HistoryHeaderButton))
                } | Out-Null
                (Get-ActivePane -App $script:app).session_id | Should -Be $beforeCollapse.session_id
                (Get-CombinedAttachedTabCount) | Should -Be ($count - 1)
                @(Get-Content $fixture.Log).Count | Should -Be 1 -Because 'focus fallback must not launch or reattach the owned native session'
            }
            finally { $collapse.Expand() }
            Wait-Until -TimeoutSec 5 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
            $button = Assert-CombinedOwnershipButton Background
            $bounds = $button.Current.BoundingRectangle
            Invoke-UiMouseDrag -App $script:app -FromX ([int]($bounds.X + $bounds.Width / 2)) -FromY ([int]($bounds.Y + $bounds.Height / 2)) `
                -ToX ([int]($bounds.X + $bounds.Width / 2)) -ToY ([int]($bounds.Y + $bounds.Height / 2)) -HoldMs 50 | Out-Null
            Wait-Until -TimeoutSec 15 -Condition { (Get-CombinedAttachedTabCount) -eq $count } | Out-Null
            $restored = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id)
            $restored.Count | Should -Be 2
            foreach ($identity in $identities) {
                @($restored | Where-Object session_id -eq $identity.sid).Count | Should -Be 1
                (Get-WtPaneStatus -App $script:app -SessionId $identity.sid).pid | Should -Be $identity.pid
            }
            $native.HasExited | Should -BeFalse
            $launch.session_id | Should -Be $fixture.SessionId
            @(Get-Content $fixture.Log).Count | Should -Be 1
            Wait-Until -TimeoutSec 15 -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $fixture.SessionId -and
                        ([string]$_.pane_session_id).Trim('{}') -eq ([string]$tab.session_id).Trim('{}') -and
                        $_.background_tab -eq $false
                }).Count -eq 1 -and @(Get-CombinedRows Recent).Count -eq 0
            } | Out-Null
            Set-CombinedView $false
            Set-CombinedQuery "$script:marker-background-$Status"
            Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Live).Count -eq 1 } | Out-Null
            Save-CombinedActionEvidence "background-$Status-before-second-close" -Screenshot -SessionId $fixture.SessionId
            Invoke-CombinedOwnedGroupContext -Tab $tab -Title "$script:marker-background-$Status"
            Get-UiTree -App $script:app -Depth 9 |
                Set-Content (Join-Path $script:evidence "background-$Status-second-close-menu.tree.txt")
            Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
            Wait-Until -TimeoutSec 10 -Condition { (Get-CombinedAttachedTabCount) -eq $count - 1 } | Out-Null
            Set-CombinedView $true
            Set-CombinedQuery (Split-Path $fixture.Folder -Leaf)
            Save-CombinedActionEvidence "background-$Status-second-detach-before-wait" -SessionId $fixture.SessionId
            try {
                Wait-Until -TimeoutSec 15 -Condition {
                    $apiRows = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $fixture.SessionId)
                    $rows = @(Get-CombinedRows Recent)
                    $rowStates = @($rows | ForEach-Object {
                        $row = $_
                        @{
                            name = $row.Current.Name; offscreen = $row.Current.IsOffscreen
                            bounds = $row.Current.BoundingRectangle.ToString()
                            width = $row.Current.BoundingRectangle.Width
                            height = $row.Current.BoundingRectangle.Height
                            parts = @(Get-CombinedRawChildren $row | Where-Object {
                                $_.Current.AutomationId -in @('HistoryOwnershipButton', 'HistoryProviderIcon')
                            } | ForEach-Object {
                                @{
                                    id = $_.Current.AutomationId; name = $_.Current.Name
                                    offscreen = $_.Current.IsOffscreen; enabled = $_.Current.IsEnabled
                                    width = $_.Current.BoundingRectangle.Width
                                    height = $_.Current.BoundingRectangle.Height
                                }
                            })
                        }
                    })
                    @{
                        at = [DateTimeOffset]::UtcNow.ToString('o')
                        session_id = $fixture.SessionId; pane_session_id = $tab.session_id
                        api_rows = $apiRows; visible_rows = $rowStates
                        native_pid = $native.Id; native_alive = -not $native.HasExited
                    } | ConvertTo-Json -Depth 12 |
                        Set-Content (Join-Path $script:evidence "background-$Status-second-detach-readiness.json")
                    $qualified = @($apiRows | Where-Object {
                        $_.provider_id -eq 'copilot' -and $_.location -eq 'Host' -and
                            -not $_.session_universe -and $_.status -eq $Status -and
                            ([string]$_.pane_session_id).Trim('{}') -eq ([string]$tab.session_id).Trim('{}') -and
                            $_.owner_window_id -eq $script:app.WindowId -and $_.background_tab -eq $true
                    })
                    $qualified.Count -eq 1 -and $rowStates.Count -eq 1 -and
                        -not $rowStates[0].offscreen -and $rowStates[0].width -gt 0 -and $rowStates[0].height -gt 0 -and
                        @($rowStates[0].parts | Where-Object {
                            $_.id -eq 'HistoryOwnershipButton' -and $_.name -eq 'Restore background tab' -and
                                -not $_.offscreen -and $_.enabled -and $_.width -gt 0 -and $_.height -gt 0
                        }).Count -eq 1 -and
                        @($rowStates[0].parts | Where-Object {
                            $_.id -eq 'HistoryProviderIcon' -and $_.name -eq 'Copilot' -and
                                -not $_.offscreen -and $_.width -gt 0 -and $_.height -gt 0
                        }).Count -eq 1
                } | Out-Null
            }
            finally {
                try {
                    Save-CombinedActionEvidence "background-$Status-second-detach-after-wait" -SessionId $fixture.SessionId
                }
                catch { Write-Warning "Second-detach diagnostic capture failed: $_" }
                $context = $null
                $contextError = $null
                try {
                    $context = Invoke-WtCli -App $script:app -Arguments @(
                        'get-pane-context', '--target', $tab.session_id, '--max-lines', '0', '--max-chars', '0')
                }
                catch { $contextError = "$_" }
                try {
                    @{
                        at = [DateTimeOffset]::UtcNow.ToString('o')
                        native_pid = $native.Id; native_alive = -not $native.HasExited
                        original_panes = $identities; pane_context = $context; pane_context_error = $contextError
                    } | ConvertTo-Json -Depth 12 |
                        Set-Content (Join-Path $script:evidence "background-$Status-second-detach-context.json")
                }
                catch { Write-Warning "Second-detach diagnostic capture failed: $_" }
            }
            Assert-CombinedOwnershipButton Background | Out-Null
            $historyRow = @(Get-CombinedRows Recent)[0]
            Set-WtWindowForeground -App $script:app -Attempts 3 -DelayMs 150 | Should -BeTrue
            $historyRow.SetFocus()
            $historyRow.Current.HasKeyboardFocus | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 15 -Condition { (Get-CombinedAttachedTabCount) -eq $count } | Out-Null
            $restoredByEnter = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id)
            $restoredByEnter.Count | Should -Be 2
            foreach ($identity in $identities) {
                @($restoredByEnter | Where-Object session_id -eq $identity.sid).Count | Should -Be 1
                (Get-WtPaneStatus -App $script:app -SessionId $identity.sid).pid | Should -Be $identity.pid
            }
        }
        catch { $primaryFailure = $_; throw }
        finally {
            Invoke-CombinedCheckedCleanup -PrimaryFailure $primaryFailure -Action {
            if ($tab -and -not $native) {
                throw 'Retaining fixture binaries: the launched native process lease was not captured.'
            }
            if ($tab -and $native -and -not $native.HasExited) {
                $currentNative = Get-Process -Id $native.Id -ErrorAction Stop
                if ($currentNative.StartTime -ne $native.StartTime -or $currentNative.Path -ne $fixture.Shim) {
                    throw 'Cleanup refuses a changed native process/start-time/executable lease.'
                }
                Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
                foreach ($pane in @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id)) {
                    Close-WtPane -App $script:app -SessionId $pane.session_id
                }
                Wait-Until -TimeoutSec 15 -Condition { $native.HasExited } | Out-Null
            }
            if (-not $native -or $native.HasExited) {
                foreach ($name in @('copilot.exe', 'copilot.obj')) {
                    $path = Join-Path $fixture.Folder $name
                    if (Test-Path $path) { Remove-Item -LiteralPath $path }
                }
            }
            }
        }
    }

    It 'History Enter resumes an unbound native session in the current window (<RecentScope>)' -ForEach @(
        @{ RecentScope = $true }, @{ RecentScope = $false }
    ) {
        $sid = [guid]::NewGuid().ToString()
        $fixture = New-CombinedCliFixture 'external-resume' -ResumeSession $sid
        $external = $null
        $resumePane = $null
        $resumeProcess = $null
        $resolverOwned = $false
        $resolver = $null
        try {
            if (-not $env:ITE2E_CANONICAL_SHIM_DIRECTORY) { throw 'External resume requires an approved existing resolver directory.' }
            $directory = [IO.Path]::GetFullPath($env:ITE2E_CANONICAL_SHIM_DIRECTORY)
            $paths = ([Environment]::GetEnvironmentVariable('PATH', 'Machine') + ';' +
                [Environment]::GetEnvironmentVariable('PATH', 'User')).Split(';') |
                Where-Object { $_ } | ForEach-Object { [IO.Path]::GetFullPath($_) }
            $directory | Should -BeIn $paths
            Test-Path $directory -PathType Container | Should -BeTrue
            $resolver = Join-Path $directory 'copilot.exe'
            if (Test-Path $resolver) { throw 'Never replace an installed Copilot executable.' }
            $resolverHash = (Get-FileHash $fixture.Shim).Hash
            foreach ($name in @('copilot.exe', 'copilot.cmd', 'copilot.ps1', 'copilot.bat', 'copilot.com', 'copilot')) {
                $path = Join-Path $directory $name
                if (Test-Path -LiteralPath $path) { throw "Canonical fixture collision: $path" }
            }
            [IO.File]::Copy($fixture.Shim, $resolver, $false)
            $resolverOwned = $true
            $resolverCreatedUtc = (Get-Item -LiteralPath $resolver).CreationTimeUtc
            @{
                path = $resolver; source = $fixture.Shim; sha256 = $resolverHash
                created_utc = $resolverCreatedUtc.ToString('o'); existed_before_create = $false
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $fixture.Folder 'canonical-resolver-file-lease.json')
            $start = [Diagnostics.ProcessStartInfo]::new($fixture.Shim)
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.RedirectStandardInput = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            $start.WorkingDirectory = $fixture.Folder
            $start.Environment.Remove('WT_SESSION') | Out-Null
            $start.Environment.Remove('WT_COM_CLSID') | Out-Null
            $start.ArgumentList.Add('--external-session')
            $start.ArgumentList.Add($sid)
            $external = [Diagnostics.Process]::Start($start)
            $externalOutput = $external.StandardOutput.ReadToEndAsync()
            $externalError = $external.StandardError.ReadToEndAsync()
            Wait-Until -TimeoutSec 15 -Condition { Test-Path $fixture.Log } | Out-Null
            $externalRecord = Get-Content $fixture.Log | Select-Object -First 1 | ConvertFrom-Json
            $externalRecord.mode | Should -Be 'external'
            $externalRecord.session_id | Should -Be $sid
            $externalRecord.pane_session_id | Should -BeNullOrEmpty
            $externalRecord.native_pid | Should -Be $external.Id
            $external.HasExited | Should -BeFalse
            Register-CombinedUnboundSession -Fixture $fixture
            Wait-Until -TimeoutSec 20 -Condition {
                @((Get-CombinedSnapshot).sessions | Where-Object {
                    $_.session_id -eq $sid -and $_.provider_id -eq 'copilot' -and $_.status -eq 'Idle' -and
                        -not $_.pane_session_id -and -not $_.owner_window_id -and -not $_.background_tab
                }).Count -eq 1
            } | Out-Null
            $admitted = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)
            $admitted.Count | Should -Be 1
            $admitted[0].origin | Should -BeIn @($null, 'Unknown') -Because 'native admission does not imply an AgentPane owner'
            $admitted[0].location | Should -Be 'Host'
            $admitted[0] | ConvertTo-Json -Depth 8 |
                Set-Content (Join-Path $script:evidence 'external-master-admitted-row.json')
            Set-CombinedFilters -AgentsOnly $true -Recent $RecentScope
            Set-CombinedQuery (Split-Path $fixture.Folder -Leaf)
            Wait-Until -TimeoutSec 15 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
            Assert-CombinedOwnershipButton None
            Save-CombinedActionEvidence 'external-unbound-before-enter' -Screenshot
            $window = [string]$script:app.WindowId
            $beforeTabs = @(Get-WtTabs -App $script:app -WindowId $window)
            $beforeWindows = @(Get-WtWindows -App $script:app).Count
            $row = @(Get-CombinedRows Recent)[0]
            Set-WtWindowForeground -App $script:app -Attempts 3 -DelayMs 150 | Should -BeTrue
            $row.SetFocus()
            $row.Current.HasKeyboardFocus | Should -BeTrue
            $selectedBeforeModifiers = Get-ActivePane -App $script:app
            foreach ($modifier in @('Ctrl', 'Alt', 'Shift')) {
                $row = @(Get-CombinedRows Recent)[0]
                $row.SetFocus()
                $row.Current.HasKeyboardFocus | Should -BeTrue
                Send-WtWindowKey -App $script:app -Vk 0x0D -Ctrl:($modifier -eq 'Ctrl') `
                    -Alt:($modifier -eq 'Alt') -Shift:($modifier -eq 'Shift') -RequireForeground | Out-Null
                Start-Sleep -Milliseconds 300
                @(Get-Content $fixture.Log | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object mode -eq resume) |
                    Should -HaveCount 0 -Because "$modifier+Enter must not invoke native resume"
                (Get-ActivePane -App $script:app).session_id | Should -Be $selectedBeforeModifiers.session_id
                @(Get-WtTabs -App $script:app -WindowId $window).Count | Should -Be $beforeTabs.Count
            }
            $row = @(Get-CombinedRows Recent)[0]
            $row.SetFocus()
            @{
                trigger = 'physical Enter'; expected_window_id = $window; expected_pid = $script:app.Pid
                foreground_hwnd = [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64()
                foreground_pid = [ItE2E.ItWtWin32Input]::GetWindowProcessId([ItE2E.ItWtWin32Input]::GetForegroundWindow())
                focused_peer = @{
                    name = $row.Current.Name; type = $row.Current.ControlType.ProgrammaticName
                    runtime_id = @($row.GetRuntimeId()); has_keyboard_focus = $row.Current.HasKeyboardFocus
                    bounds = $row.Current.BoundingRectangle.ToString()
                }
                session = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)
            } | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $script:evidence 'external-focused-row-before-enter.json')
            Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
            try {
                Wait-Until -TimeoutSec 20 -Condition {
                    @(Get-Content $fixture.Log | ForEach-Object { $_ | ConvertFrom-Json } |
                        Where-Object { $_.mode -eq 'resume' -and $_.session_id -eq $sid }).Count -eq 1
                } | Out-Null
            }
            finally {
                $errorPeer = Get-CombinedElement HistoryMessage
                @{
                    session_id = $sid
                    focused_peer = [Windows.Automation.AutomationElement]::FocusedElement.Current.Name
                    history_error = if ($errorPeer) { @{
                        name = $errorPeer.Current.Name; is_offscreen = $errorPeer.Current.IsOffscreen
                        bounds = $errorPeer.Current.BoundingRectangle.ToString()
                    } } else { $null }
                    windows = @(Get-WtWindows -App $script:app); tabs = @(Get-WtTabs -App $script:app -WindowId $window)
                    launches = @(Get-Content $fixture.Log | ForEach-Object { $_ | ConvertFrom-Json })
                    session = @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $sid)
                } | ConvertTo-Json -Depth 10 | Set-Content (Join-Path $script:evidence 'external-after-enter.json')
                Get-UiTree -App $script:app -Depth 9 | Set-Content (Join-Path $script:evidence 'external-after-enter.tree.txt')
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'external-after-enter.png') | Out-Null
                foreach ($name in @('wta-main_master.log', 'terminal-agent-pane.log')) {
                    $log = Get-ItLogText -App $script:app -Name $name -SinceStart
                    $lines = @($log -split "`n" | Where-Object { $_ -match [regex]::Escape($sid) -or $_ -match '(?i)session.*activat|resume.*command|History.*Enter' } |
                        Select-Object -Last 80)
                    [IO.File]::WriteAllLines((Join-Path $script:evidence "external-activation-$name.txt"), [string[]]$lines)
                }
            }
            $resumed = @(Get-Content $fixture.Log | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object mode -eq 'resume')[0]
            $resumePane = $resumed.pane_session_id
            $resumePane | Should -Not -BeNullOrEmpty
            $resumed.native_pid | Should -Not -Be $external.Id
            $resumed.native_command_line | Should -Match ('--resume\s+"?' + [regex]::Escape($sid) + '"?(?:\s|$)')
            $resumeProcess = Get-Process -Id $resumed.native_pid -ErrorAction Stop
            $resumeProcess.Path | Should -Be $resolver
            $null = $resumeProcess.Handle
            $context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $resumePane)
            [string]$context.pane.window_id | Should -Be $window
            @(Get-WtTabs -App $script:app -WindowId $window).Count | Should -Be ($beforeTabs.Count + 1)
            @(Get-WtWindows -App $script:app).Count | Should -Be $beforeWindows
            $external.HasExited | Should -BeFalse -Because 'explicit resume must not terminate the external original'
            $resumed | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $script:evidence 'external-resume-proof.json')
        }
        finally {
            if ($resumePane) {
                if ($resumeProcess -and -not $resumeProcess.HasExited) {
                    $current = Get-Process -Id $resumeProcess.Id -ErrorAction Stop
                    if ($current.Path -ne $resolver -or $current.StartTime -ne $resumeProcess.StartTime) {
                        throw 'Resumed fixture process/start-time/executable lease changed; retain executable.'
                    }
                }
                Close-WtPane -App $script:app -SessionId $resumePane
                if ($resumeProcess -and -not $resumeProcess.WaitForExit(10000)) {
                    throw 'Owned resumed fixture did not exit normally; retain executable.'
                }
            }
            if ($external -and -not $external.HasExited) {
                $external.StandardInput.WriteLine('exit')
                $external.StandardInput.Flush()
                if (-not $external.WaitForExit(10000)) { throw 'Owned external fixture did not exit normally; retain executable.' }
            }
            if ($external -and $external.HasExited) {
                $externalOutput.GetAwaiter().GetResult() | Set-Content (Join-Path $fixture.Folder 'external.stdout.log')
                $externalError.GetAwaiter().GetResult() | Set-Content (Join-Path $fixture.Folder 'external.stderr.log')
            }
            if ($resolverOwned) {
                (Get-FileHash $resolver).Hash | Should -Be $resolverHash
                (Get-Item -LiteralPath $resolver).CreationTimeUtc | Should -Be $resolverCreatedUtc
                if (@(Get-Process -ErrorAction Stop | Where-Object Path -eq $resolver).Count) {
                    throw 'Owned resumed fixture remains active; retain executable.'
                }
                Remove-Item -LiteralPath $resolver
            }
            if (-not $external -or $external.HasExited) {
                foreach ($name in @('copilot.exe', 'copilot.obj')) {
                    $path = Join-Path $fixture.Folder $name
                    if (Test-Path $path) { Remove-Item -LiteralPath $path }
                }
            }
        }
    }

    It 'Combined sidebar search filters both sections and preserves the query' {
        Assert-CombinedSearchState $false
        Open-CombinedSearch
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Set-CombinedQuery "$script:marker-open-00"
        Wait-Until -TimeoutSec 10 -Because 'one open row and no unrelated historical rows remain' -Condition {
            @(Get-CombinedRows Live).Count -eq 1 -and @(Get-CombinedRows Recent).Count -eq 0
        } | Out-Null
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        Set-CombinedView $false
        Assert-CombinedSearchState $true
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be "$script:marker-open-00"
        Set-CombinedView $true
        Assert-CombinedSearchState $true
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be "$script:marker-open-00"
        Set-CombinedQuery "$script:marker-history-00"
        Wait-Until -TimeoutSec 10 -Because 'history-only search leaves Agents active' -Condition {
            @(Get-CombinedRows Live).Count -eq 0 -and @(Get-CombinedRows Recent).Count -eq 1
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
                        upper_count = @(Get-CombinedRows Live).Count
                        history_count = @(Get-CombinedRows Recent).Count
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
                @(Get-CombinedRows Live).Count -gt 1 -and @(Get-CombinedRows Recent).Count -gt 1
            } | Out-Null
            (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
            if (-not $close) {
                Set-CombinedQuery 'no-match-combined-sidebar'
                Wait-Until -TimeoutSec 10 -Condition {
                    @(Get-CombinedRows Live).Count -eq 0 -and @(Get-CombinedRows Recent).Count -eq 0
                } | Out-Null
            }
        }
        Set-CombinedView $false
        Assert-CombinedSearchState $false
        Set-CombinedView $true
        Assert-CombinedSearchState $false
    }

    It 'Combined sidebar mixed rows share one scroll viewport' {
        $scroll = Get-CombinedScroll ItemsList
        $scroll.Current.VerticallyScrollable | Should -BeTrue
        Assert-CombinedBounds
        $liveBefore = @(Get-CombinedRows Live | ForEach-Object { $_.Current.BoundingRectangle.Top })
        $liveBefore.Count | Should -BeGreaterThan 0
        $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 100)
        Wait-Until -TimeoutSec 5 -Condition { (Get-CombinedScroll ItemsList).Current.VerticalScrollPercent -gt 95 } | Out-Null
        $viewport = (Get-CombinedElement ItemsList).Current.BoundingRectangle
        @(Get-CombinedRows Recent | Where-Object {
            -not $_.Current.IsOffscreen -and $viewport.IntersectsWith($_.Current.BoundingRectangle)
        }).Count | Should -BeGreaterThan 0 -Because 'the same surface reaches actual recent rows'
        @(Get-CombinedRows Live | Where-Object {
            -not $_.Current.IsOffscreen -and $viewport.IntersectsWith($_.Current.BoundingRectangle)
        }).Count | Should -Be 0 -Because 'live rows must scroll away, not occupy a second fixed viewport'
        $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
        Wait-Until -TimeoutSec 5 -Condition { (Get-CombinedScroll ItemsList).Current.VerticalScrollPercent -eq 0 } | Out-Null
        @(Get-CombinedRows Live | Where-Object { -not $_.Current.IsOffscreen }).Count | Should -BeGreaterThan 0
        Assert-CombinedBounds
    }

    It 'Recent Sessions collapses without hiding live agents' {
        Set-CombinedQuery ''
        $scroll = Get-CombinedScroll ItemsList
        $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 100)
        $header = Get-CombinedElement HistoryHeaderButton
        $header.Current.Name | Should -Be 'Recent agent sessions'
        $pattern = $header.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
        $pattern.Current.ExpandCollapseState | Should -Be ([Windows.Automation.ExpandCollapseState]::Expanded)
        $sessions = @((Get-CombinedSnapshot).sessions.session_id | Sort-Object)
        $pattern.Collapse()
        Wait-Until -TimeoutSec 5 -Condition { @(Get-CombinedRows Recent).Count -eq 0 } | Out-Null
        $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
        @(Get-CombinedRows Live).Count | Should -BeGreaterThan 0 -Because 'collapse hides only recent sessions'
        @((Get-CombinedSnapshot).sessions.session_id | Sort-Object) | Should -Be $sessions
        Set-CombinedView $false
        Set-CombinedView $true
        (Get-CombinedElement HistoryHeaderButton).GetCurrentPattern(
            [Windows.Automation.ExpandCollapsePattern]::Pattern).Current.ExpandCollapseState |
            Should -Be ([Windows.Automation.ExpandCollapseState]::Collapsed)
        (Get-CombinedElement HistoryHeaderButton).GetCurrentPattern(
            [Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        Wait-Until -TimeoutSec 5 -Condition { @(Get-CombinedRows Recent).Count -gt 0 } | Out-Null
        @((Get-CombinedSnapshot).sessions.session_id | Sort-Object) | Should -Be $sessions
    }

    It 'Recent Sessions native expansion events match Content view' {
        Set-CombinedQuery ''
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Recent).Count -gt 0 } | Out-Null
        $header = Get-CombinedElement HistoryHeaderButton
        $header.Current.ControlType | Should -Be ([Windows.Automation.ControlType]::Button)
        $pattern = $header.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
        $pattern.Current.ExpandCollapseState | Should -Be ([Windows.Automation.ExpandCollapseState]::Expanded)
        $events = [ItE2E.SidebarExpansionEvents]::new($header)
        try {
            $pattern.Collapse()
            Wait-Until -TimeoutSec 5 -Condition { $events.Snapshot() -contains 'Collapsed' } | Out-Null
            $pattern.Collapse()
            Start-Sleep -Milliseconds 300
            @($events.Snapshot() | Where-Object { $_ -eq 'Collapsed' }) | Should -HaveCount 1
            $content = @(Get-CombinedRawChildren (Get-CombinedElement ItemsList) -ContentView)
            @($content | Where-Object { $_.Current.AutomationId -in @('HistoryProviderIcon', 'HistoryOwnershipButton') }) |
                Should -HaveCount 0
            @($content | Where-Object { $_.Current.Name.Contains($script:history[0].title) }) | Should -HaveCount 0
            Get-CombinedElement HistoryHeaderButton | Should -Not -BeNullOrEmpty
            $pattern.Expand()
            Wait-Until -TimeoutSec 5 -Condition {
                $events.Snapshot() -contains 'Expanded' -and @(Get-CombinedRows Recent).Count -gt 0
            } | Out-Null
            $pattern.Expand()
            Start-Sleep -Milliseconds 300
            @($events.Snapshot() | Where-Object { $_ -eq 'Expanded' }) | Should -HaveCount 1
            Set-CombinedQuery $script:history[0].title
            Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
            $row = @(Get-CombinedRows Recent)[0]
            $scrollItem = $row.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern)
            $scrollItem.ScrollIntoView()
            $row = @(Get-CombinedRows Recent)[0]
            $viewport = (Get-CombinedElement ItemsList).Current.BoundingRectangle
            $viewport.Contains($row.Current.BoundingRectangle) | Should -BeTrue
            Assert-CombinedHistoryMetadata -Title $script:history[0].title -Provider 'custom:combined-sidebar-fixture'
            $events.Snapshot() | ConvertTo-Json | Set-Content (Join-Path $script:evidence 'native-expansion-events.json')
        }
        finally {
            $events.Dispose()
            Set-CombinedQuery ''
            (Get-CombinedElement HistoryHeaderButton).GetCurrentPattern(
                [Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        }
    }

    It 'Collapsing focused Recent Sessions preserves the active shell' {
        Set-WtPaneFocus -App $script:app -SessionId $script:tabs[0].session_id
        $active = Get-ActivePane -App $script:app
        Set-CombinedQuery ''
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Recent).Count -gt 0 } | Out-Null
        $row = @(Get-CombinedRows Recent)[0]
        $row.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern).ScrollIntoView()
        $row.SetFocus()
        [Windows.Automation.Automation]::Compare($row, [Windows.Automation.AutomationElement]::FocusedElement) | Should -BeTrue
        $snapshot = @((Get-CombinedSnapshot).sessions.session_id | Sort-Object)
        $tabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId | ForEach-Object { "$($_.tab_id):$($_.title)" })
        $fixtureBefore = Get-Content -LiteralPath $script:fixtureLog -Raw
        $header = Get-CombinedElement HistoryHeaderButton
        $pattern = $header.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
        try {
            $pattern.Collapse()
            Wait-Until -TimeoutSec 5 -Because 'native collapse moves actual focus to a visible stable fallback' -Condition {
                $focus = [Windows.Automation.AutomationElement]::FocusedElement
                $focus -and $focus.Current.ProcessId -eq $script:app.Pid -and
                    -not $focus.Current.IsOffscreen -and $focus.Current.BoundingRectangle.Height -gt 0 -and
                    [Windows.Automation.Automation]::Compare($focus, (Get-CombinedElement HistoryHeaderButton))
            } | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Be $active.session_id
            @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId | ForEach-Object { "$($_.tab_id):$($_.title)" }) | Should -Be $tabs
            @((Get-CombinedSnapshot).sessions.session_id | Sort-Object) | Should -Be $snapshot
            (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
            Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be ''
            $newLog = (Get-Content -LiteralPath $script:fixtureLog -Raw).Substring($fixtureBefore.Length)
            $newLog | Should -Not -Match '\|(prompt|new|resume|load)\|'
            $pattern.Expand()
            Wait-Until -TimeoutSec 5 -Condition { @(Get-CombinedRows Recent).Count -gt 0 } | Out-Null
            (Get-CombinedRowText (Get-CombinedRows Recent)[0]) | Should -Match ([regex]::Escape($script:history[0].title))
            (Get-ActivePane -App $script:app).session_id | Should -Be $active.session_id
        }
        finally { $pattern.Expand() }
    }

    It 'Agents live tab mutations reconcile rows without refreshing history' {
        Set-CombinedQuery $script:marker
        $selected = $script:tabs[0]
        Set-WtPaneFocus -App $script:app -SessionId $selected.session_id
        $snapshot = @((Get-CombinedSnapshot).sessions | Where-Object { $_.session_id -in $script:history.sessionId } |
            ForEach-Object { "$($_.session_id):$($_.title)" } | Sort-Object)
        $created = $null
        try {
            $created = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            Invoke-RunCommand -App $script:app -SessionId $created.session_id -Command (
                "[Console]::Write([char]27+']0;$script:marker-mutation'+[char]7)") -SettleSec 1 | Out-Null
            Wait-Until -TimeoutSec 30 -Condition {
                @(Get-CombinedRows Live | Where-Object { (Get-CombinedRowText $_).Contains("$script:marker-mutation") }).Count -eq 1
            } | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Be $created.session_id
            $context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $created.session_id)
            [string]$context.pane.session_id | Should -Be $created.session_id
            [string]$context.pane.tab_id | Should -Be ([string]$created.tab_id)
            Invoke-RunCommand -App $script:app -SessionId $created.session_id -Command (
                "[Console]::Write([char]27+']0;$script:marker-mutation-renamed'+[char]7)") -SettleSec 1 | Out-Null
            Wait-Until -TimeoutSec 10 -Condition {
                @(Get-CombinedRows Live | Where-Object { (Get-CombinedRowText $_).Contains("$script:marker-mutation-renamed") }).Count -eq 1 -and
                    @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId | Where-Object title -eq "$script:marker-mutation-renamed").Count -eq 1
            } | Out-Null
            $renamed = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $created.session_id)
            $renamed.pane.pid | Should -Be $context.pane.pid
            [string]$renamed.pane.tab_id | Should -Be ([string]$created.tab_id)
            Set-WtPaneFocus -App $script:app -SessionId $selected.session_id
            Close-WtPane -App $script:app -SessionId $created.session_id
            Wait-Until -TimeoutSec 10 -Condition {
                @(Get-CombinedRows Live | Where-Object { (Get-CombinedRowText $_).Contains("$script:marker-mutation") }).Count -eq 0
            } | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Be $selected.session_id
            @((Get-CombinedSnapshot).sessions | Where-Object { $_.session_id -in $script:history.sessionId } |
                ForEach-Object { "$($_.session_id):$($_.title)" } | Sort-Object) | Should -Be $snapshot
            Get-UiValue -App $script:app -Selector SearchTextBox | Should -Be $script:marker
            (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
        }
        finally {
            if ($created -and @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId | Where-Object { $_.title -like "$script:marker-mutation*" }).Count) {
                Close-WtPane -App $script:app -SessionId $created.session_id
            }
        }
    }

        It 'Mixed Sidebar keyboard crosses the Recent Sessions boundary without activation' {
            Set-CombinedQuery $script:marker
            $active = Get-ActivePane -App $script:app
            $heading = Get-CombinedElement HistoryHeaderButton
            $heading.SetFocus()
            $pattern = $heading.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
            $pattern.Expand()
            Send-WtWindowKey -App $script:app -Vk 0x26 -RequireForeground | Out-Null
            $lastLive = [Windows.Automation.AutomationElement]::FocusedElement
            $lastLive.Current.ProcessId | Should -Be $script:app.Pid
            (Get-CombinedRowText $lastLive) | Should -Match ([regex]::Escape("$script:marker-open-"))
            Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
            [Windows.Automation.Automation]::Compare(
                [Windows.Automation.AutomationElement]::FocusedElement, $heading) | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
            $recent = [Windows.Automation.AutomationElement]::FocusedElement
            $recent.Current.ProcessId | Should -Be $script:app.Pid
            (Get-CombinedRowText $recent) | Should -Match ([regex]::Escape("$script:marker-history-"))
            (Get-CombinedElement ItemsList).Current.BoundingRectangle.Contains($recent.Current.BoundingRectangle) | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x26 -RequireForeground | Out-Null
            [Windows.Automation.Automation]::Compare(
                [Windows.Automation.AutomationElement]::FocusedElement, $heading) | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x09 -Shift -RequireForeground | Out-Null
            [Windows.Automation.Automation]::Compare(
                [Windows.Automation.AutomationElement]::FocusedElement, $lastLive) | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
            [Windows.Automation.Automation]::Compare(
                [Windows.Automation.AutomationElement]::FocusedElement, $heading) | Should -BeTrue
            $receipts = @()
            foreach ($key in @(0x20, 0x0D)) {
                Send-WtWindowKey -App $script:app -Vk $key -RequireForeground | Out-Null
                $pattern.Current.ExpandCollapseState | Should -Be ([Windows.Automation.ExpandCollapseState]::Collapsed)
                Send-WtWindowKey -App $script:app -Vk $key -RequireForeground | Out-Null
                $pattern.Current.ExpandCollapseState | Should -Be ([Windows.Automation.ExpandCollapseState]::Expanded)
                $receipts += @{ key = $key; active = (Get-ActivePane -App $script:app).session_id
                    heading_runtime_id = @($heading.GetRuntimeId()) }
            }
            (Get-ActivePane -App $script:app).session_id | Should -Be $active.session_id
            $receipts | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $script:evidence 'boundary-keyboard.json')
        }

        It 'Sidebar header search and options show their actual action tooltips' {
            foreach ($view in @($false, $true)) {
                Set-CombinedView $view
                $targets = @(
                    @{ Id = 'FilterTabsButton'; Text = 'Sidebar display options' }
                )
                $targets += @{ Id = 'SearchTabsButton'; Text = (Get-CombinedElement SearchTabsButton).Current.Name }
                Assert-CombinedHeaderCue Tabs
                foreach ($target in $targets) {
                    $element = Get-CombinedElement $target.Id
                    $element.Current.Name | Should -Be $target.Text
                    $element.Current.HelpText | Should -Be $target.Text
                    Invoke-ItOwnedHover -App $script:app -Selector $target.Id -DwellMs 1200 -DuringHover {
                    Wait-Until -TimeoutSec 5 -Because "the actual owned $($target.Id) tooltip renders" -Condition {
                        $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                        @($root.FindAll([Windows.Automation.TreeScope]::Descendants,
                            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,
                                [Windows.Automation.ControlType]::ToolTip)) | Where-Object {
                            $_.Current.ProcessId -eq $script:app.Pid -and -not $_.Current.IsOffscreen -and
                                $_.Current.BoundingRectangle.Height -gt 0 -and
                                (Get-CombinedRowText $_).Contains($target.Text)
                        }).Count -eq 1
                    } | Out-Null
                    }
                }
            }
        }
    It 'Combined sidebar keeps both sections usable after window resizing' {
        if (-not ('ItE2E.CombinedPhysicalResize' -as [type])) {
            Add-Type -Namespace ItE2E -Name CombinedPhysicalResize -MemberDefinition @'
            [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; public int Width { get { return Right - Left; } } public int Height { get { return Bottom - Top; } } }
            [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
            [StructLayout(LayoutKind.Sequential)] public struct PLACEMENT { public uint length, flags, showCmd; public POINT minPosition, maxPosition; public RECT normalPosition; }
            [DllImport("user32.dll", SetLastError=true)] public static extern bool GetWindowPlacement(IntPtr window, ref PLACEMENT placement);
            [DllImport("user32.dll", SetLastError=true)] public static extern bool SetWindowPlacement(IntPtr window, ref PLACEMENT placement);
            [DllImport("user32.dll", SetLastError=true)] static extern bool GetWindowRect(IntPtr window, out RECT rectangle);
            [DllImport("user32.dll", SetLastError=true)] static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
            [DllImport("user32.dll", SetLastError=true)] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
            [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
            [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr window);
            [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
            [StructLayout(LayoutKind.Sequential)] public struct MONITORINFO { public uint size; public RECT monitor, work; public uint flags; }
            [StructLayout(LayoutKind.Sequential)] public struct MINMAXINFO { public POINT reserved, maxSize, maxPosition, minTrack, maxTrack; }
            [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);
            [DllImport("user32.dll", SetLastError=true)] static extern bool GetMonitorInfo(IntPtr monitor, ref MONITORINFO info);
            [DllImport("user32.dll", SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wp, ref MINMAXINFO info, uint flags, uint timeout, out UIntPtr result);
            public static MONITORINFO MonitorInfo(IntPtr window) {
                IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
                if (previous == IntPtr.Zero) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                try {
                var info = new MONITORINFO(); info.size = (uint)Marshal.SizeOf(info);
                if (!GetMonitorInfo(MonitorFromWindow(window, 2), ref info))
                    throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                return info;
                } finally { SetThreadDpiAwarenessContext(previous); }
            }
            public static MINMAXINFO TrackingLimits(IntPtr window) {
                IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
                if (previous == IntPtr.Zero) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                try {
                var info = new MINMAXINFO(); UIntPtr result;
                if (SendMessageTimeout(window, 0x24, UIntPtr.Zero, ref info, 0x22, 1000, out result) == IntPtr.Zero)
                    throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                return info;
                } finally { SetThreadDpiAwarenessContext(previous); }
            }
            public static RECT PhysicalBounds(IntPtr window) {
                IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
                if (previous == IntPtr.Zero) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                try {
                    RECT rectangle;
                    if (!GetWindowRect(window, out rectangle)) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                    return rectangle;
                } finally { SetThreadDpiAwarenessContext(previous); }
            }
            public static void ResizePhysical(IntPtr window, int width, int height) {
                IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
                if (previous == IntPtr.Zero) throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                try {
                    if (!SetWindowPos(window, IntPtr.Zero, 0, 0, width, height, 0x0016))
                        throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
                } finally { SetThreadDpiAwarenessContext(previous); }
            }
'@
        }
        $hwnd = [IntPtr][long]$script:app.Hwnd
        [ItE2E.ItWtWin32Input]::GetAncestor($hwnd, 2) | Should -Be $hwnd -Because 'only the exact owned root frame is resized'
        [ItE2E.ItWtWin32Input]::GetWindowProcessId($hwnd) | Should -Be $script:app.Pid
        $script:app.OwnedProcess.HasExited | Should -BeFalse
        $script:app.OwnedProcess.Path | Should -Be (Join-Path $script:app.InstallLocation 'WindowsTerminal.exe')
        $dpi = [ItE2E.CombinedPhysicalResize]::GetDpiForWindow($hwnd)
        $dpi | Should -BeGreaterThan 0
        $savedFrame = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
        $placement = [ItE2E.CombinedPhysicalResize+PLACEMENT]::new()
        $placement.length = [Runtime.InteropServices.Marshal]::SizeOf($placement)
        [ItE2E.CombinedPhysicalResize]::GetWindowPlacement($hwnd, [ref]$placement) | Should -BeTrue
        try {
            $normalPlacement = $placement
            $normalPlacement.showCmd = 1
            $normalPlacement.flags = 0
            [ItE2E.CombinedPhysicalResize]::SetWindowPlacement($hwnd, [ref]$normalPlacement) | Should -BeTrue
            Wait-Until -TimeoutSec 10 -Because 'the owned frame restores to normal before physical resizing' -Condition {
                -not [ItE2E.CombinedPhysicalResize]::IsZoomed($hwnd) -and
                    -not [ItE2E.CombinedPhysicalResize]::IsIconic($hwnd)
            } | Out-Null
            $original = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
            $monitor = [ItE2E.CombinedPhysicalResize]::MonitorInfo($hwnd)
            $limits = [ItE2E.CombinedPhysicalResize]::TrackingLimits($hwnd)
            $largeHeight = [Math]::Min($monitor.work.Height, [Math]::Max(760, $original.Height + 160))
            if ($limits.maxTrack.Y -gt 0) { $largeHeight = [Math]::Min($largeHeight, $limits.maxTrack.Y) }
            [ItE2E.CombinedPhysicalResize]::ResizePhysical($hwnd, $original.Width, $largeHeight)
            try {
                Wait-Until -TimeoutSec 10 -Because 'establish a measured attainable large window before testing shrink' -Condition {
                    [Math]::Abs([ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd).Height - $largeHeight) -lt 3
                } | Out-Null
            }
            finally {
                $actualPlacement = [ItE2E.CombinedPhysicalResize+PLACEMENT]::new()
                $actualPlacement.length = [Runtime.InteropServices.Marshal]::SizeOf($actualPlacement)
                $placementRead = [ItE2E.CombinedPhysicalResize]::GetWindowPlacement($hwnd, [ref]$actualPlacement)
                @{ dpi = $dpi; requested_physical_height = $largeHeight
                    actual_frame = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
                    placement_read = $placementRead; placement = $actualPlacement
                    work_area = $monitor.work; tracking_limits = $limits
                    short_constraint_failure = ($limits.minTrack.Y -gt 640)
                } | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $script:evidence 'C384-physical-large-frame.json')
            }
            $large = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
            $large.Height | Should -BeGreaterThan 640 -Because 'the large state must actually exceed the strict short state'
            $limits.minTrack.Y | Should -BeLessOrEqual 640 -Because 'an OS minimum above 640 is an explicit constraint failure, not a skip'
            $smallHeight = 640
            [ItE2E.CombinedPhysicalResize]::ResizePhysical($hwnd, $large.Width, $smallHeight)
            try {
                Wait-Until -TimeoutSec 10 -Because 'the actual owned root frame reaches 640 physical pixels, strictly below 650' -Condition {
                    $frame = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
                    $frame.Height -lt 650 -and [Math]::Abs($frame.Height - $smallHeight) -lt 3
                } | Out-Null
            }
            finally {
                $frame = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
                @{ dpi = $dpi; requested_physical_height = 640; actual_physical_height = $frame.Height
                    large_physical_height = $large.Height; root_hwnd = $hwnd.ToInt64()
                    below_650 = ($frame.Height -lt 650); resize_api = 'SetWindowPos with PER_MONITOR_AWARE_V2; no logical/UIA conversion'
                    constraint_failure = ($frame.Height -ge 650)
                } | ConvertTo-Json | Set-Content (Join-Path $script:evidence 'C384-physical-short-frame.json')
            }
            $frame.Height | Should -BeLessThan 650 -Because 'a tall original or OS-clamped minimum must never satisfy the short-window contract'
            Wait-Until -TimeoutSec 10 -Because 'the shared viewport remains usable in the measured short frame' -Condition {
                (Get-CombinedElement ItemsList).Current.BoundingRectangle.Height -gt 20
            } | Out-Null
            Assert-CombinedBounds
            (Get-CombinedScroll ItemsList).SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 100)
            Wait-Until -TimeoutSec 5 -Condition {
                @(Get-CombinedRows Recent | Where-Object { -not $_.Current.IsOffscreen }).Count -gt 0
            } | Out-Null
            (Get-CombinedScroll ItemsList).SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
            Wait-Until -TimeoutSec 5 -Condition {
                @(Get-CombinedRows Live | Where-Object { -not $_.Current.IsOffscreen }).Count -gt 0
            } | Out-Null
            $small = (Get-CombinedElement ItemsList).Current.BoundingRectangle.Height
            [ItE2E.CombinedPhysicalResize]::ResizePhysical($hwnd, $large.Width, $large.Height)
            Wait-Until -TimeoutSec 10 -Because 'the actual window grows and lays out both sections' -Condition {
                [Math]::Abs([ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd).Height - $large.Height) -lt 3 -and
                    (Get-CombinedElement ItemsList).Current.BoundingRectangle.Height -gt $small
            } | Out-Null
            Assert-CombinedBounds
        }
        finally {
            [ItE2E.CombinedPhysicalResize]::SetWindowPlacement($hwnd, [ref]$placement) | Should -BeTrue
            Wait-Until -TimeoutSec 10 -Condition {
                $restored = [ItE2E.CombinedPhysicalResize]::PhysicalBounds($hwnd)
                [Math]::Abs($restored.Height - $savedFrame.Height) -lt 3 -and
                    [Math]::Abs($restored.Width - $savedFrame.Width) -lt 3 -and
                    (Get-CombinedElement ItemsList).Current.BoundingRectangle.Height -gt 20
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
            @(Get-CombinedRows Live).Count -eq 1
        } | Out-Null
        (Get-CombinedRowText (Get-CombinedRows Live)[0]) |
            Should -Match ([regex]::Escape("$script:marker-open-00"))
        $current = Get-AgentPaneSession -App $script:app -PaneSessionId $session.PaneSessionId
        $current.AcpSessionId | Should -Be $session.AcpSessionId
        $current.HelperProcessId | Should -Be $session.HelperProcessId
        Set-CombinedQuery "$script:marker-represented"
        Wait-Until -TimeoutSec 10 -Because 'open Idle agents remain above, not duplicated into History' -Condition {
            @(Get-CombinedRows Recent).Count -eq 0 -and
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
            @(Get-CombinedRows Live).Count -eq 1 -and @(Get-CombinedRows Recent).Count -eq 1
        } | Out-Null
        Set-CombinedQuery "$script:marker-history-00"
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
        (Get-CombinedRowText (Get-CombinedRows Recent)[0]) |
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
            @(Get-CombinedRows Recent).Count -eq 0
        } | Out-Null
        Save-CombinedActionEvidence "before-detach-$Status"
        Set-CombinedView $false
        Set-CombinedQuery ''
        $title = "$script:marker-open-$('{0:D2}' -f $Index)"
        $beforeCount = Get-CombinedAttachedTabCount
        $primaryFailure = $null
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
                    @(Get-CombinedRows Recent).Count -eq 1
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
                    @(Get-CombinedRows Recent).Count -eq 0
                } | Out-Null
            }
            catch {
                Save-CombinedActionEvidence "reattached-$Status" -Screenshot
                throw
            }
        }
        catch { $primaryFailure = $_; throw }
        finally {
            Invoke-CombinedCheckedCleanup -PrimaryFailure $primaryFailure -Action {
            if ($Status -eq 'Working') {
                [IO.File]::WriteAllText($script:releasePromptPath, 'release')
            }
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
            }
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
        Wait-Until -TimeoutSec 10 -Condition { @(Get-CombinedRows Recent).Count -eq 1 } | Out-Null
        $beforeTabs = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId))
        Invoke-CombinedHistoryRow -Title $history.title -SessionId $history.sessionId
        Wait-Until -TimeoutSec 30 -Because 'the real history action surfaces its unsupported-provider error' -Condition {
            $message = Get-CombinedElement HistoryMessage
            $message -and -not $message.Current.IsOffscreen -and
                $message.Current.Name -match 'provider is unavailable'
        } | Out-Null
        @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count | Should -Be $beforeTabs.Count
        @((Get-CombinedSnapshot).sessions | Where-Object session_id -eq $history.sessionId).Count | Should -Be 1
        @(Get-CombinedRows Recent).Count | Should -Be 1 -Because 'an unavailable provider must not consume the session'
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
        $primaryFailure = $null
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
                    @(Get-CombinedRows Recent).Count -eq 1
                } | Out-Null
            }
            catch {
                Save-CombinedActionEvidence 'known-provider-action' -Screenshot
                throw
            }
            Invoke-CombinedHistoryRow -Title (Split-Path $script:evidence -Leaf) -SessionId $sid -PaneId $tab.session_id -Status Working
            Wait-Until -TimeoutSec 20 -Because 'the actual History click restores the original tab through master and COM' -Condition {
                [string](Get-ActivePane -App $script:app).session_id -eq [string]$tab.session_id -and
                    (Get-CombinedAttachedTabCount) -eq $before
            } | Out-Null
            (Get-WtPaneStatus -App $script:app -SessionId $tab.session_id).pid | Should -Be $shellPid
            $current = Get-AgentPaneSession -App $script:app -PaneSessionId $tab.FixtureSession.PaneSessionId
            $current.AcpSessionId | Should -Be $tab.FixtureSession.AcpSessionId
            $current.HelperProcessId | Should -Be $tab.FixtureSession.HelperProcessId
        }
        catch { $primaryFailure = $_; throw }
        finally {
            Invoke-CombinedCheckedCleanup -PrimaryFailure $primaryFailure -Action {
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
            }
        }
    }
}
