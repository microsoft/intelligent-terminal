#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real failed commands and the diagnostics button cross WT -> helper -> master
# -> ACP. The fixture records requests without consuming provider quota.

Describe 'Feature: Autofix detected action routing' -Tag 'Feature' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $script:requestLog = Join-Path $env:TEMP ("ite2e-autofix-routing-{0}.log" -f [guid]::NewGuid().ToString('N'))
        New-Item -ItemType File -Path $script:requestLog | Out-Null
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $command = "pwsh -NoProfile -EncodedCommand $encoded"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
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
