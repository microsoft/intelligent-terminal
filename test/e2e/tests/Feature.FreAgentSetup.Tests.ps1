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
    # Non-Copilot built-ins surface in the FRE picker only when their CLI is installed.
    $script:NonCopilot = @(
        @{ Cmd = 'claude'; Label = 'Claude' }
        @{ Cmd = 'codex';  Label = 'Codex' }
        @{ Cmd = 'gemini'; Label = 'Gemini' }
    ) | Where-Object { Get-Command $_.Cmd -ErrorAction SilentlyContinue }
    $script:HasNonCopilot = [bool]$script:NonCopilot
}

Describe 'Feature §0 FRE agent setup (overlay controls)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-TerminalFre -Package (Get-ItTestPackage)
        # Advance to the settings page (agent dropdown / toggles / hint / position live here).
        Invoke-UiElement -App $script:app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
        Start-Sleep -Seconds 1
        # Locale-robust "(installed)" suffix from the FreOverlay_AgentStatusInstalled resource, so
        # the install-state assertions work on non-en-US builds (fall back to the en-US literal).
        $rx = Get-WtReswTextRegex -Key 'FreOverlay_AgentStatusInstalled'
        $script:InstalledSfx = if ($rx) { $rx -replace '^\(\?i\)', '' } else { '(\s*\(installed\))' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Copilot preinstalled: the FRE agent dropdown shows Copilot as installed' -Skip:(-not $script:CopilotReady) {
        # The AgentComboBox renders its selected item text in the tree even while collapsed; with
        # the Copilot CLI installed it must carry the localized installed suffix.
        $shown = Test-Until -TimeoutSec 10 -IntervalSec 1 -Condition {
            (Get-UiTree -App $script:app -Depth 16) -match ("(?i)copilot[^\r\n]*" + $script:InstalledSfx)
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

    It 'Detection/suggestion dependency (the suggestion toggle disables when detection is off)' {
        $detectOn = { (Get-UiElement -App $script:app -Selector 'AutoDetectToggle').toggleState -eq 'on' }
        # Drive detection ON — the suggestion toggle must then be ENABLED (user can flip it).
        if (-not (& $detectOn)) { Invoke-UiElement -App $script:app -Selector 'AutoDetectToggle' | Out-Null; Start-Sleep -Milliseconds 800 }
        (& $detectOn) | Should -BeTrue
        Test-UiElementEnabled -App $script:app -Selector 'AutoErrorToggle' |
            Should -BeTrue -Because 'with detection on, the suggestion toggle is user-settable'

        # Turn detection OFF — the suggestion toggle must become DISABLED (greyed / not settable).
        Invoke-UiElement -App $script:app -Selector 'AutoDetectToggle' | Out-Null
        $disabled = Test-Until -TimeoutSec 8 -IntervalSec 1 -Condition {
            -not (Test-UiElementEnabled -App $script:app -Selector 'AutoErrorToggle')
        }
        $disabled | Should -BeTrue -Because 'suggestion cannot be enabled when detection is off (master-detail dependency)'

        # Restore detection ON so the overlay is left in its default state.
        Invoke-UiElement -App $script:app -Selector 'AutoDetectToggle' | Out-Null
    }

    It 'Non-Copilot agents appear as installed in the FRE agent picker' -Skip:(-not $script:HasNonCopilot) {
        # Expand the dropdown so all agent entries (not just the selected one) are in the tree,
        # then assert each installed non-Copilot CLI is offered and labelled installed.
        Invoke-UiElement -App $script:app -Selector 'AgentComboBox' -TimeoutSec 10 | Out-Null
        Start-Sleep -Milliseconds 800
        $tree = Get-UiTree -App $script:app -Depth 18
        foreach ($a in $script:NonCopilot) {
            $tree | Should -Match ("(?i)$($a.Label)[^\r\n]*" + $script:InstalledSfx) -Because "the installed $($a.Label) CLI must appear as a selectable installed agent in the FRE"
        }
    }
}
