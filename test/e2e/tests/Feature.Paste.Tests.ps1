#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2 (C065) — pasting text into the agent pane. Real paste path: put text on the OS
# clipboard, focus the agent pane, send Ctrl+V (a WT window keystroke), and assert the text lands in
# the agent input. This exercises WT's agent-pane interception -> protocol event -> WTA clipboard
# read -> draft insertion path the way a user does (not wtcli typing).
#
# Ctrl+V is a WT window accelerator, so this needs the WT window to hold foreground; when it can't be
# taken the case SKIPS (a foreground precondition) rather than failing flakily. Single-line and
# multiline cases both use the live OS clipboard; deterministic Rust tests separately cover text
# normalization, target routing, cursor insertion, stale completion, and non-live input gating.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command pwsh -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §2 agent pane paste' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:originalClipboard = Get-Clipboard -Raw -ErrorAction SilentlyContinue
        $fixtureSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $script:fixtureDir = Join-Path $env:TEMP "ItE2E paste $([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $script:fixtureDir | Out-Null
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath $fixtureSource -Destination $fixture
        $script:fixtureLog = Join-Path $script:fixtureDir 'fixture.log'
        $fixtureInvocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureInvocation))
        $command = "pwsh -NoProfile -EncodedCommand $encodedInvocation"
        $script:evidenceDir = Join-Path $PSScriptRoot '..\artifacts\right-click-copy\green'
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:paste-fixture'
            acpCustomCommand = $command
        }
        $shell = Get-ActivePane -App $script:app
        $script:ownerTabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $shell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 | Out-Null
        $script:sendPasteKey = {
            param([switch]$Shift, [switch]$PreserveFocus)

            if (-not $PreserveFocus) {
                Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agentPane) | Out-Null
            }
            Set-WtWindowForeground -App $script:app | Out-Null
            Start-Sleep -Milliseconds 300
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Send-WtWindowKey -App $script:app -Vk 0x56 -Ctrl -Shift:$Shift -RequireForeground | Out-Null
                return Wait-WtEvent -Listener $listener -TimeoutSec 5 -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)" -eq "$($script:ownerTabId)" -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                }
            }
            catch {
                if ($_.Exception.Message -eq 'Send-WtWindowKey: WT window could not be brought to the foreground (competing foreground app).') {
                    return $false
                }
                throw
            }
            finally {
                Stop-WtEventListener -Listener $listener
            }
        }
        $script:clearPasteDraft = {
            if (Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 1) {
                return
            }
            # A single Ctrl+C clears a nonempty idle draft. Never send it to the already-empty
            # input: repeated empty-input Ctrl+C presses close the agent pane.
            Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
            if (-not (Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 5)) {
                throw 'Could not restore the empty connected agent input.'
            }
        }
        $script:getInputSegment = {
            param([string]$Text)

            $lines = @($Text -split "`r?`n")
            $inputStart = -1
            for ($index = 0; $index -lt $lines.Count; $index++) {
                if ($lines[$index] -match '>\s*') { $inputStart = $index }
            }
            if ($inputStart -lt 0) { return '' }
            ($lines[$inputStart..($lines.Count - 1)] -join "`n")
        }
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($null -ne $script:originalClipboard) { Set-Clipboard -Value $script:originalClipboard }
        if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
            Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
        }
    }

    It 'Ctrl+V pastes into the agent input instead of typing a literal v' -Tag 'PasteCore' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+V paste accelerator'; return }
        $marker = "PASTE$(Get-Random -Maximum 999999)"
        Set-Clipboard -Value $marker

        try {
            & $script:clearPasteDraft
            $promptCountBefore = @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count
            $pasteEvent = & $script:sendPasteKey
            if (-not $pasteEvent) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the Ctrl+V paste accelerator'; return }
            Start-Sleep -Milliseconds 500
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30
            ([regex]::Matches($text, [regex]::Escape($marker))).Count |
                Should -Be 1 -Because 'clipboard text pasted with Ctrl+V must appear exactly once in the current draft'
            $expectedDraft = '(?m)>\s*' + [regex]::Escape($marker) + '[^A-Za-z0-9_]*$'
            (& $script:getInputSegment -Text $text) | Should -Match $expectedDraft -Because 'the draft must contain exactly the marker without a leaked literal v'
            @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count |
                Should -Be $promptCountBefore -Because 'pasting must not submit a prompt to the agent'
            $text | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'ctrl-v-single-pane.txt') -Encoding utf8NoBOM
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'ctrl-v-single.png') | Out-Null
        }
        finally {
            & $script:clearPasteDraft
        }
    }

    It 'Paste works (multiline text remains in one agent draft without submitting)' -Tag 'PasteCore' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+V paste accelerator'; return }
        $id = Get-Random -Maximum 999999
        $markers = @("MLPASTE_A_$id", "MLPASTE_B_$id", "MLPASTE_C_$id")
        Set-Clipboard -Value ($markers -join "`r`n")

        try {
            & $script:clearPasteDraft
            $promptCountBefore = @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count
            $pasteEvent = & $script:sendPasteKey
            if (-not $pasteEvent) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the multiline Ctrl+V paste accelerator'; return }
            Start-Sleep -Milliseconds 500
            $draftText = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30
            foreach ($marker in $markers) {
                ([regex]::Matches($draftText, [regex]::Escape($marker))).Count |
                    Should -Be 1 -Because 'every clipboard line must appear exactly once in the same unsent draft'
            }
            $inputSegment = & $script:getInputSegment -Text $draftText
            $expectedDraft = '(?m)>\s*' + [regex]::Escape($markers[0]) + '[^A-Za-z0-9_]*\r?\n' +
                '[^A-Za-z0-9_]*' + [regex]::Escape($markers[1]) + '[^A-Za-z0-9_]*\r?\n' +
                '[^A-Za-z0-9_]*' + [regex]::Escape($markers[2]) + '[^A-Za-z0-9_]*$'
            $inputSegment | Should -Match $expectedDraft -Because 'all clipboard lines must remain contiguous and ordered without extra draft text'
            @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count |
                Should -Be $promptCountBefore -Because 'multiline paste must not submit a prompt to the agent'
            $draftText | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'ctrl-v-multiline-pane.txt') -Encoding utf8NoBOM
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'ctrl-v-multiline.png') | Out-Null
        }
        finally {
            & $script:clearPasteDraft
        }
    }

    It 'Ctrl+Shift+V remains a paste positive control in the agent input' -Tag 'PasteCore' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+Shift+V paste accelerator'; return }
        $marker = "PASTECONTROL$(Get-Random -Maximum 999999)"
        Set-Clipboard -Value $marker

        try {
            & $script:clearPasteDraft
            $pasteEvent = & $script:sendPasteKey -Shift
            if (-not $pasteEvent) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the Ctrl+Shift+V paste accelerator'; return }
            Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30
                ([regex]::Matches($text, [regex]::Escape($marker))).Count -eq 1
            } | Should -BeTrue -Because 'Ctrl+Shift+V must prove the clipboard and structured agent paste path are healthy'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'ctrl-shift-v-control.png') | Out-Null
        }
        finally {
            & $script:clearPasteDraft
        }
    }

    It 'Ctrl+V paste survives input refocus after physical history interaction' -Tag 'PasteRefocus' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+V paste accelerator'; return }
        $id = [guid]::NewGuid().ToString('N')
        $prompt = "SCROLL_TURN_00_$id"
        $reply = "ACK_$prompt"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $prompt | Out-Null
        Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -match [regex]::Escape($reply)
        } | Should -BeTrue

        $promptRect = @(Get-UiTextBounds -App $script:app -Text $prompt)[0]
        $promptX = [Math]::Round($promptRect.Left + ($promptRect.Width / 2))
        $promptY = [Math]::Round($promptRect.Top + ($promptRect.Height / 2))
        $collapsed = $false
        for ($attempt = 0; $attempt -lt 2 -and -not $collapsed; $attempt++) {
            try {
                Invoke-UiMouseDrag -App $script:app -FromX $promptX -FromY $promptY -ToX $promptX -ToY $promptY | Out-Null
            }
            catch {
                if ($_.Exception.Message -match 'No interactive desktop is available') {
                    Set-ItResult -Skipped -Because 'physical mouse injection requires an unlocked interactive desktop'
                    return
                }
                throw
            }
            $collapsed = Test-Until -TimeoutSec 2 -IntervalSec 0.25 -Condition {
                (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -notmatch [regex]::Escape($reply)
            }
        }
        $collapsed | Should -BeTrue -Because 'the physical history click must select and collapse the completed turn'

        $inputRect = @(Get-UiTextBounds -App $script:app -Text 'Ask anything, / for commands..')[0]
        $inputX = [Math]::Round($inputRect.Left + ($inputRect.Width / 2))
        $inputY = [Math]::Round($inputRect.Top + ($inputRect.Height / 2))
        Invoke-UiMouseDrag -App $script:app -FromX $inputX -FromY $inputY -ToX $inputX -ToY $inputY | Out-Null

        $marker = "PASTEREFOCUS_$id"
        Set-Clipboard -Value $marker
        try {
            $pasteEvent = & $script:sendPasteKey -PreserveFocus
            if (-not $pasteEvent) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the Ctrl+V paste accelerator'; return }
            Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
                ([regex]::Matches($text, [regex]::Escape($marker))).Count -eq 1
            } | Should -BeTrue -Because 'Ctrl+V must paste after returning from completed-turn interaction to the input dialog'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'ctrl-v-refocus.png') | Out-Null
        }
        finally {
            & $script:clearPasteDraft
        }
    }

    It 'Ctrl+V paste stays isolated to its owner tab' -Tag 'PasteOwnerIsolation' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+V paste accelerator'; return }
        $firstPane = $script:agentPane
        $secondShell = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo' -Title 'Paste owner isolation'
        Open-AgentPane -App $script:app | Out-Null
        $secondPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $secondShell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $secondPane -TimeoutSec 60 | Should -BeTrue

        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $firstPane) | Out-Null
        $marker = "PASTEOWNER_$([guid]::NewGuid().ToString('N'))"
        Set-Clipboard -Value $marker
        try {
            $pasteEvent = & $script:sendPasteKey
            if (-not $pasteEvent) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the Ctrl+V paste accelerator'; return }
            Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                $first = Get-AgentPaneText -App $script:app -PaneSessionId $firstPane -MaxLines 30
                ([regex]::Matches($first, [regex]::Escape($marker))).Count -eq 1
            } | Should -BeTrue -Because 'the focused owner draft must receive the paste exactly once'
            $second = Get-AgentPaneText -App $script:app -PaneSessionId $secondPane -MaxLines 30
            $second | Should -Not -Match ([regex]::Escape($marker)) -Because 'a sibling agent pane must not receive another tab paste'
        }
        finally {
            Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $firstPane) | Out-Null
            & $script:clearPasteDraft
            Close-WtPane -App $script:app -SessionId $secondShell.session_id
        }
    }
}
