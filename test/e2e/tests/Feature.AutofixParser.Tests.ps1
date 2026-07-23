#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Regression coverage for issue #474 and PR #477.
#
# Feature.ShellIntegration.Tests.ps1 verifies the PowerShell-side OSC 133 contract in
# isolation. Feature.AutofixPane.Tests.ps1 verifies the existing Autofix UI and actions
# with ordinary command failures. This suite closes the integration gap between them:
# a real PowerShell parser error must cross the complete shell -> WT protocol -> WTA
# pipeline and submit exactly one Autofix prompt, while ambiguous successful commands
# must not submit one.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: PowerShell parser errors trigger Autofix end-to-end' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent      = 'copilot'
            autoFixEnabled = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 |
            Should -BeTrue -Because 'Autofix requires a connected ACP session'
        $script:sid = (Get-ActivePane -App $script:app).session_id
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'submits exactly one Autofix prompt for a PowerShell parser error' {
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Invoke-RunCommand -App $script:app -SessionId $script:sid -Command "'x'.like '*x*'" | Out-Null

            $failure = Wait-WtCommandFailure -Listener $listener -PaneId $script:sid -TimeoutSec 20
            "$($failure.params.sequence)" |
                Should -Match '(?i)osc:133;D;(?!0(\b|;|$))' -Because 'the parser error must be corrected from stale exit code 0'

            $autofix = Wait-Autofix -Listener $listener -TimeoutSec 45
            $autofix | Should -Not -BeNullOrEmpty -Because 'the parser failure mark must reach the Autofix dispatcher'

            Start-Sleep -Seconds 2
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and
                    "$($_.params.payload.initial_prompt)" -match 'command failed|Diagnose the error'
                }) | Should -HaveCount 1 -Because 'one malformed command must submit one Autofix turn'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'does not retrigger Autofix when the parser-error prompt is redrawn by blank Enter' {
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Send-WtKeys -App $script:app -SessionId $script:sid -Keys @('Enter') | Out-Null
            Start-Sleep -Seconds 3

            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$script:sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;(?!0(\b|;|$))'
                }) | Should -BeNullOrEmpty -Because 'blank input must not replay the previous parser failure'
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and
                    "$($_.params.payload.initial_prompt)" -match 'command failed|Diagnose the error'
                }) | Should -BeNullOrEmpty -Because 'a prompt redraw must not submit another Autofix turn'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}

Describe 'Feature: successful PowerShell completion does not trigger Autofix' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent      = 'copilot'
            autoFixEnabled = $true
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 |
            Should -BeTrue -Because 'negative assertions require a connected Autofix pipeline'
        $script:sid = (Get-ActivePane -App $script:app).session_id
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'does not trigger Autofix for a normal successful command' {
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Invoke-RunCommand -App $script:app -SessionId $script:sid -Command "Write-Output normal-ok-$([guid]::NewGuid())" | Out-Null
            { Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$script:sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;0(\b|;|$)'
                } } | Should -Not -Throw
            Start-Sleep -Seconds 2
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and
                    "$($_.params.payload.initial_prompt)" -match 'command failed|Diagnose the error'
                }) | Should -BeNullOrEmpty
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'does not trigger Autofix when a non-terminating error is handled successfully' {
        $missingPath = Join-Path $env:TEMP "it-autofix-parser-missing-$([guid]::NewGuid())"
        $escapedPath = $missingPath.Replace("'", "''")
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            Invoke-RunCommand -App $script:app -SessionId $script:sid -Command "Get-Item '$escapedPath' -ErrorAction SilentlyContinue; Write-Output ok" | Out-Null
            { Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$script:sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;0(\b|;|$)'
                } } | Should -Not -Throw
            Start-Sleep -Seconds 2
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and
                    "$($_.params.payload.initial_prompt)" -match 'command failed|Diagnose the error'
                }) | Should -BeNullOrEmpty -Because 'handled errors that finish successfully must remain distinct from parser failures'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
