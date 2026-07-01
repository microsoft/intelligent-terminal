#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §6 Custom agents — the parts automatable end-to-end:
#   * custom agents are a Settings-only concept (the FRE agent picker does NOT offer custom creation);
#   * a configured custom ACP agent drives the standard agent-pane behaviour (connects + chats).
# A custom agent is just a command string launched as an ACP stdio agent, so we point it at the real
# Copilot ACP command (`copilot --acp --stdio`) to exercise the custom path without a bespoke binary.
#
# Not covered here: Add/Edit/Delete custom agent via the Settings editor text fields (typing into
# the custom-command TextBox is brittle under UIA) and the Alt+Shift delegate shortcuts (WT
# accelerators are not harness-injectable) — those stay manual / UT-locked (DeriveCustomAgentId,
# CustomAgentAndPolicyTests round-trips).

BeforeDiscovery {
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature §6 custom agents' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll { Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Custom agent is Settings-only (the FRE agent picker offers no custom-agent creation)' {
        $script:app = Start-TerminalFre -Package (Get-ItTestPackage)
        Invoke-UiElement -App $script:app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
        Start-Sleep -Seconds 1
        # The FRE agent dropdown lists built-in agents only; "custom" is configured later in Settings.
        $tree = Get-UiTree -App $script:app -Depth 18
        $tree | Should -Match 'AgentComboBox' -Because 'the FRE must show the built-in agent picker'
        $tree | Should -Not -Match '(?i)custom:|Add custom|custom agent' -Because 'the FRE must NOT expose custom-agent creation (Settings-only)'
        Stop-Terminal -App $script:app
        $script:app = $null
    }

    It 'A configured custom ACP agent connects and chats in the agent pane' {
        # custom:<id> + a real ACP command exercises the custom-agent launch path end-to-end.
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent         = 'custom:copilot-acp'
            acpCustomCommand = 'copilot --acp --stdio'
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'a custom ACP agent must reach a connected session like a built-in'
        Send-AgentPrompt -App $script:app -Text 'What is 8 plus 1? Reply with only the number.' | Out-Null
        $answered = Test-Until -TimeoutSec 40 -IntervalSec 2 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 80) -match '\b9\b'
        }
        $answered | Should -BeTrue -Because 'the custom ACP agent must answer a chat prompt in the pane'
        Stop-Terminal -App $script:app
        $script:app = $null
    }

    It 'Custom failure is safe (a bad custom command does not crash the terminal)' {
        # A custom agent pointing at a non-existent executable must fail to launch WITHOUT taking
        # down the terminal — the protocol surface stays alive and a shell pane keeps responding.
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent         = 'custom:broken'
            acpCustomCommand = 'wt-nonexistent-agent-xyz --acp --stdio'
        }
        # Opening the pane may throw (nothing launches) — that's fine; the guarantee is no crash.
        try { Open-AgentPane -App $script:app -TimeoutSec 20 | Out-Null } catch { }
        # The WT process is alive and the protocol still answers.
        (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue) | Should -Not -BeNullOrEmpty -Because 'a bad custom agent command must not crash the terminal'
        { Get-ActivePane -App $script:app } | Should -Not -Throw -Because 'the protocol surface must stay responsive after a failed custom-agent launch'
    }
}
