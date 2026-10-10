# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Standalone ACP Backoff query scheduling' -Tag 'Unit' {
    BeforeAll {
        $Tokens = $null
        $ParseErrors = $null
        $Ast = [System.Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot 'Measure-CopilotAcpCpu.ps1'), [ref]$Tokens, [ref]$ParseErrors)
        if ($ParseErrors.Count) { throw ($ParseErrors | Out-String) }
        $Function = $Ast.Find({
            param($Node)
            $Node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
                $Node.Name -eq 'Test-BackoffPollDue'
        }, $false)
        . ([scriptblock]::Create($Function.Extent.Text))

        function New-PollState {
            [pscustomobject]@{ Request = $null; Failures = 0; NextPoll = 0.0 }
        }

        function New-PollRequest {
            param($Completed, [string]$Outcome = 'pending', [bool]$TimedOut = $false)
            [pscustomobject]@{ completed_seconds = $Completed; outcome = $Outcome; timed_out = $TimedOut }
        }
    }

    It 'Retains a timed-out request even beyond the usual retry and observation boundaries' {
        $State = New-PollState
        $Request = New-PollRequest -Completed $null -Outcome 'timeout' -TimedOut $true
        $State.Request = $Request
        foreach ($Now in @(5, 10, 20, 40, 60, 300)) {
            Test-BackoffPollDue $State $Now | Should -BeFalse
            [object]::ReferenceEquals($State.Request, $Request) | Should -BeTrue
            $State.Failures | Should -Be 0
        }
    }

    It 'Treats a late success as success and schedules five seconds from completion' {
        $State = New-PollState
        $State.Failures = 3
        $State.Request = New-PollRequest -Completed 8.43 -Outcome 'ok' -TimedOut $true
        Test-BackoffPollDue $State 8.43 | Should -BeFalse
        $State.Failures | Should -Be 0
        $State.NextPoll | Should -Be 13.43
        Test-BackoffPollDue $State 13.42 | Should -BeFalse
        Test-BackoffPollDue $State 13.43 | Should -BeTrue
    }

    It 'Uses capped backoff only after actual error responses and resets it on success' {
        $State = New-PollState
        $Completed = 0.0
        foreach ($Delay in @(5, 10, 20, 40, 60, 60)) {
            $State.Request = New-PollRequest -Completed $Completed -Outcome 'error'
            Test-BackoffPollDue $State $Completed | Should -BeFalse
            $State.NextPoll | Should -Be ($Completed + $Delay)
            Test-BackoffPollDue $State ($Completed + $Delay - 0.01) | Should -BeFalse
            Test-BackoffPollDue $State ($Completed + $Delay) | Should -BeTrue
            $Completed += $Delay + 0.25
        }
        $State.Request = New-PollRequest -Completed $Completed -Outcome 'ok'
        Test-BackoffPollDue $State $Completed | Should -BeFalse
        $State.Failures | Should -Be 0
        $State.NextPoll | Should -Be ($Completed + 5)
    }
}
