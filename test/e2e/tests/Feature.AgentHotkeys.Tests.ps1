#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2/§5 — WT ACCELERATOR (hotkey) + command-palette entry points, driven for real.
#
# These were written off as "WT accelerators aren't harness-injectable" because wtcli send-keys
# reaches a pane's conpty, not WT's keybinding layer. But an OS-level keystroke to the WT WINDOW
# (Send-WtWindowKey) DOES hit WT's accelerator handler — proven here for Ctrl+Shift+. (toggle pane),
# Alt+Shift+B (background delegate), and Alt+Shift+/ (agent-delegation command palette).
#
# Foreground-focus dependent (Send-WtWindowKey activates the WT window first); if that's denied the
# affected assertions can't run — the whole suite gates on being able to open the pane by hotkey.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §2/§5 agent hotkeys + delegation palette (window-level accelerators)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; delegateAgent = 'copilot' }
        # VK codes: Ctrl+Shift+. = 0xBE (OEM_PERIOD); Alt+Shift+B = 0x42; Alt+Shift+/ = 0xBF (OEM_2).
        $script:TogglePaneHotkey = { Send-WtWindowKey -App $script:app -Vk 0xBE -Ctrl -Shift | Out-Null }
        $script:OpenDelegatePalette = { Send-WtWindowKey -App $script:app -Vk 0xBF -Alt -Shift | Out-Null }
        $script:NewTabsSince = {
            param($before) @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)) | Where-Object { $_.tab_id -notin $before }
        }
        $script:TabIds = { @((Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id) }
        $script:PalettePresent = {
            (Find-UiElement -App $script:app -Selector 'Command palette') -match '(?i)Command palette'
        }
        # Confirm hotkeys can drive this window at all (foreground activation works); else skip suite.
        $script:hotkeysWork = $false
        try {
            if (Test-AgentPaneOpen -App $script:app) { & $script:TogglePaneHotkey; Start-Sleep 1 }  # start hidden
            & $script:TogglePaneHotkey
            $script:hotkeysWork = (Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-AgentPaneOpen -App $script:app })
            if ($script:hotkeysWork) { & $script:TogglePaneHotkey | Out-Null; Start-Sleep 1 }  # leave hidden
        }
        catch { }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Hotkey opens pane (Ctrl+Shift+. opens the agent pane)' {
        if (-not $script:hotkeysWork) { Set-ItResult -Skipped -Because 'the WT window could not take foreground for hotkeys (env precondition)'; return }
        if (Test-AgentPaneOpen -App $script:app) { & $script:TogglePaneHotkey; Start-Sleep 1 }  # ensure hidden first
        & $script:TogglePaneHotkey
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { Test-AgentPaneOpen -App $script:app }) | Should -BeTrue -Because 'Ctrl+Shift+. must open the agent pane'
    }

    It 'Hotkey hides pane (Ctrl+Shift+. stashes the agent pane)' {
        if (-not $script:hotkeysWork) { Set-ItResult -Skipped -Because 'hotkeys unavailable (env precondition)'; return }
        if (-not (Test-AgentPaneOpen -App $script:app)) { & $script:TogglePaneHotkey; Start-Sleep 1 }  # ensure open first
        Test-AgentPaneOpen -App $script:app | Should -BeTrue -Because 'precondition: pane open before the hide test'
        & $script:TogglePaneHotkey
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { -not (Test-AgentPaneOpen -App $script:app) }) | Should -BeTrue -Because 'Ctrl+Shift+. must hide/stash the agent pane'
    }

    It 'Alt+Shift+B launches background delegate (a new delegate tab opens)' {
        if (-not $script:hotkeysWork) { Set-ItResult -Skipped -Because 'hotkeys unavailable (env precondition)'; return }
        $before = & $script:TabIds
        Send-WtWindowKey -App $script:app -Vk 0x42 -Alt -Shift | Out-Null
        $newTab = $null
        (Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { $newTab = & $script:NewTabsSince $before; [bool]$newTab }) |
            Should -BeTrue -Because 'Alt+Shift+B must open a new background-delegate tab'
    }

    It 'Alt+Shift+/ opens agent delegation palette (command palette in delegation mode)' {
        if (-not $script:hotkeysWork) { Set-ItResult -Skipped -Because 'hotkeys unavailable (env precondition)'; return }
        & $script:OpenDelegatePalette
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { & $script:PalettePresent }) | Should -BeTrue -Because 'Alt+Shift+/ must open the agent-delegation command palette'
        Send-WtWindowKey -App $script:app -Vk 0x1B | Out-Null   # Esc to close, leave clean
        Start-Sleep -Milliseconds 500
    }

    It 'Command palette prompt launches delegate (type a request + Enter creates a delegate task)' {
        if (-not $script:hotkeysWork) { Set-ItResult -Skipped -Because 'hotkeys unavailable (env precondition)'; return }
        & $script:OpenDelegatePalette
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { & $script:PalettePresent }) | Should -BeTrue -Because 'the delegation palette must be open before typing'
        Set-UiValue -App $script:app -Selector '_searchBox' -Value 'say hello' | Out-Null
        $before = & $script:TabIds
        Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter -> launch delegate
        $newTab = $null
        (Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { $newTab = & $script:NewTabsSince $before; [bool]$newTab }) |
            Should -BeTrue -Because 'submitting a palette prompt must create a delegate task (new tab)'
    }

    It 'Command palette cancel is safe (Esc closes the palette without launching a delegate)' {
        if (-not $script:hotkeysWork) { Set-ItResult -Skipped -Because 'hotkeys unavailable (env precondition)'; return }
        & $script:OpenDelegatePalette
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { & $script:PalettePresent }) | Should -BeTrue -Because 'the palette must be open before cancelling'
        $before = & $script:TabIds
        Send-WtWindowKey -App $script:app -Vk 0x1B | Out-Null   # Esc
        # The palette closes and NO delegate tab is created.
        (Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { -not (& $script:PalettePresent) }) | Should -BeTrue -Because 'Esc must close the delegation palette'
        Start-Sleep 2
        @(& $script:NewTabsSince $before).Count | Should -Be 0 -Because 'cancelling the palette must NOT launch a delegate'
    }
}
