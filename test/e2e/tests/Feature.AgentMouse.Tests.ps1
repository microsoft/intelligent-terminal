#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #506: mouse input crosses WT/ConPTY into WTA's crossterm event reader.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent pane mouse interactions' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 |
            Should -BeTrue -Because 'the agent pane must be connected before exercising its TUI'
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
    }

    BeforeEach {
        Clear-AgentInput -App $script:app | Out-Null
        # One Ctrl+C is safe on an empty input (it only arms pane close), and clears any
        # draft or in-flight turn left by an earlier failed case. Typing below disarms it.
        Send-AgentWin32Key -App $script:app -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
    }

    It 'Mouse wheel scrolls chat without changing the draft' {
        $id = [guid]::NewGuid().ToString('N')
        $topMarker = "MOUSE_SCROLL_TOP_$id"
        $bottomMarker = "MOUSE_SCROLL_BOTTOM_$id"
        $session = Get-AgentPaneSession -App $script:app
        $viewportLines = @((
            Get-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -MaxLines 500
        ) -split "`r?`n")
        $visibleRows = [Math]::Max(1, $viewportLines.Count)
        $visibleColumns = [Math]::Max(
            1,
            [int](($viewportLines | ForEach-Object Length | Measure-Object -Maximum).Maximum)
        )
        # Fill more cells than the measured viewport can display, so this remains
        # deterministic across pane positions, window sizes, and display scales.
        $fillerCount = [Math]::Ceiling(($visibleRows * $visibleColumns * 2) / 'SCROLL_FILLER '.Length)
        $longPrompt = "$topMarker $(('SCROLL_FILLER ' * $fillerCount).Trim()) $bottomMarker"
        Send-AgentPrompt -App $script:app -PaneSessionId $session.PaneSessionId -Text $longPrompt | Out-Null
        $submitted = Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -MaxLines 100
            $stillInInput = $text -match ('(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($topMarker))
            -not $stillInInput -and (
                $text -match [regex]::Escape($topMarker) -or
                $text -match [regex]::Escape($bottomMarker)
            )
        }
        $submitted | Should -BeTrue -Because 'the long prompt must reach the real chat transcript'

        $before = Get-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -MaxLines 100
        $topVisible = $before -match [regex]::Escape($topMarker)
        $bottomVisible = $before -match [regex]::Escape($bottomMarker)
        ($topVisible -xor $bottomVisible) | Should -BeTrue -Because 'the long prompt must overflow the chat viewport with exactly one end visible'
        $scrollKind = if ($topVisible) { 'ScrollDown' } else { 'ScrollUp' }
        $targetMarker = if ($topVisible) { $bottomMarker } else { $topMarker }

        $draft = "MOUSE_SCROLL_DRAFT_$id"
        Send-AgentPrompt -App $script:app -PaneSessionId $session.PaneSessionId -Text $draft -NoSubmit | Out-Null
        Send-AgentMouseEvent -App $script:app -PaneSessionId $session.PaneSessionId -Kind $scrollKind -Count 12 | Out-Null

        $scrolled = Wait-Until -TimeoutSec 8 -IntervalSec 0.25 -Quiet -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -MaxLines 100
            if ($text -match [regex]::Escape($targetMarker)) { $text }
        }
        $scrolled | Should -Not -BeNullOrEmpty -Because 'mouse-wheel events must move the WTA chat viewport to the hidden end'
        $scrolled | Should -Match ('(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($draft)) -Because 'scrolling chat must not alter the current input draft'

        Send-AgentWin32Key -App $script:app -PaneSessionId $session.PaneSessionId -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        Start-Sleep -Milliseconds 500
        Send-AgentWin32Key -App $script:app -PaneSessionId $session.PaneSessionId -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
    }

    It 'Mouse selection copies text and clears after copy' {
        $marker = "MOUSE_COPY_$([guid]::NewGuid().ToString('N'))"
        $session = Send-AgentPrompt -App $script:app -Text $marker -NoSubmit
        Start-Sleep -Milliseconds 300

        $capture = Get-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -MaxLines 200
        $lines = $capture -split "`r?`n"
        $hits = @(
            for ($row = 0; $row -lt $lines.Count; $row++) {
                $column = $lines[$row].IndexOf($marker)
                if ($column -ge 0) {
                    [pscustomobject]@{ Row = $row; Column = $column }
                }
            }
        )
        $hits.Count | Should -Be 1 -Because 'the unique draft word must map to one deterministic TUI cell range'

        Set-Clipboard -Value 'mouse-copy-sentinel'
        Send-AgentMouseClick -App $script:app -PaneSessionId $session.PaneSessionId `
            -Column $hits[0].Column -Row $hits[0].Row -Count 2 | Out-Null
        Send-AgentWin32Key -App $script:app -PaneSessionId $session.PaneSessionId -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null

        (Get-Clipboard -Raw) | Should -Be $marker -Because 'Ctrl+C must copy the WTA mouse selection through the OS clipboard'
        $copiedPattern = Get-WtaLocalizedTextRegex -Key 'system.selection_copied'
        if (-not $copiedPattern) { $copiedPattern = '(?i)Copied' }
        Assert-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -Pattern $copiedPattern -TimeoutSec 5

        $sentinel = "MOUSE_COPY_CLEARED_$([guid]::NewGuid().ToString('N'))"
        Set-Clipboard -Value $sentinel
        Send-AgentWin32Key -App $script:app -PaneSessionId $session.PaneSessionId -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
        (Get-Clipboard -Raw) | Should -Be $sentinel -Because 'copy must clear the selection so Ctrl+C cannot replay stale text'
        (Get-AgentPaneText -App $script:app -PaneSessionId $session.PaneSessionId -MaxLines 30) |
            Should -Not -Match ('(?m)^\s*[│║|]\s*>\s*' + [regex]::Escape($marker)) -Because 'the next Ctrl+C must resume the normal nonempty-draft clear behavior'
    }
}
