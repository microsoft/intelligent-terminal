#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE — DETERMINISTIC execution-policy coverage.
#
# The FRE "Install"/Save probes each PowerShell host's execution policy and blocks
# shell integration when it refuses unsigned local scripts. The other FreFlow
# tests only ever exercise the happy path against whatever policy the machine
# happens to have; these force the *real* policy via the Windows PowerShell
# CurrentUser registry scope (HKCU — no admin; outranks LocalMachine, so it is the
# effective policy) and assert the FRE's verdict from terminal-agent-pane.log.
#
# Cases (one per outcome class the deny-list in PolicyNameBlocksUnsignedScripts has):
#   * Restricted  -> BLOCKED      (blocking policy #1; FRE surfaces problem, never completes)
#   * AllSigned   -> BLOCKED      (blocking policy #2 — the other refuse-unsigned policy)
#   * RemoteSigned-> not-blocked  (permissive; FRE installs SI and COMPLETES)
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
    # A Group Policy execution-policy override (MachinePolicy/UserPolicy) outranks the HKCU
    # CurrentUser scope these tests force, making the FRE verdict non-deterministic — skip the
    # whole suite when one is in effect rather than assert against an uncontrollable policy.
    $script:EpControllable = Test-WtExecutionPolicyControllable
    # The not-blocked case additionally needs pwsh (if present) to not independently block,
    # since the FRE blocks when EITHER host blocks and we only control WinPS via the registry.
    $script:PwshBlocks = Test-WtPwshBlocksShellIntegration
}

Describe 'Feature §0 FRE execution-policy verdict (deterministic, via registry)' -Tag 'Feature' -Skip:(-not ($script:DevReady -and $script:EpControllable)) {
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

    It 'AllSigned policy -> FRE probe reads it and BLOCKS; FRE does not complete' {
        # AllSigned is the *other* blocking policy (it refuses unsigned local scripts
        # just like Restricted). Forcing it via HKCU outranks LocalMachine, so the
        # winPs probe deterministically reads 'allsigned' regardless of the machine's
        # baseline. pwsh's policy is irrelevant here — the assertion keys on the winPs
        # probe line, and FRE blocks if WinPS blocks no matter what pwsh reports.
        $st = Set-WtExecutionPolicy -Value AllSigned
        try {
            $app = Start-TerminalFre -Package Dev
            try {
                & $script:DriveFreSave $app
                $blocked = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match "EP probe winPs policy='allsigned'.*BLOCKED"
                }
                $blocked | Should -BeTrue -Because "the probe must read the real AllSigned policy and block"
                $surfaced = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match 'Showing problem: ShellIntegration'
                }
                $surfaced | Should -BeTrue -Because "a real EP block must surface the shell-integration problem"
                $log = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
                $log | Should -Not -Match 'Completed — raising Completed event'
                Get-FreCompleted -App $app | Should -BeFalse
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }

    It 'RemoteSigned policy -> FRE probe reads it and does NOT block (winPs=ok)' -Skip:($script:PwshBlocks) {
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
                # The symmetric POSITIVE of the blocked cases: a not-blocked verdict must let
                # shell integration install and the FRE actually finish (Completed raised,
                # agentFreCompleted flag set). Asserting only "no problem shown" would pass even
                # if the FRE silently stalled for an unrelated reason — this catches that.
                $completed = Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition {
                    Get-FreCompleted -App $app
                }
                $completed | Should -BeTrue -Because "a not-blocked EP verdict must let the FRE complete"
                # ...and the not-blocked path must never surface the shell-integration EP problem.
                (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) | Should -Not -Match 'Showing problem: ShellIntegration'
            }
            finally { Stop-Terminal -App $app }
        }
        finally { Restore-WtExecutionPolicy -State $st }
    }
}
