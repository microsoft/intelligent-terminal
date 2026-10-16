#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: packaged OpenCode hook lifecycle' -Tag 'Feature', 'OpenCodeHooks' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = $null
        $script:listener = $null
        $script:drivers = [Collections.Generic.List[Diagnostics.Process]]::new()
        $script:nativeRoots = [Collections.Generic.List[string]]::new()
        $nodeCommand = Get-Command node -ErrorAction SilentlyContinue
        $script:node = if ($nodeCommand) { $nodeCommand.Source } else { $null }
        $script:driverPath = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\OpenCodeHookDriver.mjs')).Path
        $script:tuiDriverPath = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\OpenCodeNativeTui.ps1')).Path
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
        $script:bundlePath = Join-Path $selected.InstallLocation 'wt-agent-hooks\opencode'
        foreach ($relative in @(
            'v1\wt-agent-hooks.js', 'v2\wt-agent-hooks.js', 'v2\server.js',
            'v2\rpc.js', 'v2\tui.js', 'v2\package.json', 'shared\bridge.js', 'plugin.json'
        )) {
            $source = Join-Path $PSScriptRoot "..\..\..\tools\wta\wt-agent-hooks\opencode\$relative"
            (Get-FileHash -LiteralPath (Join-Path $script:bundlePath $relative)).Hash |
                Should -Be (Get-FileHash -LiteralPath $source).Hash
        }
        $script:tuiPluginPath = Join-Path $script:bundlePath 'v2\tui.js'
        $script:rpcPluginPath = Join-Path $script:bundlePath 'v2\rpc.js'
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
            $pluginPath = Join-Path $script:bundlePath "$Mode\wt-agent-hooks.js"
            foreach ($argument in @($script:driverPath, $pluginPath, $Mode, $script:fixtureDirectory, $script:tuiPluginPath)) {
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
            try {
                if ($script:app) { Stop-Terminal -App $script:app }
            }
            finally {
                foreach ($root in $script:nativeRoots) {
                    if (Test-Path -LiteralPath $root) {
                        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction Stop
                    }
                }
            }
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
            if ($mode -eq 'v2') { Send-HookAction $driver @{ action = 'view'; id = $id } }
            Assert-HookState $id 'Idle' 'agent.session.start' | Out-Null
            $rename = if ($mode -eq 'v1') { 'session.updated' } else { 'session.renamed' }
            Send-HookAction $driver @{ action = 'event'; type = $rename; data = @{ sessionID = $id; title = "Renamed $mode" } }
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
            $stop = if ($mode -eq 'v1') { 'session.idle' } else { 'session.execution.succeeded' }
            Send-HookAction $driver @{ action = 'event'; type = $stop; data = @{ sessionID = $id } }
            Assert-HookState $id 'Idle' 'agent.stop' | Out-Null
            $failure = if ($mode -eq 'v1') { 'session.error' } else { 'session.execution.failed' }
            Send-HookAction $driver @{ action = 'event'; type = $failure; data = @{ sessionID = $id; error = @{ message = 'fixture failure' } } }
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
        $serverOnly = "$prefix-server-only"
        Send-HookAction $driver @{ action = 'event'; type = 'session.created'; data = @{ sessionID = $serverOnly } }
        Send-HookAction $driver @{ action = 'prompt'; id = $serverOnly }
        Send-HookAction $driver @{ action = 'tool'; id = $serverOnly; tool = 'bash'; args = @{} }
        Send-HookAction $driver @{ action = 'event'; type = 'session.created'; data = @{ sessionID = $root } }
        Send-HookAction $driver @{ action = 'view'; id = $root }
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
        $unbound = Wait-Until -TimeoutSec 20 -Because 'disposing the TUI detaches rather than deletes its server session' -Condition {
            Read-HookSessions | Where-Object { $_.session_id -eq $root -and $_.status -eq 'Idle' -and -not $_.pane_session_id }
        }
        $unbound | Should -Not -BeNullOrEmpty
        @(Get-WtEvents -Listener $script:listener -Predicate {
            $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $root -and
            $_.params.event -eq 'agent.session.end'
        }).Count | Should -Be 0 -Because 'detaching a TUI does not delete its shared-server session'
        $unexpected = Wait-Until -TimeoutSec 3 -IntervalSec 0.2 -Quiet -Because 'completed negative actions produce no delayed bridge event' -Condition {
            @(Get-WtEvents -Listener $script:listener -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -like "$prefix-*" -and
                $_.params.agent_session_id -ne $root -and
                -not ($_.params.agent_session_id -eq $serverOnly -and
                      $_.params.event -in @('agent.prompt.submit', 'agent.tool.starting'))
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
        $versionText = (& opencode --version) -join "`n"
        $major = [regex]::Match($versionText, '(?m)^(?:opencode )?v?(\d+)\.').Groups[1].Value
        $major | Should -Not -BeNullOrEmpty -Because 'reconciliation must select the native CLI loader layout'
        $v2 = [int]$major -ge 2
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
        $variant = (Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).api_variant
        $variant | Should -Be $(if ($v2) { 'v2' } else { 'v1' })
        $activeEntrypoint = if ($v2) { Join-Path $support 'wt-agent-hooks.js' } else { $installed }
        (Get-FileHash -LiteralPath $activeEntrypoint).Hash |
            Should -Be (Get-FileHash -LiteralPath (Join-Path $script:bundlePath "$variant\wt-agent-hooks.js")).Hash
        (Get-FileHash -LiteralPath (Join-Path $support 'bridge.js')).Hash |
            Should -Be (Get-FileHash -LiteralPath (Join-Path $script:bundlePath 'shared\bridge.js')).Hash
        if ($v2) {
            Test-Path -LiteralPath $installed | Should -BeFalse -Because 'V2 must not load both managed entrypoints'
            foreach ($file in @('server.js', 'tui.js', 'rpc.js', 'package.json')) {
                (Get-FileHash -LiteralPath (Join-Path $support $file)).Hash |
                    Should -Be (Get-FileHash -LiteralPath (Join-Path $script:bundlePath "v2\$file")).Hash
            }
        }
        $bundledManifest = Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'wt-agent-hooks\opencode\plugin.json') -Raw | ConvertFrom-Json
        (Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json).version | Should -Be $bundledManifest.version
        $before = (Get-Item -LiteralPath $activeEntrypoint).LastWriteTimeUtc
        $readLease = [IO.File]::Open($activeEntrypoint, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        try {
            $result = Invoke-Native -FilePath $script:app.WtaPath -Arguments $arguments -Environment $environment -TimeoutSec 45
        }
        finally { $readLease.Dispose() }
        $result.ExitCode | Should -Be 0 -Because $result.StdErr
        @((ConvertFrom-Json -InputObject $result.StdOut).clis | Where-Object name -eq 'opencode')[0].outcome | Should -Be 'installed'
        (Get-Item -LiteralPath $activeEntrypoint).LastWriteTimeUtc | Should -Be $before -Because 'ordinary reconciliation must not recopy a current plugin'
    }

    It 'OpenCode CLI loads copied hooks and native V2 TUI binds a viewed session' {
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
        $majorVersion = [regex]::Match($version.StdOut, 'v?(\d+)\.').Groups[1].Value
        $majorVersion | Should -Not -BeNullOrEmpty -Because 'the native loader contract depends on the OpenCode major version'
        Write-ItLog -Message "Real OpenCode loader version: $($version.StdOut.Trim())"
        $nativeRoot = Join-Path $PSScriptRoot "..\.opencode-native-$([guid]::NewGuid().ToString('N'))"
        $script:nativeRoots.Add($nativeRoot)
        $nativeWork = Join-Path $nativeRoot 'work'
        New-Item -ItemType Directory -Path $nativeWork -Force | Out-Null
        $config = Join-Path $nativeRoot 'config'
        $plugins = Join-Path $config 'opencode\plugins'
        $support = Join-Path $plugins 'wt-agent-hooks'
        $v2 = [int]$majorVersion -ge 2
        New-Item -ItemType Directory -Path $support -Force | Out-Null
        $variant = if ($v2) { 'v2' } else { 'v1' }
        Copy-Item -LiteralPath (Join-Path $script:bundlePath 'shared\bridge.js') -Destination (Join-Path $support 'bridge.js')
        Copy-Item -LiteralPath (Join-Path $script:bundlePath 'plugin.json') -Destination (Join-Path $support 'plugin.json')
        if ($v2) {
            foreach ($file in @('wt-agent-hooks.js', 'server.js', 'tui.js', 'rpc.js', 'package.json')) {
                $source = Join-Path $script:bundlePath "v2\$file"
                Copy-Item -LiteralPath $source -Destination (Join-Path $support $file)
                (Get-FileHash -LiteralPath (Join-Path $support $file)).Hash | Should -Be (Get-FileHash -LiteralPath $source).Hash
            }
            Test-Path -LiteralPath (Join-Path $plugins 'wt-agent-hooks.js') | Should -BeFalse `
                -Because 'V2 must not activate both the standalone file and the directory'
        }
        else {
            $source = Join-Path $script:bundlePath 'v1\wt-agent-hooks.js'
            Copy-Item -LiteralPath $source -Destination (Join-Path $plugins 'wt-agent-hooks.js')
            (Get-FileHash -LiteralPath (Join-Path $plugins 'wt-agent-hooks.js')).Hash | Should -Be (Get-FileHash -LiteralPath $source).Hash
            Test-Path -LiteralPath (Join-Path $support 'server.js') | Should -BeFalse
        }
        $password = [guid]::NewGuid().ToString('N')
        if ($v2) {
            $credentialFile = Join-Path $nativeRoot 'server-credential.dpapi'
            $password | ConvertTo-SecureString -AsPlainText -Force |
                ConvertFrom-SecureString | Set-Content -LiteralPath $credentialFile -NoNewline
        }
        $portReservation = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
        $portReservation.Start()
        $port = $portReservation.LocalEndpoint.Port
        $portReservation.Stop()
        $start = [Diagnostics.ProcessStartInfo]::new($executable)
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        $start.WorkingDirectory = $nativeWork
        foreach ($argument in @('serve', '--hostname', '127.0.0.1', '--port', "$port", '--print-logs')) {
            $start.ArgumentList.Add($argument)
        }
        $environment = @{
            XDG_CONFIG_HOME = $config
            XDG_DATA_HOME = (Join-Path $nativeRoot 'data')
            XDG_CACHE_HOME = (Join-Path $nativeRoot 'cache')
            XDG_STATE_HOME = (Join-Path $nativeRoot 'state')
            OPENCODE_CONFIG_DIR = (Join-Path $config 'opencode')
            OPENCODE_DISABLE_AUTOUPDATE = 'true'
            WT_COM_CLSID = $script:app.ComClsid
            WT_SESSION = $script:pane
            WTCLI_PATH = $script:app.WtcliPath
        }
        if ($v2) {
            $environment.OPENCODE_SERVER_PASSWORD = $password
            $environment.OPENCODE_SERVER_USERNAME = 'opencode'
        }
        foreach ($entry in $environment.GetEnumerator()) {
            $start.Environment[$entry.Key] = $entry.Value
        }
        foreach ($key in @('OPENCODE_CLIENT', 'OPENCODE_CONFIG', 'OPENCODE_CONFIG_CONTENT')) {
            $start.Environment.Remove($key) | Out-Null
        }
        if (-not $v2) {
            $start.Environment.Remove('OPENCODE_SERVER_PASSWORD') | Out-Null
            $start.Environment.Remove('OPENCODE_SERVER_USERNAME') | Out-Null
        }
        $server = [Diagnostics.Process]::Start($start)
        $null = $server.Handle
        $stdout = $server.StandardOutput.ReadToEndAsync()
        $stderr = $server.StandardError.ReadToEndAsync()
        $base = "http://127.0.0.1:$port"
        $headers = @{ 'x-opencode-directory' = $nativeWork }
        if ($v2) {
            $headers.Authorization = 'Basic ' + [Convert]::ToBase64String(
                [Text.Encoding]::UTF8.GetBytes("opencode:$password"))
        }
        $tuiPane = $null
        $secondTuiPane = $null
        try {
            Wait-Until -TimeoutSec 30 -Because 'the newly created local OpenCode server becomes responsive' -Condition {
                if ($server.HasExited) { throw "OpenCode server exited with code $($server.ExitCode)" }
                $ownedListener = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction Stop |
                    Where-Object OwningProcess -eq $server.Id)
                if ($ownedListener.Count -lt 1) { return $false }
                if ($v2) {
                    $inventory = Invoke-RestMethod -Uri "$base/api/plugin" -Headers $headers -TimeoutSec 2
                    return $null -ne $inventory.PSObject.Properties['data']
                }
                (Invoke-RestMethod -Uri "$base/global/health" -TimeoutSec 2).healthy
            } | Should -BeTrue
            if ($v2) {
                $loaded = Wait-Until -TimeoutSec 45 -Because 'native V2 discovers and activates the copied server plugin' -Condition {
                    $inventory = Invoke-RestMethod -Uri "$base/api/plugin" -Headers $headers -TimeoutSec 15
                    $matches = @($inventory.data | Where-Object {
                        $_.id -eq 'wt-agent-hooks' -and $_.source.type -eq 'local'
                    })
                    if ($matches.Count -eq 1) { $matches[0] }
                }
                $loaded | Should -Not -BeNullOrEmpty
                $loaded.state.status | Should -Be 'active' -Because 'setup must register RPC and hooks before the copied plugin becomes active'
                $loaded.features.server | Should -BeTrue
                $loaded.features.tui | Should -BeTrue -Because 'the native loader must load the TUI entrypoint for pane binding'
                $loaded.features.rpc | Should -BeTrue
            }
            $sessionEndpoint = if ($v2) { "$base/api/session" } else { "$base/session" }
            $created = Invoke-RestMethod -Method Post -Uri $sessionEndpoint -Headers $headers -ContentType 'application/json' `
                -Body '{"title":"ITE2E native OpenCode hook"}' -TimeoutSec 45
            if ($v2) { $created = $created.data }
            $created.id | Should -Not -BeNullOrEmpty
            if (-not $v2) {
                Assert-HookState $created.id 'Idle' 'agent.session.start' | Out-Null
            }
            Invoke-RestMethod -Method Patch -Uri "$sessionEndpoint/$($created.id)" -Headers $headers -ContentType 'application/json' `
                -Body '{"title":"ITE2E native OpenCode renamed"}' -TimeoutSec 15 | Out-Null
            if (-not $v2) {
                Wait-Until -TimeoutSec 15 -Because 'the real CLI rename reaches the master row' -Condition {
                    @(Read-HookSessions | Where-Object { $_.session_id -eq $created.id -and $_.title -eq 'ITE2E native OpenCode renamed' }).Count -eq 1
                } | Should -BeTrue
            }
            if ($v2) {
                @(Read-HookSessions | Where-Object session_id -eq $created.id).Count | Should -Be 0 `
                    -Because 'V2 server-only hooks must not claim a pane before any TUI views the session'
                $tuiCommand = {
                    param([string]$SessionId, [string]$CredentialPath)
                    '"' + (Get-Command pwsh).Source + '" -NoProfile -File "' + $script:tuiDriverPath +
                        '" -Executable "' + $executable + '" -BaseUrl "' + $base + '" -SessionId "' + $SessionId +
                        '" -Config "' + $config + '" -Data "' + $environment.XDG_DATA_HOME +
                        '" -Cache "' + $environment.XDG_CACHE_HOME + '" -State "' + $environment.XDG_STATE_HOME +
                        '" -CredentialFile "' + $CredentialPath + '"'
                }
                $firstTab = New-WtTab -App $script:app -Command (& $tuiCommand $created.id $credentialFile) -Cwd $nativeWork
                $tuiPane = $firstTab.session_id
                $row = Wait-Until -TimeoutSec 60 -Because 'viewing the native V2 session binds it to the TUI pane in the authoritative master' -Condition {
                    Read-HookSessions | Where-Object {
                        $_.session_id -eq $created.id -and $_.status -eq 'Idle' -and $_.pane_session_id -and
                        $_.pane_session_id.Trim('{}').ToLowerInvariant() -eq $tuiPane.Trim('{}').ToLowerInvariant()
                    } | Select-Object -First 1
                }
                $row.status | Should -Be 'Idle'
                $row.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $tuiPane.Trim('{}').ToLowerInvariant()
                $second = (Invoke-RestMethod -Method Post -Uri $sessionEndpoint -Headers $headers -ContentType 'application/json' `
                    -Body '{"title":"ITE2E native OpenCode second pane"}' -TimeoutSec 45).data
                $second.id | Should -Not -BeNullOrEmpty
                $secondCredential = Join-Path $nativeRoot 'second-server-credential.dpapi'
                $password | ConvertTo-SecureString -AsPlainText -Force |
                    ConvertFrom-SecureString | Set-Content -LiteralPath $secondCredential -NoNewline
                $secondTuiPane = (Split-WtPane -App $script:app -SessionId $tuiPane -Direction right `
                    -Command (& $tuiCommand $second.id $secondCredential)).session_id
                $secondTuiPane | Should -Not -BeNullOrEmpty
                Wait-Until -TimeoutSec 60 -Because 'a second native TUI independently binds its own shared-server session' -Condition {
                    Read-HookSessions | Where-Object {
                        $_.session_id -eq $second.id -and $_.status -eq 'Idle' -and $_.pane_session_id -and
                        $_.pane_session_id.Trim('{}').ToLowerInvariant() -eq $secondTuiPane.Trim('{}').ToLowerInvariant()
                    }
                } | Should -Not -BeNullOrEmpty
                $panes = @(Get-WtPanes -App $script:app -TabId $firstTab.tab_id)
                foreach ($id in @($tuiPane, $secondTuiPane)) {
                    @($panes | Where-Object session_id -eq $id) | Should -HaveCount 1 -Because 'both real TUIs must share one terminal tab'
                }
                $firstRow = Read-HookSessions | Where-Object session_id -eq $created.id
                $firstRow.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $tuiPane.Trim('{}').ToLowerInvariant()
                Close-WtPane -App $script:app -SessionId $tuiPane
                $tuiPane = $null
                Wait-Until -TimeoutSec 20 -Because 'closing the first native TUI clears its binding' -Condition {
                    Read-HookSessions | Where-Object { $_.session_id -eq $created.id -and -not $_.pane_session_id }
                } | Should -Not -BeNullOrEmpty
                $remaining = Read-HookSessions | Where-Object session_id -eq $second.id
                $remaining.status | Should -Be 'Idle'
                $remaining.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $secondTuiPane.Trim('{}').ToLowerInvariant()
                Invoke-RestMethod -Method Delete -Uri "$sessionEndpoint/$($second.id)" -Headers $headers -TimeoutSec 15 | Out-Null
                Wait-Until -TimeoutSec 20 -Because 'deleting the remaining viewed native session ends its registry row' -Condition {
                    Read-HookSessions | Where-Object {
                        $_.session_id -eq $second.id -and $_.status -eq 'Ended' -and -not $_.pane_session_id
                    }
                } | Should -Not -BeNullOrEmpty
            }
            Invoke-RestMethod -Method Delete -Uri "$sessionEndpoint/$($created.id)" -Headers $headers -TimeoutSec 15 | Out-Null
            if (-not $v2) {
                Assert-HookState $created.id 'Ended' 'agent.session.end' | Out-Null
            }
        }
        finally {
            try {
                if ($tuiPane) { Close-WtPane -App $script:app -SessionId $tuiPane }
                if ($secondTuiPane) { Close-WtPane -App $script:app -SessionId $secondTuiPane }
            }
            finally {
                if (-not $server.HasExited) { $server.Kill() }
                if (-not $server.WaitForExit(5000)) { throw 'Owned native OpenCode server did not exit' }
                $serverOut = $stdout.GetAwaiter().GetResult()
                $serverErr = $stderr.GetAwaiter().GetResult()
                if ($v2) {
                    $token = $headers.Authorization.Substring('Basic '.Length)
                    $serverOut = $serverOut.Replace($password, '[redacted]').Replace($token, '[redacted]')
                    $serverErr = $serverErr.Replace($password, '[redacted]').Replace($token, '[redacted]')
                }
                Write-ItLog -Message "OpenCode server stdout: $serverOut"
                Write-ItLog -Message "OpenCode server stderr: $serverErr"
                foreach ($log in Get-ChildItem -LiteralPath $nativeRoot -Recurse -File -Filter '*.log') {
                    $diagnostics = @(Get-Content -LiteralPath $log.FullName |
                        Where-Object { $_ -match 'wt-agent-hooks|plugin|rpc|error|WT_SESSION|WT_COM_CLSID|WTCLI_PATH' }) -join "`n"
                    if ($diagnostics) {
                        $diagnostics = $diagnostics.Replace($password, '[redacted]')
                        if ($v2) { $diagnostics = $diagnostics.Replace($token, '[redacted]') }
                        Write-ItLog -Message "OpenCode native log $($log.Name): $diagnostics"
                    }
                }
                $server.Dispose()
            }
        }
    }
}
