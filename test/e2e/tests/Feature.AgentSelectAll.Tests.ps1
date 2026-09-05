#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Plain Ctrl+A must select the WTA-owned current rendered frame so the existing
# selection copy path can copy it without invoking TerminalControl scrollback selection.

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
        $fixtureSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $script:fixtureDir = Join-Path $env:TEMP "ItE2E agent select all $([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $script:fixtureDir | Out-Null
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath $fixtureSource -Destination $fixture
        $script:fixtureLog = Join-Path $script:fixtureDir 'fixture output.log'
        $fixtureInvocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureInvocation))
        $command = "pwsh -NoProfile -EncodedCommand $encodedInvocation"
        $evidencePhase = if ($env:ITE2E_SELECT_ALL_EVIDENCE_PHASE -in @('red', 'green', 'publish')) {
            $env:ITE2E_SELECT_ALL_EVIDENCE_PHASE
        }
        else {
            'current'
        }
        $script:evidenceDir = Join-Path $PSScriptRoot "..\artifacts\agent-select-all\$evidencePhase"
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null
        $script:originalClipboard = Get-ClipboardSnapshot

        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:chat-fixture'
            acpCustomCommand = $command
            'warning.confirmOnClose' = 'never'
        }
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before select-all input is sent'
    }

    AfterAll {
        try {
            if ($script:app) {
                Stop-Terminal -App $script:app
            }
        }
        finally {
            try {
                Restore-ClipboardSnapshot -Snapshot $script:originalClipboard
            }
            finally {
                try {
                    if ($script:fixtureLog -and (Test-Path -LiteralPath $script:fixtureLog)) {
                        Copy-Item -LiteralPath $script:fixtureLog -Destination (Join-Path $script:evidenceDir 'fixture.log') -Force
                    }
                }
                finally {
                    if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
                        Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
                    }
                }
            }
        }
    }

    It 'Ctrl+A selects and copies the current agent frame' {
        $id = [guid]::NewGuid().ToString('N')
        $draft = "SELECT_ALL_DRAFT_$id"
        $readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $readyPattern) {
            $readyPattern = '(?i)Ask anything.*for commands'
        }

        $hintDraft = "SELECT_ALL_HINT_DRAFT_$id"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $hintDraft -NoSubmit | Out-Null
        $hintSentinel = "SELECT_ALL_HINT_SENTINEL_$id"
        Set-Clipboard -Value $hintSentinel
        Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x41 -Sc 0x1E -Uc 1 -Modifiers 0x08 | Out-Null
        Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        $hintCopy = Wait-Until -TimeoutSec 5 -IntervalSec 0.25 -Quiet -Condition {
            $value = Get-Clipboard -Raw
            if ($value -ne $hintSentinel) { $value }
        }
        $hintCopy | Should -Match ([regex]::Escape($hintDraft)) -Because 'the sparse-frame select-all must use the existing clipboard path'
        $copiedPattern = Get-WtaLocalizedTextRegex -Key 'system.selection_copied'
        if (-not $copiedPattern) { $copiedPattern = '(?i)Copied' }
        $afterCopy = Wait-Until -TimeoutSec 5 -IntervalSec 0.25 -Quiet -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            if ($text -match $copiedPattern) { $text }
        }
        $afterCopy | Should -Not -BeNullOrEmpty -Because 'select-all copy must reuse the existing WTA copied confirmation'
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-copy.txt') -Value $afterCopy -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-copy.png') | Out-Null
        $hintClearedSentinel = "SELECT_ALL_HINT_CLEARED_$id"
        Set-Clipboard -Value $hintClearedSentinel
        Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        (Get-Clipboard -Raw) | Should -Be $hintClearedSentinel -Because 'the sparse-frame copy must clear its selection'
        Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30) -notmatch
                ('(?m)^.*>\s*' + [regex]::Escape($hintDraft))
        } | Should -BeTrue -Because 'the key after sparse-frame copy must resume existing draft clearing'

        $offscreenMarker = "SCROLL_TURN_00_$id"
        $turnMarker = $null
        $turnCount = 0
        foreach ($index in 0..15) {
            $marker = 'SCROLL_TURN_{0:D2}_{1}' -f $index, $id
            $markerReply = "ACK_$marker"
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $marker | Out-Null
            Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
                $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
                $text -match [regex]::Escape($markerReply) -and $text -match $readyPattern
            } | Should -BeTrue -Because "deterministic turn $index must complete before select-all"
            $turnCount++
            $frame = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            if ($index -gt 0 -and
                $frame -notmatch [regex]::Escape($offscreenMarker) -and
                $frame -match [regex]::Escape($markerReply)) {
                $turnMarker = $marker
                break
            }
        }
        $turnMarker | Should -Not -BeNullOrEmpty -Because 'bounded deterministic turns must move the oldest marker outside the current rendered frame'
        $reply = "ACK_$turnMarker"

        $fixturePrompts = @(Get-Content -LiteralPath $script:fixtureLog | Where-Object { $_ -match '\|prompt\|SCROLL_TURN_' })
        $fixturePrompts.Count | Should -Be $turnCount -Because 'the fixture must receive every setup prompt exactly once'

        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draft -NoSubmit | Out-Null
        $before = Wait-Until -TimeoutSec 5 -IntervalSec 0.25 -Quiet -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            if ($text -match [regex]::Escape($turnMarker) -and
                $text -match [regex]::Escape($reply) -and
                $text -notmatch [regex]::Escape($offscreenMarker) -and
                $text -match ('(?m)^.*>\s*' + [regex]::Escape($draft))) {
                $text
            }
        }
        $before | Should -Not -BeNullOrEmpty -Because 'the current frame must contain completed-turn and draft markers before select-all'
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'before-select-all.txt') -Value $before -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'before-select-all.png') | Out-Null

        $sentinel = "SELECT_ALL_SENTINEL_$id"
        Set-Clipboard -Value $sentinel
        Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x41 -Sc 0x1E -Uc 1 -Modifiers 0x08 | Out-Null

        $selected = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        $selected | Should -Match ([regex]::Escape($draft)) -Because 'Ctrl+A must not alter or submit the current draft'
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-select-all.txt') -Value $selected -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-select-all.png') | Out-Null

        Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        $copied = Wait-Until -TimeoutSec 5 -IntervalSec 0.25 -Quiet -Condition {
            $value = Get-Clipboard -Raw
            if ($value -ne $sentinel) { $value }
        }
        $copied | Should -Not -BeNullOrEmpty -Because 'Ctrl+C must replace the sentinel with the selected WTA frame'
        $copied | Should -Match ([regex]::Escape($turnMarker)) -Because 'the copied frame must include the visible completed-turn prompt'
        $copied | Should -Match ([regex]::Escape($reply)) -Because 'the copied frame must include the visible deterministic reply'
        $copied | Should -Match ([regex]::Escape($draft)) -Because 'the copied frame must include the visible unsubmitted draft'
        $copied | Should -Not -Match ([regex]::Escape($offscreenMarker)) -Because 'select-all must copy only the current rendered WTA frame, not virtual chat history or native scrollback'
        $afterCurrentFrameCopy = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-current-frame-copy.txt') -Value $afterCurrentFrameCopy -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-current-frame-copy.png') | Out-Null

        $clearedSentinel = "SELECT_ALL_CLEARED_$id"
        Set-Clipboard -Value $clearedSentinel
        Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        (Get-Clipboard -Raw) | Should -Be $clearedSentinel -Because 'copy must clear selection so a later Ctrl+C cannot replay stale frame text'
        Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30) -notmatch
                ('(?m)^.*>\s*' + [regex]::Escape($draft))
        } | Should -BeTrue -Because 'the later Ctrl+C must resume existing nonempty-draft clearing behavior'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-stale-selection-control.png') | Out-Null
    }
}
