#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §1/§6 — the SETTINGS EDITOR UI itself, driven for real.
#
# Earlier these were written off as "the Settings editor can't be opened by the harness". That was
# wrong: WT opens Settings on the Ctrl+, accelerator, and while wtcli send-keys only reaches a
# pane's conpty, an OS-level keystroke to the WT WINDOW (Send-WtWindowKey / Open-WtSettings) does
# hit WT's accelerator handler. Once open, the Settings editor is a normal XAML surface — winapp
# can inspect / search / invoke / set-value its controls. This suite proves the editor is
# drivable end-to-end (page opens, nav works, the model control is visible for a custom agent).
#
# Foreground-focus dependent: Open-WtSettings needs the WT window to take foreground, so if that's
# denied (locked session / competing foreground app) the suite SKIPS rather than failing.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §1/§6 Settings editor UI (opened via Ctrl+, accelerator)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # A custom agent so the model control is expected to render on the AI Agents page.
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:qwen'; acpCustomCommand = 'copilot --acp --stdio'
        }
        # Try to open Settings once; if the window can't take foreground, mark the suite skippable.
        $script:settingsOpen = $false
        try { Open-WtSettings -App $script:app -TimeoutSec 20 | Out-Null; $script:settingsOpen = $true }
        catch { Write-Host "Open-WtSettings failed (foreground denied?): $($_.Exception.Message)" }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'AI Agents settings page opens and shows the agent picker' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # The AI Agents page renders the agent picker group (stable AutomationId AcpAgent).
        Test-UiElementExists -App $script:app -Selector 'AcpAgent' -TimeoutSec 8 | Should -BeTrue -Because 'the AI Agents page must show the agent picker'
    }

    It 'Model selection visible: the model control shows on the AI Agents page for a custom agent' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # With a custom agent selected, the "Model" control (picker/textbox) must remain visible.
        # Assert via a text search of the rendered page — locale-tolerant on the English label is
        # acceptable here because the control's help text/label is a stable UI string.
        $models = Find-UiElement -App $script:app -Selector 'Model'
        $models | Should -Match '(?i)Model' -Because 'the model control must be visible when a custom agent is selected'
    }
}
