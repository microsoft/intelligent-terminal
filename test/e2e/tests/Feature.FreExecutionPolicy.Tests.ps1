#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE — DETERMINISTIC execution-policy coverage.
#
# The FRE Get Started flow probes each PowerShell host's execution policy before
# shell integration. Restricted and AllSigned are automatically changed to
# CurrentUser RemoteSigned, verified, and then followed by the normal profile
# install. These tests force every installed PowerShell host's CurrentUser
# policy through Set-ExecutionPolicy and assert the remediation diagnostics.
#
# Cases:
#   * Restricted   -> automatically changed to RemoteSigned; FRE completes
#   * AllSigned    -> automatically changed to RemoteSigned; FRE completes
#   * RemoteSigned -> no mutation; FRE completes
# The registry is always restored.
#
# NOT covered here, by design: the empty/"undefined"/probe-timeout fail-open path
# (the core #336 regression — an EP probe that times out must NOT block). It can't be
# triggered deterministically from a test: clearing the HKCU value only exposes the
# machine-dependent LocalMachine fallback, and forcing a >20s probe timeout would mean
# hanging powershell.exe. That behavior is pinned by the C++ unit tests instead
# (ShellIntegrationTests::PolicyName_EmptyOrUnknown_NotBlocking + QueryExecutionPolicy_*).
#
# Targets the DEV package: the build under development carries the probe-timeout
# fix + the per-host "[FRE] EP probe …" diagnostics these tests assert on; the
# store build (0.1.1681.0) has neither.
#   Invoke-Pester test/e2e/tests/Feature.FreExecutionPolicy.Tests.ps1 -Tag Feature

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    # Gate on the SAME resolution the harness uses (Resolve-ItApp -Package Dev resolves by
    # PackageFamilyName), not a raw Name match — otherwise a same-Name-but-different-PFN
    # sideload could pass the gate and then make Start-TerminalFre -Package Dev throw instead
    # of the suite cleanly skipping.
    $script:DevReady = $false
    try { $null = Resolve-ItApp -Package Dev -ErrorAction Stop; $script:DevReady = $true } catch { $script:DevReady = $false }
    # winapp drives the FRE overlay via UIA; without it BeforeAll would throw, so fold it into
    # the gate and skip the suite cleanly instead.
    $script:DevReady = $script:DevReady -and (Test-WinAppAvailable)
    # A Group Policy execution-policy override (MachinePolicy/UserPolicy) outranks the
    # CurrentUser scope these tests force, making the FRE verdict non-deterministic — skip the
    # whole suite when one is in effect rather than assert against an uncontrollable policy.
    $script:EpControllable = Test-WtExecutionPolicyControllable
}

Describe 'Feature §0 FRE automatic execution-policy remediation' -Tag 'Feature' -Skip:(-not ($script:DevReady -and $script:EpControllable)) {
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

    It 'Restricted policy -> automatic remediation completes FRE' {
        $st = Set-WtExecutionPolicy -Value Restricted
        try {
            $app = Start-TerminalFre -Package Dev
            try {
                & $script:DriveFreSave $app
                $blocked = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match "EP remediation pre winPs status=2 policy='restricted'"
                }
                $blocked | Should -BeTrue -Because 'the automatic preflight must read the real Restricted policy'
                $remediated = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                    $log -match 'EP remediation set winPs launched=1.*exit=0' -and
                        $log -match "EP remediation post winPs status=1 policy='remotesigned'" -and
                        $log -match 'EP remediation succeeded'
                }
                $remediated | Should -BeTrue -Because 'Get Started must change CurrentUser to RemoteSigned and verify it'
                $completed = Test-Until -TimeoutSec 90 -IntervalSec 2 -Condition {
                    Get-FreCompleted -App $app
                }
                $completed | Should -BeTrue -Because 'automatic remediation must continue through shell integration and complete FRE'
                $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                Test-FreProgressOrder -Log $log -Events @(
                    'setup=running'
                    'setup=completed'
                    'agent=running'
                    'agent=completed'
                    'error-detection=running'
                    'error-detection=completed'
                ) | Should -BeTrue -Because 'the checklist must complete Error Detection after remediation'
                $log | Should -Not -Match 'Showing problem: ShellIntegration'
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }

    It 'AllSigned policy -> automatic remediation completes FRE' {
        $st = Set-WtExecutionPolicy -Value AllSigned
        try {
            $app = Start-TerminalFre -Package Dev
            try {
                & $script:DriveFreSave $app
                $blocked = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match "EP remediation pre winPs status=2 policy='allsigned'"
                }
                $blocked | Should -BeTrue -Because 'the automatic preflight must read the real AllSigned policy'
                $remediated = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                    $log -match 'EP remediation set winPs launched=1.*exit=0' -and
                        $log -match "EP remediation post winPs status=1 policy='remotesigned'" -and
                        $log -match 'EP remediation succeeded'
                }
                $remediated | Should -BeTrue -Because 'Get Started must change AllSigned to RemoteSigned and verify it'
                Test-Until -TimeoutSec 90 -IntervalSec 2 -Condition {
                    Get-FreCompleted -App $app
                } | Should -BeTrue -Because 'automatic remediation must complete FRE without another click'
                (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) |
                    Should -Not -Match 'Showing problem: ShellIntegration'
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }

    It 'RemoteSigned policy -> no remediation is needed and FRE completes' {
        $st = Set-WtExecutionPolicy -Value RemoteSigned
        try {
            $app = Start-TerminalFre -Package Dev
            try {
                & $script:DriveFreSave $app
                $notBlocked = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                    $log -match "EP remediation pre winPs status=1 policy='remotesigned'" -and
                        $log -match 'EP remediation not needed'
                }
                $notBlocked | Should -BeTrue -Because 'RemoteSigned must bypass the mutation path'
                $completed = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    Get-FreCompleted -App $app
                }
                $completed | Should -BeTrue -Because 'an already-permissive policy must let FRE complete'
                $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                $log | Should -Not -Match 'EP remediation set winPs'
                $log | Should -Not -Match 'Showing problem: ShellIntegration'
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }
}
