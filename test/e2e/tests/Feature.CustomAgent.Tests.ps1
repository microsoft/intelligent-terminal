#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §6 Custom agents — the parts automatable end-to-end:
#   * custom agents are a Settings-only concept (the FRE agent picker does NOT offer custom creation);
#   * a configured custom ACP agent drives the standard agent-pane behaviour (connects + chats).
# A custom agent is just a command string launched as an ACP stdio agent, so we point it at the real
# Copilot ACP command (`copilot --acp --stdio`) to exercise the custom path without a bespoke binary.
#
# Not covered here: "Model selection visible" (whether the model textbox stays visible when a
# custom agent is selected) is a Settings-editor UI concern and the editor window cannot be opened
# by the harness, so it stays manual. The Alt+Shift delegate shortcuts are WT accelerators (not
# harness-injectable) and stay manual / UT-locked (DeriveCustomAgentId, CustomAgentAndPolicyTests).
# Add/Edit/Delete ARE covered here behaviorally: the Settings editor only rewrites
# acpAgent/acpCustomCommand, so editing those settings on a running instance (WT rebuilds the
# agent stack on a settings change) exercises the same effective launch path.

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
        Stop-Terminal -App $script:app
        $script:app = $null
    }

    It 'Edit custom ACP agent updates the command used by new agent panes' {
        # "Editing" a custom agent in the Settings editor just rewrites acpCustomCommand; the
        # effective contract is which command launches for the agent stack. We exercise that same
        # path by editing the setting on a running instance (WT rebuilds the agent stack on a
        # settings change — see Feature.AgentRestart), using Copilot as the arbitrary ACP agent.
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent         = 'custom:edit-target'
            acpCustomCommand = 'copilot --acp --stdio'
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the baseline custom ACP agent must connect before we edit it'

        # EDIT the command to a non-existent executable → the stack rebuilds and the NEW command is
        # what launches, so it can no longer connect. A False readiness here proves the edit took
        # effect (the pane uses the edited command, not the old one).
        Set-WtSetting -App $script:app -Key 'acpCustomCommand' -Value 'wt-nonexistent-agent-xyz --acp --stdio' | Out-Null
        Start-Sleep -Seconds 3
        Wait-AgentReady -App $script:app -TimeoutSec 25 | Should -BeFalse -Because 'after editing the custom command to a broken one, new agent panes must launch the EDITED command'

        # EDIT back to the working command → reconnects, proving the edit is picked up both ways.
        Set-WtSetting -App $script:app -Key 'acpCustomCommand' -Value 'copilot --acp --stdio' | Out-Null
        Start-Sleep -Seconds 3
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'editing the custom command back to a working one must reconnect'
        Stop-Terminal -App $script:app
        $script:app = $null
    }

    It 'Delete custom ACP agent returns to a valid built-in selection' {
        # "Deleting" a custom agent returns the selection to a built-in/default. We start on a
        # working custom agent (Copilot as the arbitrary ACP agent), then switch acpAgent back to a
        # built-in and clear the custom command — the agent stack must fall back to a valid,
        # connectable agent (not a dangling custom: id).
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent         = 'custom:delete-target'
            acpCustomCommand = 'copilot --acp --stdio'
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the custom ACP agent must connect before we delete it'

        # DELETE = switch back to a built-in default and clear the custom command.
        Set-WtSetting -App $script:app -Key 'acpAgent' -Value 'copilot' | Out-Null
        Set-WtSetting -App $script:app -Key 'acpCustomCommand' -Value '' | Out-Null
        Start-Sleep -Seconds 3
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'deleting the custom agent must fall back to a valid built-in that connects'
        Send-AgentPrompt -App $script:app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
        (Test-Until -TimeoutSec 40 -IntervalSec 2 -Condition {
                (Get-AgentPaneText -App $script:app -MaxLines 80) -match '\b7\b'
            }) | Should -BeTrue -Because 'the fallback built-in agent must answer a chat prompt'
        Stop-Terminal -App $script:app
        $script:app = $null
    }
}
