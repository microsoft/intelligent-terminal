#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE UI flow (the overlay click-through). Driven via winapp ui
# (the FRE is a XAML overlay with AutomationId buttons NextButton / SaveButton).
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §0 FRE overlay flow' -Tag 'Feature' -Skip:(-not $script:Ready) {

    Context 'FRE opens' {
        It 'FRE opens correctly (Welcome page shows on a fresh profile)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                Test-UiElementExists -App $app -Selector 'WelcomePage' -TimeoutSec 10 | Should -BeTrue
                Test-UiElementExists -App $app -Selector 'NextButton' -TimeoutSec 5 | Should -BeTrue
                $sidebar = Get-UiElement -App $app -Selector 'SidebarCardTitle'
                $autofix = Get-UiElement -App $app -Selector 'AutofixCardTitle'
                $sidebar | Should -Not -BeNullOrEmpty
                $autofix | Should -Not -BeNullOrEmpty
                $sidebar.name | Should -BeIn @(Get-WtReswTextValues -Key 'FreOverlay_Card2Title.Text')
                $autofix.name | Should -BeIn @(Get-WtReswTextValues -Key 'FreOverlay_Card1Title.Text')
                foreach ($name in @('SidebarImage', 'AutofixImage')) {
                    $image = Get-UiElement -App $app -Selector $name
                    $image | Should -Not -BeNullOrEmpty
                    $image.height | Should -BeGreaterThan 0
                    [math]::Abs($image.width / $image.height - 16.0 / 9.0) |
                        Should -BeLessThan 0.01 -Because 'the supplied screenshots must retain their 16:9 aspect ratio'
                }
                $language = Get-WtSetting -App $app -Key 'language'
                if (-not $language) { $language = (Get-UICulture).Name }
                if ($language -match '^(ar|fa|he|ur|ug)(-|$)|^qps-plocm$') {
                    $sidebar.x | Should -BeGreaterThan $autofix.x -Because 'RTL should mirror the sidebar-first card order'
                }
                else {
                    $sidebar.x | Should -BeLessThan $autofix.x -Because 'Sidebar and its copy must precede Autofix'
                }
            }
            finally { Stop-Terminal -App $app }
        }
    }

    Context 'FRE privacy/help link' {
        It 'FRE privacy / help link is present on the welcome page' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                $tree = Get-UiTree -App $app -Depth 8
                $tree | Should -Match 'Learn more|privacy|Privacy'
            }
            finally { Stop-Terminal -App $app }
        }
    }

    Context 'FRE completion' {
        It 'FRE can be completed (Next -> Save dismisses the overlay and marks complete)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                Test-FreShowing -App $app | Should -BeTrue
                Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
                Wait-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                # The overlay is dismissed…
                $dismissed = Test-Until -TimeoutSec 20 -Condition { -not (Test-FreShowing -App $app) }
                $dismissed | Should -BeTrue
                # …and the completion flag is persisted (written async after save/setup work).
                $flagged = Test-Until -TimeoutSec 20 -Condition { Get-FreCompleted -App $app }
                $flagged | Should -BeTrue
            }
            finally { Stop-Terminal -App $app }
        }

        It 'FRE save progress / completion leaves a usable terminal (settings valid)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
                Wait-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                Test-Until -TimeoutSec 20 -Condition { -not (Test-FreShowing -App $app) } | Out-Null
                # The terminal is usable: settings.json parses and a pane responds
                # (allow a moment for the page to settle after FRE save).
                (Get-WtSettingsObject -App $app) | Should -Not -BeNullOrEmpty
                $paneOk = Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { try { [bool](Get-ActivePane -App $app) } catch { $false } }
                $paneOk | Should -BeTrue
            }
            finally { Stop-Terminal -App $app }
        }
    }

    Context 'FRE close safety' {
        It 'FRE can be closed safely (closing the window mid-FRE leaves settings valid)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            Test-FreShowing -App $app | Should -BeTrue
            # Closing the window during FRE must not corrupt settings/state.
            Stop-Terminal -App $app
            # Relaunch (FRE was never completed, so settings.json must still parse).
            $app2 = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
            try {
                (Get-WtSettingsObject -App $app2) | Should -Not -BeNullOrEmpty
            }
            finally { Stop-Terminal -App $app2 }
        }
    }
}

