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
    $script:OpenCodeReady = [bool](Get-Command opencode -ErrorAction SilentlyContinue)
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

        Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
        (Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            (Get-UiElement -App $script:app -Selector 'ErrorDetectionComboBox').expandState -eq 'collapsed'
        }) | Should -BeTrue -Because 'the dropdown popup must be dismissed before later setting assertions'
    }

    It 'Token usage toggle is present and defaults off' {
        Test-UiElementExists -App $script:app -Selector 'ShowTokenUsageAndCostToggle' -TimeoutSec 8 |
            Should -BeTrue -Because 'the FRE settings page must expose the token usage preference'
        (Get-UiElement -App $script:app -Selector 'ShowTokenUsageAndCostToggle').toggleState |
            Should -Be 'off' -Because 'token usage and cost must be hidden by default'
    }

    It 'FRE configures automatic approval' {
        Test-UiElementExists -App $script:app -Selector 'AutomaticApprovalToggle' -TimeoutSec 8 |
            Should -BeTrue -Because 'FRE must expose automatic approval for the supported default provider'
        Test-UiElementEnabled -App $script:app -Selector 'AutomaticApprovalToggle' |
            Should -BeTrue
        (Get-UiElement -App $script:app -Selector 'AutomaticApprovalToggle').toggleState |
            Should -Be 'off' -Because 'automatic approval defaults off'

        foreach ($key in @('AIAgents_YoloMode.Header', 'AIAgents_YoloMode.HelpText')) {
            $values = @(Get-WtReswTextValues -Key $key)
            $element = Wait-Until -TimeoutSec 12 -Because "FRE to render the shared $key resource" -Condition {
                foreach ($value in $values) {
                    if ($match = Get-UiElement -App $script:app -Selector $value) {
                        return $match
                    }
                }
            }
            $element.name | Should -BeIn $values
        }
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

Describe 'Feature §0 FRE unsupported automatic approval' -Tag 'Feature' -Skip:(-not ($script:Ready -and $script:OpenCodeReady)) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -ShowFre -Settings @{
            acpAgent = 'opencode'
            'agentPane.yoloMode' = $true
            autoErrorDetectionEnabled = $false
            agentSessionManagementEnabled = $false
        }
        Invoke-UiElement -App $script:app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'FRE hides unsupported automatic approval' {
        Test-UiElementExists -App $script:app -Selector 'AutomaticApprovalToggle' -TimeoutSec 1 |
            Should -BeFalse
        Invoke-UiElement -App $script:app -Selector 'SaveButton' -TimeoutSec 20 | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'FRE to persist unsupported automatic approval off' -Condition {
            (Get-WtSetting -App $script:app -Key 'agentPane.yoloMode') -eq $false
        } | Out-Null
    }
}
