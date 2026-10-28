#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §1/§6 — the SETTINGS EDITOR UI itself, driven for real.
#
# Earlier these were written off as "the Settings editor can't be opened by the harness". That was
# wrong: WT opens Settings on the Ctrl+, accelerator, and while wtcli send-keys only reaches a
# pane's conpty, an OS-level keystroke to the WT WINDOW (Send-WtWindowKey / Open-WtSettings) does
# hit WT's accelerator handler. Once open, the Settings editor is a normal XAML surface — winapp
# can inspect / search / invoke / set-value its controls. This suite proves the editor is
# drivable end-to-end (page opens, nav works, the model control is visible for a custom agent).
#
# Foreground-focus dependent: Open-WtSettings needs the WT window to take foreground, so if that's
# denied (locked session / competing foreground app) the suite SKIPS rather than failing.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §1/§6 Settings editor UI (opened via Ctrl+, accelerator)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # A custom agent so the model control is expected to render on the AI Agents page.
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT }
        else { Join-Path $PSScriptRoot '..\artifacts' }
        $evidence = Join-Path $artifactRoot "settings-ui\$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $evidence -Force | Out-Null
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$((Join-Path $evidence 'fixture.log').Replace("'", "''"))'"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:settings-ui-fixture'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
            autoErrorDetectionEnabled = $false
            agentSessionManagementEnabled = $false
            defaultProfile = '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'
            disabledProfileSources = @(
                'Windows.Terminal.PowershellCore', 'Windows.Terminal.Wsl',
                'Windows.Terminal.Azure', 'Windows.Terminal.VisualStudio',
                'Windows.Terminal.SSH', 'Microsoft.WSL'
            )
            profiles = @{ list = @(
                @{ guid = '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'; commandline = 'cmd.exe' }
                @{ guid = '{61c54bbd-c2c6-5271-96e7-009a87ff44bf}'; commandline = 'cmd.exe'; hidden = $true }
            ) }
        }

        # Try to open Settings once; if the window can't take foreground, mark the suite skippable.
        $script:settingsOpen = $false
        try { Open-WtSettings -App $script:app -TimeoutSec 20 | Out-Null; $script:settingsOpen = $true }
        catch { Write-Host "Open-WtSettings failed (foreground denied?): $($_.Exception.Message)" }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'AI Agents settings page opens and shows the agent picker' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'LaunchNavItem'
        [xml]$manifest = Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'AppxManifest.xml') -Raw
        $startupTasks = @($manifest.SelectNodes("//*[local-name()='StartupTask' and @TaskId='StartTerminalOnLoginTask']"))
        $startupLabel = Get-WtReswTextRegex -Key 'Globals_StartOnUserLogin.Header'
        $startupLabel | Should -Not -BeNullOrEmpty
        $defaultProfileLabel = Get-WtReswTextRegex -Key 'Globals_DefaultProfile.Header'
        $defaultProfileLabel | Should -Not -BeNullOrEmpty
        (Get-UiTree -App $script:app -Depth 20) | Should -Match $defaultProfileLabel -Because 'Startup must render before checking optional settings'
        $shown = Test-Until -TimeoutSec 3 -IntervalSec 0.5 -Condition {
            (Get-UiTree -App $script:app -Depth 20) -match $startupLabel
        }
        $shown | Should -Be ($startupTasks.Count -gt 0) -Because 'a packaged app may intentionally omit the optional login task'
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # The AI Agents page renders the agent picker group (stable AutomationId AcpAgent).
        Test-UiElementExists -App $script:app -Selector 'AcpAgent' -TimeoutSec 8 | Should -BeTrue -Because 'the AI Agents page must show the agent picker'
    }

    It 'Settings retains the localized Tab Mode control' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AppearanceNavItem' | Out-Null
        $tabModeLabel = Get-WtReswTextRegex -Key 'Globals_TabLayout.Header'
        $tabModeLabel | Should -Not -BeNullOrEmpty
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                (Get-UiTree -App $script:app -Depth 18) -match $tabModeLabel
            }) | Should -BeTrue -Because 'removing the FRE choice must not remove Tab Mode from Appearance'
    }

    It 'Model selection visible: the model control shows on the AI Agents page for a custom agent' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # With a custom agent selected, the model control must be visible. The model container's
        # x:Name (AcpModel) is NOT surfaced as a winapp selector (winapp gives it a generated
        # grp-model-<hash> slug), so we assert its localized header instead — matched against an
        # ALL-LOCALES regex of AIAgents_AcpModel.Header (en-US "Model") so it holds on non-en-US
        # builds rather than hard-coding the English label.
        $rx = Get-WtReswTextRegex -Key 'AIAgents_AcpModel.Header'
        if (-not $rx) { $rx = '(?i)\bModel\b' }
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { (Get-UiTree -App $script:app -Depth 16) -match $rx }) |
            Should -BeTrue -Because 'the model control (localized "Model" header) must be visible when a custom agent is selected'
    }

    It 'Token usage toggle defaults on and persists when disabled' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'

        Test-UiElementExists -App $script:app -Selector 'ShowTokenUsageAndCostToggle' -TimeoutSec 8 |
            Should -BeTrue -Because 'Settings > Agents must expose the token usage preference'
        (Get-UiElement -App $script:app -Selector 'ShowTokenUsageAndCostToggle').toggleState |
            Should -Be 'on' -Because 'token usage and cost must be shown by default'

        Invoke-UiElement -App $script:app -Selector 'ShowTokenUsageAndCostToggle' | Out-Null
        Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
        Wait-Until -TimeoutSec 8 -Because 'the Settings toggle to persist showTokenUsageAndCost=false' -Condition {
            (Get-WtSettingsObject -App $script:app).showTokenUsageAndCost -eq $false
        } | Out-Null
        Assert-Setting -App $script:app -Key 'showTokenUsageAndCost' -Value $false
    }
}