Describe 'Feature §0 FRE Tab Mode' -Tag 'Feature', 'FreTabMode' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:package = Get-ItTestPackage
    }

    BeforeEach {
        $script:app = $null
        $script:configBackedUp = $false
        $script:targetApp = Resolve-ItApp -Package $script:package
        @(Get-WtProcessesForApp -App $script:targetApp -IncludePackageExecutables).Count |
            Should -Be 0 -Because 'FRE tests must not close user-owned package processes'
        $script:configBefore = @{}
        foreach ($path in @($script:targetApp.SettingsPath, $script:targetApp.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "A prior configuration backup requires recovery before this run: $path.e2ebak"
            }
            $script:configBefore[$path] = if (Test-Path -LiteralPath $path) {
                (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
            }
            else { $null }
        }
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT }
        else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidenceDir = Join-Path ([IO.Path]::GetFullPath($artifactRoot)) "fre-tab-mode\$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $script:evidenceDir -Force | Out-Null
        $wtaHash = (Get-FileHash -LiteralPath $script:targetApp.WtaPath -Algorithm SHA256).Hash
        $appHash = (Get-FileHash -LiteralPath (Join-Path $script:targetApp.InstallLocation 'TerminalApp.dll') -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_WTA_SHA256) { $wtaHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256 }
        if ($env:ITE2E_EXPECTED_APP_SHA256) { $appHash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256 }
        @{
            PackageFullName = $script:targetApp.PackageFullName
            InstallLocation = $script:targetApp.InstallLocation
            WtaSha256 = $wtaHash
            AppSha256 = $appHash
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'package.json') -Encoding utf8NoBOM

        Backup-WtConfig -App $script:targetApp
        $script:configBackedUp = $true
        $settings = Get-WtSettingsObject -App $script:targetApp
        if ($settings) {
            $settings.PSObject.Properties.Remove('tabLayout')
            $settings | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath $script:targetApp.SettingsPath -Encoding utf8
        }
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $fixtureLog = Join-Path $script:evidenceDir 'fixture.log'
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($fixtureLog.Replace("'", "''"))'"
        $script:freSettings = @{
            language = 'en-US'
            defaultProfile = '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'
            disabledProfileSources = @(
                'Windows.Terminal.PowershellCore', 'Windows.Terminal.Wsl',
                'Windows.Terminal.Azure', 'Windows.Terminal.VisualStudio',
                'Windows.Terminal.SSH', 'Microsoft.WSL'
            )
            # Keep the detection-off path away from the user's shell integration files.
            profiles = @{
                list = @(
                    @{ guid = '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'; name = 'FRE tab mode'; commandline = 'cmd.exe'; hidden = $false }
                    @{ guid = '{61c54bbd-c2c6-5271-96e7-009a87ff44bf}'; commandline = 'cmd.exe'; hidden = $true }
                )
            }
            acpAgent = 'custom:fre-tab-mode-fixture'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
            acpModel = ''
            autoErrorDetectionEnabled = $false
            autoFixEnabled = $false
            agentSessionManagementEnabled = $false
            'agentPane.yoloMode' = $false
            'warning.confirmOnClose' = 'never'
            firstWindowPreference = 'defaultProfile'
            startupActions = ''
            showTabsInTitlebar = $false
            alwaysShowTabs = $true
        }
    }

    AfterEach {
        try {
            if ($script:app) {
                Get-UiTree -App $script:app -Depth 18 |
                    Set-Content -LiteralPath (Join-Path $script:evidenceDir 'final-ui.txt') -Encoding utf8NoBOM
                @{
                    Picker = Get-UiElement -App $script:app -Selector 'TabModeComboBox'
                    Agent = Get-UiElement -App $script:app -Selector 'AgentComboBox'
                    Vertical = Get-UiElement -App $script:app -Selector 'SearchTabsButton'
                    Horizontal = Get-UiElement -App $script:app -Selector 'NewTabButton'
                } | ConvertTo-Json -Depth 10 |
                    Set-Content -LiteralPath (Join-Path $script:evidenceDir 'final-ui.json') -Encoding utf8NoBOM
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'final-ui.png') | Out-Null
            }
        }
        finally {
            if ($script:app) { Stop-Terminal -App $script:app -RestoreSettings $false }
            if ($script:configBackedUp) {
                if (@(Get-WtProcessesForApp -App $script:targetApp -IncludePackageExecutables).Count) {
                    throw 'Package processes remain; configuration backups are preserved rather than overwriting active settings.'
                }
                Restore-WtConfig -App $script:targetApp
                foreach ($path in $script:configBefore.Keys) {
                    $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
                    else { $null }
                    $actual | Should -Be $script:configBefore[$path] -Because 'FRE validation must restore the original settings/state bytes or absence'
                }
            }
        }
    }

    It 'FRE offers Sidebar as the initial Tab Mode' {
        $script:app = Start-Terminal -Package $script:package -ShowFre -Backup $false -CleanSettings $false -Settings $script:freSettings
        Invoke-UiElement -App $script:app -Selector 'NextButton' | Out-Null
        Wait-UiElement -App $script:app -Selector 'TabModeComboBox' | Out-Null
        $picker = Get-UiElement -App $script:app -Selector 'TabModeComboBox'
        $picker.name | Should -Be 'Tab Mode'
        $picker.y | Should -BeLessThan (Get-UiElement -App $script:app -Selector 'AgentComboBox').y
        @($picker.children | Where-Object type -eq 'ListItem').name | Should -BeExactly 'Sidebar'
        $window = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$script:app.Hwnd)
        $window.Current.ProcessId | Should -Be $script:app.Pid
        $control = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'TabModeComboBox'))
        $control.Current.HelpText | Should -BeExactly 'Toggle between the new sidebar or classic horizontal tabs view'
        (Get-WtSettingsObject -App $script:app).PSObject.Properties.Name | Should -Not -Contain 'tabLayout'
        $settingsHash = (Get-FileHash -LiteralPath $script:app.SettingsPath -Algorithm SHA256).Hash
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'default-sidebar.png') | Out-Null

        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue -Because 'the owned window must accept physical keyboard input'
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
            Should -Be 'SaveButton' -Because 'adding Tab Mode must not change the initial Enter-to-Save focus'
        $control.SetFocus()
        Send-WtWindowKey -App $script:app -Vk 0x28 -RequireForeground | Out-Null
        @((Get-UiElement -App $script:app -Selector 'TabModeComboBox').children | Where-Object type -eq 'ListItem').name |
            Should -BeExactly 'Horizontal'
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name |
            Should -Be 'ACP' -Because 'the existing agent-description link remains in keyboard order'
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
            Should -Be 'AgentComboBox' -Because 'Tab Mode must precede agent selection in keyboard order'
        (Get-FileHash -LiteralPath $script:app.SettingsPath -Algorithm SHA256).Hash | Should -Be $settingsHash
        Stop-Terminal -App $script:app -RestoreSettings $false
        $script:app = $null
        (Get-FileHash -LiteralPath $script:targetApp.SettingsPath -Algorithm SHA256).Hash | Should -Be $settingsHash
        Get-FreCompleted -App $script:targetApp | Should -BeFalse
    }

    It 'FRE retries Tab Mode after setup failure' {
        if ($script:targetApp.Package -ne 'IntelligentTerminal_rd9vj3e6a2mbr') {
            Set-ItResult -Skipped -Because 'the deterministic FRE setup-failure marker is supported only by Dev packages'
            return
        }
        $marker = Join-Path $script:targetApp.LocalStateDir 'fre-e2e-hooks-failure'
        if (Test-Path -LiteralPath $marker) { throw 'An existing FRE fault marker requires recovery before this test.' }
        try {
            New-Item -ItemType File -Path $marker | Out-Null
            $script:freSettings.tabLayout = 'horizontal'
            $script:app = Start-Terminal -Package $script:package -ShowFre -Backup $false -CleanSettings $false -Settings $script:freSettings
            Invoke-UiElement -App $script:app -Selector 'NextButton' | Out-Null
            Invoke-UiElement -App $script:app -Selector 'TabModeComboBox' | Out-Null
            Invoke-UiElement -App $script:app -Selector 'Sidebar' | Out-Null
            if (-not (Test-UiElementEnabled -App $script:app -Selector 'SessionManagementToggle')) {
                Set-ItResult -Skipped -Because 'session-management policy prevents the setup-failure preservation control'
                return
            }
            Invoke-UiElement -App $script:app -Selector 'SessionManagementToggle' | Out-Null
            Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
            Wait-UiElement -App $script:app -Selector 'ErrorText' -TimeoutSec 30 | Out-Null
            (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) |
                Should -Match '\[FRE\] E2E: forcing hooks install failure'
            Get-FreCompleted -App $script:app | Should -BeFalse
            Get-WtSetting -App $script:app -Key 'tabLayout' | Should -Be 'horizontal'
            @((Get-UiElement -App $script:app -Selector 'TabModeComboBox').children | Where-Object type -eq 'ListItem').name |
                Should -BeExactly 'Sidebar'
            (Get-UiElement -App $script:app -Selector 'SessionManagementToggle').toggleState | Should -Be 'off'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'setup-failed.png') | Out-Null

            Remove-Item -LiteralPath $marker
            Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
            Wait-Until -TimeoutSec 30 -Because 'retry to commit the retained Tab Mode choice' -Condition {
                (Get-FreCompleted -App $script:app) -and
                (Get-WtSetting -App $script:app -Key 'tabLayout') -eq 'vertical'
            } | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Not -BeNullOrEmpty
        }
        finally {
            if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker }
        }
    }

    It 'FRE saves the selected Tab Mode (<Saved>)' -TestCases @(
        @{ Initial = 'horizontal'; InitialLabel = 'Horizontal'; Saved = 'vertical'; Label = 'Sidebar' }
        @{ Initial = 'vertical'; InitialLabel = 'Sidebar'; Saved = 'horizontal'; Label = 'Horizontal' }
    ) {
        param($Initial, $InitialLabel, $Saved, $Label)
        $script:freSettings.tabLayout = $Initial
        $script:app = Start-Terminal -Package $script:package -ShowFre -Backup $false -CleanSettings $false -Settings $script:freSettings
        Invoke-UiElement -App $script:app -Selector 'NextButton' | Out-Null
        Wait-UiElement -App $script:app -Selector 'TabModeComboBox' | Out-Null
        @((Get-UiElement -App $script:app -Selector 'TabModeComboBox').children | Where-Object type -eq 'ListItem').name |
            Should -BeExactly $InitialLabel -Because 'initialization must preserve the explicit preference'
        Invoke-UiElement -App $script:app -Selector 'TabModeComboBox' | Out-Null
        Invoke-UiElement -App $script:app -Selector $Label | Out-Null
        Get-WtSetting -App $script:app -Key 'tabLayout' | Should -Be $Initial
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'selected-mode.png') | Out-Null
        Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'FRE Save to persist the chosen tab mode and complete' -Condition {
            (Get-FreCompleted -App $script:app) -and
            (Get-WtSetting -App $script:app -Key 'tabLayout') -eq $Saved
        } | Out-Null

        foreach ($phase in @('saved', 'restarted')) {
            if ($phase -eq 'restarted') {
                Stop-Terminal -App $script:app -RestoreSettings $false
                $script:app = $null
                $script:app = Start-Terminal -Package $script:package -PassFre $false -Backup $false -CleanSettings $false
            }
            Get-FreCompleted -App $script:app | Should -BeTrue
            Get-WtSetting -App $script:app -Key 'tabLayout' | Should -Be $Saved
            Test-UiElementExists -App $script:app -Selector 'NextButton' -TimeoutSec 1 | Should -BeFalse
            $layoutControl = if ($Saved -eq 'vertical') { 'SearchTabsButton' } else { 'NewTabButton' }
            Wait-Until -TimeoutSec 20 -Because "the $Saved tab layout to be visible after $phase" -Condition {
                $element = Get-UiElement -App $script:app -Selector $layoutControl
                $element -and -not $element.isOffscreen -and $element.width -gt 0
            } | Out-Null
            if ($Saved -eq 'horizontal') {
                Test-UiElementExists -App $script:app -Selector 'SearchTabsButton' -TimeoutSec 1 | Should -BeFalse
            }
            $activePane = Get-ActivePane -App $script:app
            $activePane.session_id | Should -Not -BeNullOrEmpty -Because 'the chosen layout must retain an active terminal after FRE'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "$phase-$Saved.png") | Out-Null
        }
    }
}
