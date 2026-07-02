#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §11 accessibility (C196) — the FRE must be completable with the KEYBOARD alone.
# We move keyboard focus with UIA SetFocus (winapp focus — the same focus a screen-reader / Tab user
# lands on) and ACTIVATE with a real Enter keypress sent to the window (Send-WtWindowKey), never a
# mouse click/InvokePattern. Advancing Welcome -> agent-setup -> Save via focus+Enter and reaching a
# completed FRE (agentFreCompleted=true, overlay dismissed) proves keyboard operability.
#
# Activation requires the WT window to hold foreground (Send-WtWindowKey). When it can't be taken the
# case SKIPS (a foreground precondition) rather than failing flakily.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §11 keyboard-only FRE' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll { Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force }
    AfterEach { if ($script:app) { Stop-Terminal -App $script:app; $script:app = $null } }

    It 'Keyboard-only FRE works (focus + Enter advances Welcome -> Save and completes the FRE)' {
        $script:app = Start-TerminalFre -Package (Get-ItTestPackage)
        Test-UiElementExists -App $script:app -Selector 'WelcomePage' -TimeoutSec 12 | Should -BeTrue -Because 'the FRE Welcome page must show on a fresh profile'
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for keyboard activation (Enter)'; return }
        $hwnd = [string]$script:app.Hwnd

        # Page 1 (Welcome): focus Next with the keyboard, activate with Enter.
        $advanced = $false
        for ($a = 0; $a -lt 3 -and -not $advanced; $a++) {
            Set-WtWindowForeground -App $script:app | Out-Null
            & winapp ui focus 'NextButton' -w $hwnd 2>&1 | Out-Null
            # winapp focus targets the AutomationId (locale-independent). Don't gate the Enter on the
            # focused element's NAME ("Next") — that's localized; rely on the post-action structural
            # check (SaveButton appears) + the retry loop instead.
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter activates the focused Next
            $advanced = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-UiElementExists -App $script:app -Selector 'SaveButton' -TimeoutSec 1 }
        }
        if (-not $advanced -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition for keyboard activation'; return }
        $advanced | Should -BeTrue -Because 'keyboard Enter on the focused Next button must advance to the agent-setup page (Save present)'

        # Page 2 (agent setup): focus Save with the keyboard, activate with Enter -> FRE completes.
        $completed = $false
        for ($a = 0; $a -lt 3 -and -not $completed; $a++) {
            Set-WtWindowForeground -App $script:app | Out-Null
            & winapp ui focus 'SaveButton' -w $hwnd 2>&1 | Out-Null
            # As above: don't gate on the localized focused-name "Save"; rely on the completion check.
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter activates the focused Save
            $completed = Test-Until -TimeoutSec 8 -IntervalSec 1 -Condition {
                (-not (Test-UiElementExists -App $script:app -Selector 'WelcomePage' -TimeoutSec 1)) -and (Get-FreCompleted -App $script:app)
            }
        }
        if (-not $completed -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition for keyboard activation'; return }
        $completed | Should -BeTrue -Because 'keyboard Enter on the focused Save button must complete the FRE (agentFreCompleted=true, overlay dismissed)'
    }
}
