#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: Agent only menu leaves Sidebar search' -Tag @('Feature', 'SidebarAgentOnlySearch') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        if ($env:ITE2E_PACKAGE -ne 'Dev' -or
            -not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Select the exact-source Dev package and provide its App and WTA hashes.'
        }
        $target = Resolve-ItApp -Package Dev
        $script:settingsPath = $target.SettingsPath
        Stop-StaleItInstances -App $target
        (Get-FileHash (Join-Path $target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256

        $profileId = '{' + [guid]::NewGuid() + '}'
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; defaultProfile = $profileId; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            sidebarShowAgentsOnly = $false; sidebarShowRecentAgentSessions = $false
            profiles = @{ list = @(@{
                guid = $profileId; name = 'Agent only search fixture'
                commandline = 'pwsh.exe -NoLogo -NoProfile'
            }) }
        }
        $script:app | Add-Member RequireOwnedForeground $true -Force

        function Get-SidebarElement {
            param([string]$Id)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.FindFirst(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id))
        }

        function Get-VisibleTabRows {
            $list = Get-SidebarElement ItemsList
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            $heading = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton')
            @($list.FindAll([Windows.Automation.TreeScope]::Children, $condition) |
                Where-Object {
                    -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                    -not $_.FindFirst([Windows.Automation.TreeScope]::Descendants, $heading)
                })
        }

    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'exits search and applies Agent only when its menu item is clicked on and off' {
        @(Get-VisibleTabRows).Count | Should -BeGreaterThan 0

        foreach ($agentsOnly in @($true, $false)) {
            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'Sidebar search opens' -Condition {
                $box = Get-SidebarElement SearchTextBox
                $box -and -not $box.Current.IsOffscreen
            } | Out-Null
            $query = "never-matches-agent-filter-$([guid]::NewGuid().ToString('N'))"
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $query | Out-Null
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $query
            Wait-Until -TimeoutSec 5 -Because 'search excludes the ordinary tab' -Condition {
                @(Get-VisibleTabRows).Count -eq 0
            } | Out-Null

            Invoke-UiElement -App $script:app -Selector FilterTabsButton | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'Agent only menu item is clickable' -Condition {
                $item = Get-SidebarElement AgentsOnlyFilterMenuItem
                $item -and -not $item.Current.IsOffscreen
            } | Out-Null
            (Get-SidebarElement AgentsOnlyFilterMenuItem).GetCurrentPattern(
                [Windows.Automation.TogglePattern]::Pattern).Toggle()
            Wait-Until -TimeoutSec 10 -Because 'Agent only menu choice is persisted' -Condition {
                [bool](Get-Content -LiteralPath $script:settingsPath -Raw | ConvertFrom-Json).sidebarShowAgentsOnly -eq
                    $agentsOnly
            } | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'Agent only menu click exits search' -Condition {
                $box = Get-SidebarElement SearchTextBox
                -not $box -or $box.Current.IsOffscreen
            } | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'Agent only projection replaces search results' -Condition {
                (@(Get-VisibleTabRows).Count -eq 0) -eq $agentsOnly
            } | Out-Null

            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'search reopens to verify its query was cleared' -Condition {
                $box = Get-SidebarElement SearchTextBox
                $box -and -not $box.Current.IsOffscreen
            } | Out-Null
            (Get-UiValue -App $script:app -Selector SearchTextBox) |
                Should -BeExactly '' -Because 'Agent only must not preserve the previous query'
            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
        }
    }
}
