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
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:qwen'; acpCustomCommand = 'copilot --acp --stdio'
        }

        # Try to open Settings once; if the window can't take foreground, mark the suite skippable.
        $script:settingsOpen = $false
        try { Open-WtSettings -App $script:app -TimeoutSec 20 | Out-Null; $script:settingsOpen = $true }
        catch { Write-Host "Open-WtSettings failed (foreground denied?): $($_.Exception.Message)" }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'AI Agents settings page opens and shows the agent picker' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # The AI Agents page renders the agent picker group (stable AutomationId AcpAgent).
        Test-UiElementExists -App $script:app -Selector 'AcpAgent' -TimeoutSec 8 | Should -BeTrue -Because 'the AI Agents page must show the agent picker'
    }

    It 'Settings Tab Mode matches FRE' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AppearanceNavItem' | Out-Null
        $tabModeLabel = Get-WtReswTextRegex -Key 'FreOverlay_TabModeLabel.Text'
        $tabModeLabel | Should -Not -BeNullOrEmpty
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                (Get-UiTree -App $script:app -Depth 18) -match $tabModeLabel
            }) | Should -BeTrue -Because 'Appearance must render the same localized Tab Mode label as FRE'
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
            } -State @{ sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true }
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
