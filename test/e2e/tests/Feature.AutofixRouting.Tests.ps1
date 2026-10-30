#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real failed commands and the diagnostics button cross WT -> helper -> master
# -> ACP. The fixture records requests without consuming provider quota.

Describe 'Feature: Autofix detected action routing' -Tag 'Feature' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $script:requestLog = Join-Path $env:TEMP ("ite2e-autofix-routing-{0}.log" -f [guid]::NewGuid().ToString('N'))
        New-Item -ItemType File -Path $script:requestLog | Out-Null
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $command = "pwsh -NoProfile -EncodedCommand $encoded"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
            sidebarRolloutVersion = 1
            sidebarRolloutAssignment = 'Sidebar'
            sidebarLayoutMigrationCompleted = $true
            sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'
            tabLayout = 'vertical'
            acpAgent = 'custom:autofix-routing-fixture'
            acpCustomCommand = $command
            acpModel = ''
            autoErrorDetectionEnabled = $true
            autoFixEnabled = $false
        }
        $script:targets = @()
        foreach ($label in @('A', 'B')) {
            $shell = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo -NoExit' -Title "autofix-routing-$label"
            Set-WtPaneFocus -App $script:app -SessionId $shell.session_id
            Open-AgentPane -App $script:app | Out-Null
            $agent = Wait-Until -TimeoutSec 30 -Because "helper for tab $label" -Condition {
                Get-AgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id
            }
            Wait-AgentReady -App $script:app -PaneSessionId $agent.PaneSessionId -TimeoutSec 60 |
                Should -BeTrue
            $script:targets += [pscustomobject]@{ Shell = $shell; Agent = $agent }
        }

        function Get-RoutingPromptCount($Target) {
            $pattern = '\|session/prompt\|' + [regex]::Escape($Target.Agent.AcpSessionId) + '\|'
            @(Select-String -LiteralPath $script:requestLog -Pattern $pattern).Count
        }
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($script:requestLog -and (Test-Path -LiteralPath $script:requestLog)) {
            Remove-Item -LiteralPath $script:requestLog
        }
    }

    It 'Detected Autofix clicks remain isolated between tabs' {
        Initialize-LogOffsets -App $script:app | Out-Null
        foreach ($target in $script:targets) {
            Set-WtPaneFocus -App $script:app -SessionId $target.Shell.session_id
            $listener = Start-WtEventListener -App $script:app
            try {
                Invoke-RunCommand -App $script:app -SessionId $target.Shell.session_id -Command "throw 'routing-$([guid]::NewGuid())'" | Out-Null
                Wait-WtCommandFailure -Listener $listener -PaneId $target.Shell.session_id -TimeoutSec 20 | Out-Null
                Wait-Until -TimeoutSec 20 -Because 'failure reaches this helper in Detected state' -Condition {
                    (Get-ItLogText -App $script:app -Name "wta-main_helper-$($target.Agent.HelperProcessId).log" -SinceStart) -match
                        ('surfacing Detected pill.*pane_id=' + [regex]::Escape($target.Shell.session_id))
                } | Out-Null
            }
            finally { Stop-WtEventListener -Listener $listener }
        }
        $a, $b = $script:targets
        (Get-RoutingPromptCount $a) | Should -Be 0
        (Get-RoutingPromptCount $b) | Should -Be 0
        (Test-WtWindowKeyFocusable -App $script:app) | Should -BeTrue
        Set-WtPaneFocus -App $script:app -SessionId $b.Shell.session_id
        $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
        function Find-FocusTarget([string]$id) {
            $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, $id))
        }
        $diagnostics = Find-FocusTarget 'DiagnosticsButton'
        $diagnostics | Should -Not -BeNullOrEmpty
        $diagnostics.Current.IsOffscreen | Should -BeFalse
        $diagnostics.Current.IsEnabled | Should -BeTrue
        foreach ($i in 1..3) {
            Send-WtWindowKey -App $script:app -Vk 0x75 -RequireForeground | Out-Null
        }
        [Windows.Automation.Automation]::Compare(
            [Windows.Automation.AutomationElement]::FocusedElement, (Find-FocusTarget 'AgentToggleButton')) |
            Should -BeTrue -Because 'F6 navigates to the agent region, not each action within it'
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        [Windows.Automation.Automation]::Compare(
            [Windows.Automation.AutomationElement]::FocusedElement, $diagnostics) |
            Should -BeTrue -Because 'Tab includes an actionable Detected diagnostics button'
        Send-WtWindowKey -App $script:app -Vk 0x09 -RequireForeground | Out-Null
        [Windows.Automation.AutomationElement]::FocusedElement.Current.Name |
            Should -Be 'Collapse sidebar'
        Send-WtWindowKey -App $script:app -Vk 0x09 -Shift -RequireForeground | Out-Null
        [Windows.Automation.Automation]::Compare(
            [Windows.Automation.AutomationElement]::FocusedElement, $diagnostics) | Should -BeTrue
        Send-WtWindowKey -App $script:app -Vk 0x09 -Shift -RequireForeground | Out-Null
        [Windows.Automation.Automation]::Compare(
            [Windows.Automation.AutomationElement]::FocusedElement, (Find-FocusTarget 'AgentToggleButton')) |
            Should -BeTrue
        Set-WtPaneFocus -App $script:app -SessionId $b.Shell.session_id

        # Both tabs are Detected. Clicking only B must leave A available for a
        # separate opt-in, even though the protocol broadcasts to both helpers.
        Invoke-UiElement -App $script:app -Selector 'DiagnosticsButton' | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'B submits its Autofix prompt to ACP' -Condition {
            (Get-RoutingPromptCount $b) -gt 0
        } | Out-Null
        Start-Sleep -Seconds 3
        (Get-RoutingPromptCount $b) | Should -Be 1
        (Get-RoutingPromptCount $a) | Should -Be 0 -Because 'another tab must not opt in to Autofix'

        Set-WtPaneFocus -App $script:app -SessionId $a.Shell.session_id
        Invoke-UiElement -App $script:app -Selector 'DiagnosticsButton' | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'A retained its Detected state until explicitly clicked' -Condition {
            (Get-RoutingPromptCount $a) -gt 0
        } | Out-Null
        Start-Sleep -Seconds 3
        (Get-RoutingPromptCount $a) | Should -Be 1
        (Get-RoutingPromptCount $b) | Should -Be 1
    }
}
