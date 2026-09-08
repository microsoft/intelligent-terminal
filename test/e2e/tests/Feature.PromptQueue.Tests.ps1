#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real ConPTY/WT/helper/master/ACP boundaries, with a gated local agent instead of an LLM.

BeforeDiscovery {
    $script:QueueReady = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
}

Describe 'Feature Prompt Queue' -Tag 'Feature', 'PromptQueue' -Skip:(-not $script:QueueReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:queuePackage = Get-ItTestPackage
        $script:queueFixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpQueueAgent.ps1')).Path
        $script:queueArtifacts = Join-Path $PSScriptRoot '..\artifacts\prompt-queue-fixtures'

        function Get-QueueRecords {
            param([string]$Kind)
            $records = @(Get-ChildItem -LiteralPath $script:recordDirectory -Filter '*.json' -File |
                    ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json -Depth 64 } |
                    Sort-Object sequence)
            if ($Kind) { $records | Where-Object kind -eq $Kind }
            else { $records }
        }

        function Open-QueueGate {
            param([Parameter(Mandatory)][string]$Name)
            New-Item -ItemType File -Path (Join-Path $script:controlDirectory $Name) -Force | Out-Null
        }

        function Get-QueueText {
            # The helper uses the alternate screen: capture its current viewport,
            # not the helper's separately scrollable chat history.
            Get-WtCapture -App $script:app -SessionId $script:agentPaneId -MaxLines 100
        }

        function Get-QueueTextRegex {
            param([Parameter(Mandatory)][string]$Key, [int]$Count = -1)
            $pattern = Get-WtaLocalizedTextRegex -Key $Key
            if ($Count -ge 0) {
                $pattern = $pattern.Replace([regex]::Escape('%{count}'), [string]$Count)
            }
            $pattern
        }

        function Wait-QueueText {
            param([Parameter(Mandatory)][string]$Pattern)
            Wait-Until -TimeoutSec 20 -IntervalSec 0.2 -Because "expected queue pane text; artifacts: $script:runDirectory" -Condition {
                (Get-QueueText) -match $Pattern
            } | Out-Null
        }

        function Send-QueueInput {
            param([Parameter(Mandatory)][string]$Text)
            Send-WtInput -App $script:app -SessionId $script:agentPaneId -Text $Text
            Start-Sleep -Milliseconds 150
            Send-WtKeys -App $script:app -SessionId $script:agentPaneId -Keys @('Enter')
        }

        function Wait-QueueRecordCount {
            param([string]$Kind = 'prompt', [Parameter(Mandatory)][int]$Count)
            Wait-Until -TimeoutSec 25 -IntervalSec 0.2 -Because "$Count ACP $Kind records; $script:recordDirectory" -Condition {
                @(Get-QueueRecords -Kind $Kind).Count -ge $Count
            } | Out-Null
            @(Get-QueueRecords -Kind $Kind) | Should -HaveCount $Count
        }

        function Assert-QueuePromptCountStable {
            param([Parameter(Mandatory)][int]$Count, [double]$Seconds = 1)
            $clock = [System.Diagnostics.Stopwatch]::StartNew()
            do {
                @(Get-QueueRecords -Kind prompt) | Should -HaveCount $Count
                Start-Sleep -Milliseconds 100
            } while ($clock.Elapsed.TotalSeconds -lt $Seconds)
        }

        function Assert-QueueSize {
            param([Parameter(Mandatory)][int]$Count)
            $header = Get-QueueTextRegex -Key 'queue.header'
            $anyCount = $header.Replace([regex]::Escape('%{count}'), '\d+')
            $expectedCount = $header.Replace([regex]::Escape('%{count}'), "$Count(?!\d)")
            Wait-Until -TimeoutSec 20 -IntervalSec 0.2 -Because "the current pinned queue contains $Count pending requests" -Condition {
                $viewport = Get-QueueText
                if ([string]::IsNullOrWhiteSpace($viewport)) { return $false }
                $headers = [regex]::Matches($viewport, $anyCount)
                if ($Count -eq 0) { return $headers.Count -eq 0 }
                $headers.Count -eq 1 -and $headers[0].Value -match $expectedCount
            } | Out-Null
        }

        function Start-QueueScenario {
            param([switch]$HoldSession, [bool]$AutomaticFix = $true)
            # Keep paths inside PowerShell literals: custom-command argv currently splits
            # whitespace rather than decoding quotes, including on paths without spaces.
            $invocation = "& '{0}' -ControlDirectory '{1}' -RecordDirectory '{2}'" -f
                $script:queueFixture.Replace("'", "''"),
                $script:controlDirectory.Replace("'", "''"),
                $script:recordDirectory.Replace("'", "''")
            if ($HoldSession) { $invocation += ' -HoldSession' }
            $encoded = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($invocation))
            $command = "pwsh -NoLogo -NoProfile -EncodedCommand $encoded"
            $profileId = "{$([guid]::NewGuid())}"
            $settings = @{
                acpAgent = 'custom:queue-fixture'
                acpCustomCommand = $command
                acpModel = ''
                autoFixEnabled = $AutomaticFix
                autoErrorDetectionEnabled = $true
                agentPanePosition = 'bottom'
                defaultProfile = $profileId
                profiles = @{
                    defaults = @{}
                    list = @(@{
                        guid = $profileId
                        name = 'ItE2E Queue PowerShell'
                        commandline = 'pwsh.exe -NoLogo'
                        startingDirectory = $script:runDirectory
                    })
                }
            }
            # Start-Terminal may fail after backing up configuration but before returning App.
            $recoveryApp = Resolve-ItApp -Package $script:queuePackage
            try {
                $script:app = Start-Terminal -Package $script:queuePackage -PassFre $true -Settings $settings
            }
            catch {
                Stop-StaleItInstances -App $recoveryApp
                Restore-WtConfig -App $recoveryApp
                throw
            }
            $script:sourcePaneId = [string](Get-ActivePane -App $script:app).session_id
            Open-AgentPane -App $script:app | Out-Null
            # list-panes excludes helper panes, and the session registry is populated only
            # after ACP initialization. Resolve the startup identity from this app's helpers.
            $script:agentPaneId = Wait-Until -TimeoutSec 45 -Because 'the revealed helper pane' -Condition {
                foreach ($helperId in @(Get-DescendantWtaIds -RootPid $script:app.Pid)) {
                    $process = Get-CimInstance Win32_Process -Filter "ProcessId=$helperId"
                    # PID-named logs can belong to an earlier process with a different role.
                    if ($process.CommandLine -notmatch '--connect-master\b') { continue }
                    $log = Get-ItLogText -App $script:app -Name "wta-main_helper-$helperId.log"
                    $ids = [regex]::Matches($log, 'seeded app_state\.pane_id from WT_SESSION pane_id=([a-fA-F0-9-]+)')
                    if ($ids.Count) {
                        $paneId = $ids[$ids.Count - 1].Groups[1].Value
                        if ((Get-WtPaneStatus -App $script:app -SessionId $paneId).state -match 'run') {
                            $script:helperLogName = "wta-main_helper-$helperId.log"
                            return $paneId
                        }
                    }
                }
            }
            Wait-QueueRecordCount -Kind initialize -Count 1
            if ($HoldSession) {
                Wait-QueueRecordCount -Kind session -Count 1
            }
            else {
                Wait-AgentReady -App $script:app -TimeoutSec 45 | Should -BeTrue
            }
            @(Get-QueueRecords -Kind prompt) | Should -HaveCount 0
        }

        function Invoke-QueueShellCommand {
            param(
                [Parameter(Mandatory)][string]$Marker,
                [string]$PaneId = $script:sourcePaneId,
                [string]$Directory,
                [switch]$Failure
            )
            $exitCode = if ($Failure) { 17 } else { 0 }
            $command = "Write-Output '$Marker'; cmd.exe /c 'exit $exitCode'"
            if ($Directory) {
                $command = "Set-Location -LiteralPath '$($Directory.Replace("'", "''"))'; $command"
            }
            $listener = Start-WtEventListener -App $script:app
            try {
                Invoke-RunCommand -App $script:app -SessionId $PaneId `
                    -Command $command -SettleSec 4 | Out-Null
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'vt_sequence' -and
                    "$($_.params.pane_id)" -eq $PaneId -and
                    "$($_.params.sequence)" -match "(?i)osc:133;D;$exitCode(\b|;|$)"
                }
                $event | Should -Not -BeNullOrEmpty
                @(Get-WtEvents -Listener $listener) | ConvertTo-Json -Depth 64 |
                    Set-Content -LiteralPath (Join-Path $script:runDirectory "$Marker.events.json") -Encoding utf8
            }
            finally { Stop-WtEventListener -Listener $listener }
        }

        function Assert-QueueShellContext {
            param([Parameter(Mandatory)][string]$Text, [Parameter(Mandatory)][string]$Directory)
            $context = [regex]::Match($Text, '(?s)### Shell Context\s+```json\s*(.*?)\s*```')
            $context.Success | Should -BeTrue -Because 'Autofix must carry the captured shell context'
            $shell = $context.Groups[1].Value | ConvertFrom-Json
            $shell.shell | Should -Be 'pwsh'
            $shell.cwd | Should -Be (Resolve-Path -LiteralPath $Directory).Path
        }

        function Start-HeldQueuePrompt {
            param([Parameter(Mandatory)][string]$Marker, [string]$Text = $Marker)
            Send-QueueInput $Text
            Wait-QueueRecordCount -Count 1
            Wait-QueueText ([regex]::Escape("START_$Marker"))
            (Get-QueueText) | Should -Not -Match (Get-QueueTextRegex -Key 'queue.enqueued')
        }
    }

    BeforeEach {
        $script:app = $null
        $script:agentPaneId = $null
        $script:helperLogName = $null
        $script:token = [guid]::NewGuid().ToString('N').ToUpperInvariant()
        $script:runDirectory = Join-Path $script:queueArtifacts $script:token
        $script:controlDirectory = Join-Path $script:runDirectory 'control'
        $script:recordDirectory = Join-Path $script:runDirectory 'records'
        New-Item -ItemType Directory -Path $script:controlDirectory, $script:recordDirectory -Force | Out-Null
        Write-ItLog "Queue fixture artifacts: $script:runDirectory"
    }

    AfterEach {
        try {
            if ($script:app -and $script:agentPaneId) {
                Get-QueueText | Set-Content -LiteralPath (Join-Path $script:runDirectory 'agent-pane.txt') -Encoding utf8
                @(Get-QueueRecords -Kind overlap) | Should -HaveCount 0
            }
        }
        finally {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
    }

    It 'Autofix waits for agent startup without losing the failure' {
        # Hold session/new, not initialize (which has a production 15-second deadline).
        Start-QueueScenario -HoldSession
        Invoke-QueueShellCommand "QUEUE_SUCCESS_$script:token"
        Assert-QueuePromptCountStable 0

        $failure = "QUEUE_ERROR_$script:token"
        Invoke-QueueShellCommand $failure -Failure
        Assert-QueueSize 1
        (Get-QueueText) | Should -Not -Match (Get-QueueTextRegex -Key 'queue.enqueued')
        @(Get-QueueRecords -Kind session_ready) | Should -HaveCount 0
        Assert-QueuePromptCountStable 0

        Open-QueueGate 'session.release'
        Wait-AgentReady -App $script:app -TimeoutSec 45 | Should -BeTrue
        Wait-QueueRecordCount -Count 1
        $prompt = @(Get-QueueRecords -Kind prompt)[0]
        $prompt.text | Should -Match ([regex]::Escape($failure))
        Assert-QueueShellContext -Text $prompt.text -Directory $script:runDirectory
        Wait-QueueRecordCount -Kind completion -Count 1
        Send-WtKeys -App $script:app -SessionId $script:sourcePaneId -Keys @('Enter')
        Assert-QueuePromptCountStable 1 -Seconds 2
    }

    It 'Queued user messages run once in submission order' {
        Start-QueueScenario
        $first = "QUEUE_HOLD_$script:token"
        $second = "QUEUE_HOLD_SECOND_$script:token"
        $third = "QUEUE_THIRD_$script:token"
        $top = "SCROLL_TOP_$script:token"
        $viewport = (Get-QueueText) -split '\r?\n'
        $columns = [Math]::Max(1, [int](($viewport | ForEach-Object Length | Measure-Object -Maximum).Maximum))
        $fillerCount = [int][Math]::Ceiling(($viewport.Count * $columns * 3) / 'SCROLL_FILLER '.Length)
        $firstText = "$first $top $(('SCROLL_FILLER ' * $fillerCount).Trim()) SCROLL_BOTTOM_$script:token"
        Start-HeldQueuePrompt $first -Text $firstText
        Send-QueueInput $second
        Send-QueueInput $third
        Wait-QueueText (Get-QueueTextRegex -Key 'queue.enqueued')
        Assert-QueueSize 2
        Assert-QueuePromptCountStable 1

        $beforeText = Get-QueueText
        $beforeText | Should -Not -Match ([regex]::Escape($top)) -Because 'the deterministic active transcript must overflow the chat viewport'
        $beforeScroll = $beforeText -split '\r?\n'
        # Locate the pinned rows by content rather than padding-dependent offsets.
        $headerPattern = Get-QueueTextRegex -Key 'queue.header' -Count 2
        $headerRow = @(0..($beforeScroll.Count - 1) | Where-Object { $beforeScroll[$_] -match $headerPattern })[-1]
        $secondRow = @(0..($beforeScroll.Count - 1) | Where-Object { $beforeScroll[$_] -match [regex]::Escape($second) })[-1]
        $thirdRow = @(0..($beforeScroll.Count - 1) | Where-Object { $beforeScroll[$_] -match [regex]::Escape($third) })[-1]
        $headerRow | Should -Not -BeNullOrEmpty
        $secondRow | Should -Not -BeNullOrEmpty
        $secondRow | Should -BeGreaterThan $headerRow
        $thirdRow | Should -BeGreaterThan $secondRow
        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPaneId -Kind ScrollUp -Count 100 | Out-Null
        Wait-QueueText ([regex]::Escape($top))
        $afterScroll = (Get-QueueText) -split '\r?\n'
        $afterScroll[$headerRow] | Should -Be $beforeScroll[$headerRow]
        $afterScroll[$secondRow] | Should -Be $beforeScroll[$secondRow]
        $afterScroll[$thirdRow] | Should -Be $beforeScroll[$thirdRow] `
            -Because 'chat really scrolled to its hidden top, but queued inputs stayed on the same rows'
        Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agentPaneId -Kind ScrollDown -Count 100 | Out-Null

        Open-QueueGate "$first.release"
        Wait-QueueRecordCount -Count 2
        Wait-QueueRecordCount -Kind completion -Count 1
        Assert-QueueSize 1
        Assert-QueuePromptCountStable 2 -Seconds 2
        (@(Get-QueueRecords -Kind prompt).marker -join '|') | Should -Be "$first|$second"
        Open-QueueGate "$second.release"
        Wait-QueueRecordCount -Count 3
        Wait-QueueRecordCount -Kind completion -Count 3
        $prompts = @(Get-QueueRecords -Kind prompt)
        ($prompts.marker -join '|') | Should -Be "$first|$second|$third"
        $prompts[0].text | Should -Match ('(?s)## User Request\r?\n' + [regex]::Escape($firstText) + '$')
        $prompts[1].text | Should -Match ('(?s)## User Request\r?\n' + [regex]::Escape($second) + '$')
        $prompts[2].text | Should -Match ('(?s)## User Request\r?\n' + [regex]::Escape($third) + '$')
        @($prompts.sessionId | Select-Object -Unique) | Should -HaveCount 1
        $completions = @(Get-QueueRecords -Kind completion)
        $completions[0].sequence | Should -BeLessThan $prompts[1].sequence
        $completions[1].sequence | Should -BeLessThan $prompts[2].sequence
        Wait-QueueText ([regex]::Escape("ACK_$third"))
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 3
    }

    It 'Autofix detection stays actionable while the agent connects' {
        Start-QueueScenario -HoldSession -AutomaticFix $false
        Invoke-QueueShellCommand "QUEUE_SUCCESS_$script:token"
        Test-UiElementExists -App $script:app -Selector 'DiagnosticsButton' -TimeoutSec 1 | Should -BeFalse

        $failure = "QUEUE_ERROR_$script:token"
        Invoke-QueueShellCommand $failure -Failure
        Wait-UiElement -App $script:app -Selector 'DiagnosticsButton' -TimeoutSec 15 | Out-Null
        Test-UiElementEnabled -App $script:app -Selector 'DiagnosticsButton' | Should -BeTrue
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 0
        @(Get-QueueRecords -Kind session_ready) | Should -HaveCount 0

        Invoke-UiElement -App $script:app -Selector 'DiagnosticsButton' | Out-Null
        Assert-QueueSize 1
        Assert-QueuePromptCountStable 0
        @(Get-QueueRecords -Kind session_ready) | Should -HaveCount 0

        Open-QueueGate 'session.release'
        Wait-AgentReady -App $script:app -TimeoutSec 45 | Should -BeTrue
        Wait-QueueRecordCount -Count 1
        $prompt = @(Get-QueueRecords -Kind prompt)[0]
        $prompt.text | Should -Match ([regex]::Escape($failure))
        Assert-QueueShellContext -Text $prompt.text -Directory $script:runDirectory
        Wait-QueueRecordCount -Kind completion -Count 1
        Assert-QueuePromptCountStable 1
    }

    It 'Repeated diagnostics activation submits one fix per failure' {
        Start-QueueScenario -HoldSession -AutomaticFix $false
        $failure = "QUEUE_ERROR_$script:token"
        Invoke-QueueShellCommand $failure -Failure
        Wait-UiElement -App $script:app -Selector 'DiagnosticsButton' -TimeoutSec 15 | Out-Null
        Assert-QueueSize 0
        foreach ($click in 1..3) {
            Test-UiElementEnabled -App $script:app -Selector 'DiagnosticsButton' | Should -BeTrue
            Invoke-UiElement -App $script:app -Selector 'DiagnosticsButton' | Out-Null
            Assert-QueueSize 1
        }
        @(Get-QueueRecords -Kind session_ready) | Should -HaveCount 0
        Assert-QueuePromptCountStable 0

        Open-QueueGate 'session.release'
        Wait-AgentReady -App $script:app -TimeoutSec 45 | Should -BeTrue
        Wait-QueueRecordCount -Count 1
        Wait-QueueRecordCount -Kind completion -Count 1
        @(Get-QueueRecords -Kind prompt)[0].text | Should -Match ([regex]::Escape($failure))
        Assert-QueuePromptCountStable 1 -Seconds 2
        Assert-QueueSize 0

        # Deduplication is scoped to one failure, not a permanent opt-in lock.
        $freshFailure = "QUEUE_ERROR_FRESH_$script:token"
        Invoke-QueueShellCommand $freshFailure -Failure
        Wait-UiElement -App $script:app -Selector 'DiagnosticsButton' -TimeoutSec 15 | Out-Null
        Assert-QueuePromptCountStable 1
        Assert-QueueSize 0
        Invoke-UiElement -App $script:app -Selector 'DiagnosticsButton' | Out-Null
        Wait-QueueRecordCount -Count 2
        Wait-QueueRecordCount -Kind completion -Count 2
        $freshPrompt = @(Get-QueueRecords -Kind prompt)[1]
        $freshPrompt.text | Should -Match ([regex]::Escape($freshFailure))
        $freshPrompt.text | Should -Not -Match ([regex]::Escape($failure))
        Assert-QueuePromptCountStable 2 -Seconds 2
    }

    It 'Pending queue appears automatically and updates above input' {
        Start-QueueScenario
        Assert-QueueSize 0
        $first = "QUEUE_HOLD_$script:token"
        $second = "QUEUE_HOLD_SECOND_$script:token"
        $failure = "QUEUE_ERROR_$script:token"
        Start-HeldQueuePrompt $first
        Assert-QueueSize 0
        Invoke-QueueShellCommand $failure -Failure
        Assert-QueueSize 1
        $automatic = Get-QueueTextRegex -Key 'queue.auto'
        Wait-QueueText ("1\.\s+" + $automatic)
        (Get-QueueText) | Should -Not -Match (Get-QueueTextRegex -Key 'queue.enqueued')

        Send-QueueInput $second
        Assert-QueueSize 2
        Wait-QueueText ("1\.\s+" + [regex]::Escape($second))
        Wait-QueueText ("2\.\s+" + $automatic)
        Assert-QueuePromptCountStable 1
        Open-QueueGate "$first.release"
        Wait-QueueRecordCount -Count 2
        (@(Get-QueueRecords -Kind prompt).marker -join '|') | Should -Be "$first|$second"
        Assert-QueueSize 1
        Wait-QueueText ("1\.\s+" + $automatic)
        Assert-QueuePromptCountStable 2

        Open-QueueGate "$second.release"
        Wait-QueueRecordCount -Count 3
        Wait-QueueRecordCount -Kind completion -Count 3
        @(Get-QueueRecords -Kind prompt)[2].text | Should -Match ([regex]::Escape($failure))
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 3
    }

    It 'Typed fix preserves captured evidence while waiting' {
        Start-QueueScenario -AutomaticFix $false
        $first = "QUEUE_HOLD_$script:token"
        $failure = "QUEUE_ERROR_CAPTURED_$script:token"
        $hint = "FROZEN_HINT_$script:token"
        Start-HeldQueuePrompt $first
        Invoke-QueueShellCommand $failure -Failure
        Wait-UiElement -App $script:app -Selector 'DiagnosticsButton' -TimeoutSec 15 | Out-Null
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 1

        Send-QueueInput "/fix $hint"
        Assert-QueueSize 1
        # The accepted-snapshot diagnostic is the barrier, not a sleep or merely
        # queue admission: changing shell state while capture runs is a different
        # (correctly rejected) request and would not exercise frozen evidence.
        Wait-Until -TimeoutSec 25 -IntervalSec 0.2 -Because 'the typed fix snapshot was accepted before changing source context' -Condition {
            $log = Get-ItLogText -App $script:app -Name $script:helperLogName
            @($log -split '\r?\n' | Where-Object {
                $_ -match 'autofix snapshot ready' -and $_ -match 'request_id=\d+(?:\s|$)' -and
                    $_ -match ('source_pane_id="?'+ [regex]::Escape($script:sourcePaneId) + '"?(?:\s|$)')
            }).Count -eq 1
        } | Out-Null
        Assert-QueuePromptCountStable 1

        $later = "QUEUE_SUCCESS_LATER_$script:token"
        $laterDirectory = Join-Path $script:runDirectory 'later-command'
        New-Item -ItemType Directory -Path $laterDirectory | Out-Null
        Invoke-QueueShellCommand $later -Directory $laterDirectory
        $newOutput = Get-WtCapture -App $script:app -SessionId $script:sourcePaneId -LastPrompt
        $newOutput | Should -Match ([regex]::Escape($later))
        $newOutput | Should -Not -Match ([regex]::Escape($failure))
        Assert-QueueSize 1
        Assert-QueuePromptCountStable 1

        Open-QueueGate "$first.release"
        Wait-QueueRecordCount -Count 2
        Wait-QueueRecordCount -Kind completion -Count 2
        $prompt = @(Get-QueueRecords -Kind prompt)[1]
        $prompt.text | Should -Match ([regex]::Escape($failure))
        $prompt.text | Should -Match ([regex]::Escape($hint))
        $prompt.text | Should -Not -Match ([regex]::Escape($later))
        Assert-QueueShellContext -Text $prompt.text -Directory $script:runDirectory
        @(@(Get-QueueRecords -Kind prompt).sessionId | Select-Object -Unique) | Should -HaveCount 1
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 2 -Seconds 2
    }

    It 'Stopping a turn cancels queued messages and accepts new input' {
        Open-QueueGate 'hold-cancel'
        Start-QueueScenario
        $first = "QUEUE_HOLD_$script:token"
        $fresh = "QUEUE_FRESH_$script:token"
        Start-HeldQueuePrompt $first
        Send-QueueInput "QUEUE_SECOND_$script:token"
        Send-QueueInput "QUEUE_THIRD_$script:token"
        Assert-QueueSize 2
        Send-QueueInput '/stop'
        Wait-QueueText (Get-QueueTextRegex -Key 'queue.cancelled' -Count 2)
        Wait-QueueRecordCount -Kind cancel -Count 1
        Assert-QueueSize 0
        Send-QueueInput $fresh
        Assert-QueueSize 1
        Assert-QueuePromptCountStable 1 -Seconds 0.5

        Open-QueueGate 'release-cancel'
        Wait-QueueRecordCount -Count 2
        (@(Get-QueueRecords -Kind prompt).marker -join '|') | Should -Be "$first|$fresh"
        Wait-QueueRecordCount -Kind completion -Count 2
        Wait-QueueText ([regex]::Escape("ACK_$fresh"))
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 2
    }

    It 'Failed turns discard queued messages and accept new input' {
        Start-QueueScenario
        $first = "QUEUE_HOLD_$script:token"
        $fresh = "QUEUE_FRESH_$script:token"
        Start-HeldQueuePrompt $first
        Send-QueueInput "QUEUE_SECOND_$script:token"
        Assert-QueueSize 1
        Open-QueueGate "$first.fail"
        Wait-QueueText (Get-QueueTextRegex -Key 'queue.cancelled' -Count 1)
        Wait-QueueText 'QUEUE_FIXTURE_FAILURE'
        Assert-QueueSize 0
        Assert-QueuePromptCountStable 1
        Send-QueueInput $fresh
        Wait-QueueRecordCount -Count 2
        (@(Get-QueueRecords -Kind prompt).marker -join '|') | Should -Be "$first|$fresh"
        Wait-QueueText ([regex]::Escape("ACK_$fresh"))
        Assert-QueuePromptCountStable 2
    }

    It 'Queued Autofix stays pinned to its failed source pane' {
        Start-QueueScenario
        $other = Split-WtPane -App $script:app -SessionId $script:sourcePaneId -Direction right -Command 'cmd.exe /d'
        $otherMarker = "QUEUE_SUCCESS_$script:token"
        Send-WtInput -App $script:app -SessionId $other.session_id -Text "echo $otherMarker"
        Send-WtKeys -App $script:app -SessionId $other.session_id -Keys @('Enter')
        Wait-Until -TimeoutSec 10 -Because 'the sibling cmd pane output' -Condition {
            (Get-WtCapture -App $script:app -SessionId $other.session_id -MaxLines 30) -match [regex]::Escape($otherMarker)
        } | Out-Null
        Set-WtPaneFocus -App $script:app -SessionId $script:sourcePaneId
        $first = "QUEUE_HOLD_$script:token"
        $next = "QUEUE_NEXT_$script:token"
        $failure = "QUEUE_ERROR_$script:token"
        Start-HeldQueuePrompt $first
        Invoke-QueueShellCommand $failure -Failure
        Assert-QueueSize 1
        Send-QueueInput $next
        Assert-QueueSize 2
        Set-WtPaneFocus -App $script:app -SessionId $other.session_id
        Assert-QueuePromptCountStable 1
        Open-QueueGate "$first.release"
        Wait-QueueRecordCount -Count 3
        $prompts = @(Get-QueueRecords -Kind prompt)
        $prompts[1].text | Should -Match ([regex]::Escape($next))
        $prompts[2].text | Should -Match ([regex]::Escape($failure))
        Assert-QueueShellContext -Text $prompts[2].text -Directory $script:runDirectory
        $prompts[2].text | Should -Not -Match ([regex]::Escape($otherMarker))
        @($prompts.sessionId | Select-Object -Unique) | Should -HaveCount 1
        Assert-QueuePromptCountStable 3
    }

    It 'New shell commands invalidate obsolete queued Autofix' {
        Start-QueueScenario
        $first = "QUEUE_HOLD_$script:token"
        Start-HeldQueuePrompt $first
        Invoke-QueueShellCommand "QUEUE_ERROR_OLD_$script:token" -Failure
        Assert-QueueSize 1
        Invoke-QueueShellCommand "QUEUE_SUCCESS_$script:token"
        Assert-QueueSize 0
        Open-QueueGate "$first.release"
        Wait-QueueRecordCount -Kind completion -Count 1
        Assert-QueuePromptCountStable 1

        $freshFailure = "QUEUE_ERROR_FRESH_$script:token"
        Invoke-QueueShellCommand $freshFailure -Failure
        Wait-QueueRecordCount -Count 2
        $prompt = @(Get-QueueRecords -Kind prompt)[1]
        $prompt.text | Should -Match ([regex]::Escape($freshFailure))
        Assert-QueueShellContext -Text $prompt.text -Directory $script:runDirectory
        Assert-QueuePromptCountStable 2
    }

    It 'Prompt redraws preserve queued Autofix until a real command starts' {
        Start-QueueScenario
        Invoke-RunCommand -App $script:app -SessionId $script:sourcePaneId `
            -Command 'Set-PSReadLineKeyHandler -Chord Ctrl+l -Function InvokePrompt' -SettleSec 4 | Out-Null
        $first = "QUEUE_HOLD_$script:token"
        $failure = "QUEUE_ERROR_REDRAW_$script:token"
        Start-HeldQueuePrompt $first
        Invoke-QueueShellCommand $failure -Failure
        Assert-QueueSize 1
        Wait-Until -TimeoutSec 25 -Because 'the failure evidence is captured before redrawing its prompt' -Condition {
            (Get-ItLogText -App $script:app -Name $script:helperLogName) -match 'autofix snapshot ready'
        } | Out-Null

        # InvokePrompt redraws through the editor without executing a shell command.
        $listener = Start-WtEventListener -App $script:app
        try {
            foreach ($redraw in 1..2) {
                Send-WtKeys -App $script:app -SessionId $script:sourcePaneId -Keys @('C-l')
                Wait-Until -TimeoutSec 15 -Because "prompt redraw $redraw crosses the WT event boundary" -Condition {
                    @(Get-WtEvents -Listener $listener | Where-Object {
                        $_.method -eq 'vt_sequence' -and "$($_.params.pane_id)" -eq $script:sourcePaneId -and
                            "$($_.params.sequence)" -match '^osc:133;[AB]$'
                    }).Count -ge (2 * $redraw)
                } | Out-Null
            }
            @(Get-WtEvents -Listener $listener | Where-Object {
                $_.method -eq 'vt_sequence' -and "$($_.params.pane_id)" -eq $script:sourcePaneId -and
                    "$($_.params.sequence)" -eq 'osc:133;C'
            }) | Should -HaveCount 0
            Assert-QueueSize 1
            Assert-QueuePromptCountStable 1
        }
        finally { Stop-WtEventListener -Listener $listener }

        Open-QueueGate "$first.release"
        Wait-QueueRecordCount -Count 2
        Wait-QueueRecordCount -Kind completion -Count 2
        $prompt = @(Get-QueueRecords -Kind prompt)[1]
        $prompt.text | Should -Match ([regex]::Escape($failure))
        Assert-QueueShellContext -Text $prompt.text -Directory $script:runDirectory
        Assert-QueuePromptCountStable 2

        # Negative control: genuine C/D-producing shell work, unlike redraw,
        # must still invalidate a later pending automatic fix.
        $next = "QUEUE_HOLD_NEXT_$script:token"
        Send-QueueInput $next
        Wait-QueueRecordCount -Count 3
        Wait-QueueText ([regex]::Escape("START_$next"))
        Invoke-QueueShellCommand "QUEUE_ERROR_OBSOLETE_$script:token" -Failure
        Assert-QueueSize 1
        Invoke-QueueShellCommand "QUEUE_SUCCESS_AFTER_REDRAW_$script:token"
        Assert-QueueSize 0
        Open-QueueGate "$next.release"
        Wait-QueueRecordCount -Kind completion -Count 3
        Assert-QueuePromptCountStable 3 -Seconds 2
    }
}
