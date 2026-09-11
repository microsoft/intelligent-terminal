#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Issue #841: real content -> helper -> master -> ACP ownership, with a
# deterministic stdio agent. No provider authentication or model quota is used.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = [bool]((Get-Command pwsh -ErrorAction SilentlyContinue) -and (Test-WinAppAvailable))
}

Describe 'Feature: agent pane lifetime ownership' -Tag 'Feature', 'AgentPaneLifetime' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path $artifactRoot ("agent-pane-lifetime-{0}" -f [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null

        function Get-LifetimeMaster {
            @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" |
                Where-Object {
                    $_.ParentProcessId -eq $script:app.Pid -and
                    $_.ExecutablePath -eq (Join-Path (Split-Path $script:app.WtcliPath) 'wta.exe') -and
                    $_.CommandLine -match '--master(\s|$|")' -and
                    $_.CommandLine -notmatch '--connect-master'
                })
        }

        function Assert-LifetimeSession {
            param($Session, [int]$MasterId, [int]$RequestOffset = 0, [string]$OwnerShell)
            $current = if ($OwnerShell) {
                Get-LifetimeRuntimeSession -SessionId $Session.AcpSessionId -OwnerShell $OwnerShell
            } else {
                Get-AgentPaneSession -App $script:app -PaneSessionId $Session.PaneSessionId
            }
            $current | Should -Not -BeNullOrEmpty
            $current.PaneSessionId | Should -Be $Session.PaneSessionId
            $current.HelperProcessId | Should -Be $Session.HelperProcessId
            $current.AcpSessionId | Should -Be $Session.AcpSessionId
            @(Get-LifetimeMaster).Count | Should -Be 1
            (Get-LifetimeMaster).ProcessId | Should -Be $MasterId
            (@(Get-Content -LiteralPath $script:requestLog | Select-Object -Skip $RequestOffset) -join "`n") |
                Should -Not -Match ("\|session/close\|" + [regex]::Escape($Session.AcpSessionId) + '(\r?\n|$)')
        }

        function Get-LifetimeHelpers {
            Get-CimInstance Win32_Process -Filter "Name='wta.exe'" |
                Where-Object {
                    $_.ParentProcessId -eq $script:app.Pid -and
                    $_.ExecutablePath -eq (Join-Path (Split-Path $script:app.WtcliPath) 'wta.exe') -and
                    $_.CommandLine -match '--connect-master'
                }
        }

        function Get-LifetimeRuntimeSession {
            param([string]$SessionId, [string]$OwnerShell)
            # The provenance JSONL records session/new only, not current loaded
            # bindings. Pin the live registry query to this run's master pipe;
            # a shell does not necessarily inherit package runtime discovery.
            $snapshot = "$script:requestLog.registry-$([guid]::NewGuid().ToString('N')).jsonl"
            $wta = Join-Path (Split-Path $script:app.WtcliPath) 'wta.exe'
            $helper = Get-LifetimeHelpers | Select-Object -First 1
            $pipeMatch = [regex]::Match($helper.CommandLine, '--connect-master\s+(?:"(?<pipe>[^"]+)"|(?<pipe>\S+))')
            $pipeMatch.Success | Should -BeTrue
            $pipe = $pipeMatch.Groups['pipe'].Value
            $command = "& '$($wta.Replace("'", "''"))' --json sessions list --master '$($pipe.Replace("'", "''"))' --origin agent-pane > '$($snapshot.Replace("'", "''"))' 2> '$($snapshot.Replace("'", "''")).err'; " +
                "`$LASTEXITCODE | Set-Content '$($snapshot.Replace("'", "''")).exit'"
            Invoke-RunCommand -App $script:app -SessionId $OwnerShell -Command $command | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'the in-package session registry query to complete' -Condition {
                Test-Path -LiteralPath "$snapshot.exit"
            } | Out-Null
            [int](Get-Content -LiteralPath "$snapshot.exit" -Raw) |
                Should -Be 0 -Because (Get-Content -LiteralPath "$snapshot.err" -Raw)
            $record = @(Get-Content -LiteralPath $snapshot | Where-Object { $_.Trim() } |
                ForEach-Object { $_ | ConvertFrom-Json } | Where-Object session_id -eq $SessionId)
            $record.Count | Should -Be 1
            $status = Get-WtPaneStatus -App $script:app -SessionId $record[0].pane_session_id
            $status.state | Should -Match 'run'
            [pscustomobject]@{
                PaneSessionId = $record[0].pane_session_id
                AcpSessionId = $record[0].session_id
                HelperProcessId = $status.pid
            }
        }

        function Assert-LifetimeReply {
            param($App, $Session)
            $marker = 'LIFETIME_' + [guid]::NewGuid().ToString('N').Substring(0, 12)
            Send-AgentPrompt -App $App -PaneSessionId $Session.PaneSessionId -Text $marker | Out-Null
            Assert-AgentPaneText -App $App -PaneSessionId $Session.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 20
            (Get-Content -LiteralPath $script:requestLog -Raw) |
                Should -Match ("\|lifetime-ack\|" + [regex]::Escape($Session.AcpSessionId) + '\|' + $marker)
            $marker
        }

        function Assert-LifetimeClosed {
            param($Session)
            Wait-Until -TimeoutSec 25 -IntervalSec 0.3 -Because 'the exact helper to exit and its ACP session to close' -Condition {
                -not (Get-Process -Id $Session.HelperProcessId -ErrorAction SilentlyContinue) -and
                (Get-Content -LiteralPath $script:requestLog -Raw) -match
                    ("\|session/close\|" + [regex]::Escape($Session.AcpSessionId) + '(\r?\n|$)')
            } | Out-Null
            (Get-AgentPaneSession -App $script:app -PaneSessionId $Session.PaneSessionId) | Should -BeNullOrEmpty
        }

        function Invoke-LifetimeAction {
            param($App, [string]$Name)
            Send-WtWindowKey -App $App -Vk 0x50 -Ctrl -Shift -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'command palette to open' -Condition {
                Test-CommandPaletteOpen -App $App
            } | Out-Null
            Set-UiValue -App $App -Selector '_searchBox' -Value $Name | Out-Null
            Wait-UiElement -App $App -Selector $Name -TimeoutSec 10 | Out-Null
            $env:WINAPP_CLI_TELEMETRY_OPTOUT = '1'
            & winapp ui invoke $Name -w ([string]$App.Hwnd) 2>&1 | Out-Null
            $LASTEXITCODE | Should -Be 0
        }

        function Move-LifetimeTab {
            $oldWindows = @(Get-WtWindows -App $script:app).window_id
            $oldHwnds = @(Get-WtWindowHwnds -App $script:app | ForEach-Object { [string]$_.hwnd })
            Invoke-LifetimeAction -App $script:app -Name 'IT E2E move lifetime tab'
            $windowId = Wait-Until -TimeoutSec 20 -Because 'the transferred tab to create its destination window' -Condition {
                @(Get-WtWindows -App $script:app).window_id |
                    Where-Object { $_ -notin $oldWindows } | Select-Object -First 1
            }
            $hwnd = Wait-Until -TimeoutSec 15 -Because 'the destination HWND' -Condition {
                Get-WtWindowHwnds -App $script:app |
                    Where-Object { [int]$_.pid -eq $script:app.Pid -and [string]$_.hwnd -notin $oldHwnds } |
                    Select-Object -First 1 -ExpandProperty hwnd
            }
            $destination = $script:app.PSObject.Copy()
            $destination.Hwnd = $hwnd
            $destination.WindowId = [string]$windowId
            $destination
        }
    }

    BeforeEach {
        $script:app = $null
        $script:requestLog = Join-Path $script:evidence ("acp-{0}.log" -f [guid]::NewGuid().ToString('N'))
        $script:sessionStore = $null
        $invocation = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
        if ($PersistSessions) {
            $script:sessionStore = $script:requestLog + '.sessions.json'
            $invocation += " -SessionStorePath '$($script:sessionStore.Replace("'", "''"))'"
        }
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $command = "pwsh -NoProfile -EncodedCommand $encoded"
        $panePosition = if ($Position) { $Position } else { 'bottom' }
        $target = Resolve-ItApp -Package (Get-ItTestPackage)
        try {
            $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
                acpAgent = 'custom:lifetime-fixture'
                acpCustomCommand = $command
                acpModel = ''
                agentPanePosition = $panePosition
                actions = @(@{ name = 'IT E2E move lifetime tab'; command = @{ action = 'moveTab'; window = 'new' } })
            }
        }
        catch {
            Stop-AppInstances -App $target
            Restore-WtConfig -App $target
            throw
        }
        $script:shell = Get-ActivePane -App $script:app
        $script:session = Wait-NewAgentPaneSession -App $script:app -TimeoutSec 40
        $script:session.AcpSessionId | Should -Not -BeNullOrEmpty
        @(Get-LifetimeMaster).Count | Should -Be 1
        $script:masterId = [int](Get-LifetimeMaster).ProcessId
    }

    AfterEach {
        try {
            if ($script:app) {
                Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart |
                    Set-Content -LiteralPath ($script:requestLog + '.terminal.log') -Encoding utf8
            }
        }
        finally {
            try {
                if ($script:app) { Stop-Terminal -App $script:app }
            }
            finally {
                $script:app = $null
                if ($script:sessionStore -and (Test-Path -LiteralPath $script:sessionStore)) {
                    Remove-Item -LiteralPath $script:sessionStore -Force
                }
            }
        }
    }

    It 'Saved agent sessions resume in new tabs without prewarm replacement' -Tag 'SessionResume' -ForEach @(
        @{ PersistSessions = $true }
    ) {
        $survivor = $script:session
        $resumedIds = @()
        foreach ($cycle in 1..2) {
            # Create and close distinct conversations so a stale pending target
            # cannot satisfy the second resume, or load an already-live helper.
            $beforeSeed = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
            $seedShell = New-WtTab -App $script:app -Title "resume-source-$cycle" -Cwd $script:evidence
            $seed = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $beforeSeed -TimeoutSec 40
            $seed.AcpSessionId | Should -Not -BeIn $resumedIds
            Set-WtPaneFocus -App $script:app -SessionId $seedShell.session_id
            Open-AgentPane -App $script:app | Out-Null
            $marker = Assert-LifetimeReply -App $script:app -Session $seed
            $saved = Get-Content -LiteralPath $script:sessionStore -Raw | ConvertFrom-Json -AsHashtable
            $saved[$seed.AcpSessionId].transcript | Should -BeExactly "ACK:$marker"
            Close-WtPane -App $script:app -SessionId $seedShell.session_id
            Assert-LifetimeClosed -Session $seed

            $beforeTabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId).tab_id
            $beforeHelpers = @(Get-LifetimeHelpers).ProcessId
            $beforeRequests = @(Get-Content -LiteralPath $script:requestLog).Count
            try {
                $payload = @{
                    type = 'event'; method = 'resume_in_new_agent_tab'
                    params = @{ session_id = $seed.AcpSessionId; cwd = $saved[$seed.AcpSessionId].cwd }
                } | ConvertTo-Json -Compress
                Invoke-WtCli -App $script:app -Arguments @('publish', $payload) | Out-Null
                # This control request dispatches directly to TerminalPage; it
                # is not echoed to wtcli listen. Observe the actual ACP handoff.
                Wait-Until -TimeoutSec 20 -Because 'the requested saved ACP session to finish loading' -Condition {
                    @(Get-Content -LiteralPath $script:requestLog | Select-Object -Skip $beforeRequests) -match
                        ('^\d+\|session/loaded\|' + [regex]::Escape($seed.AcpSessionId) + '$')
                } | Out-Null
                $newTab = Wait-Until -TimeoutSec 20 -Because 'the accepted resume request to open its own tab' -Condition {
                    Get-WtTabs -App $script:app -WindowId $script:app.WindowId |
                        Where-Object { $_.tab_id -notin $beforeTabs } | Select-Object -First 1
                }
                $newShell = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $newTab.tab_id)[0]
                $resumed = Get-LifetimeRuntimeSession -SessionId $seed.AcpSessionId -OwnerShell $newShell.session_id
                @{ requested = $seed; actual = $resumed; tab = $newTab; marker = $marker } |
                    ConvertTo-Json -Depth 10 |
                    Set-Content -LiteralPath "$script:requestLog.resume-$cycle.json" -Encoding utf8
                $resumed.AcpSessionId | Should -BeExactly $seed.AcpSessionId
                $resumed.HelperProcessId | Should -Not -Be $seed.HelperProcessId
                $ownerTab = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $newShell.session_id
                $helper = @(Get-LifetimeHelpers | Where-Object ProcessId -eq $resumed.HelperProcessId)
                $helper.Count | Should -Be 1
                $helper[0].CommandLine | Should -Match ('--owner-tab-id\s+"?\{?' + [regex]::Escape($ownerTab.Trim('{}')) + '\}?(?:"|\s|$)')
                Assert-Pane -App $script:app -SessionId $resumed.PaneSessionId -Match "ACK:$marker" -TimeoutSec 20

                # Wait beyond successful binding/rendering for a deferred
                # prewarm or fallback new-session to become observable.
                Start-Sleep -Seconds 3
                $delta = @(Get-Content -LiteralPath $script:requestLog | Select-Object -Skip $beforeRequests)
                $loads = @($delta | Where-Object { $_ -match '^\d+\|session/load\|' } |
                    ForEach-Object { ($_ -split '\|', 3)[2] | ConvertFrom-Json })
                $loads.Count | Should -Be 1
                $loads[0].sessionId | Should -BeExactly $seed.AcpSessionId
                $loads[0].cwd | Should -BeExactly $saved[$seed.AcpSessionId].cwd
                ($delta -join "`n") | Should -Not -Match '\|session/new\|' -Because 'resume must never allocate a blank replacement session'
                ($delta -join "`n") | Should -Not -Match '\|session/prompt\|' -Because 'the restored transcript must come from session/load, not another prompt'
                @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId |
                    Where-Object { $_.tab_id -notin $beforeTabs }).Count | Should -Be 1
                @(Get-LifetimeHelpers | Where-Object { $_.ProcessId -notin $beforeHelpers }).Count | Should -Be 1
                Assert-LifetimeSession -Session $resumed -MasterId $script:masterId -RequestOffset $beforeRequests -OwnerShell $newShell.session_id
                Stop-AgentPane -App $script:app | Out-Null
                Open-AgentPane -App $script:app | Out-Null
                Assert-LifetimeSession -Session $resumed -MasterId $script:masterId -RequestOffset $beforeRequests -OwnerShell $newShell.session_id
                Assert-Pane -App $script:app -SessionId $resumed.PaneSessionId -Match "ACK:$marker" -TimeoutSec 10
                $resumedIds += $resumed.AcpSessionId
            }
            finally {
                Get-Content -LiteralPath $script:requestLog | Select-Object -Skip $beforeRequests |
                    Set-Content -LiteralPath "$script:requestLog.resume-$cycle.requests.log" -Encoding utf8
            }
        }

        $beforeOrdinary = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
        $beforeRequests = @(Get-Content -LiteralPath $script:requestLog).Count
        $ordinaryShell = New-WtTab -App $script:app -Title 'ordinary-prewarm-after-resume'
        $ordinary = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $beforeOrdinary -TimeoutSec 40
        $ordinary.AcpSessionId | Should -Not -BeIn $resumedIds
        Set-WtPaneFocus -App $script:app -SessionId $ordinaryShell.session_id
        Test-UiElementExists -App $script:app -Selector 'AgentLabelText' | Should -BeFalse
        (Get-AgentPaneSession -App $script:app -OwnerPaneSessionId $ordinaryShell.session_id).PaneSessionId |
            Should -BeExactly $ordinary.PaneSessionId
        Open-AgentPane -App $script:app | Out-Null
        Assert-LifetimeSession -Session $ordinary -MasterId $script:masterId
        $delta = @(Get-Content -LiteralPath $script:requestLog | Select-Object -Skip $beforeRequests)
        @($delta | Where-Object { $_ -match '\|session/new\|' }).Count | Should -Be 1
        ($delta -join "`n") | Should -Not -Match '\|session/load\|'
        Assert-LifetimeSession -Session $survivor -MasterId $script:masterId
    }

    It 'Hidden agent lifetime outlives the transfer timeout' {
        Open-AgentPane -App $script:app | Out-Null
        $marker = Assert-LifetimeReply -App $script:app -Session $script:session
        Stop-AgentPane -App $script:app | Out-Null
        # The registry timeout is two minutes. Ordinary hidden content must not
        # be mistaken for an abandoned transfer when that interval elapses.
        Start-Sleep -Seconds 125
        Test-UiElementExists -App $script:app -Selector 'AgentLabelText' | Should -BeFalse
        Assert-LifetimeSession -Session $script:session -MasterId $script:masterId
        foreach ($cycle in 1..3) {
            Open-AgentPane -App $script:app | Out-Null
            Assert-AgentPaneText -App $script:app -PaneSessionId $script:session.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 10
            Stop-AgentPane -App $script:app | Out-Null
        }
        Assert-LifetimeSession -Session $script:session -MasterId $script:masterId
        Assert-LifetimeReply -App $script:app -Session $script:session | Out-Null
    }

    It 'Closing a split tab retires only its own agent content (<Hidden>)' -ForEach @(
        @{ Hidden = $false }, @{ Hidden = $true }
    ) {
        $survivor = $script:session
        $victimShell = New-WtTab -App $script:app -Title 'lifetime-close-victim'
        $victim = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $survivor.PaneSessionId -TimeoutSec 40
        Open-AgentPane -App $script:app | Out-Null
        Assert-LifetimeReply -App $script:app -Session $victim | Out-Null
        if ($Hidden) { Stop-AgentPane -App $script:app | Out-Null }
        $split = Split-WtPane -App $script:app -SessionId $victimShell.session_id -Direction right
        Close-WtPane -App $script:app -SessionId $split.session_id
        Assert-LifetimeSession -Session $victim -MasterId $script:masterId
        Close-WtPane -App $script:app -SessionId $victimShell.session_id
        Assert-LifetimeClosed -Session $victim
        Start-Sleep -Seconds 18
        Assert-LifetimeSession -Session $survivor -MasterId $script:masterId
        Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
        Assert-LifetimeReply -App $script:app -Session $survivor | Out-Null
    }

    It 'Explicit agent close drains the last lease without prewarming a replacement' {
        Open-AgentPane -App $script:app | Out-Null
        Close-WtPane -App $script:app -SessionId $script:session.PaneSessionId
        Assert-LifetimeClosed -Session $script:session
        Wait-Until -TimeoutSec 25 -Because 'the last retired master lease to expire' -Condition {
            @(Get-LifetimeMaster).Count -eq 0
        } | Out-Null
        Start-Sleep -Seconds 3
        @(Get-LifetimeMaster).Count | Should -Be 0
        @(Get-AgentPaneSessions -App $script:app).Count | Should -Be 0
        (Get-WtPaneStatus -App $script:app -SessionId $script:shell.session_id).state | Should -Match 'run'
        # Open-AgentPane's log fallback can still describe the destroyed pane.
        # Click explicitly after proving there is no live agent content.
        Invoke-UiElement -App $script:app -Selector 'AgentToggleButton' | Out-Null
        $fresh = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $script:session.PaneSessionId -TimeoutSec 40
        $fresh.HelperProcessId | Should -Not -Be $script:session.HelperProcessId
        $fresh.AcpSessionId | Should -Not -Be $script:session.AcpSessionId
        Assert-LifetimeReply -App $script:app -Session $fresh | Out-Null
    }

    It 'A draining-only master exit never respawns the agent stack' {
        Open-AgentPane -App $script:app | Out-Null
        Close-WtPane -App $script:app -SessionId $script:session.PaneSessionId
        Assert-LifetimeClosed -Session $script:session
        $master = Get-LifetimeMaster
        $master.ProcessId | Should -Be $script:masterId -Because 'the master must still be inside its retirement grace period'
        Stop-Process -Id $master.ProcessId -Force
        foreach ($sample in 1..20) {
            @(Get-LifetimeMaster).Count | Should -Be 0
            Start-Sleep -Seconds 1
        }
        @(Get-AgentPaneSessions -App $script:app).Count | Should -Be 0
        (Get-WtPaneStatus -App $script:app -SessionId $script:shell.session_id).state | Should -Match 'run'
    }

    It 'Rejected cross-window pane moves preserve both tabs' -Tag 'TransferRollback' {
        Open-AgentPane -App $script:app | Out-Null
        Assert-LifetimeReply -App $script:app -Session $script:session | Out-Null
        $targetShell = New-WtTab -App $script:app -Title 'lifetime-reject-target'
        $targetSession = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $script:session.PaneSessionId -TimeoutSec 40
        Set-WtPaneFocus -App $script:app -SessionId $targetShell.session_id
        $destination = Move-LifetimeTab
        @(Get-WtPanes -App $destination -WindowId $destination.WindowId).session_id | Should -Contain $targetShell.session_id
        @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId).session_id | Should -Contain $script:shell.session_id
        Invoke-WtCli -App $destination -Arguments @('focus-pane', '-t', $targetSession.PaneSessionId) | Out-Null
        Wait-UiElement -App $destination -Selector 'AgentLabelText' -TimeoutSec 15 | Out-Null
        Set-WtSetting -App $script:app -Key 'actions' -Value @(
            @{ name = 'IT E2E rejected pane move'; command = @{ action = 'movePane'; window = [string]$destination.WindowId; index = 0 } }
        ) | Out-Null
        # get-active-pane reports the working shell even when the agent has focus.
        Invoke-WtCli -App $destination -Arguments @('focus-pane', '-t', $targetSession.PaneSessionId) | Out-Null
        Stop-AgentPane -App $script:app | Out-Null
        Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
        $sourceTab = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId).tab_id
        $destinationTab = @(Get-WtTabs -App $destination -WindowId $destination.WindowId).tab_id
        Initialize-LogOffsets -App $script:app | Out-Null

        # A focused agent pane cannot be split. This rejects the final insertion
        # after the receiver has already prepared the borrowed shell control.
        Invoke-LifetimeAction -App $script:app -Name 'IT E2E rejected pane move'
        Assert-Log -App $script:app -Name 'terminal-agent-pane.log' -Pattern 'content transfer rolled back' -TimeoutSec 15
        @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId).tab_id | Should -Be $sourceTab
        @(Get-WtTabs -App $destination -WindowId $destination.WindowId).tab_id | Should -Be $destinationTab
        $sourcePanes = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId)
        $targetPanes = @(Get-WtPanes -App $destination -WindowId $destination.WindowId)
        $sourcePanes.Count | Should -Be 1
        $targetPanes.Count | Should -Be 1
        $sourcePanes[0].session_id | Should -Be $script:shell.session_id
        $targetPanes[0].session_id | Should -Be $targetShell.session_id
        (Get-WtPaneStatus -App $script:app -SessionId $script:shell.session_id).state | Should -Match 'run'
        $shellMarker = 'ROLLBACK_' + [guid]::NewGuid().ToString('N').Substring(0, 12)
        Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id -Command "echo $shellMarker" | Out-Null
        Assert-Pane -App $script:app -SessionId $script:shell.session_id -Match "(?m)^\s*$shellMarker\s*$" -TimeoutSec 15
        Assert-LifetimeSession -Session $script:session -MasterId $script:masterId
        Assert-LifetimeSession -Session $targetSession -MasterId $script:masterId
        Assert-LifetimeReply -App $script:app -Session $script:session | Out-Null
        Assert-LifetimeReply -App $destination -Session $targetSession | Out-Null
    }

    It 'Cross-window transfer preserves content ownership and hidden state (<Position>, <Hidden>)' -Tag 'CrossWindowLifetime' -ForEach @(
        @{ Position = 'left'; Hidden = $false },
        @{ Position = 'left'; Hidden = $true },
        @{ Position = 'bottom'; Hidden = $false },
        @{ Position = 'bottom'; Hidden = $true }
    ) {
        Open-AgentPane -App $script:app | Out-Null
        $marker = Assert-LifetimeReply -App $script:app -Session $script:session
        $extraShell = Split-WtPane -App $script:app -SessionId $script:shell.session_id -Direction right
        if ($Hidden) { Stop-AgentPane -App $script:app | Out-Null }
        $survivorShell = New-WtTab -App $script:app -Title 'lifetime-source-survivor'
        $null = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $script:session.PaneSessionId -TimeoutSec 40
        Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
        Initialize-LogOffsets -App $script:app | Out-Null
        $destination = Move-LifetimeTab
        @(Get-WtPanes -App $destination -WindowId $destination.WindowId).session_id | Should -Contain $script:shell.session_id
        @(Get-WtPanes -App $destination -WindowId $destination.WindowId).session_id | Should -Contain $extraShell.session_id
        @(Get-WtPanes -App $destination -WindowId $destination.WindowId).Count | Should -Be 2
        (Test-UiElementExists -App $destination -Selector 'AgentLabelText') | Should -Be (-not $Hidden)
        Assert-LifetimeSession -Session $script:session -MasterId $script:masterId
        Close-WtPane -App $script:app -SessionId $survivorShell.session_id
        Wait-Until -TimeoutSec 15 -Because 'the source window to close' -Condition {
            @((Get-WtWindows -App $script:app).window_id) -notcontains [int]$script:app.WindowId
        } | Out-Null
        Start-Sleep -Seconds 18
        Assert-LifetimeSession -Session $script:session -MasterId $script:masterId
        Open-AgentPane -App $destination | Out-Null
        Assert-AgentPaneText -App $destination -PaneSessionId $script:session.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 15
        Assert-LifetimeReply -App $destination -Session $script:session | Out-Null
        (Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart) |
            Should -Not -Match 'forwarding load_session|load_session requested'
    }
}
