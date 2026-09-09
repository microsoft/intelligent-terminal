#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Issue #838: exercise packaged wtcli -> COM -> TerminalPage -> ControlCore.
# Exact checklist titles below cover routing and capture, not model-generated answers.

BeforeDiscovery {
    $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
}

Describe 'Feature: consolidated pane context' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            autoErrorDetectionEnabled = $true
            autoFixEnabled = $false
        }

        function Read-TestPaneContext {
            param([string]$SessionId, [int]$Lines = 1000, [int]$Characters = 100000)
            $arguments = @('get-pane-context', '--max-lines', "$Lines", '--max-chars', "$Characters")
            if ($SessionId) { $arguments += @('--target', $SessionId) }
            Invoke-WtCli -App $script:app -Arguments $arguments
        }

        $script:shell = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo -NoExit' -Title 'pane-context-shell'
        Wait-Until -TimeoutSec 30 -Because 'integrated PowerShell prompt' -Condition {
            (Read-TestPaneContext -SessionId $script:shell.session_id).pane.shell -eq 'pwsh'
        } | Out-Null

        # No profile means no OSC marks. Encoded startup output avoids input echo
        # satisfying the completion oracle and avoids argv code-page ambiguity.
        $script:tailMarker = "pc-tail-$([guid]::NewGuid().ToString('N'))"
        $script:unicode = [char]::ConvertFromUtf32(0x1F366)
        $outputScript = "[Console]::OutputEncoding = [Text.UTF8Encoding]::new(); 0..79 | ForEach-Object { 'pc-line-{0:D3}' -f `$_ }; [Console]::WriteLine('$script:tailMarker' + [char]::ConvertFromUtf32(0x1F366))"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($outputScript))
        $script:plain = New-WtTab -App $script:app -Command "pwsh.exe -NoLogo -NoProfile -NoExit -EncodedCommand $encoded" -Title 'pane-context-unmarked'
        Wait-Until -TimeoutSec 30 -Because 'unmarked startup output completed' -Condition {
            (Read-TestPaneContext -SessionId $script:plain.session_id).content.Contains($script:tailMarker + $script:unicode)
        } | Out-Null
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'Pane context captures the completed marked command' {
        $marker = "pc-error-$([guid]::NewGuid().ToString('N'))"
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id -Command "throw '$marker'" | Out-Null
            $failure = Wait-WtCommandFailure -Listener $listener -PaneId $script:shell.session_id -TimeoutSec 20
            $failure.params.tab_id | Should -Not -BeNullOrEmpty
            $context = Read-TestPaneContext -SessionId $script:shell.session_id
            $context.output_source | Should -Be 'last_command'
            $context.has_marks | Should -BeTrue
            $context.fallback_reason | Should -Be ''
            $context.content | Should -Match ([regex]::Escape("throw '$marker'"))
            ([regex]::Matches($context.content, [regex]::Escape($marker))).Count |
                Should -BeGreaterOrEqual 2 -Because 'both the command and its error output must be captured'
            $context.truncated | Should -BeFalse
            $context.pane.session_id | Should -Be $script:shell.session_id
            $context.pane.pid | Should -Be $script:shell.pid
            $context.pane.shell | Should -Be 'pwsh'
            $context.pane.cwd | Should -Not -BeNullOrEmpty
            $context.pane.size.rows | Should -BeGreaterThan 0
            $context.pane.size.columns | Should -BeGreaterThan 0
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Pane context falls back to the newest unmarked output' {
        $context = Read-TestPaneContext -SessionId $script:plain.session_id -Lines 10
        $context.output_source | Should -Be 'buffer_tail'
        $context.has_marks | Should -BeFalse
        $context.fallback_reason | Should -Be 'marks_unavailable'
        $context.content | Should -Match 'pc-line-079'
        $context.content | Should -Match ([regex]::Escape($script:tailMarker + $script:unicode))
        $context.content | Should -Not -Match 'pc-line-000'
        $context.line_count | Should -BeLessOrEqual 10
        $context.truncated | Should -BeTrue

        $blankPane = $null
        try {
            $marker = "pc-blank-$([guid]::NewGuid().ToString('N'))"
            # Clear startup text and wait for input so no shell prompt changes the tail.
            $outputScript = "[Console]::Write([string][char]27 + '[2J' + [char]27 + '[3J' + [char]27 + '[H' + [Environment]::NewLine + [Environment]::NewLine + '$marker'); [void][Console]::ReadLine()"
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($outputScript))
            $blankPane = New-WtTab -App $script:app -Command "pwsh.exe -NoLogo -NoProfile -NoExit -EncodedCommand $encoded"
            Wait-Until -TimeoutSec 30 -Because 'blank-line fixture reached its input wait' -Condition {
                (Read-TestPaneContext -SessionId $blankPane.session_id).content.EndsWith($marker)
            } | Out-Null
            $blankContext = Read-TestPaneContext -SessionId $blankPane.session_id -Lines 3 -Characters 100
            $blankContext.content | Should -Be ("`n`n" + $marker)
            $blankContext.line_count | Should -Be 3
            $blankContext.truncated | Should -BeFalse
            $blankContext.output_source | Should -Be 'buffer_tail'
            $blankContext.has_marks | Should -BeFalse
            $oneLine = Read-TestPaneContext -SessionId $blankPane.session_id -Lines 1 -Characters 100
            $oneLine.content | Should -Be $marker
            $oneLine.line_count | Should -Be 1
            $oneLine.truncated | Should -BeTrue
        }
        finally { if ($blankPane) { Close-WtPane -App $script:app -SessionId $blankPane.session_id } }
    }

    It 'Explicit pane context stays isolated from the focused tab and split' {
        $split = $null
        try {
            Set-WtPaneFocus -App $script:app -SessionId $script:plain.session_id
            $split = Split-WtPane -App $script:app -SessionId $script:plain.session_id -Direction right -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
            Set-WtPaneFocus -App $script:app -SessionId $split.session_id
            $context = Read-TestPaneContext -SessionId $script:plain.session_id
            $context.pane.session_id | Should -Be $script:plain.session_id
            $context.content | Should -Match ([regex]::Escape($script:tailMarker))
            (Read-TestPaneContext -SessionId $script:shell.session_id).pane.tab_id | Should -Be $script:shell.tab_id
            $focusedContext = Read-TestPaneContext -Lines 0
            $focusedContext.pane.session_id | Should -Be $split.session_id
            $active = Get-ActivePane -App $script:app
            $listed = @(Get-WtPanes -App $script:app -TabId $active.tab_id -WindowId $active.window_id |
                Where-Object session_id -eq $split.session_id)
            $listed.Count | Should -Be 1
            $listed[0].size.columns | Should -BeGreaterThan 0
            $listed[0].size.columns | Should -Be $focusedContext.pane.size.columns
            $listed[0].size.rows | Should -Be $focusedContext.pane.size.rows
            foreach ($field in @('session_id', 'tab_id', 'pid', 'cwd', 'shell', 'title', 'is_agent_pane')) {
                $focusedContext.pane.$field | Should -Be $active.$field
                $focusedContext.pane.$field | Should -Be $listed[0].$field
            }
        }
        finally { if ($split) { Close-WtPane -App $script:app -SessionId $split.session_id } }
    }

    It 'Missing and closed pane context fails without active-pane fallback' {
        {
            Invoke-WtCli -App $script:app -Arguments @('get-pane-context') -SkipAuthenticate
        } | Should -Throw '*requires protocol negotiation*'
        foreach ($id in @('not-a-guid', [guid]::Empty.ToString(), '')) {
            $failure = & (Get-Module ItE2E) {
                param($App, $Target)
                Invoke-Native -FilePath $App.WtcliPath -Arguments @('get-pane-context', '--target', $Target) `
                    -Environment @{ WT_COM_CLSID = $App.ComClsid }
            } $script:app $id
            $failure.TimedOut | Should -BeFalse
            $failure.ExitCode | Should -Be 1
            $failure.StdOut | Should -BeNullOrEmpty
            ([regex]::Matches($failure.StdErr, '\[wtcli\] Invalid session ID:')).Count | Should -Be 1
        }
        $gone = New-WtTab -App $script:app -Command 'pwsh.exe -NoLogo -NoProfile -NoExit'
        Close-WtPane -App $script:app -SessionId $gone.session_id
        foreach ($id in @($gone.session_id, [guid]::NewGuid().ToString())) {
            { Read-TestPaneContext -SessionId $id } | Should -Throw
        }
        (Read-TestPaneContext -SessionId $script:plain.session_id).pane.session_id | Should -Be $script:plain.session_id
    }

    It 'Pane context metadata-only requests omit terminal content' {
        foreach ($limits in @(@(0, 100), @(10, 0))) {
            $context = Read-TestPaneContext -SessionId $script:plain.session_id -Lines $limits[0] -Characters $limits[1]
            $context.output_source | Should -Be 'metadata_only'
            $context.content | Should -Be ''
            $context.line_count | Should -Be 0
            $context.truncated | Should -BeFalse
            $context.pane.session_id | Should -Be $script:plain.session_id
            $context.pane.pid | Should -Be $script:plain.pid
            $context.pane.size.rows | Should -BeGreaterThan 0
            $context.pane.size.columns | Should -BeGreaterThan 0
        }
    }

    It 'Pane context bounds preserve Unicode and truthful truncation' {
        $full = Read-TestPaneContext -SessionId $script:plain.session_id
        $full.truncated | Should -BeFalse
        $full.content | Should -Match ([regex]::Escape($script:unicode))
        # Vary the boundary across a supplementary character, accounting for
        # UTF-16 surrogate pairs when comparing the protocol's character budget.
        $tailLength = $full.content.Length - $full.content.LastIndexOf($script:unicode)
        foreach ($limit in ($tailLength - 2)..($tailLength + 2)) {
            $context = Read-TestPaneContext -SessionId $script:plain.session_id -Characters $limit
            $context.truncated | Should -BeTrue
            $context.content | Should -Not -Match "\uFFFD|[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]"
            [regex]::Matches($context.content, '[\uD800-\uDBFF][\uDC00-\uDFFF]|[\s\S]').Count | Should -BeLessOrEqual $limit
            $full.content.EndsWith($context.content) | Should -BeTrue
        }
        $command = "0..49 | ForEach-Object { 'pc-marked-{0:D3}' -f `$_ }"
        Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id -Command $command | Out-Null
        $completed = Wait-Until -TimeoutSec 30 -Because 'marked output includes its last line' -Condition {
            $context = Read-TestPaneContext -SessionId $script:shell.session_id
            if ($context.output_source -eq 'last_command' -and $context.content.Contains('pc-marked-049')) { $context }
        }
        $newlineOffset = $completed.content.IndexOf("`n")
        $newlineOffset | Should -BeGreaterThan 0
        $atNewline = Read-TestPaneContext -SessionId $script:shell.session_id -Characters $newlineOffset
        $atNewline.content | Should -Be $completed.content.Substring(0, $newlineOffset)
        $atNewline.truncated | Should -BeTrue -Because 'a lookahead newline must not be discarded as EOF'
        $marked = Read-TestPaneContext -SessionId $script:shell.session_id -Lines 2 -Characters 100
        $marked.output_source | Should -Be 'last_command'
        $marked.truncated | Should -BeTrue
        $marked.line_count | Should -BeLessOrEqual 2
        $marked.content.Length | Should -BeLessOrEqual 100
        $marked.content.StartsWith($command) | Should -BeTrue
    }

    It 'Focused agent pane context resolves to its source terminal' {
        Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
        Open-AgentPane -App $script:app | Out-Null
        try {
            $agent = Wait-Until -TimeoutSec 30 -Because 'source tab helper identity' -Condition {
                Get-AgentPaneSession -App $script:app -OwnerPaneSessionId $script:shell.session_id
            }
            Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $agent.PaneSessionId) | Out-Null
            $context = Read-TestPaneContext
            $context.pane.session_id | Should -Be $script:shell.session_id
            $context.pane.is_agent_pane | Should -BeFalse
            $context.pane.session_id | Should -Not -Be $agent.PaneSessionId
            { Read-TestPaneContext -SessionId $agent.PaneSessionId } | Should -Throw
        }
        finally { Stop-AgentPane -App $script:app | Out-Null }
    }
}
