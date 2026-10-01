#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

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
                [bool]($list -and -not $list.Current.IsOffscreen) -eq $Agents
            } | Out-Null
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
        $search = Get-CombinedElement SearchTextBox
        if (-not $search -or $search.Current.IsOffscreen) {
            Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
        }
        Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
        foreach ($id in @('ItemsList', 'HistoryList')) {
            (Get-CombinedScroll $id).SetScrollPercent(
                [Windows.Automation.ScrollPattern]::NoScroll, 0)
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
        Set-CombinedView $true
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Assert-CombinedBounds
    }

    It 'Combined sidebar search filters both sections and preserves the query' {
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-open-00" | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'one open row and no unrelated historical rows remain' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 1 -and @(Get-CombinedRows HistoryList).Count -eq 0
        } | Out-Null
        (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Set-CombinedView $false
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be "$script:marker-open-00"
        Set-CombinedView $true
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be "$script:marker-open-00"
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-history-00" | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'history-only search leaves Agents active' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 0 -and @(Get-CombinedRows HistoryList).Count -eq 1
        } | Out-Null
        foreach ($close in @($false, $true)) {
            if ($close) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            } else {
                Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
            }
            Wait-Until -TimeoutSec 10 -Because 'clear or close restores both real lists' -Condition {
                @(Get-CombinedRows ItemsList).Count -gt 1 -and @(Get-CombinedRows HistoryList).Count -gt 1
            } | Out-Null
            (Get-CombinedElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
            if (-not $close) {
                Set-UiValue -App $script:app -Selector SearchTextBox -Value 'no-match-combined-sidebar' | Out-Null
                Wait-Until -TimeoutSec 10 -Condition {
                    @(Get-CombinedRows ItemsList).Count -eq 0 -and @(Get-CombinedRows HistoryList).Count -eq 0
                } | Out-Null
            }
        }
        Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
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
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-open-00" | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'the unique represented tab is rendered in the upper list' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 1
        } | Out-Null
        (Get-CombinedRowText (Get-CombinedRows ItemsList)[0]) |
            Should -Match ([regex]::Escape("$script:marker-open-00"))
        $current = Get-AgentPaneSession -App $script:app -PaneSessionId $session.PaneSessionId
        $current.AcpSessionId | Should -Be $session.AcpSessionId
        $current.HelperProcessId | Should -Be $session.HelperProcessId
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-represented" | Out-Null
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
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-open-00" | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'same-title different-identity history remains alongside the unique represented upper row' -Condition {
            @(Get-CombinedRows ItemsList).Count -eq 1 -and @(Get-CombinedRows HistoryList).Count -eq 1
        } | Out-Null
        Set-UiValue -App $script:app -Selector SearchTextBox -Value "$script:marker-history-00" | Out-Null
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
        Set-UiValue -App $script:app -Selector SearchTextBox -Value $nativeTitle | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'the explicitly started root identity is represented before detachment' -Condition {
            @(Get-CombinedRows HistoryList).Count -eq 0
        } | Out-Null
        Save-CombinedActionEvidence "before-detach-$Status"
        Set-CombinedView $false
        Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
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
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $nativeTitle | Out-Null
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
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $nativeTitle | Out-Null
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
        Set-UiValue -App $script:app -Selector SearchTextBox -Value $history.title | Out-Null
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
        Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
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
            Set-UiValue -App $script:app -Selector SearchTextBox -Value (Split-Path $script:evidence -Leaf) | Out-Null
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
