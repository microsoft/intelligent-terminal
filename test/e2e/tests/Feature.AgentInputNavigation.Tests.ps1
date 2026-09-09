#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Physical Up/Down -> TerminalControl/ConPTY -> WTA visual-row caret -> physical insertion/copy.
# The ACP fixture is deterministic and is used only for readiness and prompt-history compatibility.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent input visual-row navigation' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        . (Join-Path $PSScriptRoot 'helpers\TestWindowKeyboardLayout.ps1')
        $script:app = $null
        $script:keyboardLayout = $null
        $script:clipboardSaved = $false
        $script:fixtureDir = $null
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force

        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) {
            $env:ITE2E_ARTIFACT_ROOT
        }
        else {
            Join-Path $PSScriptRoot '..\artifacts'
        }
        $artifactRoot = [System.IO.Path]::GetFullPath($artifactRoot)
        $script:evidenceDir = Join-Path $artifactRoot "agent-input-navigation\$([guid]::NewGuid().ToString('N'))"
        $script:fixtureDir = Join-Path $script:evidenceDir 'fixture'
        New-Item -ItemType Directory -Force -Path $script:fixtureDir | Out-Null
        $script:fixtureLog = Join-Path $script:evidenceDir 'fixture.log'
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1') -Destination $fixture
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $command = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"

        $script:originalClipboard = Get-ClipboardSnapshot
        $script:clipboardSaved = $true
        $script:evidenceIndex = 0
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:input-navigation-fixture'
            acpCustomCommand = $command
            rightClickContextMenu = $false
            'warning.confirmOnClose' = 'never'
        }
        $shell = Get-ActivePane -App $script:app
        $script:ownerTabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $shell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before input navigation'
        $script:keyboardLayout = Enable-TestWindowEnglishKeyboardLayout -App $script:app
        $script:readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $script:readyPattern) { $script:readyPattern = '(?i)Ask anything.*for commands' }

        @{
            Package = Get-ItTestPackage
            Pid = $script:app.Pid
            Hwnd = $script:app.Hwnd
            WindowId = $script:app.WindowId
            PaneSessionId = $script:agentPane
            PreviousKeyboardLayout = ('0x{0:X}' -f $script:keyboardLayout.PreviousLayout.ToInt64())
            TestKeyboardLayout = ('0x{0:X}' -f $script:keyboardLayout.TestLayout.ToInt64())
            KeyboardLayoutChanged = $script:keyboardLayout.Changed
            StartedUtc = [DateTime]::UtcNow.ToString('o')
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'target.json') -Encoding utf8NoBOM

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
        $script:sendPhysicalKey = {
            param([int]$Vk, [switch]$Ctrl, [int]$Repeat = 1)
            Send-WtWindowKey -App $script:app -Vk $Vk -Ctrl:$Ctrl -Repeat $Repeat -RequireForeground | Out-Null
        }
        $script:assertEmptyInput = {
            Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
                (& $script:capture) -match ('(?m)^\s*[│║|]\s*>\s*' + $script:readyPattern + '[.…]*\s*[│║|]\s*$')
            } | Should -BeTrue -Because 'the current input must show the empty connected placeholder'
        }
        $script:pasteDraft = {
            param([string]$Text)
            $tail = @($Text -split "`r?`n" | Where-Object { $_.Length -gt 0 })[-1]
            $script:navigationPasteTail = $tail.Substring([Math]::Max(0, $tail.Length - 8))
            Set-Clipboard -Value $Text
            $listener = Start-WtEventListener -App $script:app -WaitForReady
            try {
                & $script:sendPhysicalKey -Vk 0x56 -Ctrl
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 5 -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                }
                $event | Should -Not -BeNullOrEmpty -Because 'physical Ctrl+V must reach the owner-scoped paste path'
                Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                    (& $script:capture) -match [regex]::Escape($script:navigationPasteTail)
                } | Should -BeTrue -Because 'the complete pasted draft must reach the input before moving the caret'
            }
            finally {
                Stop-WtEventListener -Listener $listener
            }
        }
        $script:copyDraft = {
            param([string]$Name)
            $script:navigationCopySentinel = "NAV_CLIPBOARD_$([guid]::NewGuid().ToString('N'))"
            Set-Clipboard -Value $script:navigationCopySentinel
            & $script:sendPhysicalKey -Vk 0x41 -Ctrl
            & $script:sendPhysicalKey -Vk 0x43 -Ctrl
            $copied = Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Quiet -Condition {
                $text = Get-Clipboard -Raw
                if ($text -cne $script:navigationCopySentinel) { $text }
            }
            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$Name-clipboard.txt") -Value $copied -NoNewline -Encoding utf8NoBOM
            & $script:saveEvidence -Name "$Name-before-assertion"
            $copied | Should -Not -BeNullOrEmpty -Because 'physical Ctrl+A/C must replace the clipboard sentinel'
            $copied
        }
        $script:clearDraft = {
            & $script:sendPhysicalKey -Vk 0x1B
            & $script:sendPhysicalKey -Vk 0x1B
            & $script:assertEmptyInput
        }
        $script:getRenderedInputRows = {
            param([string]$Source)
            $lines = @((& $script:capture) -split "`r?`n")
            $promptIndex = -1
            for ($i = $lines.Count - 1; $i -ge 0; $i--) {
                if ($lines[$i] -match '^\s*[│║|]\s*>\s*') {
                    $promptIndex = $i
                    break
                }
            }
            if ($promptIndex -lt 0) { throw 'Could not find the rendered input prompt row.' }
            $rows = [System.Collections.Generic.List[string]]::new()
            for ($i = $promptIndex; $i -lt $lines.Count; $i++) {
                if ($lines[$i] -notmatch '^\s*[│║|](?<body>.*)[│║|]\s*$') { break }
                $body = $Matches.body
                if ($i -eq $promptIndex) { $body = $body -replace '^\s*>\s?', '' }
                else { $body = $body -replace '^\s{1,3}', '' }
                $rows.Add($body.TrimEnd())
            }
            $joined = $rows -join ''
            if ($joined -cne $Source) {
                throw "Rendered soft-wrap discovery did not reconstruct the source exactly. Expected $($Source.Length) characters, got $($joined.Length)."
            }
            @($rows)
        }
    }

    BeforeEach {
        & $script:clearDraft
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agentPane) | Out-Null
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'an unlocked foreground desktop is required before physical keyboard input'
        }
        & $script:sendPhysicalKey -Vk 0x51
        Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match '(?m)^\s*[│║|]\s*>\s*q\s*[│║|]\s*$'
        } | Should -BeTrue -Because 'the physical focus probe must type into the test-owned agent input'
        & $script:clearDraft
    }

    AfterEach {
        if ($script:app) { & $script:saveEvidence -Name 'case-final' }
    }

    AfterAll {
        try {
            try {
                if ($script:keyboardLayout) {
                    Restore-TestWindowKeyboardLayout -App $script:app -Context $script:keyboardLayout
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

    It 'Multiline agent input arrows edit explicit rows' -Tag 'AgentInputNavigation', 'AgentInputNavigationHardNewline' {
        $id = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $lines = @(
            "Northern planning row zero $id"
            "Careful review row one $id"
            "请检查 CJK row two $id"
            "Stable editing row three $id"
            "Visible viewport row four $id"
            "Cursor travel row five $id"
            "Reliable marker row six $id"
            "Southern final row seven $id"
        )
        $draft = $lines -join "`n"
        & $script:pasteDraft -Text $draft
        $before = & $script:capture
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'hard-newline-source.txt') -Value $draft -NoNewline -Encoding utf8NoBOM
        & $script:saveEvidence -Name 'hard-newline-before-up'

        & $script:sendPhysicalKey -Vk 0x26 -Repeat 5
        & $script:sendPhysicalKey -Vk 0x51
        $lines[2] += 'q'
        $expected = $lines -join "`n"
        & $script:saveEvidence -Name 'hard-newline-after-insert'
        $copied = & $script:copyDraft -Name 'hard-newline'
        $copied | Should -BeExactly $expected -Because 'five physical Up keys must edit the explicit third row, not the final row or history'
        $before | Should -Not -Match ([regex]::Escape($lines[0])) -Because 'the first input row must begin outside the six-row viewport'
        & $script:sendPhysicalKey -Vk 0x1B
        & $script:sendPhysicalKey -Vk 0x26 -Repeat 7
        Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match [regex]::Escape($lines[0])
        } | Should -BeTrue -Because 'the viewport must follow the caret to the previously hidden first row'
        & $script:saveEvidence -Name 'hard-newline-scrolled-top'
        & $script:sendPhysicalKey -Vk 0x28 -Repeat 2
        & $script:sendPhysicalKey -Vk 0x51
        $lines[2] += 'q'
        (& $script:copyDraft -Name 'hard-newline-down') |
            Should -BeExactly ($lines -join "`n") -Because 'physical Down must return to the third row before insertion'
    }

    It 'Multiline agent input preserves the preferred display column' -Tag 'AgentInputNavigation', 'AgentInputNavigationPreferredColumn' {
        $id = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $first = "FIRST_$id-" + ('A' * 52)
        $short = "SHORT_$id"
        $last = "THIRD_$id-" + ('Z' * 58)
        $desiredColumn = 36
        $draft = "$first`n$short`n$last"
        & $script:pasteDraft -Text $draft
        & $script:saveEvidence -Name 'preferred-column-before'

        & $script:sendPhysicalKey -Vk 0x25 -Repeat ($last.Length - $desiredColumn)
        & $script:sendPhysicalKey -Vk 0x26 -Repeat 2
        & $script:sendPhysicalKey -Vk 0x51
        $expectedFirst = $first.Insert($desiredColumn, 'q')
        & $script:saveEvidence -Name 'preferred-column-after-insert'
        (& $script:copyDraft -Name 'preferred-column') |
            Should -BeExactly "$expectedFirst`n$short`n$last" -Because 'Up must clamp on the short row then recover the preferred column on the next long row'
    }

    It 'Soft-wrapped agent input arrows edit visual rows' -Tag 'AgentInputNavigation', 'AgentInputNavigationSoftWrap' {
        $frame = & $script:capture
        $frameWidth = [int](($frame -split "`r?`n" | ForEach-Object Length | Measure-Object -Maximum).Maximum)
        $frameWidth | Should -BeGreaterThan 20 -Because 'the live pane capture must expose a usable width'
        $length = [Math]::Max(60, (($frameWidth - 6) * 2) + 11)
        $alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789'
        $builder = [Text.StringBuilder]::new('SOFTWRAP')
        for ($i = $builder.Length; $i -lt $length; $i++) {
            [void]$builder.Append($alphabet[$i % $alphabet.Length])
        }
        $draft = $builder.ToString()
        & $script:pasteDraft -Text $draft
        & $script:saveEvidence -Name 'soft-wrap-after-paste'
        $rows = @(& $script:getRenderedInputRows -Source $draft)
        $rows.Count | Should -BeGreaterThan 1 -Because 'the draft length is derived from the live pane width and must soft-wrap'
        $lastLength = $rows[-1].Length
        $previousStart = $draft.Length - $lastLength - $rows[-2].Length
        $target = $previousStart + [Math]::Min($lastLength, $rows[-2].Length)
        & $script:saveEvidence -Name 'soft-wrap-before-up'

        & $script:sendPhysicalKey -Vk 0x26
        & $script:sendPhysicalKey -Vk 0x51
        $expected = $draft.Insert($target, 'q')
        & $script:saveEvidence -Name 'soft-wrap-after-insert'
        (& $script:copyDraft -Name 'soft-wrap') |
            Should -BeExactly $expected -Because 'physical Up must move one rendered row within a single logical line'
    }

    It 'Selected agent input arrows collapse without deleting text' -Tag 'AgentInputNavigation', 'AgentInputNavigationSelection' {
        $draft = "Select this complete draft`n保留这些文字"
        & $script:pasteDraft -Text $draft
        & $script:sendPhysicalKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'selection-before-up'
        & $script:sendPhysicalKey -Vk 0x26
        & $script:sendPhysicalKey -Vk 0x51
        (& $script:copyDraft -Name 'selection-up') |
            Should -BeExactly "q$draft" -Because 'Up must collapse a full-input selection to the start before typing'

        & $script:clearDraft
        & $script:pasteDraft -Text $draft
        & $script:sendPhysicalKey -Vk 0x41 -Ctrl
        & $script:saveEvidence -Name 'selection-before-down'
        & $script:sendPhysicalKey -Vk 0x28
        & $script:sendPhysicalKey -Vk 0x51
        (& $script:copyDraft -Name 'selection-down') |
            Should -BeExactly "${draft}q" -Because 'Down must collapse a full-input selection to the end before typing'
    }

    It 'Agent input arrow boundaries preserve prompt history' -Tag 'AgentInputNavigation', 'AgentInputNavigationHistory' {
        $marker = "SCROLL_TURN_00_$([guid]::NewGuid().ToString('N'))"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $marker | Out-Null
        Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match [regex]::Escape("ACK_$marker")
        } | Should -BeTrue -Because 'the deterministic fixture prompt must complete before history navigation'
        & $script:assertEmptyInput
        & $script:saveEvidence -Name 'history-before-up'

        & $script:sendPhysicalKey -Vk 0x26
        (& $script:copyDraft -Name 'history-recalled') |
            Should -BeExactly $marker -Because 'Up at the empty-input boundary must retain existing prompt recall'
        & $script:sendPhysicalKey -Vk 0x1B
        & $script:sendPhysicalKey -Vk 0x26
        & $script:sendPhysicalKey -Vk 0x28
        & $script:assertEmptyInput
        & $script:saveEvidence -Name 'history-restored-empty'
    }
}
