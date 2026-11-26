#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
}

Describe 'Feature: OSC 7501 shell prompt lifecycle' -Tag @('Feature', 'ProgramStatus') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            autoFixEnabled = $false
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
    }

    It 'PowerShell generated prompt releases transient OSC 7501 ownership' {
        $pane = Get-ActivePane -App $script:app
        $sid = $pane.session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            $command = @(
                '$esc = [char]27'
                '[Console]::Write("$esc]7501;state=working:app=copilot$esc\")'
                'Start-Sleep -Milliseconds 300'
                '[Console]::Write("$esc]7501;state=done:app=copilot:msg=ZmluaXNoZWQ=$esc\")'
            ) -join '; '
            $command | Should -Not -Match '133'

            Send-WtInput -App $script:app -SessionId $sid -Text $command
            Send-WtKeys -App $script:app -SessionId $sid -Keys @('Enter')

            Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'program_status_changed' -and
                "$($_.params.pane_id)" -eq "$sid" -and
                $_.params.unseen_done -eq 1 -and
                -not $_.params.native_lease_active
            } | Out-Null

            $events = @(Get-WtEvents -Listener $listener)
            $commandFinished = 0..($events.Count - 1) | Where-Object {
                $events[$_].method -eq 'vt_sequence' -and
                "$($events[$_].params.pane_id)" -eq "$sid" -and
                "$($events[$_].params.sequence)" -match '(?i)osc:133;D;0(\b|;|$)'
            } | Select-Object -First 1
            $promptStarted = 0..($events.Count - 1) | Where-Object {
                $events[$_].method -eq 'vt_sequence' -and
                "$($events[$_].params.pane_id)" -eq "$sid" -and
                "$($events[$_].params.sequence)" -match '(?i)osc:133;A(\b|;|$)'
            } | Select-Object -First 1
            $released = 0..($events.Count - 1) | Where-Object {
                $events[$_].method -eq 'program_status_changed' -and
                "$($events[$_].params.pane_id)" -eq "$sid" -and
                $events[$_].params.unseen_done -eq 1 -and
                -not $events[$_].params.native_lease_active
            } | Select-Object -First 1

            $commandFinished | Should -Not -BeNullOrEmpty
            $promptStarted | Should -Not -BeNullOrEmpty
            $released | Should -Not -BeNullOrEmpty
            $commandFinished | Should -BeLessThan $promptStarted
            $promptStarted | Should -BeLessOrEqual $released
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }
}
