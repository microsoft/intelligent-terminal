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
        $script:app = $null
        function script:Open-KeptTabContext {
            param([string]$Title)
            $matchesText = Find-UiElement -App $script:app -Selector $Title
            # The tab title also labels pane rows and TermControl; target its header.
            $header = [regex]::Match($matchesText, '(?m)^\s*(lbl-textview-\S+)\s+Text\s')
            if (-not $header.Success) { throw 'No exact tab-group header found.' }
            Invoke-UiClick -App $script:app -Selector $header.Groups[1].Value -Right | Out-Null
        }
        function script:Close-KeptTabFromMenu {
            param([string]$Title)
            $visible = Find-UiElement -App $script:app -Selector KeepTabRunningMenuItem
            if ($visible -notmatch 'KeepTabRunningMenuItem\s+MenuItem') {
                script:Open-KeptTabContext -Title $Title
            }
            Wait-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
            $menu = [regex]::Match((Get-UiTree -App $script:app -Depth 8), '(?m)^\s*(\S+)\s+MenuItem "Close tab"')
            if (-not $menu.Success) { throw 'No exact Close tab menu item found.' }
            Invoke-UiElement -App $script:app -Selector $menu.Groups[1].Value | Out-Null
        }
        if (-not ('ItE2E.KeptTabActivation' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ItE2E
{
    public static class KeptTabActivation
    {
        [ComImport, Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IApplicationActivationManager
        {
            [PreserveSig]
            int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
                [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
        }
        public static uint Launch(string appId, string arguments)
        {
            object instance = Activator.CreateInstance(Type.GetTypeFromCLSID(
                new Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")));
            try
            {
                uint processId;
                Marshal.ThrowExceptionForHR(((IApplicationActivationManager)instance)
                    .ActivateApplication(appId, arguments, 0, out processId));
                return processId;
            }
            finally { Marshal.ReleaseComObject(instance); }
        }
    }
}
'@
        }
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $requestLog = Join-Path $TestDrive ("keep-running-acp-{0}.log" -f [guid]::NewGuid().ToString('N'))
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($requestLog.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            language = 'en-US'
            tabLayout = 'vertical'
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
        if ($script:app) { Stop-Terminal -App $script:app }
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

        foreach ($cycle in 1..2) {
            script:Open-KeptTabContext -Title $title
            if ($cycle -eq 1) {
                Invoke-UiElement -App $script:app -Selector 'KeepTabRunningMenuItem' | Out-Null
                script:Open-KeptTabContext -Title $title
            }
            script:Close-KeptTabFromMenu -Title $title
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
            script:Open-KeptTabContext -Title $title
            Invoke-UiElement -App $script:app -Selector 'KeepTabRunningMenuItem' | Out-Null
            script:Open-KeptTabContext -Title $title
            script:Close-KeptTabFromMenu -Title $title
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
        $launchedPid = [ItE2E.KeptTabActivation]::Launch($script:app.AppUserModelId, "-p $profile")
        $launchedPid | Should -BeGreaterThan 0
        $profileWindow = Wait-Until -TimeoutSec 20 -Because 'the explicit profile launch to open one ordinary tab' -Condition {
            $windows = @(Get-WtWindows -App $script:app)
            if ($windows.Count -eq 1 -and $windows[0].tab_count -eq 1) { $windows[0] }
        }
        $script:app.Hwnd = Wait-Until -TimeoutSec 15 -Because 'the profile window to become visible' -Condition {
            Get-WtWindowHwnds -App $script:app | Where-Object pid -eq $script:app.Pid | Select-Object -First 1 -ExpandProperty hwnd
        }
        $script:app.WindowId = [string]$profileWindow.window_id
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
        (Get-ActivePane -App $script:app).session_id | Should -Not -BeIn @($retained.Shell)
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
