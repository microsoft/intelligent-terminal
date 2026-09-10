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
            param($Session, [int]$MasterId)
            $current = Get-AgentPaneSession -App $script:app -PaneSessionId $Session.PaneSessionId
            $current | Should -Not -BeNullOrEmpty
            $current.HelperProcessId | Should -Be $Session.HelperProcessId
            $current.AcpSessionId | Should -Be $Session.AcpSessionId
            @(Get-LifetimeMaster).Count | Should -Be 1
            (Get-LifetimeMaster).ProcessId | Should -Be $MasterId
            (Get-Content -LiteralPath $script:requestLog -Raw) |
                Should -Not -Match ("\|session/close\|" + [regex]::Escape($Session.AcpSessionId) + '(\r?\n|$)')
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
        $invocation = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
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
        if ($script:app) {
            try {
                Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart |
                    Set-Content -LiteralPath ($script:requestLog + '.terminal.log') -Encoding utf8
            }
            finally {
                Stop-Terminal -App $script:app
                $script:app = $null
            }
        }
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
