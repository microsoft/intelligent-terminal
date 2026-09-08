#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Physical keyboard -> TerminalControl/ConPTY -> WTA input/rendering -> OS clipboard.
# The deterministic ACP fixture supplies transcript and pending-turn controls, never a model.
# Exact checklist titles below distinguish draft editing from empty/history-focus pane copying.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent pane select all' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) {
            $env:ITE2E_ARTIFACT_ROOT
        }
        else {
            Join-Path $PSScriptRoot '..\artifacts'
        }
        $artifactRoot = [System.IO.Path]::GetFullPath($artifactRoot)
        $phase = if ($env:ITE2E_SELECT_ALL_EVIDENCE_PHASE -in @('red', 'green', 'publish')) {
            $env:ITE2E_SELECT_ALL_EVIDENCE_PHASE
        }
        else { 'run' }
        $script:evidenceDir = Join-Path $artifactRoot "agent-select-all\$phase-$([guid]::NewGuid().ToString('N'))"
        $script:fixtureDir = Join-Path $script:evidenceDir 'fixture'
        New-Item -ItemType Directory -Force -Path $script:fixtureDir | Out-Null
        $script:fixtureLog = Join-Path $script:evidenceDir 'fixture.log'
        $script:releasePromptPath = Join-Path $script:fixtureDir 'release-prompt'
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1') -Destination $fixture
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))' -ReleasePromptPath '$($script:releasePromptPath.Replace("'", "''"))'"
        $command = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
        $script:originalClipboard = Get-ClipboardSnapshot
        $script:clipboardSaved = $true
        $script:evidenceIndex = 0
        $script:keyboardLayoutChanged = $false

        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:chat-fixture'
            acpCustomCommand = $command
            rightClickContextMenu = $false
            'warning.confirmOnClose' = 'never'
        }
        $shell = Get-ActivePane -App $script:app
        $script:ownerTabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $shell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before select-all input'
        if (-not ('ItE2E.SelectAllKeyboardLayout' -as [type])) {
            Add-Type -Namespace ItE2E -Name SelectAllKeyboardLayout -MemberDefinition @'
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")] public static extern IntPtr GetKeyboardLayout(uint threadId);
    [DllImport("user32.dll")] public static extern int GetKeyboardLayoutList(int count, [Out] IntPtr[] layouts);
    [DllImport("user32.dll", SetLastError = true)] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
'@
        }
        $script:keyboardWindow = [IntPtr][int64]$script:app.Hwnd
        [uint32]$keyboardPid = 0
        $script:keyboardThread = [ItE2E.SelectAllKeyboardLayout]::GetWindowThreadProcessId($script:keyboardWindow, [ref]$keyboardPid)
        if ($keyboardPid -ne $script:app.Pid) { throw 'Keyboard-layout target does not belong to the test window.' }
        $script:previousKeyboardLayout = [ItE2E.SelectAllKeyboardLayout]::GetKeyboardLayout($script:keyboardThread)
        $layoutCount = [ItE2E.SelectAllKeyboardLayout]::GetKeyboardLayoutList(0, $null)
        $layouts = [IntPtr[]]::new($layoutCount)
        [void][ItE2E.SelectAllKeyboardLayout]::GetKeyboardLayoutList($layoutCount, $layouts)
        $script:testKeyboardLayout = @($layouts | Where-Object { $_.ToInt64() -eq 0x04090409 } | Select-Object -First 1)
        if ($script:testKeyboardLayout.Count -ne 1) { throw 'Physical letter-key assertions require an already loaded English (US) keyboard layout.' }
        $script:testKeyboardLayout = $script:testKeyboardLayout[0]
        # Letter-key assertions must commit text, not leave it in an IME composition.
        if ($script:previousKeyboardLayout -ne $script:testKeyboardLayout) {
            if (-not [ItE2E.SelectAllKeyboardLayout]::PostMessage($script:keyboardWindow, 0x0050, [IntPtr]::Zero, $script:testKeyboardLayout)) {
                throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
            }
            $script:keyboardLayoutChanged = $true
            Wait-Until -TimeoutSec 5 -Because 'the test window to activate its deterministic keyboard layout' -Condition {
                [ItE2E.SelectAllKeyboardLayout]::GetKeyboardLayout($script:keyboardThread) -eq $script:testKeyboardLayout
            } | Out-Null
        }
        $script:readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $script:readyPattern) { $script:readyPattern = '(?i)Ask anything.*for commands' }
        @{
            Package = Get-ItTestPackage
            Pid = $script:app.Pid
            Hwnd = $script:app.Hwnd
            WindowId = $script:app.WindowId
            PaneSessionId = $script:agentPane
            PreviousKeyboardLayout = ('0x{0:X}' -f $script:previousKeyboardLayout.ToInt64())
            TestKeyboardLayout = ('0x{0:X}' -f $script:testKeyboardLayout.ToInt64())
            KeyboardLayoutChanged = $script:keyboardLayoutChanged
            StartedUtc = [DateTime]::UtcNow.ToString('o')
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'target.json') -Encoding utf8NoBOM

        $script:sendKey = {
            param([int]$Vk, [switch]$Ctrl, [switch]$Shift)
            Send-WtWindowKey -App $script:app -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift -RequireForeground | Out-Null
        }
        $script:capture = {
            Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 500
        }
        $script:saveEvidence = {
            param([string]$Name)
            $script:evidenceIndex++
            $prefix = Join-Path $script:evidenceDir ('{0:D3}-{1}' -f $script:evidenceIndex, $Name)
            Set-Content -LiteralPath "$prefix.txt" -Value (& $script:capture) -NoNewline -Encoding utf8NoBOM
            Save-UiScreenshot -App $script:app -Path "$prefix.png" | Out-Null
        }
        $script:promptCount = {
            @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match '\|prompt\|' }).Count
        }
        $script:assertDraft = {
            param([string]$Text)
            $script:expectedDraftPattern = '(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($Text) + '\s*[│║|]\s*$'
            Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
                (& $script:capture) -match $script:expectedDraftPattern
            } | Should -BeTrue -Because "the live input must contain exactly '$Text', not appended/stale text"
        }
        $script:assertEmptyInput = {
            Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
                (& $script:capture) -match ('(?m)^\s*[│║|]\s*>\s*' + $script:readyPattern + '[.…]*\s*[│║|]\s*$')
            } | Should -BeTrue -Because 'the current input row must show the empty connected placeholder'
        }
        $script:copySelection = {
            param([string]$Name)
            $script:copySentinel = "CLIPBOARD_SENTINEL_$([guid]::NewGuid().ToString('N'))"
            Set-Clipboard -Value $script:copySentinel
            & $script:sendKey -Vk 0x43 -Ctrl
            $copied = Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Quiet -Condition {
                $text = Get-Clipboard -Raw
                if ($text -cne $script:copySentinel) { $text }
            }
            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$Name-clipboard.txt") -Value $copied -NoNewline -Encoding utf8NoBOM
            & $script:saveEvidence -Name "$Name-after-copy"
            $copied | Should -Not -BeNullOrEmpty -Because 'the physical copy must replace the clipboard sentinel'
            $copied
        }
        $script:pasteText = {
            param([string]$Text)
            Set-Clipboard -Value $Text
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                & $script:sendKey -Vk 0x56 -Ctrl
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 5 -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                }
                $event | Should -Not -BeNullOrEmpty -Because 'physical Ctrl+V must reach the owner-scoped WTA paste path'
            }
            finally { Stop-WtEventListener -Listener $listener }
        }
        $script:completeTurn = {
            param([string]$Marker)
            $script:completedTurnMarker = $Marker
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $Marker | Out-Null
            Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                $text = & $script:capture
                $text -match [regex]::Escape("ACK_$script:completedTurnMarker") -and $text -match $script:readyPattern
            } | Should -BeTrue -Because 'the fixture response must render and finish through the real ACP boundary'
        }
    }

    BeforeEach {
        # Escape may first dismiss a selection/history focus. Reset without repeatedly
        # sending Ctrl+C to an empty input, which would close the helper.
        & $script:sendKey -Vk 0x1B
        & $script:sendKey -Vk 0x1B
        & $script:assertEmptyInput
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agentPane) | Out-Null
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'an unlocked foreground desktop is required before physical keyboard input'
        }
        # This positive editing probe proves the actual target, not just foreground HWND.
        & $script:sendKey -Vk 0x51
        & $script:assertDraft -Text 'q'
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text ' INPUT FOCUS PROBE' -NoSubmit | Out-Null
        & $script:assertDraft -Text 'q INPUT FOCUS PROBE'
        & $script:sendKey -Vk 0x1B
        & $script:assertEmptyInput
    }

    AfterEach {
        if ($script:app) { & $script:saveEvidence -Name 'case-final' }
    }

    AfterAll {
        try {
            try {
                if ($script:keyboardLayoutChanged) {
                    [uint32]$keyboardPid = 0
                    $thread = [ItE2E.SelectAllKeyboardLayout]::GetWindowThreadProcessId($script:keyboardWindow, [ref]$keyboardPid)
                    if ($thread -ne $script:keyboardThread -or $keyboardPid -ne $script:app.Pid) {
                        throw 'Keyboard-layout restoration target no longer belongs to the test window.'
                    }
                    if (-not [ItE2E.SelectAllKeyboardLayout]::PostMessage($script:keyboardWindow, 0x0050, [IntPtr]::Zero, $script:previousKeyboardLayout)) {
                        throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
                    }
                    Wait-Until -TimeoutSec 5 -Because 'the test window to restore its original keyboard layout' -Condition {
                        [ItE2E.SelectAllKeyboardLayout]::GetKeyboardLayout($script:keyboardThread) -eq $script:previousKeyboardLayout
                    } | Out-Null
                }
            }
            finally {
                if ($script:app) { Stop-Terminal -App $script:app }
            }
        }
        finally {
            try {
                if ($script:clipboardSaved) { Restore-ClipboardSnapshot -Snapshot $script:originalClipboard }
            }
            finally {
                if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
                    Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
                }
            }
        }
    }

    It 'Ctrl+A selects and copies the current agent frame' -Tag 'AgentFrameSelectAll' {
        $id = [guid]::NewGuid().ToString('N')
        $copiedPattern = Get-WtaLocalizedTextRegex -Key 'system.selection_copied'
        if (-not $copiedPattern) { $copiedPattern = '(?i)Copied' }
        & $script:saveEvidence -Name 'empty-before-select-all'
        & $script:sendKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'empty-selected'
        $emptyCopy = & $script:copySelection -Name 'empty-frame'
        $emptyCopy | Should -Match $script:readyPattern -Because 'empty focused input must retain whole-pane selection'
        Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match $copiedPattern
        } | Should -BeTrue -Because 'pane copy must preserve the existing copied confirmation'

        $offscreenMarker = "SCROLL_TURN_00_$id"
        $turnMarker = $null
        $countBefore = & $script:promptCount
        $turnCount = 0
        foreach ($index in 0..15) {
            $marker = 'SCROLL_TURN_{0:D2}_{1}' -f $index, $id
            & $script:completeTurn -Marker $marker
            $turnCount++
            $frame = & $script:capture
            if ($index -gt 0 -and $frame -notmatch [regex]::Escape($offscreenMarker)) {
                $turnMarker = $marker
                break
            }
        }
        $turnMarker | Should -Not -BeNullOrEmpty -Because 'setup must move the oldest marker outside the current frame'
        (& $script:promptCount) | Should -Be ($countBefore + $turnCount)
        (& $script:capture) | Should -Match $script:readyPattern -Because 'pane fallback requires an empty draft'
        & $script:saveEvidence -Name 'frame-before-select-all'
        & $script:sendKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'frame-selected'
        $copied = & $script:copySelection -Name 'current-frame'
        $copied | Should -Match ([regex]::Escape($turnMarker))
        $copied | Should -Match ([regex]::Escape("ACK_$turnMarker"))
        $copied | Should -Not -Match ([regex]::Escape($offscreenMarker)) -Because 'pane copy must not include virtual history or native scrollback'
        $sentinel = "NO_STALE_FRAME_$id"
        Set-Clipboard -Value $sentinel
        & $script:sendKey -Vk 0x43 -Ctrl
        (Get-Clipboard -Raw) | Should -BeExactly $sentinel -Because 'pane copy must clear selection rather than replay it'
        & $script:sendKey -Vk 0x51
        & $script:assertDraft -Text 'q'
        & $script:sendKey -Vk 0x43 -Ctrl
        & $script:assertEmptyInput
        & $script:saveEvidence -Name 'pane-copy-cleared'
    }

    It 'Ctrl+A selects and copies only the focused agent draft' -Tag 'AgentInputSelectAll', 'AgentInputSelectAllCopy' {
        $id = [guid]::NewGuid().ToString('N')
        $turn = "SCROLL_TURN_00_$id"
        & $script:completeTurn -Marker $turn
        $countBefore = & $script:promptCount
        $plainDraft = "Select all these words in my draft $($id.Substring(0, 8))"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $plainDraft -NoSubmit | Out-Null
        & $script:assertDraft -Text $plainDraft
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'focused-copy-source.txt') -Value $plainDraft -NoNewline -Encoding utf8NoBOM
        & $script:saveEvidence -Name 'focused-copy-before-select-all'
        & $script:sendKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'focused-copy-selected'
        (& $script:copySelection -Name 'focused-copy') | Should -BeExactly $plainDraft -Because 'physical Ctrl+A then Ctrl+C must copy only the focused draft, not the rendered frame'
        & $script:assertDraft -Text $plainDraft
        & $script:sendKey -Vk 0x1B
        & $script:sendKey -Vk 0x1B
        & $script:assertEmptyInput

        $frame = & $script:capture
        $width = [int](($frame -split "`r?`n" | ForEach-Object Length | Measure-Object -Maximum).Maximum)
        $width | Should -BeGreaterThan 20 -Because 'a measurable input viewport is required to prove wrapped source copying'
        $draft = "DRAFT_$id " + ('wrap ' * [Math]::Ceiling($width / 5)) + "é中`nsecond_$id"
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'focused-draft-source.txt') -Value $draft -NoNewline -Encoding utf8NoBOM
        & $script:pasteText -Text $draft
        Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match [regex]::Escape("second_$id")
        } | Should -BeTrue -Because 'the final logical line must reach the input before selecting'
        & $script:saveEvidence -Name 'draft-before-select-all'
        & $script:sendKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'draft-selected'
        & $script:sendKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'draft-selected-again'
        (& $script:copySelection -Name 'focused-draft') | Should -BeExactly $draft -Because 'copy must contain source newlines and Unicode, never borders, wrap rows, or chat messages'
        & $script:sendKey -Vk 0x41 -Ctrl
        (& $script:copySelection -Name 'focused-draft-retained') | Should -BeExactly $draft -Because 'copy must not clear or mutate the draft'
        (& $script:promptCount) | Should -Be $countBefore -Because 'selecting and copying must not submit a prompt'
    }

    It 'Selected agent draft supports cut deletion and replacement' -Tag 'AgentInputSelectAll' {
        $id = [guid]::NewGuid().ToString('N')
        $turn = "SCROLL_TURN_00_$id"
        & $script:completeTurn -Marker $turn
        $countBefore = & $script:promptCount
        foreach ($action in @('cut', 'backspace', 'delete', 'typing', 'paste')) {
            $draft = "EDIT_${action}_$($id.Substring(0, 8))"
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draft -NoSubmit | Out-Null
            & $script:assertDraft -Text $draft
            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$action-source.txt") -Value $draft -NoNewline -Encoding utf8NoBOM
            & $script:saveEvidence -Name "$action-before-select-all"
            & $script:sendKey -Vk 0x41 -Ctrl
            & $script:saveEvidence -Name "$action-selected"
            $replacement = ''
            switch ($action) {
                'cut' {
                    $sentinel = "CUT_SENTINEL_$id"
                    Set-Clipboard -Value $sentinel
                    & $script:sendKey -Vk 0x58 -Ctrl
                    $cut = Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Quiet -Condition {
                        $text = Get-Clipboard -Raw
                        if ($text -cne $sentinel) { $text }
                    }
                    Set-Content -LiteralPath (Join-Path $script:evidenceDir 'cut-clipboard.txt') -Value $cut -NoNewline -Encoding utf8NoBOM
                    $cut | Should -BeExactly $draft -Because 'Ctrl+X must copy exactly the selected draft'
                }
                'backspace' { & $script:sendKey -Vk 0x08 }
                'delete' { & $script:sendKey -Vk 0x2E }
                'typing' {
                    & $script:sendKey -Vk 0x51
                    $replacement = 'q'
                }
                'paste' {
                    $replacement = "PASTED_$($id.Substring(0, 8))"
                    & $script:pasteText -Text $replacement
                }
            }
            if ($replacement) {
                & $script:assertDraft -Text $replacement
                & $script:saveEvidence -Name "$action-result"
                & $script:sendKey -Vk 0x51
                & $script:assertDraft -Text "${replacement}q"
                & $script:sendKey -Vk 0x43 -Ctrl
            }
            Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 5 |
                Should -BeTrue -Because "$action must remove the entire selected draft, not a single character"
            if (-not $replacement) { & $script:saveEvidence -Name "$action-result" }
            (& $script:capture) | Should -Match ([regex]::Escape("ACK_$turn")) -Because 'editing must preserve the completed conversation'
            (& $script:promptCount) | Should -Be $countBefore -Because "$action must not submit the draft"
        }
    }

    It 'Agent draft selection cancels and collapses without losing text' -Tag 'AgentInputSelectAll' {
        $id = [guid]::NewGuid().ToString('N')
        $countBefore = & $script:promptCount
        foreach ($action in @(
            @{ Name = 'escape'; Vk = 0x1B; Prefix = $false }
            @{ Name = 'left'; Vk = 0x25; Prefix = $true }
            @{ Name = 'right'; Vk = 0x27; Prefix = $false }
        )) {
            $draft = "KEEP_$($id.Substring(0, 8))"
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draft -NoSubmit | Out-Null
            & $script:assertDraft -Text $draft
            & $script:sendKey -Vk 0x41 -Ctrl
            & $script:saveEvidence -Name "$($action.Name)-selected"
            & $script:sendKey -Vk $action.Vk
            & $script:assertDraft -Text $draft
            & $script:saveEvidence -Name "$($action.Name)-collapsed"
            & $script:sendKey -Vk 0x51
            $expected = if ($action.Prefix) { "q$draft" } else { "${draft}q" }
            & $script:assertDraft -Text $expected
            & $script:saveEvidence -Name "$($action.Name)-typed"
            & $script:sendKey -Vk 0x43 -Ctrl
            & $script:assertEmptyInput
        }
        (& $script:promptCount) | Should -Be $countBefore -Because 'Escape and caret collapse must never submit a draft'
    }

    It 'Ctrl+A preserves pane selection while history owns focus' -Tag 'AgentInputSelectAll' {
        $id = [guid]::NewGuid().ToString('N')
        $turn = "SCROLL_TURN_00_$id"
        & $script:completeTurn -Marker $turn
        $draft = "HISTORY_DRAFT_$($id.Substring(0, 8))"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draft -NoSubmit | Out-Null
        & $script:assertDraft -Text $draft
        $countBefore = & $script:promptCount
        $rows = @((& $script:capture) -split "`r?`n")
        $hit = $null
        for ($row = 0; $row -lt $rows.Count; $row++) {
            $index = $rows[$row].IndexOf($turn, [StringComparison]::Ordinal)
            if ($index -ge 0 -and $rows[$row] -notmatch 'ACK_') {
                $hit = @{ Row = $row; Column = $index + 2 }
                break
            }
        }
        $hit | Should -Not -BeNullOrEmpty -Because 'a visible completed-turn header is required for history focus'
        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane -Column $hit.Column -Row $hit.Row | Out-Null
        Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            $text = & $script:capture
            $text -match [regex]::Escape($turn) -and $text -notmatch [regex]::Escape("ACK_$turn")
        } | Should -BeTrue -Because 'clicking the completed turn must collapse it and move editing focus out of the draft'
        & $script:assertDraft -Text $draft
        & $script:saveEvidence -Name 'history-before-select-all'
        & $script:sendKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'history-pane-selected'
        $copied = & $script:copySelection -Name 'history-pane'
        $copied | Should -Match ([regex]::Escape($turn)) -Because 'a nonempty background draft must not steal Ctrl+A from history'
        $copied | Should -Match ([regex]::Escape($draft))
        $copied | Should -Not -BeExactly $draft
        & $script:assertDraft -Text $draft
        (& $script:promptCount) | Should -Be $countBefore -Because 'history Enter must not submit the background draft'
        & $script:sendKey -Vk 0x1B
        & $script:sendKey -Vk 0x43 -Ctrl
        & $script:assertEmptyInput
    }

    It 'Copying the selected agent draft does not cancel a running turn' -Tag 'AgentInputSelectAll' {
        $id = [guid]::NewGuid().ToString('N')
        $turn = "SCROLL_TURN_00_$id"
        $draft = "RUNNING_DRAFT_$($id.Substring(0, 8))"
        try {
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "$turn HOLD_FOR_RELEASE" | Out-Null
            Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                (& $script:capture) -match [regex]::Escape("PENDING_$turn")
            } | Should -BeTrue -Because 'the pending fixture chunk must render before testing cancellation'
            (Get-Content -LiteralPath $script:fixtureLog -Raw) | Should -Match ('\|held\|' + [regex]::Escape($turn))
            (& $script:capture) | Should -Not -Match ([regex]::Escape("ACK_$turn"))
            $countBefore = & $script:promptCount
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draft -NoSubmit | Out-Null
            & $script:assertDraft -Text $draft
            & $script:sendKey -Vk 0x41 -Ctrl
            & $script:saveEvidence -Name 'running-draft-selected'
            (& $script:copySelection -Name 'running-draft') | Should -BeExactly $draft
            & $script:assertDraft -Text $draft
            & $script:sendKey -Vk 0x41 -Ctrl
            & $script:sendKey -Vk 0x1B
            & $script:assertDraft -Text $draft
            & $script:saveEvidence -Name 'running-selection-dismissed'
            & $script:sendKey -Vk 0x51
            & $script:assertDraft -Text "${draft}q"
            (Get-Content -LiteralPath $script:fixtureLog -Raw) | Should -Not -Match '\|cancel\|' -Because 'selected copy and Escape must not send ACP cancellation'
            Set-Content -LiteralPath $script:releasePromptPath -Value 'release' -NoNewline -Encoding utf8NoBOM
            Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                (& $script:capture) -match [regex]::Escape("ACK_$turn")
            } | Should -BeTrue -Because 'the same still-running ACP turn must render its final response after copy'
            & $script:assertDraft -Text "${draft}q"
            (& $script:promptCount) | Should -Be $countBefore
            (Get-Content -LiteralPath $script:fixtureLog -Raw) | Should -Not -Match '\|cancel\|'
            & $script:saveEvidence -Name 'running-turn-completed-draft-retained'
        }
        finally {
            # Release a pending fixture even when the baseline copy oracle fails.
            Set-Content -LiteralPath $script:releasePromptPath -Value 'release' -NoNewline -Encoding utf8NoBOM
        }
    }
}
