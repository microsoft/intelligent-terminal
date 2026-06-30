#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE — the FRE-overlay-specific agent-setup items that ARE automatable via
# winapp UIA but were previously left manual. The FRE's SECOND page (reached via NextButton) hosts
# the agent dropdown, the auto-error toggles, the session-management toggle + its install hint, and
# the pane-position picker, all as named XAML controls. Deterministic: assert on those controls /
# their rendered state — no agent/LLM involved.
#
# Not covered here (genuinely not cleanly UIA-observable, kept manual/UT):
#   * detection→suggestion "disabled" dependency — the toggle tree exposes only [on]/[off], not the
#     enabled/disabled state (the dependency itself is UT-locked: EffectiveAutoFixFalseWhenDetectionOff);
#   * "Copilot without install" / "install failure messages" — need a destructive uninstalled/failed
#     CLI state to induce.

BeforeDiscovery {
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue))
    $script:CopilotReady = [bool](Get-Command copilot -ErrorAction SilentlyContinue)
}

Describe 'Feature §0 FRE agent setup (overlay controls)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-TerminalFre -Package (Get-ItTestPackage)
        # Advance to the settings page (agent dropdown / toggles / hint / position live here).
        Invoke-UiElement -App $script:app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
        Start-Sleep -Seconds 1
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Copilot preinstalled: the FRE agent dropdown shows Copilot as installed' -Skip:(-not $script:CopilotReady) {
        # The AgentComboBox renders its selected item text in the tree even while collapsed; with
        # the Copilot CLI installed it must be labelled "(installed)".
        $shown = Test-Until -TimeoutSec 10 -IntervalSec 1 -Condition {
            (Get-UiTree -App $script:app -Depth 16) -match '(?i)copilot.*\(installed\)'
        }
        $shown | Should -BeTrue -Because 'with the Copilot CLI installed, the FRE agent picker must list it as installed'
    }

    It 'Session hook hints appear only when the session-management toggle is on' {
        $tree = { Get-UiTree -App $script:app -Depth 18 }
        $smOn = { (& $tree) -match 'SessionManagementToggle[^\r\n]*\[on\]' }
        $hint = { [bool]((& $tree) -match 'SessionManagementHint') }

        # Drive to a known ON state (default), then assert the install hint is shown.
        if (-not (& $smOn)) { Invoke-UiElement -App $script:app -Selector 'SessionManagementToggle' | Out-Null; Start-Sleep -Milliseconds 800 }
        (& $smOn) | Should -BeTrue -Because 'the session-management toggle should be enableable in the FRE'
        (& $hint) | Should -BeTrue -Because 'the install-hooks hint row is shown while session management is enabled'

        # Toggle OFF — the informational hint row must disappear.
        Invoke-UiElement -App $script:app -Selector 'SessionManagementToggle' | Out-Null
        Start-Sleep -Milliseconds 800
        (& $smOn) | Should -BeFalse
        Test-Until -TimeoutSec 8 -IntervalSec 1 -Condition { -not (& $hint) } |
            Should -BeTrue -Because 'the install-hooks hint must be hidden when session management is off'

        # Restore ON so the suite leaves the overlay in its default state.
        Invoke-UiElement -App $script:app -Selector 'SessionManagementToggle' | Out-Null
    }
}
