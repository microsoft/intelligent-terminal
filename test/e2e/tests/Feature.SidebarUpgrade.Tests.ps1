#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# No real provider prompts. Preflight closes the exact-source Dev package before fixture setup.
Describe 'Feature: Sidebar upgrade' -Tag @('Feature', 'SidebarUpgrade') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        . (Join-Path $PSScriptRoot 'helpers\SidebarPersistenceFault.ps1')
        if ($env:ITE2E_PACKAGE -ne 'Dev') { throw 'Sidebar upgrade requires explicit Dev.' }
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Exact-source app and WTA hashes are required.'
        }
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        function Assert-UpgradeInactive {
            @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables) | Should -HaveCount 0
        }
        function Get-UpgradeVisiblePeer {
            param([string]$Id)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                $_.Current.ProcessId -eq $script:app.Pid -and -not $_.Current.IsOffscreen -and
                    $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0
            })
        }
        function Start-UpgradeFixture {
            param([bool]$Migration, [bool]$Shown, [string]$Layout = 'horizontal')
            Assert-UpgradeInactive
            Backup-WtConfig -App $script:target
            $script:ownsConfig = $true
            Clear-WtConfig -App $script:target
            Invoke-FrePass -App $script:target | Out-Null
            Set-WtState -App $script:target -Key sidebarLayoutMigrationCompleted -Value $Migration | Out-Null
            Set-WtState -App $script:target -Key sidebarIntroductionShown -Value $Shown | Out-Null
            Set-WtSettings -App $script:target -Settings @{
                language = 'en-US'; tabLayout = $Layout; startupActions = ''
                firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                autoErrorDetectionEnabled = $false; 'warning.confirmOnClose' = 'never'
                acpAgent = 'custom:sidebar-upgrade'; acpCustomCommand = 'cmd.exe /c exit'
            } | Out-Null
            $script:app = Start-Terminal -Package Dev -Backup $false -CleanSettings $false -PassFre $false
        }
        function Restart-UpgradeFixture {
            Stop-Terminal -App $script:app -RestoreSettings $false
            Assert-UpgradeInactive
            $script:app = Start-Terminal -Package Dev -Backup $false -CleanSettings $false -PassFre $false
        }
        function Invoke-UpgradePaletteAction {
            param([string]$Action)
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x50 -Ctrl -Shift -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 5 -Condition { Test-CommandPaletteOpen -App $script:app } | Out-Null
            Set-UiValue -App $script:app -Selector '_searchBox' -Value $Action | Out-Null
            $result = & (Get-Module ItE2E) {
                param($App, $Name)
                Invoke-WinAppUi -App $App -UiArgs @('invoke', $Name)
            } $script:app $Action
            $result.ExitCode | Should -Be 0
        }
    }
    BeforeEach {
        $script:app = $null
        $script:ownsConfig = $false
        Assert-UpgradeInactive
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Recover existing backup before upgrade fixtures: $path"
            }
        }
    }
    AfterEach {
        if ($script:app) {
            Stop-Terminal -App $script:app -RestoreSettings $false
        }
        if ($script:app -or $script:ownsConfig) {
            Assert-UpgradeInactive
            Restore-WtConfig -App $script:target
        }
    }
    It 'Sidebar upgrade migrates Horizontal only once' {
        Start-UpgradeFixture -Migration $false -Shown $true
        Wait-Until -TimeoutSec 10 -Because 'upgrade persists its completed migration and Sidebar preference' -Condition {
            (Get-WtStateObject -App $script:app).sidebarLayoutMigrationCompleted -eq $true -and
                (Get-WtSetting -App $script:app -Key tabLayout) -eq 'vertical'
        } | Out-Null
        @(Get-UpgradeVisiblePeer ItemsList) | Should -HaveCount 1
        Set-WtSetting -App $script:app -Key tabLayout -Value horizontal | Out-Null
        Restart-UpgradeFixture
        (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be horizontal
        (Get-WtStateObject -App $script:app).sidebarLayoutMigrationCompleted | Should -BeTrue
        @(Get-UpgradeVisiblePeer VerticalTabsHeader) | Should -HaveCount 0
        @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
    }
    It 'Sidebar introduction is independent of completed migration' {
        Start-UpgradeFixture -Migration $true -Shown $false
        (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be horizontal
        (Get-WtStateObject -App $script:app).sidebarIntroductionShown | Should -BeFalse
        @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
        Set-WtSetting -App $script:app -Key tabLayout -Value vertical | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'only actual visible Sidebar presentation consumes the pending tip' -Condition {
            @(Get-UpgradeVisiblePeer SidebarIntroductionTip).Count -eq 1 -and
                (Get-WtStateObject -App $script:app).sidebarIntroductionShown -eq $true
        } | Out-Null
        Restart-UpgradeFixture
        (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be vertical
        (Get-WtStateObject -App $script:app).sidebarIntroductionShown | Should -BeTrue
        @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
    }
    It 'Sidebar migration persistence failure warns and retries without false completion' {
        Backup-WtConfig -App $script:target
        $script:ownsConfig = $true
        $script:target | Add-Member ConfigBackupOwned $true -Force
        Clear-WtConfig -App $script:target
        Invoke-FrePass -App $script:target | Out-Null
        Set-WtState -App $script:target -Key sidebarLayoutMigrationCompleted -Value $false | Out-Null
        Set-WtState -App $script:target -Key sidebarIntroductionShown -Value $true | Out-Null
        Set-WtSettings -App $script:target -Settings @{
            language = 'en-US'; tabLayout = 'horizontal'; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            autoErrorDetectionEnabled = $false; 'warning.confirmOnClose' = 'never'
            acpAgent = 'custom:sidebar-upgrade'; acpCustomCommand = 'cmd.exe /c exit'
        } | Out-Null
        $lock = Open-TestSidebarPersistenceLock -App $script:target -Path $script:target.StatePath
        try {
            $script:app = Start-Terminal -Package Dev -Backup $false -CleanSettings $false -PassFre $false
            $messages = @(Get-WtReswTextValues -Key FailedToWriteToSettings)
            $messages.Count | Should -BeGreaterThan 0
            $warning = {
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $root.Current.ProcessId | Should -Be $script:app.Pid
                @($root.FindAll([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.Condition]::TrueCondition) | Where-Object {
                    $name = $_.Current.Name
                    $_.Current.ProcessId -eq $script:app.Pid -and -not $_.Current.IsOffscreen -and
                        $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0 -and
                        @($messages | Where-Object { $name.Contains($_) }).Count -gt 0
                })
            }
            Wait-Until -TimeoutSec 10 -Because 'a real shared-file write failure produces a visible warning' -Condition {
                @(& $warning).Count -gt 0
            } | Out-Null
            $warningPeers = @(& $warning)
            $dialog = $warningPeers[0]
            while ($dialog -and $dialog.Current.ClassName -notmatch 'ContentDialog') {
                $dialog = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($dialog)
            }
            $dialog | Should -Not -BeNullOrEmpty -Because 'only the identified warning dialog may be dismissed'
            $dialog.Current.ProcessId | Should -Be $script:app.Pid
            $okNames = @(Get-WtReswTextValues -Key Ok)
            $buttons = @($dialog.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.Condition]::TrueCondition) | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Button -and
                    $_.Current.Name -in $okNames -and $_.Current.ProcessId -eq $script:app.Pid -and
                    -not $_.Current.IsOffscreen -and $_.Current.IsEnabled
            })
            $buttons | Should -HaveCount 1 -Because 'ambiguous or foreign modal controls must fail, never receive guessed clicks'
            $buttons[0].GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
            Wait-Until -TimeoutSec 5 -Because 'the confirmed warning closes and exposes the effective Horizontal layout' -Condition {
                @(& $warning).Count -eq 0 -and @(Get-UpgradeVisiblePeer TabView).Count -eq 1
            } | Out-Null
            (Get-WtStateObject -App $script:app).sidebarLayoutMigrationCompleted | Should -BeFalse
            (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be horizontal
            @(Get-UpgradeVisiblePeer VerticalTabsHeader) | Should -HaveCount 0 -Because 'effective layout must agree with the rolled-back disk preference'
            @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
        }
        finally { $lock.Dispose() }
        Restart-UpgradeFixture
        Wait-Until -TimeoutSec 10 -Because 'releasing the real sharing fault permits the first durable migration' -Condition {
            (Get-WtStateObject -App $script:app).sidebarLayoutMigrationCompleted -eq $true -and
                (Get-WtSetting -App $script:app -Key tabLayout) -eq 'vertical'
        } | Out-Null
        @(Get-UpgradeVisiblePeer ItemsList) | Should -HaveCount 1
        Set-WtSetting -App $script:app -Key tabLayout -Value horizontal | Out-Null
        Restart-UpgradeFixture
        (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be horizontal
        (Get-WtStateObject -App $script:app).sidebarLayoutMigrationCompleted | Should -BeTrue
    }

    It 'Absent Sidebar upgrade flags migrate once without bypassing fresh FRE' {
        Backup-WtConfig -App $script:target
        $script:ownsConfig = $true
        Clear-WtConfig -App $script:target
        Set-WtSettings -App $script:target -Settings @{
            language = 'en-US'; startupActions = ''; firstWindowPreference = 'defaultProfile'
            windowingBehavior = 'useNew'; autoErrorDetectionEnabled = $false
            acpAgent = 'custom:sidebar-upgrade'; acpCustomCommand = 'cmd.exe /c exit'
        } | Out-Null
        $settings = Get-WtSettingsObject -App $script:target
        $settings.PSObject.Properties.Remove('tabLayout')
        $settings | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath $script:target.SettingsPath
        $state = Get-WtStateObject -App $script:target
        if (-not $state) { $state = [pscustomobject]@{} }
        foreach ($key in @('sidebarLayoutMigrationCompleted', 'sidebarIntroductionShown', 'agentFreCompleted')) {
            $state.PSObject.Properties.Remove($key)
        }
        $state | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath $script:target.StatePath
        $script:app = Start-Terminal -Package Dev -Backup $false -CleanSettings $false -PassFre $false -ShowFre
        Test-FreShowing -App $script:app | Should -BeTrue
        Wait-Until -TimeoutSec 10 -Condition {
            (Get-WtSetting -App $script:app -Key tabLayout) -eq 'vertical' -and
                (Get-WtStateObject -App $script:app).sidebarLayoutMigrationCompleted -eq $true
        } | Out-Null
        [bool](Get-WtStateObject -App $script:app).sidebarIntroductionShown | Should -BeFalse
        @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
        Stop-Terminal -App $script:app -RestoreSettings $false
        Assert-UpgradeInactive
        $script:app = Start-Terminal -Package Dev -Backup $false -CleanSettings $false -PassFre $false -ShowFre
        Test-FreShowing -App $script:app | Should -BeTrue
        (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be vertical
        [bool](Get-WtStateObject -App $script:app).sidebarIntroductionShown | Should -BeFalse
    }

    It 'Collapsed Sidebar defers its pending introduction until visible' {
        Start-UpgradeFixture -Migration $true -Shown $false
        Set-WtWindowForeground -App $script:app | Should -BeTrue
        Send-WtWindowKey -App $script:app -Vk 0x50 -Ctrl -Shift -RequireForeground | Out-Null
        Wait-Until -TimeoutSec 5 -Condition { Test-CommandPaletteOpen -App $script:app } | Out-Null
        Set-WtSetting -App $script:app -Key tabLayout -Value vertical | Out-Null
        Wait-Until -TimeoutSec 10 -Condition { @(Get-UpgradeVisiblePeer VerticalTabsHeader).Count -eq 1 } | Out-Null
        Set-UiValue -App $script:app -Selector '_searchBox' -Value 'Toggle sidebar' | Out-Null
        $result = & (Get-Module ItE2E) {
            param($App)
            Invoke-WinAppUi -App $App -UiArgs @('invoke', 'Toggle sidebar')
        } $script:app
        $result.ExitCode | Should -Be 0
        Wait-Until -TimeoutSec 5 -Condition {
            -not (Test-CommandPaletteOpen -App $script:app) -and
                @(Get-UpgradeVisiblePeer VerticalTabsHeader).Count -eq 0
        } | Out-Null
        foreach ($i in 1..4) {
            Start-Sleep -Milliseconds 500
            [bool](Get-WtStateObject -App $script:app).sidebarIntroductionShown | Should -BeFalse
            @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
        }
        Invoke-UpgradePaletteAction 'Toggle sidebar'
        Wait-Until -TimeoutSec 10 -Condition {
            @(Get-UpgradeVisiblePeer VerticalTabsHeader).Count -eq 1 -and
                @(Get-UpgradeVisiblePeer SidebarIntroductionTip).Count -eq 1 -and
                (Get-WtStateObject -App $script:app).sidebarIntroductionShown -eq $true
        } | Out-Null
    }

    It 'Owned windows share the durable Sidebar introduction without duplicates' {
        Start-UpgradeFixture -Migration $true -Shown $false
        $first = $script:app
        $before = @(Get-WtWindows -App $first).window_id
        $beforeHwnds = @(Get-WtWindowHwnds -App $first | Where-Object pid -eq $first.Pid).hwnd
        $tab = New-WtTab -App $first -Title 'ItE2E introduction second owned window' -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
        Invoke-UpgradePaletteAction 'Move tab to a new window'
        $secondId = Wait-Until -TimeoutSec 10 -Condition {
            $new = @(Get-WtWindows -App $first | Where-Object window_id -NotIn $before)
            if ($new.Count -eq 1) { $new[0].window_id }
        }
        $secondHwnd = Wait-Until -TimeoutSec 10 -Condition {
            $new = @(Get-WtWindowHwnds -App $first | Where-Object { $_.pid -eq $first.Pid -and $_.hwnd -notin $beforeHwnds })
            if ($new.Count -eq 1) { $new[0].hwnd }
        }
        $second = $first.PSObject.Copy()
        $second.Hwnd = $secondHwnd; $second.WindowId = $secondId
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        [ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][long]$second.Hwnd) | Should -Be $first.Pid
        $context = Invoke-WtCli -App $first -Arguments @('get-pane-context', '--target', $tab.session_id)
        [string]$context.pane.window_id | Should -Be ([string]$secondId)
        $script:app = $second
        Set-WtWindowForeground -App $second | Should -BeTrue
        Set-WtSetting -App $second -Key tabLayout -Value vertical | Out-Null
        Wait-Until -TimeoutSec 10 -Condition {
            @(Get-UpgradeVisiblePeer SidebarIntroductionTip).Count -eq 1 -and
                (Get-WtStateObject -App $second).sidebarIntroductionShown -eq $true
        } | Out-Null
        $script:app = $first
        Set-WtWindowForeground -App $first | Should -BeTrue
        Wait-Until -TimeoutSec 10 -Condition { @(Get-UpgradeVisiblePeer VerticalTabsHeader).Count -eq 1 } | Out-Null
        foreach ($i in 1..4) {
            Start-Sleep -Milliseconds 500
            @(Get-UpgradeVisiblePeer SidebarIntroductionTip) | Should -HaveCount 0
            (Get-WtStateObject -App $first).sidebarIntroductionShown | Should -BeTrue
        }
        @((Get-WtWindows -App $first).window_id | Where-Object { $_ -in @($first.WindowId, $secondId) }) | Should -HaveCount 2
    }
}
