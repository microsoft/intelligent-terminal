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
                Test-CommandPaletteOpen -App $script:app
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

# The custom-delegate ENGINE (deterministic, no accelerators/foreground needed): a CUSTOM delegate
# is an arbitrary command line passed as `wta delegate --delegate-agent <cmdline>` (the exact call
# WT builds from delegateAgent=custom:<id> + delegateCustomCommand). Unlike a built-in id, the
# whole command line is used verbatim (coordinator::resolve_delegate_runtime_commandline), so this
# exercises the custom-command path. We drive it directly (like Feature.Delegate does for the
# built-in delegate) to cover the custom cwd + custom error cases without the flaky UI entry points.
Describe 'Feature §6 custom delegate engine (wta delegate custom command)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; delegateAgent = 'copilot' }
        # Launch a delegate with an explicit CUSTOM command line and return the NEW tab + its panes.
        $script:RunCustomDelegate = {
            param($Prompt, $CustomCmd, $Cwd)
            $wid = [string]$script:app.WindowId
            $before = @((Get-WtTabs -App $script:app -WindowId $wid).tab_id)
            $args = @('delegate', $Prompt, '--agent', 'copilot --acp --stdio', '--delegate-agent', $CustomCmd)
            if ($Cwd) { $args += @('--cwd', $Cwd) }
            Invoke-Wta -App $script:app -Arguments $args -TimeoutSec 40 -Raw | Out-Null
            $newTab = $null
            for ($i = 0; $i -lt 30 -and -not $newTab; $i++) {
                $newTab = @(Get-WtTabs -App $script:app -WindowId $wid) | Where-Object { $_.tab_id -notin $before } | Select-Object -First 1
                if (-not $newTab) { Start-Sleep -Milliseconds 500 }
            }
            $panes = if ($newTab) { @(Get-WtPanes -App $script:app -WindowId $wid -TabId ([string]$newTab.tab_id)) } else { @() }
            @{ Tab = $newTab; Panes = $panes }
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Custom delegate cwd is correct (the custom command launches in the requested cwd)' {
        # A self-identifying custom command proves the CONFIGURED custom command (not a built-in) ran,
        # and the pane cwd proves it started in the requested --cwd (the source pane's working dir).
        $marker = "custdeleg$(Get-Random -Maximum 99999)"
        $cwd = Join-Path $env:TEMP $marker
        New-Item -ItemType Directory -Force -Path $cwd | Out-Null
        $d = & $script:RunCustomDelegate -Prompt 'hi' -CustomCmd "cmd /k echo $marker" -Cwd $cwd
        $d.Tab | Should -Not -BeNullOrEmpty -Because 'a custom delegate must create a new tab'
        ($d.Panes.cwd -join '|') | Should -Match ([regex]::Escape($marker)) -Because 'the custom delegate must start in the requested --cwd'
        $sid = $d.Panes[0].session_id
        (Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition {
                (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 25) -match $marker
            }) | Should -BeTrue -Because 'the CONFIGURED custom command (not a built-in) must be the one launched'
    }

    It 'Custom delegate errors are clear (a bad custom command surfaces an actionable error)' {
        # A non-existent custom command must fail VISIBLY in the delegate tab (not silently).
        $d = & $script:RunCustomDelegate -Prompt 'hi' -CustomCmd 'wt-bogus-custom-delegate-xyz --nope'
        $d.Tab | Should -Not -BeNullOrEmpty -Because 'even a failing custom delegate opens a tab so the error is visible'
        $sid = $d.Panes[0].session_id
        (Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition {
                (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 40) -match "(?i)not recognized|not found|is not recognized|exited with code|cannot find|no such file"
            }) | Should -BeTrue -Because 'a bad custom delegate command must surface a clear, actionable error in the tab'
    }
}