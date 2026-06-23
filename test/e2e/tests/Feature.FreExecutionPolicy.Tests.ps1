#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE — DETERMINISTIC execution-policy coverage.
#
# The FRE "Install"/Save probes each PowerShell host's execution policy and blocks
# shell integration when it refuses unsigned local scripts. The other FreFlow
# tests only ever exercise the happy path against whatever policy the machine
# happens to have; these force the *real* policy via the Windows PowerShell
# CurrentUser registry scope (HKCU — no admin; outranks LocalMachine, so it is the
# effective policy) and assert the FRE's verdict from terminal-agent-pane.log, for
# BOTH the blocked and not-blocked cases. The registry is always restored.
#
# Targets the DEV package: the build under development carries the probe-timeout
# fix + the per-host "[FRE] EP probe …" diagnostics these tests assert on; the
# store build (0.1.1681.0) has neither.
#   Invoke-Pester test/e2e/tests/Feature.FreExecutionPolicy.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:DevReady = [bool](Get-AppxPackage | Where-Object { $_.Name -eq 'IntelligentTerminal' })
}

Describe 'Feature §0 FRE execution-policy verdict (deterministic, via registry)' -Tag 'Feature' -Skip:(-not $script:DevReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # Safety-net snapshot so the machine's policy is restored even if a test
        # throws before its own finally runs.
        $script:epSnapshot = Get-WtExecutionPolicyState

        # Drive the FRE wizard to Save. Defined in BeforeAll (not the Describe body)
        # so Pester v5 exposes the $script: scriptblock to the It blocks.
        $script:DriveFreSave = {
            param($App)
            Invoke-UiElement -App $App -Selector 'NextButton' -TimeoutSec 15 | Out-Null
            Wait-UiElement -App $App -Selector 'SaveButton' -TimeoutSec 15 | Out-Null
            Invoke-UiElement -App $App -Selector 'SaveButton' -TimeoutSec 15 | Out-Null
        }
    }
    AfterAll {
        if ($script:epSnapshot) { Restore-WtExecutionPolicy -State $script:epSnapshot }
    }

    It 'Restricted policy -> FRE probe reads it and BLOCKS; FRE does not complete' {
        $st = Set-WtExecutionPolicy -Value Restricted
        try {
            $app = Start-TerminalFre -Package Dev
            try {
                & $script:DriveFreSave $app
                # The probe must read the real policy ('restricted') for Windows
                # PowerShell and return the BLOCKED verdict — this is the correct,
                # actionable case (Restricted genuinely refuses unsigned scripts).
                $blocked = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match "EP probe winPs policy='restricted'.*BLOCKED"
                }
                $blocked | Should -BeTrue -Because "the probe must read the real Restricted policy and block"
                # The blocked verdict drives FreOverlay::_SaveAndInstallAsync to surface
                # the shell-integration problem (FreProblemKind::ShellIntegrationExecutionPolicy)
                # and co_return *without* raising Completed — so the FRE does not finish.
                $surfaced = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match 'Showing problem: ShellIntegration'
                }
                $surfaced | Should -BeTrue -Because "a real EP block must surface the shell-integration problem"
                # ...and the FRE must NOT have completed.
                $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                $log | Should -Not -Match 'Completed — raising Completed event'
                Get-FreCompleted -App $app | Should -BeFalse
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }

    It 'RemoteSigned policy -> FRE probe reads it and does NOT block (winPs=ok)' {
        # RemoteSigned permits *local* unsigned scripts, so our $PROFILE block runs
        # and the probe must NOT block shell integration.
        $st = Set-WtExecutionPolicy -Value RemoteSigned
        try {
            $app = Start-TerminalFre -Package Dev
            try {
                & $script:DriveFreSave $app
                $notBlocked = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match "EP probe winPs policy='remotesigned'.*not-blocked"
                }
                $notBlocked | Should -BeTrue -Because "the probe must read the real RemoteSigned policy and not block"
                # The not-blocked path must never surface the shell-integration EP problem.
                # (Asserted after a short settle so a late-arriving problem would be caught.)
                Start-Sleep -Seconds 3
                (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) | Should -Not -Match 'Showing problem: ShellIntegration'
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }
}
