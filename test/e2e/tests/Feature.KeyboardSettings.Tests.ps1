#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §11 accessibility (C197) — the Settings editor must be operable with the KEYBOARD
# alone. The editor is opened with the Ctrl+, accelerator (a real keypress, Open-WtSettings), then we
# move keyboard focus with UIA SetFocus (winapp focus — the focus a Tab / screen-reader user lands
# on) and ACTIVATE with a real Enter keypress (Send-WtWindowKey), never a mouse click. Navigating to
# the "Agents" page via focus+Enter and seeing its content render proves keyboard operability.
#
# Activation requires the WT window to hold foreground; when it can't be taken the case SKIPS.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §11 keyboard-only Settings' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Keyboard-only Settings works (Ctrl+, opens Settings; focus + Enter navigates to the Agents page)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+, accelerator / keyboard activation'; return }
        # Open the Settings editor via the Ctrl+, keyboard accelerator.
        Open-WtSettings -App $script:app | Out-Null
        $hwnd = [string]$script:app.Hwnd
        (Test-UiElementExists -App $script:app -Selector 'AIAgentsNavItem' -TimeoutSec 10) |
            Should -BeTrue -Because 'Ctrl+, must open the Settings editor (nav items present)'

        # Focus the Agents nav item with the keyboard and activate it with Enter; the AI Agents page
        # (AcpAgent group) must render — proving keyboard navigation of the Settings nav.
        $navigated = $false
        for ($a = 0; $a -lt 3 -and -not $navigated; $a++) {
            Set-WtWindowForeground -App $script:app | Out-Null
            & winapp ui focus 'AIAgentsNavItem' -w $hwnd 2>&1 | Out-Null
            # winapp focus targets the AutomationId (locale-independent). Don't gate the Enter on the
            # focused element's NAME ("Agents") — that's localized; rely on the post-action structural
            # check (the AcpAgent group renders) + the retry loop instead.
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter activates the focused nav item
            $navigated = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-UiElementExists -App $script:app -Selector 'AcpAgent' -TimeoutSec 1 }
        }
        if (-not $navigated -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition for keyboard activation'; return }
        $navigated | Should -BeTrue -Because 'keyboard Enter on the focused Agents nav item must open the AI Agents page (AcpAgent group renders)'
    }
}