Describe 'Feature profile global agent selection' -Tag 'Feature', 'ProfileGlobalAgent' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $script:profileGuid = '{42e4499d-63ee-48ce-9fe9-d8d72e0dfb98}'
        $script:profileName = 'ITE2E Global Agent'
        $script:fixedProfileGuid = '{42e4499d-63ee-48ce-9fe9-d8d72e0dfb99}'
        $script:fixedProfileName = 'ITE2E Fixed Agent'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            language = 'en-US'
            acpAgent = 'copilot'
            delegateAgent = 'claude'
            autoErrorDetectionEnabled = $false
            agentSessionManagementEnabled = $false
            defaultProfile = $script:fixedProfileGuid
            firstWindowPreference = 'defaultProfile'
            profiles = @{
                defaults = @{ agentPaneBackend = 'host:codex'; commandPaletteAgent = 'host:codex' }
                list = @(
                    @{
                        guid = $script:profileGuid
                        name = $script:profileName
                        commandline = 'cmd.exe'
                        agentPaneBackend = 'host:copilot'
                        commandPaletteAgent = 'host:claude'
                    }
                    @{
                        guid = $script:fixedProfileGuid
                        name = $script:fixedProfileName
                        commandline = 'cmd.exe'
                        agentPaneBackend = 'host:copilot'
                        commandPaletteAgent = 'host:claude'
                    }
                )
            }
        }

        function Get-ProfileAgentExpander {
            param([string]$Header)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::IsExpandCollapsePatternAvailableProperty, $true)
            $expander = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                Where-Object { $_.Current.Name.StartsWith($Header) } | Select-Object -First 1
            $expander | Should -Not -BeNullOrEmpty
            $expander
        }

        function Get-ProfileAgentCombo {
            param([string]$Header)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $expander = Get-ProfileAgentExpander -Header $Header
            $expander.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ComboBox)
            Wait-Until -TimeoutSec 8 -Because "$Header must expose its agent choices" -Condition {
                $script:profileAgentCombo = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                    Where-Object { $_.Current.Name.StartsWith($Header) } | Select-Object -First 1
                $null -ne $script:profileAgentCombo
            } | Out-Null
            $script:profileAgentCombo
        }

        function Assert-ProfileAgentPreview {
            param([string]$Header, [string]$ExpectedLabel)
            $expander = Get-ProfileAgentExpander -Header $Header
            $expander.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)
            # The separately bound accessible name can be correct while the visible preview is broken.
            $shown = Test-Until -TimeoutSec 8 -Condition {
                $script:profileAgentPreviewNames = @($expander.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                    Where-Object { -not $_.Current.IsOffscreen -and -not $_.Current.BoundingRectangle.IsEmpty } |
                    ForEach-Object { $_.Current.Name }
                $script:profileAgentPreviewNames -contains $ExpectedLabel -and
                    -not ($script:profileAgentPreviewNames -match 'Microsoft\.Terminal\.Settings\.Editor\.AgentEntry')
            }
            $shown | Should -BeTrue -Because "$Header must visibly preview '$ExpectedLabel', not [$($script:profileAgentPreviewNames -join '; ')]"
        }

        function Get-ProfileAgentSelection {
            param($Combo)
            $Combo.GetCurrentPattern([Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()[0].Current.Name
        }

        function Select-ProfileAgent {
            param($Combo, [string]$Name)
            $expand = $Combo.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
            $expand.Expand()
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem))
            Wait-Until -TimeoutSec 8 -Because "$Name must be a distinct selectable option" -Condition {
                $script:profileAgentChoice = $Combo.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
                $null -ne $script:profileAgentChoice
            } | Out-Null
            $script:profileAgentChoice.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
            if ($expand.Current.ExpandCollapseState -eq [Windows.Automation.ExpandCollapseState]::Expanded) { $expand.Collapse() }
        }

        function Save-ProfileAgents {
            param([AllowEmptyString()][string]$Pane, [AllowEmptyString()][string]$Palette,
                [string]$ProfileGuid = $script:profileGuid)
            Invoke-UiElement -App $script:app -Selector SaveButton | Out-Null
            try {
                # Empty WinRT strings serialize as explicit JSON null, not an absent override.
                Wait-Until -TimeoutSec 8 -Because 'both profile selections must persist exactly' -Condition {
                    $profile = @((Get-WtSettingsObject -App $script:app).profiles.list) |
                        Where-Object guid -eq $ProfileGuid | Select-Object -First 1
                    $null -ne $profile -and
                        $profile.PSObject.Properties.Name -contains 'agentPaneBackend' -and
                        $profile.PSObject.Properties.Name -contains 'commandPaletteAgent' -and
                        [string]$profile.agentPaneBackend -ceq $Pane -and [string]$profile.commandPaletteAgent -ceq $Palette
                } | Out-Null
            }
            catch {
                @((Get-WtSettingsObject -App $script:app).profiles.list) |
                    Where-Object { $_.guid -in @($script:profileGuid, $script:fixedProfileGuid) } |
                    Select-Object guid, name, agentPaneBackend, commandPaletteAgent | ConvertTo-Json | Write-Host
                throw
            }
        }

        function Open-ProfileAgentSettings {
            param([string]$Name = $script:profileName)
            Open-WtSettings -App $script:app | Out-Null
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $nav = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SettingsNav'))
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::IsSelectionItemPatternAvailableProperty, $true))
            $profile = $nav.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $profile | Should -Not -BeNullOrEmpty
            $profile.SetFocus()
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'profile navigation must update the visible page' -Condition {
                (Get-UiTree -App $script:app -Selector NavigationBreadcrumbBar -Depth 5) -match [regex]::Escape($Name)
            } | Out-Null
        }

        function Change-GlobalAgents {
            param([string]$Pane, [string]$Palette)
            Send-WtWindowKey -App $script:app -Vk 0x57 -Ctrl -Shift | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'the old Settings clone must close' -Condition {
                -not (Test-UiElementExists -App $script:app -Selector SettingsNav -TimeoutSec 1)
            } | Out-Null
            Set-WtSetting -App $script:app -Key acpAgent -Value $Pane | Out-Null
            Set-WtSetting -App $script:app -Key delegateAgent -Value $Palette | Out-Null
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Profile agent pickers distinguish global and fixed selections' {
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Match '^GitHub Copilot'
        (Get-ProfileAgentSelection $palette) | Should -Match '^Claude'

        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        Select-ProfileAgent $pane 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent $palette 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''

        Change-GlobalAgents -Pane claude -Palette codex
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Match '^GitHub Copilot'
        (Get-ProfileAgentSelection $palette) | Should -Match '^Claude'
        Save-ProfileAgents -Pane 'host:copilot' -Palette 'host:claude' -ProfileGuid $script:fixedProfileGuid

        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Be 'Use global setting (Claude)'
        (Get-ProfileAgentSelection $palette) | Should -Be 'Use global setting (Codex)'

        Change-GlobalAgents -Pane copilot -Palette claude
        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Be 'Use global setting (GitHub Copilot)'
        (Get-ProfileAgentSelection $palette) | Should -Be 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''
    }

    It 'Profile agent picker previews show selected labels' -Tag 'ProfileAgentPreview' {
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $fixedPane = Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')
        $fixedPalette = Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')
        Assert-ProfileAgentPreview -Header 'Agent pane agent' -ExpectedLabel $fixedPane
        Assert-ProfileAgentPreview -Header 'Command palette agent' -ExpectedLabel $fixedPalette

        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        Select-ProfileAgent $pane 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent $palette 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''
        Invoke-SettingsNav -App $script:app -NavItem LaunchNavItem | Out-Null
        Open-ProfileAgentSettings
        Assert-ProfileAgentPreview -Header 'Agent pane agent' -ExpectedLabel 'Use global setting (GitHub Copilot)'
        Assert-ProfileAgentPreview -Header 'Command palette agent' -ExpectedLabel 'Use global setting (Claude)'
    }
}

