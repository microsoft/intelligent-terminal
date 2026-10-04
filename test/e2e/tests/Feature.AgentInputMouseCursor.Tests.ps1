#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Physical mouse/keyboard -> TerminalControl/ConPTY -> draft caret -> exact clipboard source.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent input mouse cursor' -Tag 'Feature', 'AgentInputMouseCursor' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\TestWindowKeyboardLayout.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TestAgentInputClick.ps1')
        $script:app = $null
        $script:layout = $null
        $script:clipboardSaved = $false
        $script:fixtureDir = $null
        $script:index = 0
        $script:physicalReady = $false
        $script:launchStarted = $null
        $target = Resolve-ItApp -Package (Get-ItTestPackage)
        $script:target = $target
        $binaryHash = (Get-FileHash -LiteralPath $target.WtaPath -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_WTA_SHA256) {
            $binaryHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        }
        $installRoot = [IO.Path]::GetDirectoryName($target.WtaPath).TrimEnd('\') + '\'
        if (@(Get-Process | Where-Object {
            $_.Path -and $_.Path.StartsWith($installRoot, [StringComparison]::OrdinalIgnoreCase)
        }).Count) {
            throw 'This physical suite requires an unused selected package; it will not close user sessions.'
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) "agent-input-mouse-cursor\$([guid]::NewGuid().ToString('N'))"
        $script:fixtureDir = Join-Path $script:evidence 'fixture'
        New-Item -ItemType Directory -Force -Path $script:fixtureDir | Out-Null
        $fixture = Join-Path $script:fixtureDir 'Mock-AcpChatAgent.ps1'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1') -Destination $fixture
        $script:fixtureLog = Join-Path $script:evidence 'fixture.log'
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $command = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
        $script:clipboard = Get-ClipboardSnapshot
        $script:clipboardSaved = $true
        $script:launchStarted = Get-Date
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:mouse-cursor-fixture'
            acpCustomCommand = $command
            rightClickContextMenu = $false
            'warning.confirmOnClose' = 'never'
        }
        $shell = Get-ActivePane -App $script:app
        $script:ownerTab = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $shell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:pane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:pane -TimeoutSec 60 | Should -BeTrue
        $script:layout = Enable-TestWindowEnglishKeyboardLayout -App $script:app
        @{ Package = $target.Package; WtaPath = $target.WtaPath; WtaSha256 = $binaryHash; StartedUtc = [DateTime]::UtcNow.ToString('o') } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json') -Encoding utf8

        $script:key = {
            param([int]$Vk, [switch]$Ctrl)
            Send-WtWindowKey -App $script:app -Vk $Vk -Ctrl:$Ctrl -RequireForeground | Out-Null
        }
        $script:capture = {
            Get-AgentPaneText -App $script:app -PaneSessionId $script:pane -MaxLines 500
        }
        $script:save = {
            param([string]$Name)
            $script:index++
            $prefix = Join-Path $script:evidence ('{0:D3}-{1}' -f $script:index, $Name)
            & $script:capture | Set-Content -LiteralPath "$prefix.txt" -Encoding utf8
            Save-UiScreenshot -App $script:app -Path "$prefix.png" | Out-Null
        }
        $script:paste = {
            param([string]$Text, [string]$VisibleTail)
            Set-Clipboard -Value $Text
            $listener = Start-WtEventListener -App $script:app -WaitForReady
            try {
                & $script:key -Vk 0x56 -Ctrl
                Wait-WtEvent -Listener $listener -TimeoutSec 10 -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTab)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:pane)".Trim('{}')
                } | Should -Not -BeNullOrEmpty
                Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                    (& $script:capture).Contains($VisibleTail)
                } | Should -BeTrue -Because 'the complete draft must arrive before clicking'
            }
            finally { Stop-WtEventListener -Listener $listener }
        }
        $script:copy = {
            param([string]$Name)
            $sentinel = "MOUSE_COPY_$([guid]::NewGuid().ToString('N'))"
            Set-Clipboard -Value $sentinel
            & $script:key -Vk 0x41 -Ctrl
            & $script:key -Vk 0x43 -Ctrl
            $copied = Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Quiet -Condition {
                $value = Get-Clipboard -Raw
                if ($value -cne $sentinel) { $value }
            }
            $copied | Set-Content -LiteralPath (Join-Path $script:evidence "$Name-clipboard.txt") -NoNewline -Encoding utf8
            & $script:save -Name "$Name-after-copy"
            $copied
        }
        $script:click = {
            param([string]$Anchor, [int]$Column)
            Invoke-TestAgentInputClick -App $script:app -Anchor $Anchor -Column $Column |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "click-$script:index.json") -Encoding utf8
        }
    }

    BeforeEach {
        $script:physicalReady = $false
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:pane) | Out-Null
        if (-not (Set-WtWindowForeground -App $script:app)) {
            Set-ItResult -Skipped -Because 'physical input requires an unlocked foreground desktop before the action'
            return
        }
        $script:physicalReady = $true
        & $script:key -Vk 0x1B
        & $script:key -Vk 0x1B
        & $script:key -Vk 0x51
        Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match '(?m)^\s*[│║|]\s*>\s*q\s*[│║|]\s*$'
        } | Should -BeTrue -Because 'physical typing must target this agent draft'
        & $script:key -Vk 0x1B
    }

    AfterEach {
        if ($script:physicalReady -and $script:app) { & $script:save -Name 'case-final' }
    }

    AfterAll {
        try {
            try {
                if ($script:layout) { Restore-TestWindowKeyboardLayout -App $script:app -Context $script:layout }
            }
            finally {
                if ($script:app) {
                    Stop-Terminal -App $script:app
                }
                elseif ($script:launchStarted) {
                    foreach ($process in @(Get-WtProcessesForApp -App $script:target)) {
                        if ($process.Path -ine $script:target.WindowsTerminal -or $process.StartTime -lt $script:launchStarted) {
                            throw 'Startup cleanup cannot establish a test-owned process.'
                        }
                        $recovery = $script:target.PSObject.Copy()
                        $recovery.Pid = $process.Id
                        $recovery | Add-Member -NotePropertyName Launched -NotePropertyValue $true -Force
                        Stop-Terminal -App $recovery -RestoreSettings $false
                    }
                    Restore-WtConfig -App $script:target
                }
            }
        }
        finally {
            try {
                if ($script:clipboardSaved) { Restore-ClipboardSnapshot -Snapshot $script:clipboard }
            }
            finally {
                if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
                    Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
                }
            }
        }
    }

    It 'Mouse clicks reposition the agent draft caret' -Tag 'AgentInputMouseCursorBasic' {
        $draft = "MOUSE_$([guid]::NewGuid().ToString('N').Substring(0, 8))_alpha_bravo"
        & $script:paste -Text $draft -VisibleTail $draft
        & $script:save -Name 'basic-before-click'
        & $script:click -Anchor $draft -Column 3
        & $script:save -Name 'basic-after-click'
        & $script:key -Vk 0x51
        $expected = $draft.Insert(3, 'q')
        (& $script:copy -Name 'basic') | Should -BeExactly $expected -Because 'typing must insert at the physically clicked cell, not append at the previous caret'
    }

    It 'Mouse clicks target visible multiline draft rows' -Tag 'AgentInputMouseCursorMultiline' {
        $id = [guid]::NewGuid().ToString('N').Substring(0, 6)
        $lines = @(0..7 | ForEach-Object { "ROW${_}_${id}_content" })
        $draft = $lines -join "`n"
        & $script:paste -Text $draft -VisibleTail $lines[-1]
        & $script:save -Name 'multiline-before-click'
        & $script:click -Anchor $lines[3] -Column 2
        & $script:save -Name 'multiline-after-click'
        & $script:key -Vk 0x51
        $lines[3] = $lines[3].Insert(2, 'q')
        (& $script:copy -Name 'multiline') | Should -BeExactly ($lines -join "`n")
    }

    It 'Mouse clicks follow soft-wrapped draft text' -Tag 'AgentInputMouseCursorWrapped' {
        $parts = @(0..199 | ForEach-Object { 'WRAP_{0:D3}_abcdef' -f $_ })
        $draft = $parts -join ' '
        & $script:paste -Text $draft -VisibleTail $parts[-1]
        $visible = @([regex]::Matches((& $script:capture), 'WRAP_\d{3}_abcdef') | ForEach-Object Value)
        $visible.Count | Should -BeGreaterThan 3 -Because 'the wrapped viewport must expose multiple complete anchors'
        $anchor = $visible[1]
        $position = $draft.IndexOf($anchor, [StringComparison]::Ordinal) + 2
        & $script:save -Name 'wrapped-before-click'
        & $script:click -Anchor $anchor -Column 2
        & $script:save -Name 'wrapped-after-click'
        & $script:key -Vk 0x51
        (& $script:copy -Name 'wrapped') | Should -BeExactly $draft.Insert($position, 'q')
    }

    It 'Mouse clicks preserve Unicode text and collapse draft selection' -Tag 'AgentInputMouseCursorUnicode' {
        $anchor = "UNICODE_$([guid]::NewGuid().ToString('N').Substring(0, 6))_"
        $draft = $anchor + '中文 alpha'
        & $script:paste -Text $draft -VisibleTail '中文 alpha'
        & $script:key -Vk 0x41 -Ctrl
        & $script:save -Name 'unicode-selected-before-click'
        & $script:click -Anchor $anchor -Column ($anchor.Length + 1)
        & $script:save -Name 'unicode-after-click'
        & $script:key -Vk 0x51
        (& $script:copy -Name 'unicode') | Should -BeExactly ($anchor + 'q中文 alpha')
    }
}
