#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Canonical action and Legacy parse-only compatibility. The protocol oracle is the C++ event sent
# when a user activates a detected failure: auto_error_handling_request_analysis.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: triggerAutoErrorHandling action' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            autoErrorHandling = 'detectErrorsAutomatically'
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'triggerAutoErrorHandling requests analysis through auto_error_handling_request_analysis' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'WT window cannot take foreground for the canonical Ctrl+Alt+. action'
            return
        }
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null
            Wait-AutoErrorHandlingDetection -Listener $listener -PaneId $sid -TimeoutSec 20 | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0xBE -Ctrl -Alt | Out-Null
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 15 -Predicate {
                $_.method -eq 'auto_error_handling_request_analysis' -and
                "$($_.params.pane_id)" -eq "$sid"
            }
            $event | Should -Not -BeNullOrEmpty
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}

Describe 'Feature: Legacy triggerAutofix parse compatibility' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            autoErrorHandling = 'detectErrorsAutomatically'
            actions = @(
                @{
                    command = 'triggerAutofix'
                    keys = 'ctrl+alt+m'
                }
            )
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Legacy triggerAutofix still parses to the canonical Auto error handling action' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Legacy compatibility binding'
            return
        }
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null
            Wait-AutoErrorHandlingDetection -Listener $listener -PaneId $sid -TimeoutSec 20 | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x4D -Ctrl -Alt | Out-Null
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 15 -Predicate {
                $_.method -eq 'auto_error_handling_request_analysis' -and
                "$($_.params.pane_id)" -eq "$sid"
            }
            $event | Should -Not -BeNullOrEmpty
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
