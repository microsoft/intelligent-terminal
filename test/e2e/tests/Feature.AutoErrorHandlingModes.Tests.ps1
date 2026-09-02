#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Canonical three-state Auto error handling behavior. Each mode gets a fresh process so settings,
# helper logs, and request state cannot bleed between cases.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: Auto error handling Off mode' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            autoErrorHandling = 'off'
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Off works (failures produce no Auto error handling activity)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $tag = "ITE2E_OFF_$(Get-Random)"
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-RunCommand -App $script:app -SessionId $sid -Command "Write-Output $tag; noSuchCommand$(Get-Random)" | Out-Null
            Assert-Pane -App $script:app -SessionId $sid -Match $tag -TimeoutSec 10
            Start-Sleep -Seconds 3
            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq "$sid" -and
                    "$($_.params.sequence)" -match '(?i)osc:133;D;'
                }) | Should -BeNullOrEmpty -Because 'Off must suppress shell-failure observation'
            { Wait-AutoErrorHandling -Listener $listener -TimeoutSec 5 } |
                Should -Throw -Because 'Off must not contact the agent'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}

Describe 'Feature: Auto error handling detect-only mode' -Tag 'Feature' -Skip:(-not $script:Ready) {
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

    It 'Detect errors automatically works (detects without contacting the agent)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null
            { Wait-WtCommandFailure -Listener $listener -PaneId $sid -TimeoutSec 20 } | Should -Not -Throw
            { Wait-AutoErrorHandlingDetection -Listener $listener -PaneId $sid -TimeoutSec 20 } | Should -Not -Throw
            { Wait-AutoErrorHandling -Listener $listener -TimeoutSec 8 } |
                Should -Throw -Because 'detect-only mode must not contact the agent'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}

Describe 'Feature: Auto error handling send-to-agent mode' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'copilot'
            autoErrorHandling = 'detectErrorsAndSendToAgentForFixesAutomatically'
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Detect errors and send them to the agent for fixes automatically. works' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null
            { Wait-WtCommandFailure -Listener $listener -PaneId $sid -TimeoutSec 20 } | Should -Not -Throw
            { Wait-AutoErrorHandling -Listener $listener -TimeoutSec 45 } |
                Should -Not -Throw -Because 'the third mode must contact the connected agent'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
