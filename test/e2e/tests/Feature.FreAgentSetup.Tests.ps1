#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE — the FRE-overlay-specific agent-setup items that ARE automatable via
# winapp UIA but were previously left manual. The FRE's SECOND page (reached via NextButton) hosts
# the agent dropdown, the error-detection dropdown, the session-management toggle, and
# the pane-position picker, all as named XAML controls. Deterministic: assert on those controls /
# their rendered state — no agent/LLM involved.
#
# Not covered here (genuinely not cleanly UIA-observable, kept manual/UT):
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
        # Advance to the settings page (agent dropdown / preferences / position live here).
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

    It 'Setup hints are not rendered inside setting cards' {
        foreach ($hint in @('AgentInstallHintRow', 'AutoDetectShellIntegrationHintRow', 'SessionManagementHintRow')) {
            Test-UiElementExists -App $script:app -Selector $hint -TimeoutSec 1 |
                Should -BeFalse -Because "the FRE should not render the $hint inline hint"
        }
    }

    It 'Error detection is a single dropdown with all three modes' {
        Test-UiElementExists -App $script:app -Selector 'ErrorDetectionComboBox' -TimeoutSec 8 |
            Should -BeTrue -Because 'the FRE settings page must expose one error-detection dropdown'

        Invoke-UiElement -App $script:app -Selector 'ErrorDetectionComboBox' | Out-Null
        Start-Sleep -Milliseconds 800
        $tree = Get-UiTree -App $script:app -Depth 18
        foreach ($option in @(
            @{ Key = 'FreOverlay_ErrorDetectionDetectOption.Content'; Fallback = 'Detect errors' }
            @{ Key = 'FreOverlay_ErrorDetectionAutoFixOption.Content'; Fallback = 'Detect and fix errors' }
            @{ Key = 'FreOverlay_ErrorDetectionOffOption.Content'; Fallback = 'Off' }
        )) {
            $rx = Get-WtReswTextRegex -Key $option.Key
            if (-not $rx) { $rx = [regex]::Escape($option.Fallback) }
            $tree | Should -Match $rx -Because "the error-detection dropdown must include '$($option.Fallback)'"
        }

        Test-UiElementExists -App $script:app -Selector 'AutoDetectToggle' -TimeoutSec 1 |
            Should -BeFalse -Because 'the former detection toggle is replaced by the dropdown'
        Test-UiElementExists -App $script:app -Selector 'AutoErrorToggle' -TimeoutSec 1 |
            Should -BeFalse -Because 'the subordinate automatic-error setting is removed'

        Invoke-UiElement -App $script:app -Selector 'ErrorDetectionComboBox' | Out-Null
    }

    It 'Token usage toggle is present and defaults off' {
        Test-UiElementExists -App $script:app -Selector 'ShowTokenUsageAndCostToggle' -TimeoutSec 8 |
            Should -BeTrue -Because 'the FRE settings page must expose the token usage preference'
        (Get-UiElement -App $script:app -Selector 'ShowTokenUsageAndCostToggle').toggleState |
            Should -Be 'off' -Because 'token usage and cost must be hidden by default'
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
