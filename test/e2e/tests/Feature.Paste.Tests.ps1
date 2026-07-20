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

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §2 agent pane paste' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        $script:sendPasteKey = {
            Set-WtWindowForeground -App $script:app | Out-Null
            Start-Sleep -Milliseconds 300
            try {
                Send-WtWindowKey -App $script:app -Vk 0x56 -Ctrl -RequireForeground | Out-Null
                return $true
            }
            catch {
                if ($_.Exception.Message -eq 'Send-WtWindowKey: WT window could not be brought to the foreground (competing foreground app).') {
                    return $false
                }
                throw
            }
        }
        $script:clearPasteDraft = {
            param([string[]]$Markers)

            $markerPattern = ($Markers | ForEach-Object { [regex]::Escape($_) }) -join '|'
            $inputPattern = '(?m)^\s*[│║|].*(?:' + $markerPattern + ')'
            $inputRowPattern = '(?m)^\s*[│║|]\s*>\s*'
            if ((Get-AgentPaneText -App $script:app -MaxLines 30) -match $inputPattern) {
                # In a nonempty idle input, Ctrl+C clears the buffer. Never send it when the
                # marker is absent: two Ctrl+C presses on an empty input can close the pane. Use
                # a Win32 key event because wtcli's tokenized C-c is not reliable across versions.
                Send-AgentWin32Key -App $script:app -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
            }
            if (-not (Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
                $text = Get-AgentPaneText -App $script:app -MaxLines 30
                -not [string]::IsNullOrWhiteSpace($text) -and
                $text -match $inputRowPattern -and
                $text -notmatch $inputPattern
            })) {
                throw 'Could not clear pasted test text from the agent draft.'
            }
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Paste works (clipboard text pasted with Ctrl+V lands in the agent input)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+V paste accelerator'; return }
        $marker = "PASTE$(Get-Random -Maximum 999999)"
        $singleDraftRow = '(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($marker)
        Set-Clipboard -Value $marker

        try {
            $pasted = $false
            $pasteInjected = $false
            for ($a = 0; $a -lt 4 -and -not $pasted; $a++) {
                & $script:clearPasteDraft -Markers @($marker)
                # The agent pane holds keyboard focus after Open-AgentPane, so Ctrl+V pastes into it
                # (an explicit winapp focus on the pane label can move focus away).
                if (-not (& $script:sendPasteKey)) { continue }
                $pasteInjected = $true
                $pasted = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
                    (Get-AgentPaneText -App $script:app -MaxLines 30) -match $singleDraftRow
                }
            }
            if (-not $pasteInjected) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the Ctrl+V paste accelerator'; return }
            $pasted | Should -BeTrue -Because 'clipboard text pasted with Ctrl+V must appear in the agent input'
        }
        finally {
            & $script:clearPasteDraft -Markers @($marker)
        }
    }

    It 'Paste works (multiline clipboard text stays in one agent draft without submitting)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+V paste accelerator'; return }
        $id = Get-Random -Maximum 999999
        $markers = @("MLPASTE_A_$id", "MLPASTE_B_$id", "MLPASTE_C_$id")
        $firstDraftRow = '(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($markers[0])
        $continuationRowB = '(?m)^\s*[│║|]\s{3,}' + [regex]::Escape($markers[1])
        $continuationRowC = '(?m)^\s*[│║|]\s{3,}' + [regex]::Escape($markers[2])
        Set-Clipboard -Value ($markers -join "`r`n")

        try {
            $draftText = $null
            $pasteInjected = $false
            for ($a = 0; $a -lt 4 -and -not $draftText; $a++) {
                & $script:clearPasteDraft -Markers $markers
                if (-not (& $script:sendPasteKey)) { continue }
                $pasteInjected = $true
                $draftText = Wait-Until -TimeoutSec 5 -IntervalSec 0.5 -Quiet -Condition {
                    $text = Get-AgentPaneText -App $script:app -MaxLines 30
                    if (
                        $text -match $firstDraftRow -and
                        $text -match $continuationRowB -and
                        $text -match $continuationRowC
                    ) {
                        $text
                    }
                }
            }
            if (-not $pasteInjected) { Set-ItResult -Skipped -Because 'WT could not hold foreground for the multiline Ctrl+V paste accelerator'; return }
            $draftText | Should -Not -BeNullOrEmpty -Because 'all clipboard lines must reach one agent draft after Ctrl+V is injected'
            $draftText | Should -Match $firstDraftRow -Because 'the first pasted line must remain in the input box'
            $draftText | Should -Match $continuationRowB -Because 'the second pasted line must be a continuation of the same draft'
            $draftText | Should -Match $continuationRowC -Because 'the third pasted line must be a continuation of the same draft'
        }
        finally {
            & $script:clearPasteDraft -Markers $markers
        }
    }
}
