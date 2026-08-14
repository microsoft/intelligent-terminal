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

BeforeDiscovery {
    $script:TriangleClickReady = [bool](
        (Get-AppxPackage | Where-Object { $_.PackageFamilyName -eq 'IntelligentTerminal_rd9vj3e6a2mbr' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: completed-turn triangle mouse click' -Tag 'CompletedTurnMouse' -Skip:(-not $script:TriangleClickReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $fixtureSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $script:fixtureDir = Join-Path $env:TEMP "ItE2E mouse triangle $([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $script:fixtureDir | Out-Null
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath $fixtureSource -Destination $fixture
        $script:fixtureLog = Join-Path $script:fixtureDir 'fixture output.log'
        $fixtureInvocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedInvocation = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureInvocation))
        $command = "pwsh -NoProfile -EncodedCommand $encodedInvocation"
        $evidencePhase = if ($env:ITE2E_MOUSE_EVIDENCE_PHASE -in @('red', 'green')) {
            $env:ITE2E_MOUSE_EVIDENCE_PHASE
        }
        else {
            'current'
        }
        $script:evidenceDir = Join-Path $PSScriptRoot "..\artifacts\mouse-interactions\$evidencePhase"
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null

        $script:app = Start-Terminal -Package 'Dev' -PassFre $true -Settings @{
            acpAgent = 'custom:chat-fixture'
            acpCustomCommand = $command
        }
        $shell = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before triangle hit-testing'
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
        if ($script:fixtureLog -and (Test-Path -LiteralPath $script:fixtureLog)) {
            Copy-Item -LiteralPath $script:fixtureLog -Destination (Join-Path $script:evidenceDir 'fixture.log') -Force
        }
        if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
            Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
        }
    }

    It 'Clicking the triangle collapses and re-expands a completed turn' {
        $id = [guid]::NewGuid().ToString('N')
        $prompt = "SCROLL_TURN_00_$id"
        $reply = "ACK_$prompt"
        $replyPattern = [regex]::Escape($reply)
        $readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $readyPattern) {
            $readyPattern = '(?i)Ask anything.*for commands'
        }

        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $prompt | Out-Null
        $turnCompleted = Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            $text -match $replyPattern -and $text -match $readyPattern
        }
        $turnCompleted | Should -BeTrue -Because 'the deterministic turn must complete before its collapsed triangle is clicked'

        $fixturePrompts = @(Get-Content -LiteralPath $script:fixtureLog | Where-Object { $_ -match ('\|prompt\|' + [regex]::Escape($prompt)) })
        $fixturePrompts.Count | Should -Be 1 -Because 'the fixture must receive the setup prompt exactly once'

        $before = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'setup-capture.txt') -Value $before -Encoding utf8NoBOM
        $lines = $before -split "`r?`n"
        $completedRowPattern = '>\s*' + [regex]::Escape($prompt)
        $promptRows = @(
            for ($row = 0; $row -lt $lines.Count; $row++) {
                if ($lines[$row] -match $completedRowPattern) {
                    [pscustomobject]@{ Row = $row; Text = $lines[$row] }
                }
            }
        )
        $promptRows.Count | Should -Be 1 -Because 'the completed-turn header must map to one visible row'
        $triangleColumn = $promptRows[0].Text.Length - $promptRows[0].Text.TrimStart().Length
        $triangleColumn | Should -BeGreaterOrEqual 0 -Because 'the first non-space cell of a completed-turn header is its triangle'
        $before | Should -Match $replyPattern -Because 'expanded turn details must start visible'
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'before-click.txt') -Value $before -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'before-click.png') | Out-Null

        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column $triangleColumn -Row $promptRows[0].Row | Out-Null

        $collapsed = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            $text -notmatch $replyPattern -and $text -match ('>\s*' + [regex]::Escape($prompt))
        }
        $after = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-click.txt') -Value $after -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-click.png') | Out-Null
        $collapsed | Should -BeTrue -Because 'clicking only the visible triangle must collapse the completed turn'

        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column $triangleColumn -Row $promptRows[0].Row | Out-Null
        $reexpanded = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            $text -match $replyPattern -and $text -match ('>\s*' + [regex]::Escape($prompt))
        }
        $reexpanded | Should -BeTrue -Because 'clicking the collapsed triangle must re-expand the same turn'
        $afterReexpand = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-reexpand.txt') -Value $afterReexpand -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-reexpand.png') | Out-Null

        $promptColumn = $triangleColumn + 4
        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column $promptColumn -Row $promptRows[0].Row | Out-Null
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) |
            Should -Match $replyPattern -Because 'clicking prompt text must not collapse the turn'

        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPane `
            -Kind Down -Column $triangleColumn -Row $promptRows[0].Row | Out-Null
        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPane `
            -Kind Drag -Column $promptColumn -Row $promptRows[0].Row | Out-Null
        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPane `
            -Kind Up -Column $triangleColumn -Row $promptRows[0].Row | Out-Null
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) |
            Should -Match $replyPattern -Because 'dragging from the triangle must remain text selection and not collapse the turn'
    }
}
