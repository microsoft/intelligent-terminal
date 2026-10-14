#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: packaged OpenCode hook lifecycle' -Tag 'Feature', 'OpenCodeHooks' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = $null
        $script:listener = $null
        $script:drivers = [Collections.Generic.List[Diagnostics.Process]]::new()
        $nodeCommand = Get-Command node -ErrorAction SilentlyContinue
        $script:node = if ($nodeCommand) { $nodeCommand.Source } else { $null }
        $script:driverPath = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\OpenCodeHookDriver.mjs')).Path
        $script:fixtureDirectory = Join-Path $TestDrive 'opencode'
        New-Item -ItemType Directory -Path $script:fixtureDirectory | Out-Null
        $selected = Resolve-ItApp -Package (Get-ItTestPackage)
        $selected.Package | Should -Not -Be 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe' -Because 'this suite validates the feature-source Dev build, not production'
        foreach ($binary in @(
            @{ Path = (Join-Path $selected.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 },
            @{ Path = $selected.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 }
        )) {
            $binary.Hash | Should -Not -BeNullOrEmpty -Because 'pin the exact feature-source Dev payload before running'
            (Get-FileHash -LiteralPath $binary.Path).Hash | Should -Be $binary.Hash
        }
        $script:pluginPath = Join-Path $selected.InstallLocation 'wt-agent-hooks\opencode\wt-agent-hooks.js'
        $sourcePlugin = Join-Path $PSScriptRoot '..\..\..\tools\wta\wt-agent-hooks\opencode\wt-agent-hooks.js'
        (Get-FileHash -LiteralPath $script:pluginPath).Hash | Should -Be (Get-FileHash -LiteralPath $sourcePlugin).Hash
        $mock = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $customCommand = '"' + (Get-Command pwsh).Source + '" -NoProfile -File "' + $mock + '" -LogPath "' + (Join-Path $TestDrive 'acp.jsonl') + '"'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:hook-lifecycle'
            acpCustomCommand = $customCommand
            agentSessionManagementEnabled = $false
            tabMode = 'horizontal'
        }
        $script:pane = (Get-ActivePane -App $script:app).session_id
        $pipeFile = Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt'
        $script:pipe = Wait-Until -TimeoutSec 45 -Because 'the owned package master pipe' -Condition {
            if (-not (Test-Path -LiteralPath $pipeFile)) { return $null }
            $candidate = (Get-Content -LiteralPath $pipeFile -Raw).Trim()
            if ([string]::IsNullOrWhiteSpace($candidate)) { return $null }
            $masters = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($script:app.Pid) AND Name='wta.exe'" |
                Where-Object {
                    $_.ExecutablePath -eq $script:app.WtaPath -and $_.CommandLine -match '\s--master\s' -and
                    $_.CommandLine.Contains($candidate)
                })
            if ($masters.Count -ne 1) { return $null }
            $probe = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $candidate, '--json', '--include-status'
            ) -Raw
            if ($probe.ExitCode -eq 0) { $candidate }
        }
        $script:listener = Start-WtEventListener -App $script:app -WaitForReady -ExistingOnly

        function Read-HookSessions {
            $result = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $script:pipe, '--json', '--include-status', '--origin', 'all'
            ) -Raw
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            @((ConvertFrom-Json -InputObject $result.StdOut).sessions)
        }

        function Start-HookDriver([string]$Mode) {
            $start = [Diagnostics.ProcessStartInfo]::new($script:node)
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.RedirectStandardInput = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            foreach ($argument in @($script:driverPath, $script:pluginPath, $Mode, $script:fixtureDirectory)) {
                $start.ArgumentList.Add($argument)
            }
            $start.Environment['WT_COM_CLSID'] = $script:app.ComClsid
            $start.Environment['WT_SESSION'] = $script:pane
            $start.Environment['WTCLI_PATH'] = $script:app.WtcliPath
            $start.Environment.Remove('OPENCODE_CLIENT') | Out-Null
            $process = [Diagnostics.Process]::Start($start)
            $null = $process.Handle
            $script:drivers.Add($process)
            $process | Add-Member HookErrorTask $process.StandardError.ReadToEndAsync()
            $ready = $process.StandardOutput.ReadLineAsync()
            $ready.Wait(10000) | Should -BeTrue
            ($ready.Result | ConvertFrom-Json).ready | Should -BeTrue
            $process
        }

        function Send-HookAction($Driver, [hashtable]$Action) {
            $Action.token = [guid]::NewGuid().ToString('N')
            $Driver.StandardInput.WriteLine(($Action | ConvertTo-Json -Depth 20 -Compress))
            $Driver.StandardInput.Flush()
            $response = $Driver.StandardOutput.ReadLineAsync()
            $response.Wait(10000) | Should -BeTrue -Because 'each controlled SDK action and its native bridge children must complete'
            ($response.Result | ConvertFrom-Json).completed | Should -Be $Action.token
        }

        function Assert-HookState([string]$Id, [string]$Status, [string]$Topic) {
            $event = Wait-WtEvent -Listener $script:listener -Predicate ({
                $_.method -eq 'agent_event' -and $_.params.cli_source -eq 'opencode' -and
                $_.params.agent_session_id -eq $Id -and $_.params.event -eq $Topic
            }.GetNewClosure())
            $event.params.pane_id.Trim('{}').ToLowerInvariant() | Should -Be $script:pane.Trim('{}').ToLowerInvariant()
            $row = Wait-Until -TimeoutSec 20 -Because "OpenCode $Id reaches $Status in the real master" -Condition {
                Read-HookSessions | Where-Object { $_.session_id -eq $Id -and $_.status -eq $Status } | Select-Object -First 1
            }
            if ($Status -eq 'Ended') {
                $row.pane_session_id | Should -BeNullOrEmpty
            }
            else {
                $row.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $script:pane.Trim('{}').ToLowerInvariant()
            }
            $row
        }
    }

    AfterAll {
        $failures = [Collections.Generic.List[string]]::new()
        foreach ($driver in $script:drivers) {
            try {
                if (-not $driver.HasExited) {
                    $driver.StandardInput.Close()
                    if (-not $driver.WaitForExit(5000)) {
                        $driver.Kill()
                        throw "Owned OpenCode hook fixture did not exit: $($driver.Id)"
                    }
                }
                $driver.ExitCode | Should -Be 0 -Because ($driver.HookErrorTask.GetAwaiter().GetResult())
            }
            catch { $failures.Add($_.Exception.Message) }
            finally { $driver.Dispose() }
        }
        try {
            if ($script:listener) { Stop-WtEventListener -Listener $script:listener }
        }
        finally {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
        if ($failures.Count) { throw ($failures -join "`n") }
    }

    It 'OpenCode hook entrypoints deliver lifecycle to WTA' {
        if (-not $script:node) {
            Set-ItResult -Skipped -Because 'Node.js is not installed for controlled SDK inputs'
            return
        }
        foreach ($mode in @('v1', 'v2')) {
            $driver = Start-HookDriver $mode
            $id = "ite2e-opencode-$mode-$([guid]::NewGuid().ToString('N'))"
            Send-HookAction $driver @{ action = 'event'; type = 'session.created'; data = @{ sessionID = $id; title = 'Original' } }
            Assert-HookState $id 'Idle' 'agent.session.start' | Out-Null
            Send-HookAction $driver @{ action = 'event'; type = 'session.renamed'; data = @{ sessionID = $id; title = "Renamed $mode" } }
            Wait-Until -TimeoutSec 15 -Because 'renaming updates the authoritative row title' -Condition {
                @(Read-HookSessions | Where-Object { $_.session_id -eq $id -and $_.title -eq "Renamed $mode" }).Count -eq 1
            } | Should -BeTrue
            Send-HookAction $driver @{ action = 'prompt'; id = $id }
            (Assert-HookState $id 'Working' 'agent.prompt.submit').title | Should -Be "Renamed $mode"
            Send-HookAction $driver @{ action = 'tool'; id = $id; tool = 'question'; args = @{ questions = @('Continue?') } }
            Assert-HookState $id 'Attention' 'agent.tool.starting' | Out-Null
            Send-HookAction $driver @{ action = 'event'; type = 'question.asked'; data = @{ sessionID = $id } }
            Assert-HookState $id 'Attention' 'agent.notification' | Out-Null
            Send-HookAction $driver @{ action = 'event'; type = 'question.replied'; data = @{ sessionID = $id } }
            Assert-HookState $id 'Working' 'agent.prompt.submit' | Out-Null
            Send-HookAction $driver @{ action = 'event'; type = 'session.execution.succeeded'; data = @{ sessionID = $id } }
            Assert-HookState $id 'Idle' 'agent.stop' | Out-Null
            Send-HookAction $driver @{ action = 'event'; type = 'session.execution.failed'; data = @{ sessionID = $id; error = @{ message = 'fixture failure' } } }
            Assert-HookState $id 'Error' 'agent.error' | Out-Null
            Send-HookAction $driver @{ action = 'event'; type = 'session.deleted'; data = @{ sessionID = $id } }
            Assert-HookState $id 'Ended' 'agent.session.end' | Out-Null
            Send-HookAction $driver @{ action = 'dispose' }
            Send-HookAction $driver @{ action = 'dispose' }
        }
    }

    It 'OpenCode hook suppression prevents ghost sessions' {
        if (-not $script:node) {
            Set-ItResult -Skipped -Because 'Node.js is not installed for controlled SDK inputs'
            return
        }
        $driver = Start-HookDriver 'v2'
        $prefix = "ite2e-opencode-negative-$([guid]::NewGuid().ToString('N'))"
        $root = "$prefix-root"
        Send-HookAction $driver @{ action = 'event'; type = 'session.created'; data = @{ sessionID = $root } }
        Assert-HookState $root 'Idle' 'agent.session.start' | Out-Null
        Send-HookAction $driver @{ action = 'event'; type = 'session.created'; data = @{ sessionID = "$prefix-child"; parentID = $root } }
        Send-HookAction $driver @{ action = 'prompt'; id = "$prefix-child" }
        Send-HookAction $driver @{ action = 'tool'; id = "$prefix-child"; tool = 'bash'; args = @{} }
        Send-HookAction $driver @{
            action = 'event'; type = 'session.created'; data = @{ sessionID = "$prefix-foreign" }
            location = @{ directory = "$($script:fixtureDirectory)-other"; workspaceID = 'fixture-workspace' }
        }
        Send-HookAction $driver @{ action = 'lookup-race'; id = "$prefix-deleted" }
        Send-HookAction $driver @{ action = 'lookup-race'; id = "$prefix-disposed"; dispose = $true }
        Assert-HookState $root 'Ended' 'agent.session.end' | Out-Null
        $unexpected = Wait-Until -TimeoutSec 3 -IntervalSec 0.2 -Quiet -Because 'completed negative actions produce no delayed bridge event' -Condition {
            @(Get-WtEvents -Listener $script:listener -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -like "$prefix-*" -and
                $_.params.agent_session_id -ne $root
            }).Count -gt 0
        }
        $unexpected | Should -Not -BeTrue
        @(Read-HookSessions | Where-Object { $_.session_id -like "$prefix-*" -and $_.session_id -ne $root }).Count | Should -Be 0
    }

    It 'OpenCode copied hooks upgrade without force' {
        if (-not (Get-Command opencode -ErrorAction SilentlyContinue)) {
            Set-ItResult -Skipped -Because 'opencode CLI is not installed or discoverable for reconciliation'
            return
        }
        $config = Join-Path $TestDrive 'upgrade-config'
        $plugins = Join-Path $config 'opencode\plugins'
        $support = Join-Path $plugins 'wt-agent-hooks'
        New-Item -ItemType Directory -Path $support -Force | Out-Null
        $installed = Join-Path $plugins 'wt-agent-hooks.js'
        Set-Content -LiteralPath $installed -Value '// Managed by Intelligent Terminal: wt-agent-hooks; old copied fixture'
        $manifest = Join-Path $support 'plugin.json'
        Set-Content -LiteralPath $manifest -Value '{"name":"wt-agent-hooks","version":"0.1.6","managed_by":"Intelligent Terminal: wt-agent-hooks"}'
        $environment = @{
            XDG_CONFIG_HOME = $config
            WTA_HOOKS_BUNDLE_DIR = (Join-Path $script:app.InstallLocation 'wt-agent-hooks')
            WT_COM_CLSID = $script:app.ComClsid
        }
        $arguments = @('--json', 'hooks', 'install', '--cli', 'opencode')
        $result = Invoke-Native -FilePath $script:app.WtaPath -Arguments $arguments -Environment $environment -TimeoutSec 45
        $result.ExitCode | Should -Be 0 -Because "$($result.StdOut)`n$($result.StdErr)"
        (Get-FileHash -LiteralPath $installed).Hash | Should -Be (Get-FileHash -LiteralPath $script:pluginPath).Hash
        $bundledManifest = Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'wt-agent-hooks\opencode\plugin.json') -Raw | ConvertFrom-Json
        (Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).version | Should -Be $bundledManifest.version
        $before = (Get-Item -LiteralPath $installed).LastWriteTimeUtc
        $readLease = [IO.File]::Open($installed, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            $result = Invoke-Native -FilePath $script:app.WtaPath -Arguments $arguments -Environment $environment -TimeoutSec 45
        }
        finally { $readLease.Dispose() }
        $result.ExitCode | Should -Be 0 -Because $result.StdErr
        @((ConvertFrom-Json -InputObject $result.StdOut).clis | Where-Object name -eq 'opencode')[0].outcome | Should -Be 'installed'
        (Get-Item -LiteralPath $installed).LastWriteTimeUtc | Should -Be $before -Because 'ordinary reconciliation must not recopy a current plugin'
    }

    It 'OpenCode CLI loads copied hooks' {
        $executable = $env:ITE2E_OPENCODE_EXE
        if (-not $executable) {
            $command = Get-Command opencode -ErrorAction SilentlyContinue
            if (-not $command) {
                Set-ItResult -Skipped -Because 'opencode CLI is not installed and no native executable override was supplied'
                return
            }
            $executable = if ($command.Source.EndsWith('.exe')) { $command.Source }
            else { Join-Path (Split-Path $command.Source) 'node_modules\opencode-ai\bin\opencode.exe' }
        }
        Test-Path -LiteralPath $executable -PathType Leaf | Should -BeTrue `
            -Because 'set ITE2E_OPENCODE_EXE for a native CLI installed outside the Windows npm layout'
        $version = Invoke-Native -FilePath $executable -Arguments @('--version') -TimeoutSec 15
        $version.ExitCode | Should -Be 0 -Because $version.StdErr
        $version.StdOut.Trim() | Should -Not -BeNullOrEmpty
        Write-ItLog -Message "Real OpenCode loader version: $($version.StdOut.Trim())"
        $config = Join-Path $TestDrive 'server-config'
        $plugins = Join-Path $config 'opencode\plugins'
        New-Item -ItemType Directory -Path $plugins -Force | Out-Null
        Copy-Item -LiteralPath $script:pluginPath -Destination (Join-Path $plugins 'wt-agent-hooks.js')
        $portReservation = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $portReservation.Start()
        $port = $portReservation.LocalEndpoint.Port
        $portReservation.Stop()
        $start = [Diagnostics.ProcessStartInfo]::new($executable)
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.WorkingDirectory = $script:fixtureDirectory
        foreach ($argument in @('serve', '--hostname', '127.0.0.1', '--port', "$port", '--print-logs')) {
            $start.ArgumentList.Add($argument)
        }
        foreach ($entry in @{
            XDG_CONFIG_HOME = $config
            XDG_DATA_HOME = (Join-Path $TestDrive 'server-data')
            XDG_CACHE_HOME = (Join-Path $TestDrive 'server-cache')
            XDG_STATE_HOME = (Join-Path $TestDrive 'server-state')
            OPENCODE_CONFIG_DIR = (Join-Path $config 'opencode')
            OPENCODE_DISABLE_AUTOUPDATE = 'true'
            WT_COM_CLSID = $script:app.ComClsid
            WT_SESSION = $script:pane
            WTCLI_PATH = $script:app.WtcliPath
        }.GetEnumerator()) {
            $start.Environment[$entry.Key] = $entry.Value
        }
        foreach ($key in @('OPENCODE_CLIENT', 'OPENCODE_CONFIG', 'OPENCODE_CONFIG_CONTENT', 'OPENCODE_SERVER_PASSWORD', 'OPENCODE_SERVER_USERNAME')) {
            $start.Environment.Remove($key) | Out-Null
        }
        $server = [Diagnostics.Process]::Start($start)
        $null = $server.Handle
        $stdout = $server.StandardOutput.ReadToEndAsync()
        $stderr = $server.StandardError.ReadToEndAsync()
        $base = "http://127.0.0.1:$port"
        $headers = @{ 'x-opencode-directory' = $script:fixtureDirectory }
        try {
            Wait-Until -TimeoutSec 30 -Because 'the newly created local OpenCode server becomes responsive' -Condition {
                if ($server.HasExited) { throw "OpenCode server exited: $($stderr.GetAwaiter().GetResult())" }
                $ownedListener = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction Stop |
                    Where-Object OwningProcess -eq $server.Id)
                if ($ownedListener.Count -lt 1) { return $false }
                (Invoke-RestMethod -Uri "$base/global/health" -TimeoutSec 2).healthy
            } | Should -BeTrue
            $created = Invoke-RestMethod -Method Post -Uri "$base/session" -Headers $headers -ContentType 'application/json' `
                -Body '{"title":"ITE2E native OpenCode hook"}' -TimeoutSec 45
            $created.id | Should -Not -BeNullOrEmpty
            Assert-HookState $created.id 'Idle' 'agent.session.start' | Out-Null
            Invoke-RestMethod -Method Patch -Uri "$base/session/$($created.id)" -Headers $headers -ContentType 'application/json' `
                -Body '{"title":"ITE2E native OpenCode renamed"}' -TimeoutSec 15 | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'the real CLI rename reaches the master row' -Condition {
                @(Read-HookSessions | Where-Object { $_.session_id -eq $created.id -and $_.title -eq 'ITE2E native OpenCode renamed' }).Count -eq 1
            } | Should -BeTrue
            Invoke-RestMethod -Method Delete -Uri "$base/session/$($created.id)" -Headers $headers -TimeoutSec 15 | Out-Null
            Assert-HookState $created.id 'Ended' 'agent.session.end' | Out-Null
        }
        finally {
            if (-not $server.HasExited) { $server.Kill() }
            if (-not $server.WaitForExit(5000)) { throw 'Owned native OpenCode server did not exit' }
            Write-ItLog -Message "OpenCode server stdout: $($stdout.GetAwaiter().GetResult())"
            Write-ItLog -Message "OpenCode server stderr: $($stderr.GetAwaiter().GetResult())"
            $server.Dispose()
        }
    }
}
