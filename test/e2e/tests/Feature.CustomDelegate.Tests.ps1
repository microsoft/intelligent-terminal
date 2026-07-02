#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §6 — custom DELEGATE agent launched via the WT accelerators.
#
# A delegate is just a command line WT launches in a new tab from the current context. With a
# CUSTOM delegate configured (delegateAgent=custom:<id> + delegateCustomCommand), the Alt+Shift+B
# shortcut and the Alt+Shift+/ delegation palette must launch THAT command. We make the custom
# command self-identifying (`cmd /k echo <marker>`) so the new tab's pane visibly proves the
# configured custom command — not a built-in — was the one launched. Accelerators are driven with
# Send-WtWindowKey (window-level OS keys reach WT's keybinding layer).

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §6 custom delegate via accelerators (Alt+Shift+B / Alt+Shift+/)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:marker = "CUSTOMDELEG$(Get-Random -Maximum 999999)"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent              = 'copilot'
            delegateAgent         = 'custom:markerdelegate'
            delegateCustomCommand = "cmd /k echo $($script:marker)"
        }
        $script:TabIds = { @((Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).tab_id) }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Alt+Shift+B uses custom delegate (background shortcut launches the custom command)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        $wid = [string]$script:app.WindowId
        $before = @((Get-WtTabs -App $script:app -WindowId $wid).tab_id)
        # Retry the accelerator: an occasional cold-start / foreground-contention miss means the
        # keystroke lands but no delegate tab spawns — re-send and re-wait a few times.
        $seen = $false
        for ($attempt = 0; $attempt -lt 3 -and -not $seen; $attempt++) {
            Send-WtWindowKey -App $script:app -Vk 0x42 -Alt -Shift | Out-Null   # Alt+Shift+B
            for ($i = 0; $i -lt 8 -and -not $seen; $i++) {
                Start-Sleep 1
                foreach ($nt in (@(Get-WtTabs -App $script:app -WindowId $wid) | Where-Object { $_.tab_id -notin $before })) {
                    foreach ($p in (Get-WtPanes -App $script:app -WindowId $wid -TabId ([string]$nt.tab_id))) {
                        $cap = ''; try { $cap = Get-WtCapture -App $script:app -SessionId $p.session_id -MaxLines 25 } catch { $cap = '' }
                        if ($cap -match $script:marker) { $seen = $true }
                    }
                }
            }
        }
        if (-not $seen -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window could not take foreground for hotkeys (competing foreground app)'; return }
        $seen | Should -BeTrue -Because 'Alt+Shift+B must launch the CONFIGURED custom delegate command (marker visible in the new tab)'
    }

    It 'Alt+Shift+/ uses custom delegate (delegation palette launches the custom command)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys (competing foreground app in this session)'; return }
        $wid = [string]$script:app.WindowId
        $seen = $false
        for ($attempt = 0; $attempt -lt 3 -and -not $seen; $attempt++) {
            Send-WtWindowKey -App $script:app -Vk 0xBF -Alt -Shift | Out-Null   # Alt+Shift+/
            $paletteOpen = Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                (Find-UiElement -App $script:app -Selector 'Command palette') -match '(?i)Command palette'
            }
            if (-not $paletteOpen) { continue }
            Set-UiValue -App $script:app -Selector '_searchBox' -Value 'do a thing' | Out-Null
            $before = @((Get-WtTabs -App $script:app -WindowId $wid).tab_id)
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter -> launch custom delegate
            for ($i = 0; $i -lt 8 -and -not $seen; $i++) {
                Start-Sleep 1
                foreach ($nt in (@(Get-WtTabs -App $script:app -WindowId $wid) | Where-Object { $_.tab_id -notin $before })) {
                    foreach ($p in (Get-WtPanes -App $script:app -WindowId $wid -TabId ([string]$nt.tab_id))) {
                        $cap = ''; try { $cap = Get-WtCapture -App $script:app -SessionId $p.session_id -MaxLines 25 } catch { $cap = '' }
                        if ($cap -match $script:marker) { $seen = $true }
                    }
                }
            }
        }
        if (-not $seen -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window could not take foreground for hotkeys (competing foreground app)'; return }
        $seen | Should -BeTrue -Because 'the palette must launch the CONFIGURED custom delegate command (marker visible in the new tab)'
    }
}