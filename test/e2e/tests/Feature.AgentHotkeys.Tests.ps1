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

BeforeDiscovery {
    $script:LayoutHotkeyReady = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue))
}

Describe 'Feature: layout-aware agent history and sidebar hotkeys' -Tag @('Feature', 'LayoutHotkeys') -Skip:(-not $script:LayoutHotkeyReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $root = if ($env:ITE2E_ARTIFACT_ROOT) {
            $env:ITE2E_ARTIFACT_ROOT
        }
        else {
            Join-Path $PSScriptRoot '..\artifacts'
        }
        $script:evidenceDir = Join-Path ([IO.Path]::GetFullPath($root)) "agent-hotkeys\$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null
        $script:fixtureLog = Join-Path $script:evidenceDir 'fixture.log'
        $script:releasePromptPath = Join-Path $script:evidenceDir 'release-prompt'
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))' -ReleasePromptPath '$($script:releasePromptPath.Replace("'", "''"))'"
        $script:fixtureCommand = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
        $script:OpenAgentHistoryHotkey = {
            param($App)
            Send-WtWindowKey -App $App -Vk 0xBF -Ctrl -Shift -RequireForeground | Out-Null
        }
        $script:ToggleSidebarHotkey = {
            param($App)
            Send-WtWindowKey -App $App -Vk 0x53 -Ctrl -Shift -RequireForeground | Out-Null
        }
        $script:HistorySearchFocused = {
            param($App)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$App.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistorySearchTextBox')
            $search = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $search -and -not $search.Current.IsOffscreen -and $search.Current.HasKeyboardFocus
        }
        $script:StartLayoutApp = {
            param([ValidateSet('horizontal', 'vertical')][string]$Layout)
            Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
                language = 'en-US'
                tabLayout = $Layout
                actions = @()
                keybindings = @()
                'warning.confirmOnClose' = 'never'
                acpAgent = 'custom:layout-hotkey-fixture'
                acpCustomCommand = $script:fixtureCommand
                acpModel = ''
            }
        }
    }

    # One result owns both checklist contracts, including their shared focus and data-preservation oracles.
    It 'Agent history hotkey toggles the layout-appropriate history surface; Sidebar show/collapse hotkey works' {
        $horizontal = $null
        try {
            $horizontal = & $script:StartLayoutApp 'horizontal'
            if (-not (Test-WtWindowKeyFocusable -App $horizontal)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys'
                return
            }
            Open-AgentPane -App $horizontal | Out-Null
            Wait-AgentReady -App $horizontal -TimeoutSec 30 | Out-Null
            Save-UiScreenshot -App $horizontal -Path (Join-Path $script:evidenceDir 'history-horizontal-before.png') | Out-Null
            & $script:OpenAgentHistoryHotkey $horizontal
            (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                Test-SessionListShown -App $horizontal -TimeoutSec 1
            }) | Should -BeTrue -Because 'Ctrl+Shift+/ must open the existing agent-pane session view in classic horizontal layout'
            Save-UiScreenshot -App $horizontal -Path (Join-Path $script:evidenceDir 'history-horizontal-after.png') | Out-Null
            & $script:OpenAgentHistoryHotkey $horizontal
            (Test-Until -TimeoutSec 8 -Condition {
                -not (Test-AgentPaneOpen -App $horizontal)
            }) | Should -BeTrue -Because 'a second Ctrl+Shift+/ must hide horizontal agent sessions'
            Save-UiScreenshot -App $horizontal -Path (Join-Path $script:evidenceDir 'history-horizontal-hidden.png') | Out-Null
        }
        finally {
            if ($horizontal) { Stop-Terminal -App $horizontal }
        }

        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys'
                return
            }
            Wait-UiElement -App $vertical -Selector 'SearchTabsButton' | Out-Null
            Test-UiElementExists -App $vertical -Selector 'HistorySearchTextBox' -TimeoutSec 1 |
                Should -BeFalse -Because 'Sidebar History must start closed'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-vertical-before.png') | Out-Null
            & $script:OpenAgentHistoryHotkey $vertical
            $historyOpened = Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                $history = Get-UiElement -App $vertical -Selector 'HistorySearchTextBox'
                $history -and -not $history.isOffscreen -and $history.width -gt 0 -and $history.height -gt 0
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-vertical-after.png') | Out-Null
            $historyOpened | Should -BeTrue -Because 'Ctrl+Shift+/ must open Sidebar History, not the agent-pane sessions view, in vertical layout'
            Test-AgentPaneOpen -App $vertical | Should -BeFalse -Because 'opening Sidebar History must not also open the WTA agent pane'
            (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) |
                Should -BeTrue -Because 'the history accelerator must focus the sidebar search box'

            & $script:OpenAgentHistoryHotkey $vertical
            $historyHidden = Test-Until -TimeoutSec 8 -Condition {
                $history = Get-UiElement -App $vertical -Selector 'HistorySearchTextBox'
                -not ($history -and -not $history.isOffscreen -and $history.width -gt 0 -and $history.height -gt 0)
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-vertical-hidden.png') | Out-Null
            $historyHidden | Should -BeTrue -Because 'a second Ctrl+Shift+/ must hide Sidebar History while search owns focus'
            $tabs = Get-UiElement -App $vertical -Selector 'SearchTabsButton'
            ($tabs -and -not $tabs.isOffscreen -and $tabs.width -gt 0) |
                Should -BeTrue -Because 'closing history must restore the normal expanded sidebar'
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) |
                Should -BeTrue -Because 'history must reopen and focus search after being toggled closed'

            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
            $hiddenHistory = Get-UiElement -App $vertical -Selector 'HistorySearchTextBox'
            ($hiddenHistory -and -not $hiddenHistory.isOffscreen -and $hiddenHistory.width -gt 0) |
                Should -BeFalse -Because 'collapsing the sidebar must close its history view'

            & $script:OpenAgentHistoryHotkey $vertical
            $reopened = Test-Until -TimeoutSec 8 -Condition { & $script:HistorySearchFocused $vertical }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-from-collapsed-sidebar.png') | Out-Null
            $reopened | Should -BeTrue -Because 'the history accelerator must expand a collapsed sidebar and restore history search focus'

            & $script:OpenAgentHistoryHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            $terminal = Get-ActivePane -App $vertical
            $split = Split-WtPane -App $vertical -SessionId $terminal.session_id -Direction right -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            Open-AgentPane -App $vertical | Out-Null
            Wait-AgentReady -App $vertical -TimeoutSec 30 | Out-Null
            $agent = Get-AgentPaneSession -App $vertical
            $agent.AcpSessionId | Should -Match '^chat-fixture-\d+-\d+$' -Because 'this public regression must never submit to a real provider'
            (Get-UiElement -App $vertical -Selector AgentLabelText).name |
                Should -Match '^Chat Fixture\b' -Because 'a provider fallback must fail before the deterministic test prompt is sent'
            $turn = "SCROLL_TURN_00_$([guid]::NewGuid().ToString('N'))"
            $agentDraft = "CHAT_DRAFT_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            $terminalDraft = "SHELL_DRAFT_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            $splitDraft = "SPLIT_DRAFT_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            Send-AgentPrompt -App $vertical -PaneSessionId $agent.PaneSessionId -Text "$turn HOLD_FOR_RELEASE" | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'the deterministic agent turn to be pending' -Condition {
                (Get-AgentPaneText -App $vertical -PaneSessionId $agent.PaneSessionId).Contains("PENDING_$turn")
            } | Out-Null
            Send-AgentPrompt -App $vertical -PaneSessionId $agent.PaneSessionId -Text $agentDraft -NoSubmit | Out-Null
            Send-WtInput -App $vertical -SessionId $terminal.session_id -Text $terminalDraft | Out-Null
            Send-WtInput -App $vertical -SessionId $split.session_id -Text $splitDraft | Out-Null
            $verifyPreservation = {
                (Get-AgentPaneText -App $vertical -PaneSessionId $agent.PaneSessionId) |
                    Should -MatchExactly ([regex]::Escape($agentDraft)) -Because 'the unsent agent draft must survive the view transition'
                foreach ($draft in @(
                    @{ Id = $terminal.session_id; Text = $terminalDraft }
                    @{ Id = $split.session_id; Text = $splitDraft }
                )) {
                    (Get-WtCapture -App $vertical -SessionId $draft.Id -MaxLines 30).TrimEnd() |
                        Should -MatchExactly ([regex]::Escape($draft.Text) + '$') -Because 'the unsent terminal draft must remain intact'
                }
                $log = Get-Content -LiteralPath $script:fixtureLog -Raw
                $log | Should -Match ('\|held\|' + [regex]::Escape($turn))
                $log | Should -Not -Match ('\|cancel\|' + [regex]::Escape($agent.AcpSessionId))
                $log | Should -Not -Match ('\|released\|' + [regex]::Escape($turn))
            }
            Wait-Until -TimeoutSec 5 -Condition {
                (Get-AgentPaneText -App $vertical -PaneSessionId $agent.PaneSessionId).Contains($agentDraft) -and
                    (Get-WtCapture -App $vertical -SessionId $split.session_id -MaxLines 30).TrimEnd().EndsWith($splitDraft)
            } | Out-Null
            & $verifyPreservation
            $origins = @(
                @{ Name = 'terminal'; Id = $terminal.session_id; Agent = $false; Draft = $terminalDraft }
                @{ Name = 'split'; Id = $split.session_id; Agent = $false; Draft = $splitDraft }
                @{ Name = 'agent'; Id = $agent.PaneSessionId; Agent = $true; Draft = $agentDraft }
            )
            foreach ($origin in $origins) {
                Set-WtWindowForeground -App $vertical | Should -BeTrue
                if ($origin.Agent) {
                    Invoke-WtCli -App $vertical -Arguments @('focus-pane', '-t', $origin.Id) | Out-Null
                }
                else {
                    Set-WtPaneFocus -App $vertical -SessionId $origin.Id
                }
                $originFocus = Wait-Until -TimeoutSec 5 -Because "the $($origin.Name) input to receive focus" -Condition {
                    $focused = [Windows.Automation.AutomationElement]::FocusedElement
                    if ($focused -and $focused.Current.ProcessId -eq $vertical.Pid -and $focused.Current.HasKeyboardFocus -and
                        $focused.Current.ClassName -eq 'TermControl' -and
                        (($focused.Current.Name -eq 'Agent Pane') -eq $origin.Agent)) {
                        $pattern = $null
                        if ($focused.TryGetCurrentPattern([Windows.Automation.TextPattern]::Pattern, [ref]$pattern) -and
                            $pattern.DocumentRange.GetText(-1).Contains($origin.Draft)) { $focused }
                    }
                }
                $focusRestored = {
                    [Windows.Automation.Automation]::Compare(
                        $originFocus, [Windows.Automation.AutomationElement]::FocusedElement)
                }.GetNewClosure()
                foreach ($initiallyCollapsed in @($false, $true)) {
                    if ($initiallyCollapsed) {
                        & $script:ToggleSidebarHotkey $vertical
                        Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                    }
                    foreach ($closeWithButton in @($false, $true)) {
                        & $script:OpenAgentHistoryHotkey $vertical
                        (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue
                        if ($closeWithButton) { Invoke-UiElement -App $vertical -Selector HistoryCloseButton | Out-Null }
                        else { & $script:OpenAgentHistoryHotkey $vertical }
                        $expectedLabel = if ($initiallyCollapsed) { 'Expand sidebar' } else { 'Collapse sidebar' }
                        Wait-UiElement -App $vertical -Selector $expectedLabel | Out-Null
                        (Test-Until -TimeoutSec 5 -Condition $focusRestored) |
                            Should -BeTrue -Because "closing history must restore the exact $($origin.Name) input and original sidebar state"
                        & $verifyPreservation
                    }
                    if ($initiallyCollapsed) {
                        & $script:ToggleSidebarHotkey $vertical
                        Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
                        (Test-Until -TimeoutSec 5 -Condition $focusRestored) |
                            Should -BeTrue -Because 'expanding the sidebar must not steal input focus or activate search'
                    }
                }
                & $script:OpenAgentHistoryHotkey $vertical
                (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue
                & $script:ToggleSidebarHotkey $vertical
                Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                (Test-Until -TimeoutSec 5 -Condition {
                    $focused = [Windows.Automation.AutomationElement]::FocusedElement
                    $focused -and $focused.Current.ProcessId -eq $vertical.Pid -and $focused.Current.HasKeyboardFocus -and
                        $focused.Current.Name -ne 'Agent Pane' -and $focused.Current.ClassName -eq 'TermControl'
                }) | Should -BeTrue -Because 'collapsing from history must choose a visible terminal without restoring an agent origin'
                Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir "focus-$($origin.Name)-restored.png") | Out-Null
                & $verifyPreservation
                $fallbackFocus = [Windows.Automation.AutomationElement]::FocusedElement
                & $script:ToggleSidebarHotkey $vertical
                Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
                (Test-Until -TimeoutSec 5 -Condition {
                    [Windows.Automation.Automation]::Compare(
                        $fallbackFocus, [Windows.Automation.AutomationElement]::FocusedElement)
                }) |
                    Should -BeTrue -Because 'expanding the sidebar must not steal input focus'
            }

            $boundSession = $agent.AcpSessionId
            Invoke-WtCli -App $vertical -Arguments @('focus-pane', '-t', $agent.PaneSessionId) | Out-Null
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue
            Send-WtWindowKey -App $vertical -Vk 0xBE -Ctrl -Shift -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { -not (Test-AgentPaneOpen -App $vertical) }) | Should -BeTrue
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $focused -and $focused.Current.ProcessId -eq $vertical.Pid -and $focused.Current.HasKeyboardFocus -and
                    $focused.Current.Name -ne 'Agent Pane' -and $focused.Current.ClassName -eq 'TermControl'
            }) | Should -BeTrue -Because 'a hidden agent origin must fall back to a visible terminal without reopening the agent'
            Test-AgentPaneOpen -App $vertical | Should -BeFalse
            (Get-AgentPaneSession -App $vertical -PaneSessionId $agent.PaneSessionId).AcpSessionId |
                Should -BeExactly $boundSession -Because 'view toggling must not replace or terminate the agent session'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'focus-hidden-agent-fallback.png') | Out-Null
            Open-AgentPane -App $vertical | Out-Null
            & $verifyPreservation

            Set-WtPaneFocus -App $vertical -SessionId $split.session_id
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue
            Close-WtPane -App $vertical -SessionId $split.session_id
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $focused -and $focused.Current.ProcessId -eq $vertical.Pid -and $focused.Current.HasKeyboardFocus -and
                    $focused.Current.Name -ne 'Agent Pane' -and $focused.Current.ClassName -eq 'TermControl'
            }) | Should -BeTrue -Because 'a closed terminal origin must fall back without recreating the pane or blocking input'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'focus-closed-terminal-fallback.png') | Out-Null
            (Get-WtCapture -App $vertical -SessionId $terminal.session_id -MaxLines 30).TrimEnd() |
                Should -MatchExactly ([regex]::Escape($terminalDraft) + '$')
            (Get-AgentPaneText -App $vertical -PaneSessionId $agent.PaneSessionId) |
                Should -MatchExactly ([regex]::Escape($agentDraft))
            (Get-Content -LiteralPath $script:fixtureLog -Raw) |
                Should -Not -Match ('\|cancel\|' + [regex]::Escape($agent.AcpSessionId))
            [IO.File]::WriteAllText($script:releasePromptPath, 'release')
            Wait-Until -TimeoutSec 10 -Because 'the same pending agent work to complete after all view transitions' -Condition {
                (Get-AgentPaneText -App $vertical -PaneSessionId $agent.PaneSessionId).Contains("ACK_$turn")
            } | Out-Null
            (Get-AgentPaneText -App $vertical -PaneSessionId $agent.PaneSessionId) |
                Should -MatchExactly ([regex]::Escape($agentDraft))
            (Get-AgentPaneSession -App $vertical -PaneSessionId $agent.PaneSessionId).AcpSessionId |
                Should -BeExactly $boundSession
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'drafts-and-agent-work-preserved.png') | Out-Null
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
        $horizontal = $null
        try {
            $horizontal = & $script:StartLayoutApp 'horizontal'
            if (-not (Test-WtWindowKeyFocusable -App $horizontal)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys'
                return
            }
            Wait-UiElement -App $horizontal -Selector 'NewTabButton' | Out-Null
            $pane = Get-ActivePane -App $horizontal
            Set-WtPaneFocus -App $horizontal -SessionId ([string]$pane.session_id)
            $draftMarker = "HOTKEY_NOOP_$([guid]::NewGuid().ToString('N'))"
            $draftCommand = "echo $draftMarker"
            Send-WtInput -App $horizontal -SessionId ([string]$pane.session_id) -Text $draftCommand | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'the focused shell to echo the unsent draft before testing the accelerator' -Condition {
                (Get-WtCapture -App $horizontal -SessionId ([string]$pane.session_id) -MaxLines 30).TrimEnd().EndsWith($draftCommand)
            } | Out-Null
            $before = Get-WtCapture -App $horizontal -SessionId ([string]$pane.session_id) -MaxLines 30
            $before | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'sidebar-horizontal-before.txt') -Encoding utf8
            Save-UiScreenshot -App $horizontal -Path (Join-Path $script:evidenceDir 'sidebar-horizontal-before.png') | Out-Null
            & $script:ToggleSidebarHotkey $horizontal
            Start-Sleep -Milliseconds 500
            $after = Get-WtCapture -App $horizontal -SessionId ([string]$pane.session_id) -MaxLines 30
            $after | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'sidebar-horizontal-after.txt') -Encoding utf8
            Save-UiScreenshot -App $horizontal -Path (Join-Path $script:evidenceDir 'sidebar-horizontal-after.png') | Out-Null
            $after | Should -BeExactly $before -Because 'the sidebar-only accelerator must be consumed without typing into the active horizontal pane'
            Send-WtWindowKey -App $horizontal -Vk 0x0D -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 8 -IntervalSec 0.25 -Condition {
                (Get-WtCapture -App $horizontal -SessionId ([string]$pane.session_id) -MaxLines 30) -match
                    "(?m)^\s*$([regex]::Escape($draftMarker))\s*$"
            }) | Should -BeTrue -Because 'input and output must remain live after the sidebar hotkey, not frozen by a leaked Ctrl+S'
            Get-WtCapture -App $horizontal -SessionId ([string]$pane.session_id) -MaxLines 30 |
                Set-Content -LiteralPath (Join-Path $script:evidenceDir 'sidebar-horizontal-liveness.txt') -Encoding utf8
            Save-UiScreenshot -App $horizontal -Path (Join-Path $script:evidenceDir 'sidebar-horizontal-liveness.png') | Out-Null
            $newTab = Get-UiElement -App $horizontal -Selector 'NewTabButton'
            ($newTab -and -not $newTab.isOffscreen -and $newTab.width -gt 0 -and $newTab.height -gt 0) |
                Should -BeTrue -Because 'Ctrl+Shift+S must not change classic horizontal tabs into a sidebar or hide their chrome'
            Test-UiElementExists -App $horizontal -Selector 'SearchTabsButton' -TimeoutSec 1 |
                Should -BeFalse -Because 'the sidebar-only action must be a safe no-op in horizontal layout'
        }
        finally {
            if ($horizontal) { Stop-Terminal -App $horizontal }
        }

        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'horizontal'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys'
                return
            }
            Set-WtSetting -App $vertical -Key tabLayout -Value 'vertical' | Out-Null
            Wait-UiElement -App $vertical -Selector 'SearchTabsButton' | Out-Null
            $sidebarReady = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition {
                $button = Get-UiElement -App $vertical -Selector 'Collapse sidebar'
                $button -and -not $button.isOffscreen -and $button.width -gt 0 -and $button.height -gt 0
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'sidebar-expanded-before.png') | Out-Null
            $sidebarReady | Should -BeTrue -Because 'switching to vertical layout must initialize the sidebar toggle action label before the first hotkey'

            & $script:ToggleSidebarHotkey $vertical
            $sidebarCollapsed = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition {
                $button = Get-UiElement -App $vertical -Selector 'Expand sidebar'
                $button -and -not $button.isOffscreen -and $button.width -gt 0 -and $button.height -gt 0
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'sidebar-after-first-hotkey.png') | Out-Null
            $sidebarCollapsed | Should -BeTrue -Because 'Ctrl+Shift+S must collapse the expanded vertical sidebar'

            & $script:ToggleSidebarHotkey $vertical
            $sidebarExpanded = Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition {
                $button = Get-UiElement -App $vertical -Selector 'Collapse sidebar'
                $button -and -not $button.isOffscreen -and $button.width -gt 0 -and $button.height -gt 0
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'sidebar-expanded-after.png') | Out-Null
            $sidebarExpanded | Should -BeTrue -Because 'a second Ctrl+Shift+S must show the collapsed vertical sidebar'
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
    }

    It 'Sidebar rail hover hints show the shortcut' -Tag 'SidebarHint' {
        $vertical = $null
        $originalCursor = $null
        try {
            $tooltipCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ToolTip)
            $states = @(
                @{ Name = 'collapse'; Label = 'Collapse sidebar'; Chord = 'Ctrl+Shift+S' }
                @{ Name = 'expand'; Label = 'Expand sidebar'; Chord = 'Ctrl+Shift+S'; Collapsed = $true }
                @{ Name = 'additional'; Label = 'Collapse sidebar'; Chord = 'Ctrl+Shift+Y'; Reload = @{
                    actions = @(@{ command = 'toggleSidebar'; keys = 'ctrl+shift+y' })
                } }
                @{ Name = 'rebound'; Label = 'Collapse sidebar'; Chord = 'Ctrl+Shift+Y'; Reload = @{
                    actions = @(
                        @{ command = 'unbound'; keys = 'ctrl+shift+s' }
                        @{ command = 'toggleSidebar'; keys = 'ctrl+shift+y' }
                    )
                } }
                @{ Name = 'unbound'; Label = 'Collapse sidebar'; Chord = ''; Reload = @{
                    actions = @(@{ command = 'unbound'; keys = 'ctrl+shift+s' })
                } }
                @{ Name = 'reassigned'; Label = 'Collapse sidebar'; Chord = ''; Reload = @{
                    actions = @(@{ command = 'copy'; keys = 'ctrl+shift+s' })
                } }
                @{ Name = 'overridden'; Label = 'Collapse sidebar'; Chord = ''; Reload = @{
                    actions = @(@{ command = 'copy'; id = 'Terminal.ToggleSidebar' })
                } }
            )
            $observed = @{}

            foreach ($state in $states) {
                $vertical = & $script:StartLayoutApp 'vertical'
                if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                    Set-ItResult -Skipped -Because 'WT window cannot take foreground for physical hover'
                    return
                }
                if (-not $originalCursor) {
                    $originalCursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
                }
                $processCondition = [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$vertical.Pid)
                if ($state.Collapsed) {
                    & $script:ToggleSidebarHotkey $vertical
                }
                if ($state.Reload) { Set-WtSettings -App $vertical -Settings $state.Reload | Out-Null }
                Wait-UiElement -App $vertical -Selector $state.Label | Out-Null
                if ($state.Name -eq 'collapse') {
                    $referenceHover = & (Get-Module ItE2E) {
                        param($App)
                        Invoke-WinAppUi -App $App -UiArgs @('hover', 'AgentToggleButton', '--dwell-time', '1200', '--json')
                    } $vertical
                    if ($referenceHover.ExitCode -ne 0) { throw "Agent Pane reference hover failed: $($referenceHover.StdErr)" }
                    Save-UiScreenshot -App $vertical -CaptureScreen -Path (Join-Path $script:evidenceDir 'agent-pane-hint-reference.png') | Out-Null
                }
                $button = Get-UiElement -App $vertical -Selector $state.Label
                ($button -and -not $button.isOffscreen -and $button.width -gt 0 -and $button.height -gt 0) |
                    Should -BeTrue -Because 'the hovered rail toggle must be visible'
                Test-WtWindowKeyFocusable -App $vertical | Should -BeTrue -Because 'physical hover needs the owned window in the foreground'
                $hover = & (Get-Module ItE2E) {
                    param($App, $Label)
                    Invoke-WinAppUi -App $App -UiArgs @('hover', $Label, '--dwell-time', '1200', '--json')
                } $vertical $state.Label
                if ($hover.ExitCode -ne 0) {
                    throw "Native hover failed: $($hover.StdErr)"
                }
                $screenshot = Join-Path $script:evidenceDir "sidebar-hint-$($state.Name).png"
                Save-UiScreenshot -App $vertical -CaptureScreen -Path $screenshot | Out-Null
                $hover.StdOut | Set-Content -LiteralPath "$screenshot.hover.json" -Encoding utf8
                $tooltip = Wait-Until -TimeoutSec 8 -Because "the $($state.Name) tooltip to reflect the effective sidebar binding" -Condition {
                    $windows = [Windows.Automation.AutomationElement]::RootElement.FindAll(
                        [Windows.Automation.TreeScope]::Children, $processCondition)
                    foreach ($window in $windows) {
                        $tips = $window.FindAll([Windows.Automation.TreeScope]::Subtree, $tooltipCondition)
                        foreach ($tip in $tips) {
                            if ($tip.Current.IsOffscreen -or $tip.Current.BoundingRectangle.Width -le 0) { continue }
                            $names = @($tip.Current.Name)
                            $children = $tip.FindAll(
                                [Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)
                            $names += @($children | ForEach-Object { $_.Current.Name })
                            $text = ($names | Where-Object { $_ } | Select-Object -Unique) -join "`n"
                            if ($text.Contains($state.Label)) {
                                $observed[$state.Name] = $text
                                if ($state.Chord) {
                                    $titles = @($children | Where-Object {
                                        $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                                        [string]::Equals($_.Current.Name, $state.Label, [StringComparison]::Ordinal)
                                    })
                                    $shortcuts = @($children | Where-Object {
                                        $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text -and
                                        [string]::Equals($_.Current.Name, $state.Chord, [StringComparison]::Ordinal)
                                    })
                                    if ($titles.Count -eq 1 -and $shortcuts.Count -eq 1 -and
                                        $shortcuts[0].Current.BoundingRectangle.Left -gt $titles[0].Current.BoundingRectangle.Right -and
                                        [Math]::Abs($shortcuts[0].Current.BoundingRectangle.Top - $titles[0].Current.BoundingRectangle.Top) -lt 1) {
                                        return $text
                                    }
                                }
                                elseif ($text.Trim() -eq $state.Label) { return $text }
                            }
                        }
                    }
                    $null
                }
                Test-Path -LiteralPath $screenshot | Should -BeTrue -Because 'each visible tooltip needs an actual screenshot'
                $observed[$state.Name] = $tooltip
                $windowRoot = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$vertical.Hwnd))
                $windowBounds = $windowRoot.Current.BoundingRectangle
                if (-not [ItE2E.ItWtWin32Input]::SetCursorPos(
                    [int]($windowBounds.Left + $windowBounds.Width / 2), [int]($windowBounds.Top + $windowBounds.Height / 2))) {
                    throw 'Could not move the pointer off the toggle before changing its state'
                }
                Start-Sleep -Milliseconds 250
                Stop-Terminal -App $vertical
                $vertical = $null
            }
            $observed | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'sidebar-hints.json') -Encoding utf8
            foreach ($state in $states) {
                if ($state.Chord) {
                    $observed[$state.Name] | Should -MatchExactly ([regex]::Escape($state.Chord))
                }
                else {
                    $observed[$state.Name].Trim() | Should -BeExactly $state.Label
                }
            }
        }
        finally {
            try {
                if ($originalCursor -and -not [ItE2E.ItWtWin32Input]::SetCursorPos($originalCursor[0], $originalCursor[1])) {
                    throw 'Could not restore the original physical pointer position'
                }
            }
            finally {
                if ($vertical) { Stop-Terminal -App $vertical }
            }
        }
    }
}