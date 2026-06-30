#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: §1 "Policy lock UI works" / §0 "FRE respects policy locks" — the agent
# Group-Policy gates (AgentPolicy.h). Automated WITHOUT admin by driving the HKCU policy hive
# (Software\Policies\Microsoft\IntelligentTerminal), which the C++ reader honors as a fallback
# after HKLM — a faithful, elevation-free proxy for a real machine GPO. Same approach as the
# WinPS execution-policy suite (Feature.FreExecutionPolicy.Tests.ps1).
#
# These are DETERMINISTIC, LLM-free assertions: they assert the ABSENCE of a policy-gated
# behavior, which does not depend on any model output. The runtime enforces the gate at spawn
# time — TerminalPage passes `--no-autofix` to the WTA helper when EffectiveAutoFixEnabled() is
# false (TerminalPage.cpp:1695-1697, 1832-1834) — so a blocked policy means autofix can never
# fire, regardless of the LLM.
#
# REQUIRES AN ELEVATED RUNNER: the `Software\Policies` registry subtree is ACL-restricted to
# administrators, so writing it needs elevation (unlike the WinPS ExecutionPolicy key). When the
# runner can't write the policy hive (or a machine GPO already overrides it),
# Test-WtAgentPolicyControllable is $false and this suite SKIPS cleanly — non-elevated CI stays
# green, exactly like Feature.FreExecutionPolicy.Tests.ps1.

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

    It 'A policy-blocked AllowAutoFix suppresses autofix even though the failure is still detected' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            # Unique command so autofix de-dup can never be the reason nothing fires.
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null

            # 1) The failure IS detected — shell integration still emits OSC 133;D;<nonzero>.
            #    This isolates the result: it's autofix specifically that's gated, not detection.
            { Wait-WtCommandFailure -Listener $listener -PaneId $sid -TimeoutSec 20 } |
                Should -Not -Throw -Because 'PowerShell shell integration must still mark the failure; only the autofix RESPONSE is policy-gated'

            # 2) Autofix is NOT triggered — no agent prompt is ever submitted. Deterministic:
            #    the helper was spawned with --no-autofix, so this can never fire under the block.
            { Wait-Autofix -Listener $listener -TimeoutSec 20 } |
                Should -Throw -Because 'AllowAutoFix=Blocked must prevent autofix from asking the agent for a fix'
        }
        finally { Stop-WtEventListener -Listener $listener }
    }
}
