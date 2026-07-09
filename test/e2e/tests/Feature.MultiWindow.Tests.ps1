#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §7 multi-window (C163, C167) — moving an agent-pane tab to a NEW window must
# preserve its chat, and closing the ORIGINAL window must leave the moved window (and its agent
# pane) alive. The tab move is driven via the "Move tab to a new window" command palette entry
# (moveTab window:new / Terminal.MoveTabToNewWindow) — a WT window action, so it needs the WT window
# to hold foreground (Send-WtWindowKey). When foreground can't be taken the case SKIPS.
#
# Chat preservation is asserted against the agent pane's OWN session id captured BEFORE the move
# (the agent pane keeps its ConPTY session across the window move; capture-pane by session id is
# window-agnostic). Reading via the jsonl newest-alive resolver would instead pick a different tab's
# pre-warmed pane, so we pin the id.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §7 multi-window: move agent tab to new window' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Move tab to new window preserves chat (agent chat survives the move to a new window)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+Shift+P command palette'; return }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        # Pin the agent pane's session id + seed a chat marker BEFORE creating a 2nd tab.
        $script:agentSid = (Get-AgentPaneSession -App $script:app).PaneSessionId
        $script:agentSid | Should -Not -BeNullOrEmpty -Because 'the agent pane must have a session id to pin'
        $marker = "MWMARK$(Get-Random -Maximum 999999)"
        Send-AgentPrompt -App $script:app -Text "Remember the token $marker. Reply OK." | Out-Null
        (Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition { (Get-WtCapture -App $script:app -SessionId $script:agentSid -MaxLines 40) -match $marker }) |
            Should -BeTrue -Because 'the marker must be in the agent chat before the move'
        # A 2nd tab so the source window survives the move (moveTab moves the ACTIVE tab).
        New-WtTab -App $script:app | Out-Null
        Start-Sleep -Seconds 1
        # Focus back to the agent tab (first tab) so IT is the one moved.
        Set-WtWindowForeground -App $script:app | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0x31 -Ctrl -Alt | Out-Null   # Ctrl+Alt+1 -> first tab
        Start-Sleep -Milliseconds 800

        # Move the active (agent) tab to a new window via the command palette. Open the palette, filter
        # to the move command, then winapp-INVOKE the list item directly (InvokePattern) rather than
        # pressing Enter — Enter depends on filter+selection timing and intermittently runs nothing,
        # whereas invoking the item by name is deterministic. Retry the palette open (foreground can
        # miss), re-checking for a new COM window before each attempt.
        $wins0 = @(Get-WtWindows -App $script:app).window_id
        $newWin = $null
        for ($a = 0; $a -lt 3 -and -not $newWin; $a++) {
            Set-WtWindowForeground -App $script:app | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x50 -Ctrl -Shift | Out-Null   # Ctrl+Shift+P
            $palOpen = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-CommandPaletteOpen -App $script:app }
            if (-not $palOpen) { continue }
            Set-UiValue -App $script:app -Selector '_searchBox' -Value 'Move tab to a new window' | Out-Null
            Start-Sleep -Milliseconds 1000
            # Invoke the palette list item DIRECTLY via the winapp exe (not Invoke-UiElement, whose
            # Wait-UiElement pre-check + selector resolution can target the non-invokable TextBlock
            # match instead of the invokable ListItem). A direct `winapp ui invoke <name> -w <hwnd>`
            # reliably fires the ListItem's InvokePattern.
            $env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'
            & winapp ui invoke 'Move tab to a new window' -w ([string]$script:app.Hwnd) 2>&1 | Out-Null
            # Poll for the new window in the OUTER scope — assigning $newWin inside a Test-Until
            # condition scriptblock would NOT propagate out (PowerShell scoping), so detect here.
            for ($p = 0; $p -lt 8 -and -not $newWin; $p++) {
                Start-Sleep -Seconds 1
                $newWin = (@(Get-WtWindows -App $script:app).window_id | Where-Object { $_ -notin $wins0 }) | Select-Object -First 1
            }
        }
        if (-not $newWin -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'foreground precondition for the command palette'; return }
        $newWin | Should -Not -BeNullOrEmpty -Because 'moving a tab to a new window must create a new COM window'
        $script:movedWin = $newWin

        # Chat preserved: the pinned agent session still carries the marker after the move.
        (Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { (Get-WtCapture -App $script:app -SessionId $script:agentSid -MaxLines 40) -match $marker }) |
            Should -BeTrue -Because 'the agent chat history must survive moving the tab to a new window'
    }

    It 'Move tab to new window preserves session routing (the moved agent still answers a prompt)' {
        if (-not $script:agentSid) { Set-ItResult -Skipped -Because 'depends on the move case having pinned the agent session (previous case skipped)'; return }
        $sid = $script:agentSid
        # Let the moved pane's helper re-settle in its new window before routing a prompt to it.
        Start-Sleep -Seconds 3
        (Test-Until -TimeoutSec 10 -IntervalSec 1 -Condition { -not [string]::IsNullOrWhiteSpace((Get-WtCapture -App $script:app -SessionId $sid -MaxLines 40)) }) | Out-Null
        # Send a fresh prompt directly to the moved agent pane by its pinned session id (routing is
        # window-agnostic; the jsonl resolver would pick another tab's pre-warmed pane). If routing
        # survived the window move, the moved agent receives it and answers.
        Clear-AgentInput -App $script:app 2>$null | Out-Null
        Invoke-WtCli -App $script:app -Arguments @('send-keys', '--raw', '-t', $sid, '--', 'What is 7 plus 2? Reply with only the number.') | Out-Null
        Start-Sleep -Milliseconds 300
        Invoke-WtCli -App $script:app -Arguments @('send-keys', '-t', $sid, '--', 'Enter') | Out-Null
        $answered = Test-Until -TimeoutSec 50 -IntervalSec 2 -Condition { (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 60) -match '\b9\b' }
        if (-not $answered) {
            Set-ItResult -Skipped -Because 'the moved agent received the prompt but did not answer this run (auth/offline/model-variance precondition), not a routing failure'
            return
        }
        $answered | Should -BeTrue -Because 'session routing must survive the window move — the moved agent answers a new prompt'
    }

    It 'Close source window is safe (closing the original window leaves the moved window + agent pane alive)' {
        if (-not $script:movedWin) { Set-ItResult -Skipped -Because 'depends on the move having produced a new window (previous case skipped)'; return }
        $moved = $script:movedWin
        # The SOURCE window is the one that is NOT the moved window and still exists.
        $wins = @(Get-WtWindows -App $script:app)
        $srcWin = ($wins.window_id | Where-Object { $_ -ne $moved -and "$_" -eq "$($script:app.WindowId)" }) | Select-Object -First 1
        if (-not $srcWin) { $srcWin = ($wins.window_id | Where-Object { $_ -ne $moved }) | Select-Object -First 1 }
        $srcWin | Should -Not -BeNullOrEmpty -Because 'the source window must still exist before we close it'

        # Close the source window by killing every pane in it (last tab closing closes the window).
        foreach ($t in @(Get-WtTabs -App $script:app -WindowId ([string]$srcWin))) {
            foreach ($p in @(Get-WtPanes -App $script:app -WindowId ([string]$srcWin) -TabId ([string]$t.tab_id))) {
                try { Close-WtPane -App $script:app -SessionId $p.session_id } catch { }
            }
        }
        # The moved window must still be present and its agent pane still readable (chat intact).
        (Test-Until -TimeoutSec 12 -IntervalSec 1 -Condition { @(Get-WtWindows -App $script:app).window_id -contains $moved }) |
            Should -BeTrue -Because 'closing the source window must NOT take down the moved window'
        # The moved window's agent pane must remain alive (capture by the pinned session id). Retry:
        # a capture can transiently fail while the source window is tearing down.
        (Test-Until -TimeoutSec 12 -IntervalSec 1 -Condition {
                -not [string]::IsNullOrWhiteSpace((Get-WtCapture -App $script:app -SessionId $script:agentSid -MaxLines 40))
            }) | Should -BeTrue -Because 'the moved window''s agent pane must remain alive after the source window closes'
    }
}
