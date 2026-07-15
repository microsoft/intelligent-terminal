#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2/§5 — WT ACCELERATOR (hotkey) + command-palette entry points, driven for real.
#
# These were written off as "WT accelerators aren't harness-injectable" because wtcli send-keys
# reaches a pane's conpty, not WT's keybinding layer. But an OS-level keystroke to the WT WINDOW
# (Send-WtWindowKey, which foregrounds via AttachThreadInput) DOES hit WT's accelerator handler —
# proven for Ctrl+Shift+. (toggle pane), Alt+Shift+B (background delegate), Alt+Shift+/ (delegation
# palette).
#
# Window-level key injection needs the WT window to hold foreground. In an agent-driven session the
# controlling terminal can steal foreground back, so each accelerator action is retried and, if it
# still produces no effect AND the window can't be brought to the foreground, the case SKIPS (a
# foreground precondition) rather than failing flakily. When foreground IS available the assertions
# are real. The delegate ENGINE itself is separately covered deterministically by Feature.Delegate.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §2/§5 agent hotkeys + delegation palette (window-level accelerators)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; delegateAgent = 'copilot' }
        # VK codes: Ctrl+Shift+. = 0xBE (OEM_PERIOD); Alt+Shift+B = 0x42; Alt+Shift+/ = 0xBF (OEM_2).
        $script:TogglePaneHotkey = { Send-WtWindowKey -App $script:app -Vk 0xBE -Ctrl -Shift | Out-Null }
        $script:OpenDelegatePalette = { Send-WtWindowKey -App $script:app -Vk 0xBF -Alt -Shift | Out-Null }
        $script:TabIds = { @((Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id) }
        $script:NewTabCountSince = { param($before) @(@(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)) | Where-Object { $_.tab_id -notin $before }).Count }
        $script:PalettePresent = { Test-CommandPaletteOpen -App $script:app }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Hotkey opens pane (Ctrl+Shift+. opens the agent pane)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        if (Test-AgentPaneOpen -App $script:app) { & $script:TogglePaneHotkey; Start-Sleep 1 }  # ensure hidden first
        $ok = $false
        for ($a = 0; $a -lt 3 -and -not $ok; $a++) { & $script:TogglePaneHotkey; $ok = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-AgentPaneOpen -App $script:app } }
        if (-not $ok -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window could not take foreground for window keys (competing foreground app)'; return }
        $ok | Should -BeTrue -Because 'Ctrl+Shift+. must open the agent pane'
    }

    It 'Hotkey hides pane (Ctrl+Shift+. stashes the agent pane)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        if (-not (Test-AgentPaneOpen -App $script:app)) { for ($a = 0; $a -lt 3 -and -not (Test-AgentPaneOpen -App $script:app); $a++) { & $script:TogglePaneHotkey; Start-Sleep 1 } }
        if (-not (Test-AgentPaneOpen -App $script:app)) { if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition'; return } }
        Test-AgentPaneOpen -App $script:app | Should -BeTrue -Because 'precondition: pane open before the hide test'
        $ok = $false
        for ($a = 0; $a -lt 3 -and -not $ok; $a++) { & $script:TogglePaneHotkey; $ok = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { -not (Test-AgentPaneOpen -App $script:app) } }
        if (-not $ok -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition'; return }
        $ok | Should -BeTrue -Because 'Ctrl+Shift+. must hide/stash the agent pane'
    }

    It 'Alt+Shift+B launches background delegate (a new delegate tab opens)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        $before = & $script:TabIds
        $ok = $false
        for ($a = 0; $a -lt 3 -and -not $ok; $a++) {
            Send-WtWindowKey -App $script:app -Vk 0x42 -Alt -Shift | Out-Null
            $ok = Test-Until -TimeoutSec 8 -IntervalSec 1 -Condition { (& $script:NewTabCountSince $before) -gt 0 }
        }
        if (-not $ok -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition'; return }
        $ok | Should -BeTrue -Because 'Alt+Shift+B must open a new background-delegate tab'
    }

    It 'Alt+Shift+/ opens agent delegation palette (command palette in delegation mode)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        $ok = $false
        for ($a = 0; $a -lt 3 -and -not $ok; $a++) { & $script:OpenDelegatePalette; $ok = Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { & $script:PalettePresent } }
        if (-not $ok -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition'; return }
        $ok | Should -BeTrue -Because 'Alt+Shift+/ must open the agent-delegation command palette'
        Send-WtWindowKey -App $script:app -Vk 0x1B | Out-Null   # Esc to close, leave clean
        Start-Sleep -Milliseconds 500
    }

    It 'Command palette prompt launches delegate (type a request + Enter creates a delegate task)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        $ok = $false
        for ($a = 0; $a -lt 3 -and -not $ok; $a++) {
            & $script:OpenDelegatePalette
            if (-not (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { & $script:PalettePresent })) { continue }
            Set-UiValue -App $script:app -Selector '_searchBox' -Value 'say hello' | Out-Null
            $before = & $script:TabIds
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter -> launch delegate
            $ok = Test-Until -TimeoutSec 10 -IntervalSec 1 -Condition { (& $script:NewTabCountSince $before) -gt 0 }
        }
        if (-not $ok -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition'; return }
        $ok | Should -BeTrue -Because 'submitting a palette prompt must create a delegate task (new tab)'
    }

    It 'Command palette cancel is safe (Esc closes the palette without launching a delegate)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        $opened = $false
        for ($a = 0; $a -lt 3 -and -not $opened; $a++) { & $script:OpenDelegatePalette; $opened = Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { & $script:PalettePresent } }
        if (-not $opened -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition'; return }
        $opened | Should -BeTrue -Because 'the palette must be open before cancelling'
        $before = & $script:TabIds
        Send-WtWindowKey -App $script:app -Vk 0x1B | Out-Null   # Esc
        (Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { -not (& $script:PalettePresent) }) | Should -BeTrue -Because 'Esc must close the delegation palette'
        Start-Sleep 2
        (& $script:NewTabCountSince $before) | Should -Be 0 -Because 'cancelling the palette must NOT launch a delegate'
    }
}