Describe 'Feature Settings editor language direction' -Tag 'Feature', 'SettingsRtl' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:languageResults = @{}
    }

    It 'Settings follows UI direction: <Language>' -ForEach @(
        @{ Language = 'ar-SA'; Rtl = $true }
        @{ Language = 'he-IL'; Rtl = $true }
        @{ Language = 'qps-plocm'; Rtl = $true }
        @{ Language = 'en-US'; Rtl = $false }
    ) {
        $app = $null
        $foregroundUnavailable = $false
        $script:languageResults[$Language] = 'Failed'
        try {
            $app = Start-Terminal -Package (Get-ItTestPackage) -TimeoutSec 60 -Settings @{
                language = $Language
                tabLayout = 'horizontal'
                initialCols = 120
                initialRows = 35
                firstWindowPreference = 'defaultProfile'
                minimizeToNotificationArea = $false
                alwaysShowNotificationIcon = $false
                showTokenUsageAndCost = $true
                'aiIntegration.coordinator.enabled' = $false
            } -State @{ featureRolloutId = '{00000000-0000-0000-0000-000000000001}'; sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'; sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true }
            if (-not (Test-WtWindowKeyFocusable -App $app)) {
                $foregroundUnavailable = $true
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+, accelerator'
                return
            }
            Open-WtSettings -App $app -TimeoutSec 25 | Out-Null
            $settingsRoot = Get-UiElement -App $app -Selector SettingsNav
            $settingsRoot | Should -Not -BeNullOrEmpty
            $center = $settingsRoot.x + $settingsRoot.width / 2
            foreach ($selector in 'SettingsSearchBox', 'SaveButton', 'ResetButton') {
                $element = Get-UiElement -App $app -Selector $selector
                $element | Should -Not -BeNullOrEmpty -Because "$selector must be visible"
                $element.isOffscreen | Should -BeFalse
                $onRight = $element.x + $element.width / 2 -gt $center
                $expectedOnRight = if ($selector -eq 'SettingsSearchBox') { $Rtl } else { -not $Rtl }
                $onRight | Should -Be $expectedOnRight -Because "$selector must follow $Language layout"
            }
            Invoke-SettingsNav -App $app -NavItem AIAgentsNavItem | Out-Null
            $tokenUsageToggle = Get-UiElement -App $app -Selector ShowTokenUsageAndCostToggle
            $tokenUsageToggle.toggleState | Should -Be 'on'
            $tokenUsageToggle.isOffscreen | Should -BeFalse
            ($tokenUsageToggle.x + $tokenUsageToggle.width / 2 -gt $center) |
                Should -Be (-not $Rtl) -Because "the setting control must follow $Language layout"
            Invoke-UiElement -App $app -Selector ShowTokenUsageAndCostToggle | Out-Null
            Invoke-UiElement -App $app -Selector SaveButton -TimeoutSec 15 | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'Save must persist the changed preference' -Condition {
                (Get-WtSettingsObject -App $app).showTokenUsageAndCost -eq $false
            } | Out-Null
            Assert-Setting -App $app -Key language -Value $Language
            Invoke-UiElement -App $app -Selector ShowTokenUsageAndCostToggle | Out-Null
            (Get-UiElement -App $app -Selector ShowTokenUsageAndCostToggle).toggleState | Should -Be 'on'
            Invoke-UiElement -App $app -Selector ResetButton -TimeoutSec 15 | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'Discard must restore the saved preference' -Condition {
                (Get-UiElement -App $app -Selector ShowTokenUsageAndCostToggle).toggleState -eq 'off'
            } | Out-Null
            Assert-Setting -App $app -Key showTokenUsageAndCost -Value $false
            Invoke-SettingsNav -App $app -NavItem LaunchNavItem | Out-Null
        }
        finally {
            if ($app) { Stop-Terminal -App $app | Out-Null }
            if ($foregroundUnavailable) { $script:languageResults[$Language] = 'Skipped' }
        }
        $script:languageResults[$Language] = 'Passed'
    }

    It 'Settings follows UI direction: complete matrix' {
        $script:languageResults.Values | Should -Not -Contain 'Failed' -Because 'every language must pass'
        if ($script:languageResults.Count -ne 4 -or $script:languageResults.Values -contains 'Skipped') {
            Set-ItResult -Skipped -Because 'not all four language cases completed'
            return
        }
    }
}
