#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: new tabs leave Sidebar search' -Tag @('Feature', 'SidebarNewTabSearch') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        if ($env:ITE2E_PACKAGE -ne 'Dev' -or
            -not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Select the exact-source Dev package and provide its App and WTA hashes.'
        }
        $target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $target
        (Get-FileHash (Join-Path $target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256

        $script:profileName = "Search exit shell $([guid]::NewGuid().ToString('N').Substring(0, 8))"
        $profileId = '{' + [guid]::NewGuid() + '}'
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; defaultProfile = $profileId; startupActions = ''
            sidebarShowAgentsOnly = $false
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            profiles = @{ list = @(@{
                guid = $profileId; name = $script:profileName
                commandline = 'pwsh.exe -NoLogo -NoProfile'
            }) }
        }
        $script:app | Add-Member RequireOwnedForeground $true -Force

        function Get-SearchBox {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTextBox')
            $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        }

        function Get-NewTabSplitButton {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ClassNameProperty, 'Microsoft.UI.Xaml.Controls.SplitButton')
            $buttons = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) |
                Where-Object { -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 })
            $buttons.Count | Should -Be 1 -Because 'the expanded Sidebar has one visible new-tab split button'
            $buttons[0]
        }

        function Get-ProfileMenuItem {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::NameProperty, $script:profileName)
            @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) |
                Where-Object {
                    $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem -and
                    -not $_.Current.IsOffscreen
                }) | Select-Object -First 1
        }

        function Assert-NewTabLeavesSearch {
            Wait-Until -TimeoutSec 5 -Because 'new tab exits search and focuses its terminal' -Condition {
                $box = Get-SearchBox
                $focus = [Windows.Automation.AutomationElement]::FocusedElement
                if (($box -and (-not $box.Current.IsOffscreen -or
                    $box.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '')) -or
                    -not $focus -or $focus.Current.ProcessId -ne $script:app.Pid -or
                    $focus.Current.ClassName -ne 'TermControl' -or -not $focus.Current.HasKeyboardFocus) {
                    return $false
                }
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $list = $root.FindFirst(
                    [Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
                if (-not $list) { return $false }
                $condition = [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
                $visibleRows = @($list.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) |
                    Where-Object { -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 })
                @($visibleRows | Where-Object {
                    $selection = $null
                    $_.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$selection) -and
                        $selection.Current.IsSelected
                }).Count -eq 1
            } | Out-Null

            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'search reopens to verify its query was cleared' -Condition {
                $box = Get-SearchBox
                $box -and -not $box.Current.IsOffscreen
            } | Out-Null
            (Get-UiValue -App $script:app -Selector SearchTextBox) |
                Should -BeExactly '' -Because 'opening search again must not restore the previous query'
            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
        }
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    BeforeEach {
        $script:previousPane = Get-ActivePane -App $script:app
        Set-WtWindowForeground -App $script:app | Should -BeTrue
        Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
        Wait-Until -TimeoutSec 5 -Because 'Sidebar search opens' -Condition {
            $box = Get-SearchBox
            $box -and -not $box.Current.IsOffscreen
        } | Out-Null
        $script:query = "never-matches-new-tab-$([guid]::NewGuid().ToString('N'))"
        Set-UiValue -App $script:app -Selector SearchTextBox -Value $script:query | Out-Null
        (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -BeExactly $script:query
        $script:createdPane = $null
    }

    AfterEach {
        if ($script:createdPane) {
            Close-WtPane -App $script:app -SessionId $script:createdPane.session_id
        }
        if (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue) {
            $box = Get-SearchBox
            if ($box -and -not $box.Current.IsOffscreen) {
                Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            }
        }
    }

    It 'closes search and focuses the new tab after clicking plus' {
        $button = Get-NewTabSplitButton
        $button.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()

        Wait-Until -TimeoutSec 10 -Because 'plus creates and selects a new tab' -Condition {
            $active = Get-ActivePane -App $script:app
            if ($active.session_id -ne $script:previousPane.session_id) {
                $script:createdPane = $active
                return $true
            }
            $false
        } | Out-Null
        Assert-NewTabLeavesSearch
    }

    It 'closes search and focuses the new tab after choosing a dropdown profile' {
        $button = Get-NewTabSplitButton
        $button.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        $profileItem = Wait-Until -TimeoutSec 5 -Because 'new-tab menu shows the selected profile' -Condition {
            Get-ProfileMenuItem
        }
        $profileItem.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()

        Wait-Until -TimeoutSec 10 -Because 'dropdown profile creates and selects a new tab' -Condition {
            $active = Get-ActivePane -App $script:app
            if ($active.session_id -ne $script:previousPane.session_id) {
                $script:createdPane = $active
                return $true
            }
            $false
        } | Out-Null
        Assert-NewTabLeavesSearch
    }
}
