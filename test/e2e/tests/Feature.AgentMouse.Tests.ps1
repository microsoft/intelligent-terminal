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
        $evidencePhase = if ($env:ITE2E_MOUSE_EVIDENCE_PHASE -in @('red', 'green', 'extension-red', 'extension-green')) {
            $env:ITE2E_MOUSE_EVIDENCE_PHASE
        }
        else {
            'current'
        }
        $script:evidenceDir = Join-Path $PSScriptRoot "..\artifacts\mouse-interactions\$evidencePhase"
        New-Item -ItemType Directory -Force -Path $script:evidenceDir | Out-Null
        $script:originalClipboard = Get-Clipboard -Raw -ErrorAction SilentlyContinue

        $script:app = Start-Terminal -Package 'Dev' -PassFre $true -Settings @{
            acpAgent = 'custom:chat-fixture'
            acpCustomCommand = $command
            rightClickContextMenu = $false
            'warning.confirmOnClose' = 'never'
        }
        $shell = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $agentSession = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30
        $script:agentPane = $agentSession.PaneSessionId
        $helper = Get-CimInstance Win32_Process -Filter "ProcessId=$($agentSession.HelperProcessId)"
        if (-not $helper.CommandLine -or $helper.CommandLine -notmatch '--owner-tab-id\s+"?\{?(?<tab>[0-9a-fA-F-]{36})') {
            throw 'Could not resolve the helper owner tab identity from its process command line.'
        }
        $script:ownerTabId = $Matches.tab
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before triangle hit-testing'
        $script:getMouseInputSegment = {
            param([string]$Text)

            $lines = @($Text -split "`r?`n")
            $inputStart = -1
            for ($index = 0; $index -lt $lines.Count; $index++) {
                if ($lines[$index] -match '>\s*') { $inputStart = $index }
            }
            if ($inputStart -lt 0) { return '' }
            $lines[$inputStart..($lines.Count - 1)] -join "`n"
        }
        $script:clearMouseDraft = {
            param([string]$ExpectedMarker)

            if (Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 1) { return $true }
            if ([string]::IsNullOrEmpty($ExpectedMarker)) { return $false }
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30
            $input = & $script:getMouseInputSegment -Text $text
            if (([regex]::Matches($input, [regex]::Escape($ExpectedMarker))).Count -ne 1) {
                return $false
            }
            Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
            Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 5
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
        if ($null -ne $script:originalClipboard) {
            Set-Clipboard -Value $script:originalClipboard
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

        $prefixColumn = $triangleColumn + 2
        $promptColumn = $triangleColumn + 4
        $rowEndColumn = $promptRows[0].Text.IndexOf($prompt) + $prompt.Length + 8
        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column $rowEndColumn -Row $promptRows[0].Row | Out-Null
        $rowEndCollapsed = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -notmatch $replyPattern
        }
        $rowEndCollapsed | Should -BeTrue -Because 'clicking unused space at the end of a prompt row must collapse the turn'

        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column $prefixColumn -Row $promptRows[0].Row | Out-Null
        $prefixReexpanded = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -match $replyPattern
        }
        $prefixReexpanded | Should -BeTrue -Because 'clicking the prompt prefix must re-expand the same turn'

        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPane `
            -Kind Down -Column $triangleColumn -Row $promptRows[0].Row | Out-Null
        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPane `
            -Kind Drag -Column $promptColumn -Row $promptRows[0].Row | Out-Null
        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPane `
            -Kind Up -Column $triangleColumn -Row $promptRows[0].Row | Out-Null
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) |
            Should -Match $replyPattern -Because 'dragging from the triangle must remain text selection and not collapse the turn'
    }

    It 'Clicking a multiline prompt row selects and collapses its completed turn' -Tag 'CompletedTurnPromptMouse' {
        $id = [guid]::NewGuid().ToString('N')
        $lineOne = "SCROLL_TURN_00_$id"
        $lineTwo = "MOUSE_INPUT_SECOND_$id"
        $reply = "ACK_$lineOne"
        $replyPattern = [regex]::Escape($reply)
        $readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $readyPattern) {
            $readyPattern = '(?i)Ask anything.*for commands'
        }

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Escape | Out-Null

        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $lineOne -NoSubmit | Out-Null
        Send-AgentShiftEnter -App $script:app -PaneSessionId $script:agentPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $lineTwo -NoSubmit | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null

        $turnCompleted = Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            $text -match $replyPattern -and $text -match $readyPattern
        }
        $turnCompleted | Should -BeTrue -Because 'the multiline deterministic turn must complete before prompt hit-testing'

        $fixturePrompts = @(Get-Content -LiteralPath $script:fixtureLog | Where-Object { $_ -match ('\|prompt\|' + [regex]::Escape($lineOne)) })
        $fixturePrompts.Count | Should -Be 1 -Because 'the fixture must receive the multiline prompt exactly once'

        $before = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        $lines = $before -split "`r?`n"
        $firstRows = @(
            for ($row = 0; $row -lt $lines.Count; $row++) {
                if ($lines[$row] -match ('>\s*' + [regex]::Escape($lineOne))) {
                    [pscustomobject]@{ Row = $row; Column = $lines[$row].IndexOf($lineOne) }
                }
            }
        )
        $secondRows = @(
            for ($row = 0; $row -lt $lines.Count; $row++) {
                if ($lines[$row].Contains($lineTwo)) {
                    [pscustomobject]@{ Row = $row; Column = $lines[$row].IndexOf($lineTwo) }
                }
            }
        )
        $firstRows.Count | Should -Be 1 -Because 'the first prompt line must map to one completed-turn row'
        $secondRows.Count | Should -Be 1 -Because 'the second prompt line must map to one completed-turn row'
        $secondRows[0].Row | Should -BeGreaterThan $firstRows[0].Row -Because 'the prompt must remain visibly multiline after completion'
        $before | Should -Match $replyPattern -Because 'expanded details must be visible before prompt click'
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'before-prompt-click.txt') -Value $before -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'before-prompt-click.png') | Out-Null

        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column ($secondRows[0].Column + 2) -Row $secondRows[0].Row | Out-Null

        $collapsed = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -notmatch $replyPattern
        }
        $after = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-prompt-click.txt') -Value $after -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-prompt-click.png') | Out-Null
        $collapsed | Should -BeTrue -Because 'clicking the second rendered prompt line must collapse the completed turn'

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null
        $reexpanded = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -match $replyPattern
        }
        $afterEnter = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-prompt-enter.txt') -Value $afterEnter -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-prompt-enter.png') | Out-Null
        $reexpanded | Should -BeTrue -Because 'Enter must re-expand the completed turn selected by its prompt click'

        $afterEnterLines = $afterEnter -split "`r?`n"
        $reexpandedFirstRow = @(
            for ($row = 0; $row -lt $afterEnterLines.Count; $row++) {
                if ($afterEnterLines[$row] -match ('>\s*' + [regex]::Escape($lineOne))) {
                    [pscustomobject]@{ Row = $row; Text = $afterEnterLines[$row] }
                }
            }
        )
        $reexpandedFirstRow.Count | Should -Be 1
        $rowEndColumn = $reexpandedFirstRow[0].Text.IndexOf($lineOne) + $lineOne.Length + 8
        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane `
            -Column $rowEndColumn -Row $reexpandedFirstRow[0].Row | Out-Null
        $collapsedAgain = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -notmatch $replyPattern
        }
        $collapsedAgain | Should -BeTrue -Because 'row-end whitespace must select and collapse the turn before input focus recovery'

        $collapsedCapture = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        $collapsedLines = $collapsedCapture -split "`r?`n"
        $inputRows = @(
            for ($row = 0; $row -lt $collapsedLines.Count; $row++) {
                if ($collapsedLines[$row] -match $readyPattern) {
                    [pscustomobject]@{ Row = $row; Text = $collapsedLines[$row] }
                }
            }
        )
        $inputRows.Count | Should -Be 1 -Because 'the connected input dialog must expose one visible placeholder row'
        Send-AgentMouseClick -App $script:app -PaneSessionId $script:agentPane -Column 8 -Row $inputRows[0].Row | Out-Null

        $draft = "INPUT_FOCUS_DRAFT_$id"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $draft -NoSubmit | Out-Null
        $inputFocused = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            $text -match ('(?m)^.*>\s*' + [regex]::Escape($draft)) -and $text -notmatch $replyPattern
        }
        $afterInputFocus = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'after-input-focus.txt') -Value $afterInputFocus -Encoding utf8NoBOM
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'after-input-focus.png') | Out-Null
        $inputFocused | Should -BeTrue -Because 'clicking the input dialog must clear completed-turn selection and route typing to the current draft'
    }

    It 'Right-click copies the agent text selection with physical mouse input' -Tag 'RightClickCopy' {
        $originalClipboard = Get-Clipboard -Raw -ErrorAction SilentlyContinue
        $id = [guid]::NewGuid().ToString('N')
        $prompt = "SCROLL_TURN_00_$id"
        $reply = "ACK_$prompt"
        $replyPattern = [regex]::Escape($reply)
        $rightClickEvidenceDir = Join-Path $PSScriptRoot '..\artifacts\right-click-copy\green'
        New-Item -ItemType Directory -Force -Path $rightClickEvidenceDir | Out-Null

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Escape | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $prompt | Out-Null
        Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -match $replyPattern
        } | Should -BeTrue -Because 'the deterministic reply must render before physical selection'

        $rectangles = @(Get-UiTextBounds -App $script:app -Text $reply)
        $rectangles.Count | Should -Be 1 -Because 'the unique reply marker must expose one UIA text rectangle'
        $rect = $rectangles[0]
        $cellWidth = $rect.Width / $reply.Length
        $fromX = [Math]::Round($rect.Right - ($cellWidth / 2))
        $toX = [Math]::Round($rect.Left + ($cellWidth / 2))
        $y = [Math]::Round($rect.Top + ($rect.Height / 2))

        Save-UiScreenshot -App $script:app -Path (Join-Path $rightClickEvidenceDir 'before-right-click-selection.png') | Out-Null
        try {
            Invoke-UiMouseDrag -App $script:app -FromX $fromX -FromY $y -ToX $toX -ToY $y | Out-Null
        }
        catch {
            if ($_.Exception.Message -match 'No interactive desktop is available') {
                Set-ItResult -Skipped -Because 'physical mouse injection requires an unlocked interactive desktop'
                return
            }
            throw
        }
        Start-Sleep -Milliseconds 300
        Save-UiScreenshot -App $script:app -Path (Join-Path $rightClickEvidenceDir 'selected-before-right-click.png') | Out-Null

        $pasteMarker = $null
        try {
            Set-Clipboard -Value "RIGHT_CLICK_SETUP_$id"
            Send-AgentWin32Key -App $script:app -PaneSessionId $script:agentPane -Vk 0x43 -Sc 0x2E -Uc 3 -Modifiers 0x08 | Out-Null
            (Get-Clipboard -Raw) | Should -Be $reply -Because 'Ctrl+C must prove the physical selection geometry before testing right-click'
            $copiedPattern = Get-WtaLocalizedTextRegex -Key 'system.selection_copied'
            if (-not $copiedPattern) { $copiedPattern = '(?i)Copied' }
            Start-Sleep -Milliseconds 1800
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) |
                Should -Not -Match $copiedPattern -Because 'the Ctrl+C confirmation must expire before right-click tests its own hint'

            Invoke-UiMouseDrag -App $script:app -FromX $fromX -FromY $y -ToX $toX -ToY $y | Out-Null
            Start-Sleep -Milliseconds 300
            $sentinel = "RIGHT_CLICK_SENTINEL_$id"
            Set-Clipboard -Value $sentinel
            $clickX = [Math]::Round(($fromX + $toX) / 2)
            $copyListener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Invoke-UiMouseDrag -App $script:app -FromX $clickX -FromY $y -ToX $clickX -ToY $y -Right | Out-Null
                Start-Sleep -Milliseconds 300

                (Get-Clipboard -Raw) | Should -Be $reply -Because 'right-click must copy the exact physical WTA selection'
                @(Get-WtEvents -Listener $copyListener -Predicate { $_.method -eq 'agent_paste_text' }).Count |
                    Should -Be 0 -Because 'an actual text selection must copy without also requesting Default Paste'
                Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -Pattern $copiedPattern -TimeoutSec 5
                Save-UiScreenshot -App $script:app -Path (Join-Path $rightClickEvidenceDir 'after-right-click-copy.png') | Out-Null
            }
            finally {
                Stop-WtEventListener -Listener $copyListener
            }

            $pasteMarker = "RIGHT_CLICK_AFTER_COPY_$id"
            Set-Clipboard -Value $pasteMarker
            $pasteListener = Start-WtEventListener -App $script:app
            $secondPasteObserved = $false
            try {
                Start-Sleep -Milliseconds 400
                Invoke-UiMouseDrag -App $script:app -FromX $clickX -FromY $y -ToX $clickX -ToY $y -Right | Out-Null
                $pasteEvent = Wait-WtEvent -Listener $pasteListener -TimeoutSec 5 -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                }
                $pasteEvent | Should -Not -BeNullOrEmpty -Because 'after copy clears the selection, the next right-click must request Default Paste'
                Start-Sleep -Milliseconds 300
                @(Get-WtEvents -Listener $pasteListener -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                }).Count | Should -Be 1 -Because 'one physical Right Down must request Default Paste exactly once'
                $secondPasteObserved = Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
                    $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 30
                    ([regex]::Matches($text, [regex]::Escape($pasteMarker))).Count -eq 1
                }
                $secondPasteObserved | Should -BeTrue -Because 'the second right-click must paste rather than replay stale selected text'
                (Get-Clipboard -Raw) | Should -Be $pasteMarker
                Save-UiScreenshot -App $script:app -Path (Join-Path $rightClickEvidenceDir 'after-second-right-click.png') | Out-Null
                $markerToClear = $pasteMarker
                $pasteMarker = $null
                (& $script:clearMouseDraft -ExpectedMarker $markerToClear) |
                    Should -BeTrue -Because 'the known pasted marker must be cleared with one Ctrl+C'
            }
            finally {
                Stop-WtEventListener -Listener $pasteListener
            }
        }
        finally {
            if ($pasteMarker) {
                $markerToClear = $pasteMarker
                $pasteMarker = $null
                $null = & $script:clearMouseDraft -ExpectedMarker $markerToClear
            }
            if ($null -ne $originalClipboard) { Set-Clipboard -Value $originalClipboard }
        }
    }

    It 'Right-click without text selection pastes throughout the Chat pane' -Tag 'RightClickPaste' {
        $id = [guid]::NewGuid().ToString('N')
        $prompt = "SCROLL_TURN_00_$id"
        $reply = "ACK_$prompt"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text $prompt | Out-Null
        Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -match [regex]::Escape($reply)
        } | Should -BeTrue
        $promptCountBefore = @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count

        $historyRect = @(Get-UiTextBounds -App $script:app -Text $reply)[0]
        $promptRect = @(Get-UiTextBounds -App $script:app -Text $prompt)[0]
        $inputRect = @(Get-UiTextBounds -App $script:app -Text 'Ask anything, / for commands..')[0]
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$script:app.Hwnd)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            'Agent Pane')
        $agentControl = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) |
            Where-Object { $_.Current.ClassName -eq 'TermControl' } |
            Select-Object -First 1
        $agentControl | Should -Not -BeNullOrEmpty
        $paneRect = $agentControl.Current.BoundingRectangle

        $points = @(
            [pscustomobject]@{ Name = 'history'; X = [Math]::Round($historyRect.Left + ($historyRect.Width / 2)); Y = [Math]::Round($historyRect.Top + ($historyRect.Height / 2)) }
            [pscustomobject]@{ Name = 'input'; X = [Math]::Round($inputRect.Left + ($inputRect.Width / 2)); Y = [Math]::Round($inputRect.Top + ($inputRect.Height / 2)) }
            [pscustomobject]@{ Name = 'blank'; X = [Math]::Round($paneRect.Right - 24); Y = [Math]::Round($historyRect.Top + ($historyRect.Height / 2)) }
        )

        foreach ($point in $points) {
            $marker = "RIGHT_CLICK_PASTE_$($point.Name)_$([guid]::NewGuid().ToString('N'))"
            Set-Clipboard -Value $marker
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                try {
                    Invoke-UiMouseDrag -App $script:app -FromX $point.X -FromY $point.Y -ToX $point.X -ToY $point.Y -Right | Out-Null
                }
                catch {
                    if ($_.Exception.Message -match 'No interactive desktop is available') {
                        Set-ItResult -Skipped -Because 'physical mouse injection requires an unlocked interactive desktop'
                        return
                    }
                    throw
                }
                Start-Sleep -Milliseconds 1500
                $allEvents = @(Get-WtEvents -Listener $listener)
                $allEvents | ConvertTo-Json -Depth 64 |
                    Set-Content -LiteralPath (Join-Path $script:evidenceDir "right-click-paste-$($point.Name)-events.json") -Encoding utf8NoBOM
                $matchingEvents = @($allEvents | Where-Object {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                })
                $observed = @($allEvents | ForEach-Object {
                    "$($_.method):window=$($_.params.window_id),tab=$($_.params.tab_id),pane=$($_.params.pane_id)"
                }) -join '; '
                $matchingEvents.Count | Should -Be 1 -Because "$($point.Name) Right Down must request owner-scoped Default Paste exactly once; observed [$observed]"
                Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
                    $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
                    $input = & $script:getMouseInputSegment -Text $text
                    ([regex]::Matches($input, [regex]::Escape($marker))).Count -eq 1
                } | Should -BeTrue -Because "$($point.Name) right-click paste must enter the current draft exactly once"
                @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count |
                    Should -Be $promptCountBefore -Because 'right-click paste must not submit the draft'
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir "right-click-paste-$($point.Name).png") | Out-Null
            }
            finally {
                Stop-WtEventListener -Listener $listener
            }
            (& $script:clearMouseDraft -ExpectedMarker $marker) |
                Should -BeTrue -Because 'the known pasted marker must be cleared with one Ctrl+C'
        }

        $promptX = [Math]::Round($promptRect.Left + ($promptRect.Width / 2))
        $promptY = [Math]::Round($promptRect.Top + ($promptRect.Height / 2))
        Invoke-UiMouseDrag -App $script:app -FromX $promptX -FromY $promptY -ToX $promptX -ToY $promptY | Out-Null
        Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100) -notmatch [regex]::Escape($reply)
        } | Should -BeTrue -Because 'physical title click must establish completed-turn navigation highlight'

        $marker = "RIGHT_CLICK_PASTE_highlight_$([guid]::NewGuid().ToString('N'))"
        Set-Clipboard -Value $marker
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Milliseconds 400
            try {
                Invoke-UiMouseDrag -App $script:app -FromX $promptX -FromY $promptY -ToX $promptX -ToY $promptY -Right | Out-Null
            }
            catch {
                if ($_.Exception.Message -match 'No interactive desktop is available') {
                    Set-ItResult -Skipped -Because 'physical mouse injection requires an unlocked interactive desktop'
                    return
                }
                throw
            }
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 5 -Predicate {
                $_.method -eq 'agent_paste_text' -and
                "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                "$($_.params.window_id)" -eq "$($script:app.WindowId)"
            }
            $event | Should -Not -BeNullOrEmpty
            Start-Sleep -Milliseconds 300
            @(Get-WtEvents -Listener $listener -Predicate {
                $_.method -eq 'agent_paste_text' -and
                "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                "$($_.params.window_id)" -eq "$($script:app.WindowId)"
            }).Count | Should -Be 1 -Because 'one physical Right Down on a navigation highlight must request Default Paste exactly once'
            Test-Until -TimeoutSec 5 -IntervalSec 0.25 -Condition {
                $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
                $input = & $script:getMouseInputSegment -Text $text
                ([regex]::Matches($input, [regex]::Escape($marker))).Count -eq 1
            } | Should -BeTrue -Because 'navigation highlight is not selected text and must clear before paste'
            @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue | Where-Object { $_ -match '\|prompt\|' }).Count |
                Should -Be $promptCountBefore -Because 'right-click paste from a navigation highlight must not submit'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDir 'right-click-paste-completed-highlight.png') | Out-Null
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
        (& $script:clearMouseDraft -ExpectedMarker $marker) |
            Should -BeTrue -Because 'the known highlighted-turn paste marker must be cleared with one Ctrl+C'
    }
}
