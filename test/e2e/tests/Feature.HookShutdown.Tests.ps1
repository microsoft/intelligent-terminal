#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Exercise the real package COM server without opening windows, restoring layouts,
# editing configuration, launching an agent, or consuming model quota.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $null -ne (Resolve-ItApp -Package (Get-ItTestPackage) -IfInstalled)
}

Describe 'Feature: non-activating hook delivery' -Tag 'Feature', 'HookShutdown' -Skip:(-not $script:Ready) {
    BeforeAll {
        $script:app = Resolve-ItApp -Package (Get-ItTestPackage)
        $script:owned = [Collections.Generic.List[Diagnostics.Process]]::new()
        $script:caseStarted = $null
        $script:legacyScript = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\legacy-hook-bundle\send-event.ps1')).Path
        $script:pwsh = (Get-Command pwsh).Source
        $script:windowsPowerShell = (Get-Command powershell.exe).Source

        # Resolve only from the selected manifest: Resolve-WtComClsid itself
        # activates COM and would invalidate the stopped-server oracle.
        $knownClsids = & (Get-Module ItE2E) { @($script:ItBrandClsids.Values) }
        [xml]$manifest = Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'AppxManifest.xml') -Raw
        $classes = $manifest.SelectNodes("//*[local-name()='ExeServer' and @Executable='WindowsTerminal.exe']/*[local-name()='Class']")
        $clsids = @($classes | ForEach-Object { ([guid]$_.Id).ToString('B').ToUpperInvariant() } |
            Where-Object { $_ -in $knownClsids })
        $clsids.Count | Should -Be 1
        $script:app.ComClsid = $clsids[0]
        $script:app.WtcliPath | Should -Be (Join-Path $script:app.InstallLocation 'wtcli.exe')

        foreach ($binary in @(
            @{ Path = $script:app.WindowsTerminal; Expected = $env:ITE2E_EXPECTED_TERMINAL_SHA256 },
            @{ Path = $script:app.WtcliPath; Expected = $env:ITE2E_EXPECTED_WTCLI_SHA256 }
        )) {
            if ($binary.Expected) {
                (Get-FileHash -LiteralPath $binary.Path).Hash | Should -Be $binary.Expected `
                    -Because 'the selected package must contain the intended feature-branch binary'
            }
        }

        function Get-TestServers {
            @(Get-WtProcessesForApp -App $script:app |
                Where-Object Path -eq $script:app.WindowsTerminal)
        }

        function Register-TestServers {
            if (-not $script:caseStarted) { return }
            foreach ($process in @(Get-TestServers)) {
                $alreadyOwned = @($script:owned | Where-Object {
                    $_.Id -eq $process.Id -and $_.StartTime -eq $process.StartTime
                }).Count -gt 0
                if ($process.StartTime -ge $script:caseStarted -and -not $alreadyOwned) {
                    $null = $process.Handle
                    $script:owned.Add($process)
                }
            }
        }

        function Stop-TestServer {
            param([Parameter(Mandatory)][Diagnostics.Process]$Process)
            if (-not $Process.HasExited) {
                $current = Get-Process -Id $Process.Id -ErrorAction SilentlyContinue
                if ($current -and $current.Path -eq $script:app.WindowsTerminal -and
                    $current.StartTime -eq $Process.StartTime) {
                    # The fixture deliberately has no window. Terminate only its
                    # retained process, also exercising ROT cleanup after a crash.
                    Stop-Process -Id $Process.Id -Force
                }
                $Process.WaitForExit(5000) | Should -BeTrue
            }
        }

        function Assert-NoTestServer {
            $observation = @{ Unexpected = @() }
            # Cached hooks return before their asynchronous wtcli child finishes.
            Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Quiet -Because 'no replacement after the completed hook' -Condition {
                $observation.Unexpected = @(Get-TestServers)
                $observation.Unexpected.Count -gt 0
            } | Out-Null
            Register-TestServers
            $observation.Unexpected += @(Get-TestServers)
            $observation.Unexpected.Count | Should -Be 0 -Because 'notifications must never activate a package COM server'
        }

        function Start-TestComServer {
            @(Get-TestServers).Count | Should -Be 0
            try {
                $response = Invoke-WtCli -App $script:app -Arguments @('list-windows') -TimeoutSec 20
            }
            finally { Register-TestServers }
            @($response.windows).Count | Should -Be 0 -Because 'ordinary COM activation must leave saved layouts deferred'
            $servers = @(Get-TestServers)
            $servers.Count | Should -Be 1
            $process = $script:owned | Where-Object {
                $_.Id -eq $servers[0].Id -and $_.StartTime -eq $servers[0].StartTime
            } | Select-Object -First 1
            $process | Should -Not -BeNullOrEmpty
            (Get-CimInstance Win32_Process -Filter "ProcessId=$($process.Id)").CommandLine |
                Should -Match '(?i)\s-Embedding\s*$'
            $process
        }

        function Invoke-TestHook {
            param(
                [ValidateSet('native', 'legacy', 'cached')][string]$Transport,
                [string]$SessionId,
                [string]$Event = 'agent.session.end',
                [switch]$Unattributed
            )
            $environment = @{
                WT_COM_CLSID = $script:app.ComClsid
                WT_SESSION = $script:paneId
                PATH = $script:app.InstallLocation + ';' + $env:PATH
            }
            $payload = @{ session_id = $SessionId; reason = 'user_exit' } | ConvertTo-Json -Compress
            try {
                if ($Transport -eq 'legacy') {
                    $eventJson = @{
                        cli_source = 'copilot'; agent_session_id = $SessionId; payload = @{ reason = 'user_exit' }
                    } | ConvertTo-Json -Compress
                    $arguments = @('send-event', '-e', $Event)
                    if (-not $Unattributed) { $arguments += @('-p', $script:paneId) }
                    return Invoke-Native -FilePath $script:app.WtcliPath -Arguments ($arguments + @($eventJson)) `
                        -Environment $environment -TimeoutSec 15
                }
                if ($Transport -eq 'cached') {
                    $command = "'$payload' | & '$($script:windowsPowerShell.Replace("'", "''"))' " +
                        "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File '$($script:legacyScript.Replace("'", "''"))' " +
                        "-CliSource copilot '$Event'; exit `$LASTEXITCODE"
                }
                else {
                    $command = "'$payload' | & '$($script:app.WtcliPath.Replace("'", "''"))' " +
                        "agent-hook --cli-source copilot --event '$Event'; exit `$LASTEXITCODE"
                }
                $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
                Invoke-Native -FilePath $script:pwsh -Arguments @('-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) `
                    -Environment $environment -TimeoutSec 15
            }
            finally { Register-TestServers }
        }

        function Assert-StoppedHooks {
            foreach ($transport in @('native', 'legacy', 'cached')) {
                $result = Invoke-TestHook -Transport $transport -SessionId "stopped-$transport-$([guid]::NewGuid())"
                $result.TimedOut | Should -BeFalse
                if ($transport -eq 'legacy') {
                    $result.ExitCode | Should -Not -Be 0
                    $result.StdErr | Should -Match 'Connection failed'
                }
                else {
                    $result.ExitCode | Should -Be 0
                    $result.StdOut | Should -BeNullOrEmpty
                    $result.StdErr | Should -BeNullOrEmpty
                }
                Assert-NoTestServer
            }
        }
    }

    BeforeEach {
        $script:caseStarted = $null
        @(Get-TestServers).Count | Should -Be 0 -Because 'close the selected package first; never terminate existing user processes'
        $script:caseStarted = Get-Date
        $script:paneId = [guid]::NewGuid().ToString()
        $script:stateHashes = @{}
        foreach ($file in @($script:app.SettingsPath, $script:app.StatePath) +
            @(Get-ChildItem -LiteralPath $script:app.LocalStateDir -Filter 'buffer_*.txt' -File | ForEach-Object FullName)) {
            $script:stateHashes[$file] = if (Test-Path -LiteralPath $file) { (Get-FileHash -LiteralPath $file).Hash } else { $null }
        }
    }

    AfterEach {
        if ($script:caseStarted) {
            try {
                Register-TestServers
                foreach ($process in $script:owned) { Stop-TestServer -Process $process }
                @(Get-TestServers).Count | Should -Be 0
                foreach ($file in $script:stateHashes.Keys) {
                    if ($null -eq $script:stateHashes[$file]) {
                        Test-Path -LiteralPath $file | Should -BeFalse
                    }
                    else {
                        (Get-FileHash -LiteralPath $file).Hash | Should -Be $script:stateHashes[$file] `
                            -Because 'headless hook checks must not change settings, saved layouts, or buffers'
                    }
                }
            }
            finally {
                foreach ($process in $script:owned) { $process.Dispose() }
                $script:owned.Clear()
                $script:caseStarted = $null
            }
        }
    }

    It 'Hooks do not activate a stopped Terminal' {
        Assert-StoppedHooks
    }

    It 'Live hooks reuse the fixed COM class' {
        $process = Start-TestComServer
        $listener = Start-WtEventListener -App $script:app -WaitForReady
        try {
            foreach ($transport in @('native', 'legacy', 'cached')) {
                $sessionId = "live-$transport-$([guid]::NewGuid())"
                $result = Invoke-TestHook -Transport $transport -SessionId $sessionId -Unattributed:($transport -eq 'legacy')
                $result.TimedOut | Should -BeFalse
                $result.ExitCode | Should -Be 0
                $event = Wait-WtEvent -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $sessionId
                }
                $event.params.event | Should -Be 'agent.session.end'
                $event.params.cli_source | Should -Be 'copilot'
                if ($transport -eq 'legacy') { $event.params.pane_id | Should -BeNullOrEmpty }
                else { $event.params.pane_id | Should -Be $script:paneId }
            }
            $watch = [Diagnostics.Stopwatch]::StartNew()
            Wait-Until -TimeoutSec 10 -IntervalSec 0.1 -Because 'a legitimate headless listener survives without a startup timeout' -Condition {
                $process.HasExited -or $watch.Elapsed.TotalSeconds -ge 6
            } | Out-Null
            $process.HasExited | Should -BeFalse
            $listener.Process.HasExited | Should -BeFalse
            $process.Refresh()
            $process.MainWindowHandle | Should -Be 0
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Late hooks cannot restart Terminal' {
        $process = Start-TestComServer
        Stop-TestServer -Process $process
        Assert-StoppedHooks
        Assert-StoppedHooks

        # Non-agent notifications retain the public command's activation behavior.
        $result = Invoke-TestHook -Transport legacy -SessionId "generic-$([guid]::NewGuid())" -Event 'fixture.completed'
        $result.TimedOut | Should -BeFalse
        $result.ExitCode | Should -Be 0
        @(Get-TestServers).Count | Should -Be 1
        @((Invoke-WtCli -App $script:app -Arguments @('list-windows')).windows).Count | Should -Be 0
    }

    It 'Passive WTA transports do not activate Terminal' {
        $process = Start-TestComServer
        $listener = Start-WtEventListener -App $script:app -ExistingOnly -WaitForReady
        $marker = [guid]::NewGuid().ToString('N')
        $json = @{ type = 'event'; method = 'agent_event'; params = @{ event = 'fixture.passive'; marker = $marker } } |
            ConvertTo-Json -Compress
        $publish = {
            param([bool]$ExistingOnly)
            $flag = if ($ExistingOnly) { ' --existing-only' } else { '' }
            $command = "'$json' | & '$($script:app.WtcliPath.Replace("'", "''"))' publish$flag --stdin; exit `$LASTEXITCODE"
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
            Invoke-Native -FilePath $script:pwsh -Arguments @('-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) `
                -Environment @{ WT_COM_CLSID = $script:app.ComClsid } -TimeoutSec 15
        }
        try {
            $result = & $publish $true
            $result.ExitCode | Should -Be 0
            Wait-WtEvent -Listener $listener -Predicate {
                $_.method -eq 'agent_event' -and $_.params.event -eq 'fixture.passive' -and $_.params.marker -eq $marker
            } | Should -Not -BeNullOrEmpty
            Stop-TestServer -Process $process
        }
        finally { Stop-WtEventListener -Listener $listener }

        foreach ($attempt in 1..2) {
            $result = & $publish $true
            $result.TimedOut | Should -BeFalse
            $result.ExitCode | Should -Not -Be 0
            $result.StdErr | Should -Match 'Connection failed'
            $retry = Start-WtEventListener -App $script:app -ExistingOnly
            try {
                $retry.Process.WaitForExit(5000) | Should -BeTrue
                $retry.Process.ExitCode | Should -Not -Be 0
            }
            finally { Stop-WtEventListener -Listener $retry }
            Assert-NoTestServer
        }

        # Explicit public publication still has its normal activation behavior.
        $result = & $publish $false
        $result.TimedOut | Should -BeFalse
        $result.ExitCode | Should -Be 0
        Register-TestServers
        @(Get-TestServers).Count | Should -Be 1
    }
}
