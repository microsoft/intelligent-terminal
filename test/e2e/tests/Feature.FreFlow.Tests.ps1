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

Describe 'Feature §0 FRE simplified settings' -Tag 'Feature', 'FreTabMode' -Skip:(-not $script:Ready) {
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
        Stop-StaleItInstances -App $script:targetApp
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
            foreach ($key in @('tabLayout', 'renderAgentMarkdown', 'showTokenUsageAndCost')) {
                $settings.PSObject.Properties.Remove($key)
            }
            $settings | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath $script:targetApp.SettingsPath -Encoding utf8
        }
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $fixtureLog = Join-Path $script:evidenceDir 'fixture.log'
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($fixtureLog.Replace("'", "''"))'"
        # Isolate FRE persistence from the independent sidebar upgrade migration.
        $script:freState = @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
            sidebarRolloutVersion = 1
            sidebarRolloutAssignment = 'Sidebar'
            sidebarLayoutMigrationCompleted = $true
            sidebarIntroductionShown = $true
        }
        foreach ($key in $script:freState.Keys) {
            Set-WtState -App $script:targetApp -Key $key -Value $script:freState[$key] | Out-Null
        }
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

    It 'FRE omits advanced display choices (<Language>)' -TestCases @(
        @{ Language = 'en-US' }
        @{ Language = 'ar-SA' }
    ) {
        param($Language)
        $script:freSettings.language = $Language
        $script:app = Start-Terminal -Package $script:package -ShowFre -Backup $false -CleanSettings $false -Settings $script:freSettings
        Invoke-UiElement -App $script:app -Selector 'NextButton' | Out-Null
        Wait-UiElement -App $script:app -Selector 'AgentComboBox' | Out-Null
        $window = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$script:app.Hwnd)
        $window.Current.ProcessId | Should -Be $script:app.Pid
        foreach ($removed in @('TabModeComboBox', 'RenderAgentMarkdownToggle', 'ShowTokenUsageAndCostToggle')) {
            $window.FindFirst(
                [System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.PropertyCondition]::new(
                    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $removed)) |
                Should -BeNullOrEmpty -Because "$removed must not remain in the FRE automation tree"
        }
        $previous = $null
        foreach ($retained in @('AgentComboBox', 'PanePositionComboBox', 'ErrorDetectionComboBox', 'SessionManagementToggle')) {
            $control = Get-UiElement -App $script:app -Selector $retained
            $control | Should -Not -BeNullOrEmpty
            $control.name | Should -Not -BeNullOrEmpty
            $control.width | Should -BeGreaterThan 0
            $control.isOffscreen | Should -BeFalse
            if ($previous) { $control.y | Should -BeGreaterThan $previous.y }
            $previous = $control
        }
        $settingsHash = (Get-FileHash -LiteralPath $script:app.SettingsPath -Algorithm SHA256).Hash
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "settings-$Language.png") | Out-Null

        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue -Because 'the owned window must accept physical keyboard input'
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
            Should -Be 'SaveButton' -Because 'simplifying the form must retain Enter-to-Save focus'
        $agent = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'AgentComboBox'))
        $agent.SetFocus()
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
            Should -Be 'PanePositionComboBox'
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
            Should -Be 'ErrorDetectionComboBox'
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        $sessions = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'SessionManagementToggle'))
        $sessions | Should -Not -BeNullOrEmpty
        if ($sessions.Current.IsEnabled) {
            [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
                Should -Be 'SessionManagementToggle'
            Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        }
        $approval = $window.FindFirst(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'AutomaticApprovalToggle'))
        if ($approval -and $approval.Current.IsEnabled -and -not $approval.Current.IsOffscreen) {
            [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
                Should -Be 'AutomaticApprovalToggle'
            $approval.Current.Name | Should -Not -BeNullOrEmpty
            Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        }
        [System.Windows.Automation.AutomationElement]::FocusedElement.Current.AutomationId |
            Should -Be 'SaveButton' -Because 'Tab skips the removed display toggles'
        (Get-FileHash -LiteralPath $script:app.SettingsPath -Algorithm SHA256).Hash | Should -Be $settingsHash
        Stop-Terminal -App $script:app -RestoreSettings $false
        $script:app = $null
        (Get-FileHash -LiteralPath $script:targetApp.SettingsPath -Algorithm SHA256).Hash | Should -Be $settingsHash
        Get-FreCompleted -App $script:targetApp | Should -BeFalse
    }

    It 'FRE preserves display preferences through setup retry' {
        if ($script:targetApp.Package -ne 'IntelligentTerminal_rd9vj3e6a2mbr') {
            Set-ItResult -Skipped -Because 'the deterministic FRE setup-failure marker is supported only by Dev packages'
            return
        }
        $marker = Join-Path $script:targetApp.LocalStateDir 'fre-e2e-hooks-failure'
        if (Test-Path -LiteralPath $marker) { throw 'An existing FRE fault marker requires recovery before this test.' }
        try {
            New-Item -ItemType File -Path $marker | Out-Null
            $script:freSettings.tabLayout = 'horizontal'
            $script:freSettings.renderAgentMarkdown = $false
            $script:freSettings.showTokenUsageAndCost = $false
            # The hook-failure boundary applies to a supported built-in, not a custom ACP fixture.
            $script:freSettings.acpAgent = 'copilot'
            $script:app = Start-Terminal -Package $script:package -ShowFre -Backup $false -CleanSettings $false -Settings $script:freSettings
            Invoke-UiElement -App $script:app -Selector 'NextButton' | Out-Null
            $window = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$script:app.Hwnd)
            $agentCondition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'AgentComboBox')
            $agentControl = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $agentCondition)
            $agentControl | Should -Not -BeNullOrEmpty
            $agentRuntimeId = $agentControl.GetRuntimeId() -join ','
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
            Get-WtSetting -App $script:app -Key 'renderAgentMarkdown' | Should -BeFalse
            Get-WtSetting -App $script:app -Key 'showTokenUsageAndCost' | Should -BeFalse
            (Get-UiElement -App $script:app -Selector 'SessionManagementToggle').toggleState | Should -Be 'off'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'setup-failed.png') | Out-Null

            # Reload only the agent identity before retry so completion cannot bootstrap real Copilot.
            Set-WtAgent -App $script:app -Agent 'custom:fre-tab-mode-fixture' | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'native settings reload to reach the existing FRE overlay' -Condition {
                $log = Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart
                $log -match '_ReconcileAgentSettings: entered\. current\.acp=custom:fre-tab-mode-fixture '
            } | Out-Null
            $reloadedAgent = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $agentCondition)
            $reloadedAgent | Should -Not -BeNullOrEmpty
            ($reloadedAgent.GetRuntimeId() -join ',') | Should -Be $agentRuntimeId -Because 'retry must reuse the same FRE instance'
            Get-FreCompleted -App $script:app | Should -BeFalse
            Remove-Item -LiteralPath $marker
            Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
            Wait-Until -TimeoutSec 30 -Because 'retry to finish without changing display preferences' -Condition {
                Get-FreCompleted -App $script:app
            } | Out-Null
            Get-WtSetting -App $script:app -Key 'acpAgent' | Should -Be 'custom:fre-tab-mode-fixture'
            Get-WtSetting -App $script:app -Key 'tabLayout' | Should -Be 'horizontal'
            Get-WtSetting -App $script:app -Key 'renderAgentMarkdown' | Should -BeFalse
            Get-WtSetting -App $script:app -Key 'showTokenUsageAndCost' | Should -BeFalse
            (Get-ActivePane -App $script:app).session_id | Should -Not -BeNullOrEmpty
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'setup-retry-completed.png') | Out-Null
        }
        finally {
            if (Test-Path -LiteralPath $marker) { Remove-Item -LiteralPath $marker }
        }
    }

    It 'FRE preserves display preferences on Save and restart (<Preference>)' -TestCases @(
        @{ Preference = 'absent'; Layout = $null; Enabled = $null }
        @{ Preference = 'horizontal-off'; Layout = 'horizontal'; Enabled = $false }
        @{ Preference = 'vertical-on'; Layout = 'vertical'; Enabled = $true }
    ) {
        param($Preference, $Layout, $Enabled)
        if ($Layout) {
            $script:freSettings.tabLayout = $Layout
            $script:freSettings.renderAgentMarkdown = $Enabled
            $script:freSettings.showTokenUsageAndCost = $Enabled
        }
        $script:app = Start-Terminal -Package $script:package -ShowFre -Backup $false -CleanSettings $false -Settings $script:freSettings
        Invoke-UiElement -App $script:app -Selector 'NextButton' | Out-Null
        Wait-UiElement -App $script:app -Selector 'AgentComboBox' | Out-Null
        Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'FRE Save to complete without changing display preferences' -Condition {
            Get-FreCompleted -App $script:app
        } | Out-Null

        foreach ($phase in @('saved', 'restarted')) {
            if ($phase -eq 'restarted') {
                Stop-Terminal -App $script:app -RestoreSettings $false
                $script:app = $null
                $script:app = Start-Terminal -Package $script:package -PassFre $false -Backup $false -CleanSettings $false
            }
            Get-FreCompleted -App $script:app | Should -BeTrue
            $settings = Get-WtSettingsObject -App $script:app
            foreach ($key in @('tabLayout', 'renderAgentMarkdown', 'showTokenUsageAndCost')) {
                if ($Layout) {
                    $settings.$key | Should -Be $(if ($key -eq 'tabLayout') { $Layout } else { $Enabled })
                }
                else {
                    $settings.PSObject.Properties.Name | Should -Not -Contain $key
                }
            }
            Test-UiElementExists -App $script:app -Selector 'NextButton' -TimeoutSec 1 | Should -BeFalse
            $effectiveLayout = if ($Layout) { $Layout } else { 'vertical' }
            $layoutControl = if ($effectiveLayout -eq 'vertical') { 'SearchTabsButton' } else { 'NewTabButton' }
            Wait-Until -TimeoutSec 20 -Because "the $effectiveLayout tab layout to be visible after $phase" -Condition {
                $element = Get-UiElement -App $script:app -Selector $layoutControl
                $element -and -not $element.isOffscreen -and $element.width -gt 0
            } | Out-Null
            if ($effectiveLayout -eq 'horizontal') {
                Test-UiElementExists -App $script:app -Selector 'SearchTabsButton' -TimeoutSec 1 | Should -BeFalse
            }
            $activePane = Get-ActivePane -App $script:app
            $activePane.session_id | Should -Not -BeNullOrEmpty -Because 'the preserved layout must retain an active terminal after FRE'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "$phase-$Preference.png") | Out-Null
        }
    }
}
