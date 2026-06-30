#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: §1 "Policy lock UI works" / §0 "FRE respects policy locks" — the agent
# Group-Policy gates (AgentPolicy.h). Automated WITHOUT admin by driving the HKCU policy hive
# (Software\Policies\Microsoft\IntelligentTerminal), which the C++ reader honors as a fallback
# after HKLM — a faithful, elevation-free proxy for a real machine GPO. Same approach as the
# WinPS execution-policy suite (Feature.FreExecutionPolicy.Tests.ps1).
#
# These are DETERMINISTIC, LLM-free assertions: they assert the ABSENCE of a policy-gated
# behavior, which does not depend on any model output. The gate is enforced at TWO points and
# this suite leans on the source-level one:
#   * TerminalPage.cpp:5172-5173 — the VtSequenceReceived handler early-returns when
#     IsAutoFixPolicyLocked() is true, BEFORE raising the protocol event. So with the policy
#     Blocked the autofix pipeline is gated AT THE SOURCE: a failing command's OSC 133;D mark is
#     not even forwarded to wtcli listeners, and no autofix prompt is ever submitted.
#   * TerminalPage.cpp:1695-1697 — the WTA helper is additionally spawned with --no-autofix.
# Positive control: Feature.AutofixPane.Tests.ps1 — the IDENTICAL setup WITHOUT this policy DOES
# fire autofix, so a clean run here proves the policy (not a broken pane) suppressed it.
#
# REQUIRES A WRITABLE POLICY HIVE: the `Software\Policies` subtree is ACL-restricted to
# administrators. Run test/e2e/tools/Enable-WtAgentPolicyTesting.ps1 ONCE (elevated) to grant
# the current user write on its own policy key; after that this suite runs non-elevated. When the
# hive isn't writable (or a machine GPO overrides it), Test-WtAgentPolicyControllable is $false
# and this suite SKIPS cleanly — non-elevated CI stays green, like Feature.FreExecutionPolicy.

BeforeDiscovery {
    # Needs the package + copilot (agent pane) + winapp (Open-AgentPane), AND the HKCU policy
    # hive must be the effective one (no machine GPO overriding it) for a deterministic verdict.
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue))
    $script:PolicyControllable = Test-WtAgentPolicyControllable
}

Describe 'Feature §1 agent Group Policy locks (AllowAutoFix)' -Tag 'Feature' -Skip:(-not ($script:Ready -and $script:PolicyControllable)) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # Block autofix by POLICY while leaving the user setting ON — so the ONLY thing that can
        # suppress autofix is the GPO. Set before launch (the policy snapshot is read at startup).
        $script:policyState = Set-WtAgentPolicy -Policy @{ AllowAutoFix = 'Blocked' }
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the agent pane still connects under an autofix policy block (the gate suppresses autofix, not the pane)'
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($script:policyState) { Restore-WtAgentPolicy -State $script:policyState }
    }

    It 'AllowAutoFix=Blocked suppresses autofix end-to-end on a real command failure' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            # Unique command so autofix de-dup can never be the reason nothing fires.
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null

            # The policy gate early-returns in the VtSequenceReceived handler before the autofix
            # pipeline runs (TerminalPage.cpp:5172-5173), so the failing command never produces an
            # autofix request. Deterministic and LLM-free. (Positive control: Feature.AutofixPane
            # — the identical setup WITHOUT this policy DOES fire autofix, so a clean pass here
            # means the policy suppressed it, not a dead pane: the pane already reached Connected
            # in BeforeAll.)
            { Wait-Autofix -Listener $listener -TimeoutSec 25 } |
                Should -Throw -Because 'AllowAutoFix=Blocked must prevent autofix from asking the agent for a fix'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
