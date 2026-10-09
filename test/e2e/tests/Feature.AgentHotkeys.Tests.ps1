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
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{ acpAgent = 'copilot'; delegateAgent = 'copilot' }
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
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
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
        $script:HistoryVisible = {
            param($App)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$App.Hwnd))
            if ($window.Current.ProcessId -ne $App.Pid) { throw 'History window changed ownership.' }
            $history = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton'))
            $history -and -not $history.Current.IsOffscreen -and
                $history.Current.BoundingRectangle.Width -gt 0 -and $history.Current.BoundingRectangle.Height -gt 0
        }
        $script:AgentsViewShown = {
            param($App)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$App.Hwnd))
            if ($window.Current.ProcessId -ne $App.Pid) { throw 'Agents view window changed ownership.' }
            $history = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton'))
            $header = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'VerticalTabsHeader'))
            $history -and -not $history.Current.IsOffscreen -and
                $history.Current.BoundingRectangle.Width -gt 0 -and $history.Current.BoundingRectangle.Height -gt 0 -and
                $header -and -not $header.Current.IsOffscreen -and $header.Current.Name -eq 'Tabs'
        }
        $script:AssertSearchOff = {
            param($App)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$App.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTabsButton')
            $button = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $button | Should -Not -BeNullOrEmpty
            $toggle = $button.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
            $toggle.Current.ToggleState | Should -Be ([Windows.Automation.ToggleState]::Off)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTextBox')
            $search = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            [bool]($search -and -not $search.Current.IsOffscreen -and
                $search.Current.BoundingRectangle.Height -gt 0) | Should -BeFalse
        }
        $script:OpenSharedSearch = {
            param($App)
            $search = Get-UiElement -App $App -Selector SearchTextBox
            if (-not $search -or $search.isOffscreen -or $search.height -le 0) {
                Invoke-UiClick -App $App -Selector SearchTabsButton | Out-Null
            }
        }
        $script:HistorySearchFocused = {
            param($App)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$App.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTextBox')
            $search = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $historyCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton')
            $history = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $historyCondition)
            $history -and -not $history.Current.IsOffscreen -and
                $search -and -not $search.Current.IsOffscreen -and $search.Current.HasKeyboardFocus
        }
        $script:TabSearchFocused = {
            param($App)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$App.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTextBox')
            $search = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $historyCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryHeaderButton')
            $history = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $historyCondition)
            $search -and -not $search.Current.IsOffscreen -and $search.Current.HasKeyboardFocus
        }
        $script:StartLayoutApp = {
            param([ValidateSet('horizontal', 'vertical')][string]$Layout)
            $target = Resolve-ItApp -Package (Get-ItTestPackage)
            if (@(Get-WtProcessesForApp -App $target -IncludePackageExecutables).Count) {
                throw 'Refusing to replace an existing selected-package window for layout hotkey tests.'
            }
            Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
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

    It 'Agent history hotkey toggles the layout-appropriate history surface' -Tag 'SidebarHistory' {
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
            $terminal = Wait-Until -TimeoutSec 8 -Because 'the vertical window has its actual shell and header before the physical accelerator' -Condition {
                $pane = Get-ActivePane -App $vertical
                if ($pane -and $pane.session_id -and $pane.window_id -eq $vertical.WindowId -and
                    (Get-WtSetting -App $vertical -Key tabLayout) -eq 'vertical') { $pane }
            }
            Set-WtPaneFocus -App $vertical -SessionId $terminal.session_id
            Wait-UiElement -App $vertical -Selector VerticalTabsHeader | Out-Null
            & $script:AssertSearchOff $vertical
            (& $script:HistoryVisible $vertical) |
                Should -BeFalse -Because 'Sidebar History must start closed'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-vertical-before.png') | Out-Null
            & $script:OpenAgentHistoryHotkey $vertical
            $historyOpened = Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                & $script:AgentsViewShown $vertical
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-vertical-after.png') | Out-Null
            $historyOpened | Should -BeTrue -Because 'Ctrl+Shift+/ must open Sidebar History, not the agent-pane sessions view, in vertical layout'
            Test-AgentPaneOpen -App $vertical | Should -BeFalse -Because 'opening Sidebar History must not also open the WTA agent pane'
            & $script:AssertSearchOff $vertical

            & $script:OpenAgentHistoryHotkey $vertical
            $historyHidden = Test-Until -TimeoutSec 8 -Condition {
                -not (& $script:HistoryVisible $vertical)
            }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-vertical-hidden.png') | Out-Null
            $historyHidden | Should -BeTrue -Because 'a second Ctrl+Shift+/ must hide Sidebar History without activating search'
            & $script:AssertSearchOff $vertical
            $tabs = Get-UiElement -App $vertical -Selector 'SearchTabsButton'
            ($tabs -and -not $tabs.isOffscreen -and $tabs.width -gt 0) |
                Should -BeTrue -Because 'closing history must restore the normal expanded sidebar'
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:AgentsViewShown $vertical }) |
                Should -BeTrue -Because 'history must reopen without implicitly activating search'
            & $script:AssertSearchOff $vertical
            & $script:OpenSharedSearch $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue

            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
            (& $script:HistoryVisible $vertical) |
                Should -BeFalse -Because 'collapsing the sidebar must close its history view'

            & $script:OpenAgentHistoryHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
            & $script:OpenAgentHistoryHotkey $vertical
            $reopened = Test-Until -TimeoutSec 8 -Condition { & $script:AgentsViewShown $vertical }
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-from-collapsed-sidebar.png') | Out-Null
            $reopened | Should -BeTrue -Because 'the history accelerator must expand a collapsed sidebar without activating search'
            & $script:AssertSearchOff $vertical

            $historyTerminal = Get-ActivePane -App $vertical
            Set-WtPaneFocus -App $vertical -SessionId ([string]$historyTerminal.session_id)
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'from focus outside History, the sidebar hotkey must enter tab search instead of collapsing the rail'
            (& $script:HistoryVisible $vertical) |
                Should -BeTrue -Because 'entering shared search must preserve the selected Recent scope'
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null

            & $script:OpenAgentHistoryHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:AgentsViewShown $vertical }) | Should -BeTrue
            & $script:AssertSearchOff $vertical
            & $script:OpenAgentHistoryHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
            Invoke-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
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
                Set-TestSidebarScope -App $vertical -Recent $false
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
                    foreach ($closeWithButton in @($false, $true)) {
                        if ($initiallyCollapsed) {
                            Invoke-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
                            Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                        }
                        Set-WtWindowForeground -App $vertical | Should -BeTrue
                        $originFocus.SetFocus()
                        Wait-Until -TimeoutSec 5 -Because 'the exact intended input owns focus after applying sidebar state' -Condition {
                            $focused = [Windows.Automation.AutomationElement]::FocusedElement
                            $focused -and $focused.Current.HasKeyboardFocus -and
                                [Windows.Automation.Automation]::Compare($originFocus, $focused)
                        } | Out-Null
                        $entryFocused = [Windows.Automation.AutomationElement]::FocusedElement
                        $entryPhase = "entry-$($origin.Name)-collapsed-$initiallyCollapsed-button-$closeWithButton"
                        @{
                            at = [DateTimeOffset]::UtcNow.ToString('o'); phase = $entryPhase
                            requested_source_pane_id = $origin.Id
                            active_pane = Get-ActivePane -App $vertical
                            actual_focus = if ($entryFocused) { @{
                                automation_id = $entryFocused.Current.AutomationId; class = $entryFocused.Current.ClassName
                                name = $entryFocused.Current.Name; process_id = $entryFocused.Current.ProcessId
                            } }
                            matches_original_source = [bool]$entryFocused -and
                                [Windows.Automation.Automation]::Compare($originFocus, $entryFocused)
                            shell_ids = @($terminal.session_id, $split.session_id)
                            helper_pane_id = $agent.PaneSessionId; helper_pid = $agent.HelperProcessId
                            fixture_acp_session_id = $agent.AcpSessionId
                        } | ConvertTo-Json -Depth 8 |
                            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$entryPhase.json")
                        [Windows.Automation.Automation]::Compare($originFocus, $entryFocused) |
                            Should -BeTrue -Because 'the entry source must be the exact requested Agent or shell control'
                        & $script:OpenAgentHistoryHotkey $vertical
                        (Test-Until -TimeoutSec 5 -Condition { & $script:AgentsViewShown $vertical }) | Should -BeTrue
                        & $script:AssertSearchOff $vertical
                        (Test-Until -TimeoutSec 5 -Condition $focusRestored) |
                            Should -BeTrue -Because 'passive Agents navigation must leave the exact source input focused'
                        if ($closeWithButton) {
                            Invoke-UiElement -App $vertical -Selector FilterTabsButton | Out-Null
                            Invoke-UiElement -App $vertical -Selector RecentAgentSessionsFilterMenuItem | Out-Null
                        }
                        else { & $script:OpenAgentHistoryHotkey $vertical }
                        $expectedLabel = 'Collapse sidebar'
                        Wait-UiElement -App $vertical -Selector $expectedLabel | Out-Null
                        $restored = Test-Until -TimeoutSec 5 -Condition {
                            if ($closeWithButton) {
                                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                                $focused -and $focused.Current.ProcessId -eq $vertical.Pid -and
                                    $focused.Current.AutomationId -eq 'FilterTabsButton'
                            } else { & $focusRestored }
                        }
                        $focused = [Windows.Automation.AutomationElement]::FocusedElement
                        $phase = "restore-$($origin.Name)-collapsed-$initiallyCollapsed-button-$closeWithButton"
                        @{
                            at = [DateTimeOffset]::UtcNow.ToString('o'); phase = $phase; restored = $restored
                            source_pane_id = $origin.Id; active_pane = Get-ActivePane -App $vertical
                            helpers = @(Get-AgentPaneSessions -App $vertical)
                            expected_focus = @{
                                automation_id = $originFocus.Current.AutomationId; class = $originFocus.Current.ClassName
                                name = $originFocus.Current.Name; process_id = $originFocus.Current.ProcessId
                            }
                            actual_focus = if ($focused) { @{
                                automation_id = $focused.Current.AutomationId; class = $focused.Current.ClassName
                                name = $focused.Current.Name; process_id = $focused.Current.ProcessId
                            } }
                            search_focus = & $script:TabSearchFocused $vertical
                            agents_search_focus = & $script:HistorySearchFocused $vertical
                            sidebar_state = $expectedLabel
                            shell_ids = @($terminal.session_id, $split.session_id)
                            fixture_acp_session_id = $agent.AcpSessionId; fixture_marker = $turn
                        } | ConvertTo-Json -Depth 8 |
                            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$phase.json")
                        if (-not $restored) {
                            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir "$phase.png") | Out-Null
                            Get-UiTree -App $vertical -Depth 4 |
                                Set-Content -LiteralPath (Join-Path $script:evidenceDir "$phase.tree.txt")
                        }
                        Get-ItLogText -App $vertical -Name 'terminal-agent-pane.log' -SinceStart |
                            Select-String -Pattern 'sidebar.*history|history.*source|history.*restor|sourcepane|source pane' |
                            ForEach-Object Line |
                            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$phase.binding.log")
                        $restored | Should -BeTrue -Because 'the hotkey retains source input focus; standard menu dismissal returns to Filter without recollapsing the rail'
                        & $verifyPreservation
                    }
                    & $verifyPreservation
                }
                if ($origin.Agent) {
                    Invoke-WtCli -App $vertical -Arguments @('focus-pane', '-t', $origin.Id) | Out-Null
                }
                else {
                    Set-WtPaneFocus -App $vertical -SessionId $origin.Id
                }
                (Test-Until -TimeoutSec 5 -Condition $focusRestored) |
                    Should -BeTrue -Because 'the source input must own focus before invoking the History hotkey'
                & $script:OpenAgentHistoryHotkey $vertical
                (Test-Until -TimeoutSec 5 -Condition { & $script:AgentsViewShown $vertical }) | Should -BeTrue
                & $script:AssertSearchOff $vertical
                & $script:OpenSharedSearch $vertical
                (Test-Until -TimeoutSec 5 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue
                & $script:ToggleSidebarHotkey $vertical
                Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                (Test-Until -TimeoutSec 5 -Condition $focusRestored) |
                    Should -BeTrue -Because "collapsing from history should return to the still-visible $($origin.Name) input without reading History's saved source"
                Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir "focus-$($origin.Name)-restored.png") | Out-Null
                & $verifyPreservation
                Invoke-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
                & $verifyPreservation
            }

            foreach ($origin in $origins) {
                Set-WtWindowForeground -App $vertical | Should -BeTrue
                if ($origin.Agent) {
                    Invoke-WtCli -App $vertical -Arguments @('focus-pane', '-t', $origin.Id) | Out-Null
                }
                else {
                    Set-WtPaneFocus -App $vertical -SessionId $origin.Id
                }
                $sourceFocus = [Windows.Automation.AutomationElement]::FocusedElement
                & $script:ToggleSidebarHotkey $vertical
                Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
                (Test-Until -TimeoutSec 5 -Condition { & $script:TabSearchFocused $vertical }) |
                    Should -BeTrue -Because "the $($origin.Name) input must enter Search tabs without collapsing the rail"
                & $script:ToggleSidebarHotkey $vertical
                Wait-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                (Test-Until -TimeoutSec 5 -Condition {
                    [Windows.Automation.Automation]::Compare(
                        $sourceFocus, [Windows.Automation.AutomationElement]::FocusedElement)
                }) | Should -BeTrue -Because "the hotkey must return to the exact $($origin.Name) input when it is still available"
                & $verifyPreservation
                Invoke-UiElement -App $vertical -Selector 'Expand sidebar' | Out-Null
                Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            }

            $boundSession = $agent.AcpSessionId
            Invoke-WtCli -App $vertical -Arguments @('focus-pane', '-t', $agent.PaneSessionId) | Out-Null
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 5 -Condition { & $script:AgentsViewShown $vertical }) | Should -BeTrue
            & $script:AssertSearchOff $vertical
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
            (Test-Until -TimeoutSec 5 -Condition { & $script:AgentsViewShown $vertical }) | Should -BeTrue
            & $script:AssertSearchOff $vertical
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
    }

    It 'Agent history hotkey toggles the layout-appropriate history surface and restores tab search focus' -Tag 'SidebarHistoryTabSearch' {
        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for physical history input'
                return
            }
            Wait-UiElement -App $vertical -Selector SearchTabsButton | Out-Null
            $pane = Get-ActivePane -App $vertical
            Set-WtPaneFocus -App $vertical -SessionId $pane.session_id
            $draft = "HISTORY_TAB_SEARCH_DRAFT_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            Send-WtInput -App $vertical -SessionId $pane.session_id -Text $draft | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'the unsent shell draft before history' -Condition {
                (Get-WtCapture -App $vertical -SessionId $pane.session_id -MaxLines 30).TrimEnd().EndsWith($draft)
            } | Out-Null

            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'the Sidebar shortcut must first focus ordinary tab search'
            $query = "history-focus-$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            Set-UiValue -App $vertical -Selector SearchTextBox -Value $query | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { & $script:TabSearchFocused $vertical }) | Should -BeTrue
            $searchFocus = [Windows.Automation.AutomationElement]::FocusedElement
            $searchFocus.Current.AutomationId | Should -Be 'SearchTextBox'

            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:HistorySearchFocused $vertical }) |
                Should -BeTrue -Because 'History must take focus from tab search'
            (Get-UiValue -App $vertical -Selector SearchTextBox) | Should -Be $query
            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'closing History by hotkey must return focus to the prior tab search'
            [Windows.Automation.Automation]::Compare(
                $searchFocus, [Windows.Automation.AutomationElement]::FocusedElement) |
                Should -BeTrue -Because 'the exact explicitly opened search control must regain focus'
            (Get-UiValue -App $vertical -Selector SearchTextBox) | Should -Be $query
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            (Get-WtCapture -App $vertical -SessionId $pane.session_id -MaxLines 30).TrimEnd() |
                Should -Match ([regex]::Escape($draft) + '$')
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'history-returns-to-tab-search.png') | Out-Null

            & $script:OpenAgentHistoryHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:HistorySearchFocused $vertical }) | Should -BeTrue
            Invoke-UiElement -App $vertical -Selector FilterTabsButton | Out-Null
            Invoke-UiElement -App $vertical -Selector RecentAgentSessionsFilterMenuItem | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $focused -and $focused.Current.ProcessId -eq $vertical.Pid -and
                    $focused.Current.AutomationId -eq 'FilterTabsButton'
            }) | Should -BeTrue -Because 'standard filter-menu dismissal returns focus to Filter'
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$vertical.Hwnd)
            $currentSearch = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTextBox'))
            [Windows.Automation.Automation]::Compare($searchFocus, $currentSearch) |
                Should -BeTrue -Because 'scope changes preserve the exact shared search control'
            (Get-UiValue -App $vertical -Selector SearchTextBox) | Should -Be $query
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
    }

    It 'Sidebar hotkey opens search and returns to input' -Tag 'SidebarHotkey' {
        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for window-level keys'
                return
            }
            Wait-UiElement -App $vertical -Selector 'SearchTabsButton' | Out-Null
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'sidebar-expanded-before.png') | Out-Null

            $pane = Get-ActivePane -App $vertical
            Set-WtPaneFocus -App $vertical -SessionId ([string]$pane.session_id)
            $shellFocus = [Windows.Automation.AutomationElement]::FocusedElement
            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'the hotkey must enter and focus Search tabs from an expanded rail'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'sidebar-after-first-hotkey.png') | Out-Null
            Set-WtPaneFocus -App $vertical -SessionId ([string]$pane.session_id)
            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'the hotkey must reenter Search tabs without collapsing an expanded rail'

            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'CompactNewTabButton' | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $shellFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'the hotkey must return to the shell input used before entering the sidebar'
            Test-UiElementExists -App $vertical -Selector SearchTextBox -TimeoutSec 1 |
                Should -BeFalse -Because 'collapsing the rail must close tab search'
            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'entering a collapsed sidebar must expand and focus tab search'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'sidebar-expanded-after.png') | Out-Null
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'CompactNewTabButton' | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $shellFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'repeated sidebar hotkeys must return focus to the same shell'

            Invoke-UiClick -App $vertical -Selector SearchTabsButton | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'the mouse can open tab search without creating a hotkey return source'
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$vertical.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTabsButton')
            $searchButton = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $searchButton | Should -Not -BeNullOrEmpty
            $searchButton.SetFocus()
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector 'CompactNewTabButton' | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $shellFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'exiting from a still-visible sidebar button must fall back to a terminal even without a saved source'
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
    }

    It 'Sidebar hotkey opens search and returns to input after pointer search replaces hotkey session' -Tag 'SidebarStaleSource' {
        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for physical keyboard input'
                return
            }
            Wait-UiElement -App $vertical -Selector SearchTabsButton | Out-Null
            $source = Get-ActivePane -App $vertical
            $other = Split-WtPane -App $vertical -SessionId $source.session_id -Direction right -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'

            Set-WtPaneFocus -App $vertical -SessionId $source.session_id
            (Test-Until -TimeoutSec 5 -Condition {
                [string](Get-ActivePane -App $vertical).session_id -eq [string]$source.session_id
            }) | Should -BeTrue -Because 'split A must be the active pane saved by the first hotkey'
            $sourceFocus = [Windows.Automation.AutomationElement]::FocusedElement
            $sourceFocus.Current.ClassName | Should -Be 'TermControl'
            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'the first hotkey must capture split A as its return source'

            Send-WtWindowKey -App $vertical -Vk 0x1B -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition {
                $search = Get-UiElement -App $vertical -Selector SearchTextBox
                -not ($search -and -not $search.isOffscreen -and $search.width -gt 0)
            }) | Should -BeTrue -Because 'Escape ends the hotkey-opened search session'
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null

            Set-WtPaneFocus -App $vertical -SessionId $other.session_id
            (Test-Until -TimeoutSec 5 -Condition {
                [string](Get-ActivePane -App $vertical).session_id -eq [string]$other.session_id
            }) | Should -BeTrue -Because 'split B must be active before the pointer opens a new search'
            $otherFocus = Wait-Until -TimeoutSec 5 -Because 'split B to take keyboard focus' -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                if ($focused -and $focused.Current.ProcessId -eq $vertical.Pid -and
                    $focused.Current.ClassName -eq 'TermControl' -and
                    -not [Windows.Automation.Automation]::Compare($sourceFocus, $focused)) { $focused }
            }
            $otherFocus | Should -Not -BeNullOrEmpty
            Invoke-UiClick -App $vertical -Selector SearchTabsButton | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'pointer search must start a new session without split A as its hotkey source'
            $sourceFocus.Current.IsOffscreen | Should -BeFalse -Because 'the stale split A target remains visible and focusable'
            [string](Get-ActivePane -App $vertical).session_id |
                Should -BeExactly ([string]$other.session_id) -Because 'split B remains the active fallback'

            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector CompactNewTabButton | Out-Null
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'pointer-search-return.png') | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $otherFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'pointer-opened search must return to active split B, not the stale hotkey source in split A'

            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'a later hotkey must capture a fresh source from split B'
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector CompactNewTabButton | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $otherFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'the next hotkey-only session must still return to its fresh split B source'
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
    }

    It 'Sidebar hotkey opens search and returns to input after pointer replaces an agent source' -Tag 'SidebarStaleSource' {
        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for physical keyboard input'
                return
            }
            Wait-UiElement -App $vertical -Selector SearchTabsButton | Out-Null
            $shell = Get-ActivePane -App $vertical
            Open-AgentPane -App $vertical | Out-Null
            Wait-AgentReady -App $vertical -TimeoutSec 30 | Out-Null
            $agent = Get-AgentPaneSession -App $vertical
            $agent.AcpSessionId | Should -Match '^chat-fixture-\d+-\d+$' -Because 'this regression uses a deterministic provider without real inference'
            Invoke-WtCli -App $vertical -Arguments @('focus-pane', '-t', $agent.PaneSessionId) | Out-Null
            $agentFocus = Wait-Until -TimeoutSec 5 -Because 'the visible Agent chat input to take focus' -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                if ($focused -and $focused.Current.ProcessId -eq $vertical.Pid -and
                    $focused.Current.ClassName -eq 'TermControl' -and $focused.Current.Name -eq 'Agent Pane') { $focused }
            }
            $agentFocus | Should -Not -BeNullOrEmpty
            & $script:ToggleSidebarHotkey $vertical
            (Test-Until -TimeoutSec 6 -Condition { & $script:TabSearchFocused $vertical }) |
                Should -BeTrue -Because 'the first hotkey enters search from the Agent input'
            Send-WtWindowKey -App $vertical -Vk 0x1B -RequireForeground | Out-Null
            (Test-Until -TimeoutSec 5 -Condition {
                $search = Get-UiElement -App $vertical -Selector SearchTextBox
                -not ($search -and -not $search.isOffscreen -and $search.width -gt 0)
            }) | Should -BeTrue
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null

            Set-WtPaneFocus -App $vertical -SessionId $shell.session_id
            $shellFocus = Wait-Until -TimeoutSec 5 -Because 'the active shell to receive focus' -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                if ($focused -and $focused.Current.ProcessId -eq $vertical.Pid -and
                    $focused.Current.ClassName -eq 'TermControl' -and
                    -not [Windows.Automation.Automation]::Compare($agentFocus, $focused)) { $focused }
            }
            $shellFocus | Should -Not -BeNullOrEmpty
            Invoke-UiClick -App $vertical -Selector SearchTabsButton | Out-Null
            (Test-Until -TimeoutSec 5 -Condition { & $script:TabSearchFocused $vertical }) | Should -BeTrue
            Test-AgentPaneOpen -App $vertical | Should -BeTrue -Because 'the stale Agent source remains visible and focusable'
            & $script:ToggleSidebarHotkey $vertical
            Wait-UiElement -App $vertical -Selector CompactNewTabButton | Out-Null
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'pointer-search-agent-source-return.png') | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $shellFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'pointer search must return to the current shell, not a stale visible Agent input'
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
    }

    It 'Sidebar hotkey opens search and returns to input while command palette Toggle sidebar changes visibility' -Tag 'SidebarActionSource' {
        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for physical command palette input'
                return
            }
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            $pane = Get-ActivePane -App $vertical
            Set-WtPaneFocus -App $vertical -SessionId $pane.session_id
            $draft = "SIDEBAR_ACTION_DRAFT_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            Send-WtInput -App $vertical -SessionId $pane.session_id -Text $draft | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'the unsent shell draft' -Condition {
                (Get-WtCapture -App $vertical -SessionId $pane.session_id -MaxLines 30).TrimEnd().EndsWith($draft)
            } | Out-Null

            $name = 'IT E2E toggle sidebar'
            Set-WtSettings -App $vertical -Settings @{
                actions = @(@{ name = $name; command = 'toggleSidebar' })
            } | Out-Null
            $invokePalette = {
                Send-WtWindowKey -App $vertical -Vk 0x50 -Ctrl -Shift -RequireForeground | Out-Null
                (Test-Until -TimeoutSec 6 -Condition { Test-CommandPaletteOpen -App $vertical }) | Should -BeTrue
                Set-UiValue -App $vertical -Selector '_searchBox' -Value $name | Out-Null
                Wait-UiElement -App $vertical -Selector $name | Out-Null
                & winapp ui invoke $name -w ([string]$vertical.Hwnd) 2>&1 | Out-Null
                $LASTEXITCODE | Should -Be 0
            }.GetNewClosure()

            & $invokePalette
            Wait-UiElement -App $vertical -Selector CompactNewTabButton | Out-Null
            [bool](& $script:TabSearchFocused $vertical) | Should -BeFalse -Because 'the palette action must collapse, not enter Search tabs'
            (Get-WtCapture -App $vertical -SessionId $pane.session_id -MaxLines 30).TrimEnd() |
                Should -Match ([regex]::Escape($draft) + '$')
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'palette-toggle-collapsed.png') | Out-Null

            & $invokePalette
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            $search = Get-UiElement -App $vertical -Selector SearchTextBox
            [bool]($search -and -not $search.isOffscreen -and $search.width -gt 0) |
                Should -BeFalse -Because 'the palette action must expand without opening tab search'
            (Get-WtCapture -App $vertical -SessionId $pane.session_id -MaxLines 30).TrimEnd() |
                Should -Match ([regex]::Escape($draft) + '$')
        }
        finally {
            if ($vertical) { Stop-Terminal -App $vertical }
        }
    }

    It 'Sidebar hotkey opens search and returns to input when Collapse button has focus' -Tag 'SidebarTitlebarFocus' {
        $vertical = $null
        try {
            $vertical = & $script:StartLayoutApp 'vertical'
            if (-not (Test-WtWindowKeyFocusable -App $vertical)) {
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for physical titlebar input'
                return
            }
            Wait-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
            $pane = Get-ActivePane -App $vertical
            Set-WtPaneFocus -App $vertical -SessionId $pane.session_id
            $shellFocus = Wait-Until -TimeoutSec 5 -Because 'the original shell to have keyboard focus' -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                if ($focused -and $focused.Current.ProcessId -eq $vertical.Pid -and
                    $focused.Current.ClassName -eq 'TermControl') { $focused }
            }
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$vertical.Hwnd))
            $nameCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::NameProperty, 'Collapse sidebar')
            $typeCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Button)
            $button = Wait-Until -TimeoutSec 5 -Because 'the expanded rail toggle button' -Condition {
                $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.AndCondition]::new($nameCondition, $typeCondition))
            }
            $button.Current.IsOffscreen | Should -BeFalse
            $button.SetFocus()
            $button.Current.HasKeyboardFocus | Should -BeTrue
            $focusedBefore = $button.Current.HasKeyboardFocus
            & $script:ToggleSidebarHotkey $vertical
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'titlebar-after-shortcut.png') | Out-Null
            $focusedAfter = [Windows.Automation.AutomationElement]::FocusedElement
            [pscustomobject]@{
                ButtonFocusedBefore = $focusedBefore
                ButtonFocusedAfter = $button.Current.HasKeyboardFocus
                FocusedClassAfter = if ($focusedAfter) { $focusedAfter.Current.ClassName } else { '' }
                FocusedNameAfter = if ($focusedAfter) { $focusedAfter.Current.Name } else { '' }
                FocusedIdAfter = if ($focusedAfter) { $focusedAfter.Current.AutomationId } else { '' }
                ExpandedButtonVisible = Test-UiElementExists -App $vertical -Selector 'Collapse sidebar' -TimeoutSec 1
                CollapsedButtonVisible = Test-UiElementExists -App $vertical -Selector 'Expand sidebar' -TimeoutSec 1
                SearchFocused = [bool](& $script:TabSearchFocused $vertical)
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'titlebar-after-shortcut.json') -Encoding utf8
            Wait-UiElement -App $vertical -Selector CompactNewTabButton | Out-Null
            (Test-Until -TimeoutSec 6 -Condition {
                [Windows.Automation.Automation]::Compare(
                    $shellFocus, [Windows.Automation.AutomationElement]::FocusedElement)
            }) | Should -BeTrue -Because 'focus on the Collapse button must count as inside the sidebar'
            Save-UiScreenshot -App $vertical -Path (Join-Path $script:evidenceDir 'titlebar-hotkey-return.png') | Out-Null
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
                @{ Name = 'additional'; Label = 'Expand sidebar'; Chord = 'Ctrl+Shift+Y'; Collapsed = $true; Reload = @{
                    actions = @(@{ command = 'toggleSidebar'; keys = 'ctrl+shift+y' })
                } }
                @{ Name = 'rebound'; Label = 'Expand sidebar'; Chord = 'Ctrl+Shift+Y'; Collapsed = $true; Reload = @{
                    actions = @(
                        @{ command = 'unbound'; keys = 'ctrl+shift+s' }
                        @{ command = 'toggleSidebar'; keys = 'ctrl+shift+y' }
                    )
                } }
                @{ Name = 'unbound'; Label = 'Expand sidebar'; Chord = ''; Collapsed = $true; Reload = @{
                    actions = @(@{ command = 'unbound'; keys = 'ctrl+shift+s' })
                } }
                @{ Name = 'reassigned'; Label = 'Expand sidebar'; Chord = ''; Collapsed = $true; Reload = @{
                    actions = @(@{ command = 'copy'; keys = 'ctrl+shift+s' })
                } }
                @{ Name = 'overridden'; Label = 'Expand sidebar'; Chord = ''; Collapsed = $true; Reload = @{
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
                    Invoke-UiElement -App $vertical -Selector 'Collapse sidebar' | Out-Null
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