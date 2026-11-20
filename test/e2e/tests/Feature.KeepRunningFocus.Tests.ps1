#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Exercises the shared focus-pane boundary used by Agent history and session
# activation: a kept tab must be reattached, not recreated from a command line.

BeforeDiscovery {
    $script:Ready = [bool]((Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: focus kept sessions' -Tag 'Feature', 'KeepRunning' -Skip:(-not $script:Ready) {
    BeforeEach {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\TabHeaderContext.ps1')
        . (Join-Path $PSScriptRoot 'helpers\PackageProfileActivation.ps1')
        . (Join-Path $PSScriptRoot 'helpers\KeptTabReattachment.ps1')
        if (-not ('ItE2E.KeptWindowGeometry' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
namespace ItE2E {
    public static class KeptWindowGeometry {
        [StructLayout(LayoutKind.Sequential)]
        public struct Rect { public int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)]
        struct MonitorInfo { public int Size; public Rect Monitor, Work; public uint Flags; }
        delegate bool EnumWindow(IntPtr window, IntPtr parameter);
        [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindow callback, IntPtr parameter);
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
        [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int count);
        [DllImport("user32.dll", SetLastError=true)] static extern bool GetWindowRect(IntPtr window, out Rect bounds);
        [DllImport("user32.dll", SetLastError=true)] static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
        [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
        [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr window, uint flags);
        [DllImport("user32.dll", SetLastError=true)] static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
        [DllImport("user32.dll", SetLastError=true)] static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
        public static Rect Frame(IntPtr window) {
            IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
            if (previous == IntPtr.Zero) throw new Win32Exception();
            try {
                Rect bounds;
                if (!GetWindowRect(window, out bounds)) throw new Win32Exception();
                return bounds;
            } finally { SetThreadDpiAwarenessContext(previous); }
        }
        public static void Place(IntPtr window) {
            IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
            if (previous == IntPtr.Zero) throw new Win32Exception();
            try {
                var info = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
                if (!GetMonitorInfo(MonitorFromWindow(window, 2), ref info)) throw new Win32Exception();
                int width = Math.Min(1000, info.Work.Right - info.Work.Left - 160);
                int height = Math.Min(700, info.Work.Bottom - info.Work.Top - 160);
                if (!SetWindowPos(window, IntPtr.Zero, info.Work.Left + 80, info.Work.Top + 80, width, height, 0x14))
                    throw new Win32Exception();
            } finally { SetThreadDpiAwarenessContext(previous); }
        }
        public static void RestoreFromTray(uint pid) {
            IntPtr target = IntPtr.Zero;
            int matches = 0;
            var brands = new[] { "Intelligent Terminal", "Windows Terminal Dev", "Windows Terminal Preview", "Windows Terminal Canary" };
            EnumWindows((window, parameter) => {
                uint owner;
                GetWindowThreadProcessId(window, out owner);
                if (owner == pid) {
                    var name = new StringBuilder(256);
                    GetClassName(window, name, name.Capacity);
                    var receiverClass = name.ToString();
                    if (Array.Exists(brands, brand => receiverClass == brand || receiverClass.StartsWith(brand + " ", StringComparison.Ordinal))) {
                        target = window;
                        matches++;
                    }
                }
                return true;
            }, IntPtr.Zero);
            if (matches != 1) throw new InvalidOperationException("Expected one owned Terminal notification receiver.");
            UIntPtr result;
            // WindowEmperor's notification callback, NIN_SELECT. Not a physical tray click.
            if (SendMessageTimeout(target, 0x403, UIntPtr.Zero, new IntPtr(0x400), 2, 10000, out result) == IntPtr.Zero)
                throw new Win32Exception();
        }
    }
}
'@
        }
        $script:app = $null
        $script:originalRunToken = $env:ITE2E_RUN_TOKEN
        $script:originalOwnedReceipt = $env:ITE2E_OWNED_PROCESS_RECEIPT
        $env:ITE2E_RUN_TOKEN = [guid]::NewGuid().ToString('N')
        $env:ITE2E_OWNED_PROCESS_RECEIPT = Join-Path $TestDrive ("keep-running-owned-$env:ITE2E_RUN_TOKEN.jsonl")
        function script:Close-KeptTabFromMenu {
            param([string]$PaneSessionId, [string]$Title)
            $visible = Find-UiElement -App $script:app -Selector KeepTabRunningMenuItem
            if ($visible -notmatch 'KeepTabRunningMenuItem\s+MenuItem') {
                Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $PaneSessionId -Title $Title
            }
            Wait-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
            $menu = [regex]::Match((Get-UiTree -App $script:app -Depth 8), '(?m)^\s*(\S+)\s+MenuItem "Close tab"')
            if (-not $menu.Success) { throw 'No exact Close tab menu item found.' }
            Invoke-UiElement -App $script:app -Selector $menu.Groups[1].Value | Out-Null
        }
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $requestLog = Join-Path $TestDrive ("keep-running-acp-{0}.log" -f [guid]::NewGuid().ToString('N'))
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($requestLog.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'; sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'; sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'
            tabLayout = 'vertical'
            showTabsInTitlebar = $true
            launchMode = 'default'
            'warning.confirmOnClose' = 'never'
            firstWindowPreference = 'defaultProfile'
            startupActions = ''
            windowingBehavior = 'useNew'
            autoErrorDetectionEnabled = $false
            acpAgent = 'custom:keep-running-fixture'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $encoded"
            acpModel = ''
        }
    }
    AfterEach {
        try {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
        finally {
            $env:ITE2E_RUN_TOKEN = $script:originalRunToken
            $env:ITE2E_OWNED_PROCESS_RECEIPT = $script:originalOwnedReceipt
        }
    }

    It 'Focusing a kept session reattaches its original tab' {
        $other = Get-ActivePane -App $script:app
        Wait-NewAgentPaneSession -App $script:app -TimeoutSec 40 | Out-Null
        $existingHelpers = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
        $title = 'IT-kept-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
        $target = New-WtTab -App $script:app -Command 'pwsh -NoProfile' -Title $title
        $split = Split-WtPane -App $script:app -SessionId $target.session_id -Direction right -Size 0.35 -Command 'pwsh -NoProfile'
        Set-WtPaneFocus -App $script:app -SessionId $target.session_id
        $helper = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $existingHelpers -TimeoutSec 40
        $helper.AcpSessionId | Should -Not -BeNullOrEmpty
        $before = @{}
        foreach ($id in @($target.session_id, $split.session_id, $helper.PaneSessionId)) {
            $before[$id] = Get-WtPaneStatus -App $script:app -SessionId $id
        }
        $marker = [guid]::NewGuid().ToString('N')
        Send-WtInput -App $script:app -SessionId $target.session_id -Text "`$keepFocusProbe = '$marker'`r"
        Send-WtInput -App $script:app -SessionId $target.session_id -Text 'Write-Output ($keepFocusProbe + ":before")'
        Send-WtKeys -App $script:app -SessionId $target.session_id -Keys Enter
        Wait-Until -TimeoutSec 10 -Because 'the original shell to initialize its retained variable' -Condition {
            (Get-WtCapture -App $script:app -SessionId $target.session_id) -match "$marker`:before"
        } | Out-Null
        $windowId = [string]$target.window_id
        $getTabCount = {
            @((Get-WtWindows -App $script:app) | Where-Object { [string]$_.window_id -eq $windowId })[0].tab_count
        }
        $tabCount = & $getTabCount
        $originalFrame = [ItE2E.KeptWindowGeometry]::Frame([IntPtr][long]$script:app.Hwnd)

        foreach ($cycle in 1..2) {
            Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $target.session_id -Title $title
            if ($cycle -eq 1) {
                Invoke-UiElement -App $script:app -Selector 'KeepTabRunningMenuItem' | Out-Null
                Close-TestOwnedTabFlyout -App $script:app -HeaderPoint $script:app.LastCanonicalHeaderPoint
                Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $target.session_id -Title $title
            }
            script:Close-KeptTabFromMenu -PaneSessionId $target.session_id -Title $title
            Wait-Until -TimeoutSec 10 -Because 'the kept tab to leave the visible tab strip' -Condition {
                (& $getTabCount) -eq ($tabCount - 1)
            } | Out-Null

            Set-WtPaneFocus -App $script:app -SessionId $other.session_id
            (& $getTabCount) | Should -Be ($tabCount - 1) -Because 'focusing another tab must not restore this one'
            { Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', [guid]::NewGuid().ToString()) } |
                Should -Throw
            (& $getTabCount) | Should -Be ($tabCount - 1) -Because 'an unknown pane must not restore an unrelated kept tab'

            Set-WtPaneFocus -App $script:app -SessionId $target.session_id
            (& $getTabCount) | Should -Be $tabCount
            foreach ($id in $before.Keys) {
                $current = Get-WtPaneStatus -App $script:app -SessionId $id
                $current.pid | Should -Be $before[$id].pid -Because 'reattach must retain each original shell/helper process'
                $current.state | Should -Be 'running'
            }
            $currentHelper = Get-AgentPaneSession -App $script:app -PaneSessionId $helper.PaneSessionId
            $currentHelper.HelperProcessId | Should -Be $helper.HelperProcessId
            $currentHelper.AcpSessionId | Should -Be $helper.AcpSessionId
            Send-WtInput -App $script:app -SessionId $target.session_id -Text "Write-Output (`$keepFocusProbe + ':after$cycle')`r"
            Wait-Until -TimeoutSec 10 -Because 'the restored original shell to retain variables and accept input' -Condition {
                (Get-WtCapture -App $script:app -SessionId $target.session_id) -match "$marker`:after$cycle"
            } | Out-Null
            Set-WtPaneFocus -App $script:app -SessionId $target.session_id
            (& $getTabCount) | Should -Be $tabCount -Because 'repeated activation must not duplicate the tab'
            $frame = [ItE2E.KeptWindowGeometry]::Frame([IntPtr][long]$script:app.Hwnd)
            foreach ($edge in 'Left', 'Top', 'Right', 'Bottom') {
                $frame.$edge | Should -Be $originalFrame.$edge -Because 'reattaching into an existing window must not move or resize it'
            }
        }
    }

    It 'Tray restoration preserves window geometry and live sessions across five closes' {
        $target = Get-ActivePane -App $script:app
        $helper = Wait-NewAgentPaneSession -App $script:app -TimeoutSec 40
        $helper.AcpSessionId | Should -Not -BeNullOrEmpty
        $pids = @{}
        foreach ($id in @($target.session_id, $helper.PaneSessionId)) {
            $pids[$id] = (Get-WtPaneStatus -App $script:app -SessionId $id).pid
        }
        $tabs = @(Get-WtTabs -App $script:app)
        $tabs.Count | Should -Be 1
        Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $target.session_id -Title $tabs[0].title
        Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
        Close-TestOwnedTabFlyout -App $script:app -HeaderPoint $script:app.LastCanonicalHeaderPoint
        [ItE2E.KeptWindowGeometry]::Place([IntPtr][long]$script:app.Hwnd)
        Start-Sleep -Milliseconds 500
        $baseline = [ItE2E.KeptWindowGeometry]::Frame([IntPtr][long]$script:app.Hwnd)

        foreach ($cycle in 1..5) {
            Send-WtWindowKey -App $script:app -Vk 0x73 -Alt -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'the owned window to close while its sessions remain alive' -Condition {
                @(Get-WtWindows -App $script:app).Count -eq 0
            } | Out-Null
            [ItE2E.KeptWindowGeometry]::RestoreFromTray([uint32]$script:app.Pid)
            $window = Wait-Until -TimeoutSec 20 -Because 'tray recovery to restore exactly the original tab' -Condition {
                $windows = @(Get-WtWindows -App $script:app)
                if ($windows.Count -eq 1 -and $windows[0].tab_count -eq 1) { $windows[0] }
            }
            $script:app.WindowId = [string]$window.window_id
            $script:app.Hwnd = Wait-Until -TimeoutSec 15 -Because 'the restored receiver to become visible' -Condition {
                Get-WtWindowHwnds -App $script:app | Where-Object pid -eq $script:app.Pid |
                    Select-Object -First 1 -ExpandProperty hwnd
            }
            Wait-Until -TimeoutSec 15 -Because 'the same retained pane to be active after layout' -Condition {
                (Get-ActivePane -App $script:app).session_id -eq $target.session_id
            } | Out-Null
            $frame = [ItE2E.KeptWindowGeometry]::Frame([IntPtr][long]$script:app.Hwnd)
            foreach ($edge in 'Left', 'Top', 'Right', 'Bottom') {
                [Math]::Abs($frame.$edge - $baseline.$edge) | Should -BeLessOrEqual 2 `
                    -Because "cycle $cycle must preserve the original frame, not accumulate drift"
            }
            foreach ($id in $pids.Keys) {
                $status = Get-WtPaneStatus -App $script:app -SessionId $id
                $status.pid | Should -Be $pids[$id]
                $status.state | Should -Be 'running'
            }
            $currentHelper = Get-AgentPaneSession -App $script:app -PaneSessionId $helper.PaneSessionId
            $currentHelper.HelperProcessId | Should -Be $helper.HelperProcessId
            $currentHelper.AcpSessionId | Should -Be $helper.AcpSessionId
        }
    }

    It 'Bare launch opens a new tab without attaching kept tabs' {
        Wait-NewAgentPaneSession -App $script:app -TimeoutSec 40 | Out-Null
        $retained = @()
        foreach ($index in 1..2) {
            $oldHelpers = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
            $title = 'IT-startup-kept-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
            $tab = New-WtTab -App $script:app -Command 'pwsh -NoProfile' -Title $title
            $helper = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $oldHelpers -TimeoutSec 40
            $helper.AcpSessionId | Should -Not -BeNullOrEmpty
            $paneIds = @($tab.session_id, $helper.PaneSessionId)
            if ($index -eq 1) {
                $split = Split-WtPane -App $script:app -SessionId $tab.session_id -Direction right -Size 0.35 -Command 'pwsh -NoProfile'
                $paneIds += $split.session_id
            }
            $pids = @{}
            foreach ($id in $paneIds) {
                $pids[$id] = (Get-WtPaneStatus -App $script:app -SessionId $id).pid
            }
            Set-WtPaneFocus -App $script:app -SessionId $tab.session_id
            Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $tab.session_id -Title $title
            Invoke-UiElement -App $script:app -Selector 'KeepTabRunningMenuItem' | Out-Null
            Close-TestOwnedTabFlyout -App $script:app -HeaderPoint $script:app.LastCanonicalHeaderPoint
            Invoke-TestTabHeaderContextMenu -App $script:app -PaneSessionId $tab.session_id -Title $title
            script:Close-KeptTabFromMenu -PaneSessionId $tab.session_id -Title $title
            $retained += [pscustomobject]@{ Title = $title; Shell = $tab.session_id; Helper = $helper; Pids = $pids }
        }

        Send-WtWindowKey -App $script:app -Vk 0x73 -Alt -RequireForeground | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'all terminal windows to close while the kept tabs stay alive' -Condition {
            @(Get-WtWindows -App $script:app).Count -eq 0
        } | Out-Null
        Get-Process -Id $script:app.Pid -ErrorAction Stop | Should -Not -BeNullOrEmpty

        # Explicit profile launches still open a new tab instead of consuming the
        # kept groups. The following bare AUMID activation matches Start menu Open.
        $profile = Get-WtSetting -App $script:app -Key 'defaultProfile'
        $profile | Should -Not -BeNullOrEmpty
        $script:app.AppUserModelId | Should -Not -BeNullOrEmpty
        $retainedPaneIds = @($retained | ForEach-Object { $_.Pids.Keys })
        $receiptRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $launch = Invoke-TestPackagedProfileActivation -App $script:app -Profile ([string]$profile) -ReceiptPath (
            Join-Path $receiptRoot ('keep-profile-activation-' + [guid]::NewGuid().ToString('N') + '.json'))
        $launch.HResult | Should -Be 0
        $profileWindow = Wait-Until -TimeoutSec 20 -Because 'the explicit profile launch to open one ordinary tab' -Condition {
            $windows = @(Get-WtWindows -App $script:app)
            if ($windows.Count -eq 1 -and $windows[0].tab_count -eq 1) { $windows[0] }
        }
        $script:app.Hwnd = Wait-Until -TimeoutSec 15 -Because 'the profile window to become visible' -Condition {
            Get-WtWindowHwnds -App $script:app | Where-Object pid -eq $script:app.Pid | Select-Object -First 1 -ExpandProperty hwnd
        }
        $script:app.WindowId = [string]$profileWindow.window_id
        $profileHwnd = [IntPtr][long]$script:app.Hwnd
        [ItE2E.ItWtWin32Input]::GetWindowProcessId($profileHwnd) | Should -Be $script:app.Pid
        [ItE2E.ItWtWin32Input]::GetAncestor($profileHwnd, 2) | Should -Be $profileHwnd
        [ItE2E.ItWtWin32Input]::IsWindowVisible($profileHwnd) | Should -BeTrue
        (Get-ActivePane -App $script:app).session_id | Should -Not -BeIn $retainedPaneIds
        foreach ($tab in $retained) {
            Test-UiElementExists -App $script:app -Selector $tab.Title |
                Should -BeFalse -Because 'explicit profile activation must leave each entire kept group detached'
            foreach ($id in $tab.Pids.Keys) {
                $status = Get-WtPaneStatus -App $script:app -SessionId $id
                $status.pid | Should -Be $tab.Pids[$id]
                $status.state | Should -Be 'running'
            }
            (Get-AgentPaneSession -App $script:app -PaneSessionId $tab.Helper.PaneSessionId).AcpSessionId |
                Should -Be $tab.Helper.AcpSessionId
        }
        Send-WtWindowKey -App $script:app -Vk 0x73 -Alt -RequireForeground | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'the profile window to close without terminating kept tabs' -Condition {
            @(Get-WtWindows -App $script:app).Count -eq 0
        } | Out-Null

        $script:app.AppUserModelId | Should -Not -BeNullOrEmpty
        Start-Process -FilePath explorer.exe -ArgumentList "shell:AppsFolder\$($script:app.AppUserModelId)" | Out-Null
        $newWindow = Wait-Until -TimeoutSec 25 -Because 'Start menu activation to open one new ordinary tab' -Condition {
            $windows = @(Get-WtWindows -App $script:app)
            if ($windows.Count -eq 1 -and $windows[0].tab_count -eq 1) { $windows[0] }
        }
        $script:app.Hwnd = Wait-Until -TimeoutSec 15 -Because 'the new window to become visible' -Condition {
            Get-WtWindowHwnds -App $script:app | Where-Object pid -eq $script:app.Pid | Select-Object -First 1 -ExpandProperty hwnd
        }
        $script:app.WindowId = [string]$newWindow.window_id
        (Get-ActivePane -App $script:app).session_id | Should -Not -BeIn $retainedPaneIds
        foreach ($tab in $retained) {
            Test-UiElementExists -App $script:app -Selector $tab.Title | Should -BeFalse -Because 'ordinary launch must leave kept tabs detached'
            foreach ($id in $tab.Pids.Keys) {
                $status = Get-WtPaneStatus -App $script:app -SessionId $id
                $status.pid | Should -Be $tab.Pids[$id]
                $status.state | Should -Be 'running'
            }
            $helper = Get-AgentPaneSession -App $script:app -PaneSessionId $tab.Helper.PaneSessionId
            $helper.AcpSessionId | Should -Be $tab.Helper.AcpSessionId
        }
        $expectedTabs = 1
        foreach ($tab in $retained) {
            Set-WtPaneFocus -App $script:app -SessionId $tab.Shell
            Wait-TestKeptTabReattachment -App $script:app -RequestedPane $tab.Shell -Title $tab.Title `
                -ExpectedTabs ($expectedTabs + 1) -RetainedPaneIds @($tab.Pids.Keys) `
                -OriginalPids $tab.Pids -OriginalHelper $tab.Helper
            $expectedTabs++
            Wait-Until -TimeoutSec 10 -Because 'the retained pane to become active in the reattached tab' -Condition {
                (Get-ActivePane -App $script:app).session_id -eq $tab.Shell -and
                    (Get-WtWindows -App $script:app).tab_count -eq $expectedTabs
            } | Out-Null
            foreach ($id in $tab.Pids.Keys) {
                (Get-WtPaneStatus -App $script:app -SessionId $id).pid | Should -Be $tab.Pids[$id]
            }
            (Get-WtWindows -App $script:app).tab_count | Should -Be $expectedTabs -Because 'only explicit session activation should attach each kept tab'
        }
        @(Get-WtWindows -App $script:app).Count | Should -Be 1
        (Get-WtWindows -App $script:app).tab_count | Should -Be 3
    }
}
