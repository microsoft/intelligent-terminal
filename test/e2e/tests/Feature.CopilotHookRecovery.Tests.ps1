#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: native Copilot hook recovery' -Tag @('Feature', 'CopilotHookRecovery') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:app = $null
        $script:listener = $null
        $script:fixtures = [Collections.Generic.List[object]]::new()
        $script:package = Get-ItTestPackage
        if ($script:package -cnotmatch '^IntelligentTerminal\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20}_rd9vj3e6a2mbr$') {
            throw 'Select an inactive private Dev package explicitly; this suite never runs against Store or ordinary Dev.'
        }
        $script:target = Resolve-ItApp -Package $script:package
        Assert-WtPackageInactive -App $script:target
        foreach ($name in @('ITE2E_HOOK_RECOVERY_BUILD_RECEIPT', 'ITE2E_SOURCE_COMMIT', 'ITE2E_EXPECTED_APP_SHA256', 'ITE2E_EXPECTED_WTA_SHA256')) {
            if (-not [Environment]::GetEnvironmentVariable($name)) { throw "Required build evidence: $name" }
        }
        $script:root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $head = (& git -C $script:root rev-parse HEAD).Trim()
        $head | Should -Be $env:ITE2E_SOURCE_COMMIT
        $receipt = Get-Content -LiteralPath $env:ITE2E_HOOK_RECOVERY_BUILD_RECEIPT -Raw | ConvertFrom-Json
        $receipt.PackageFamilyName | Should -BeExactly $script:package
        $receipt.LocalOverrides | Should -Contain 'private-package-only hook mutation refusal'
        & git -C $script:root diff --quiet $receipt.Commit HEAD -- tools\wta src build\templates
        if ($LASTEXITCODE -ne 0) { throw 'Product sources differ from the recorded package build.' }
        & git -C $script:root diff --quiet HEAD -- tools\wta src build\templates
        if ($LASTEXITCODE -ne 0) { throw 'Uncommitted product sources invalidate the package receipt.' }
        (Get-FileHash -LiteralPath $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash -LiteralPath $receipt.SourceMsix).Hash | Should -Be $receipt.SourceMsixSha256
        $zip = [IO.Compression.ZipFile]::OpenRead($receipt.SourceMsix)
        try {
            foreach ($binary in @('TerminalApp.dll', 'WindowsTerminal.exe', 'wta.exe', 'wtcli.exe')) {
                $entry = $zip.GetEntry($binary)
                if (-not $entry) { throw "The recorded MSIX lacks $binary" }
                $stream = $entry.Open()
                try { $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)) }
                finally { $stream.Dispose() }
                (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation $binary)).Hash |
                    Should -Be $hash -Because 'the installed payload must be the exact source-proven package'
            }
        }
        finally { $zip.Dispose() }
        $script:copilot = (Get-Command copilot.exe -ErrorAction Stop | Select-Object -First 1).Source
        $script:pwsh = (Get-Command pwsh.exe -ErrorAction Stop | Select-Object -First 1).Source
        $script:sessionRoot = if ($env:COPILOT_HOME) {
            Join-Path $env:COPILOT_HOME 'session-state'
        } else { Join-Path $env:USERPROFILE '.copilot\session-state' }
        $script:evidence = Join-Path $env:ITE2E_ARTIFACT_ROOT ('copilot-recovery-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $script:hookHashes = @{}
        foreach ($relative in @('.copilot\settings.json', '.claude\settings.json',
                '.claude\plugins\known_marketplaces.json', '.claude\plugins\installed_plugins.json', '.codex\config.toml')) {
            $path = Join-Path $env:USERPROFILE $relative
            $script:hookHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
        }
        $profile = '{' + [guid]::NewGuid().ToString('D') + '}'
        $environment = @{
            PATH = "$($script:target.InstallLocation);$env:PATH"
            WTCLI_PATH = $script:target.WtcliPath
            WT_WTCLI_PATH = $script:target.WtcliPath
            COPILOT_ALLOW_ALL = 'true'
            COPILOT_AUTO_UPDATE = 'false'
        }
        $script:app = Start-Terminal -Package $script:package -TimeoutSec 90 -PassFre $true -CleanSettings $false -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; tabLayoutVerticalWidth = 500
            agentSessionManagementEnabled = $true; acpAgent = 'copilot'
            autoErrorDetectionEnabled = $false; autoFixEnabled = $false
            startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            'warning.confirmOnClose' = 'never'; defaultProfile = $profile
            profiles = @{ defaults = @{
                environment = $environment; shellIntegrationEnabled = $false
                'compatibility.reloadEnvironmentVariables' = $false
            }; list = @(@{
                guid = $profile; name = 'Copilot recovery E2E'
                commandline = "`"$script:pwsh`" -NoLogo -NoProfile"
                startingDirectory = $script:evidence
            }) }
        }
        $script:app.Launched | Should -BeTrue
        $script:shell = Get-ActivePane -App $script:app
        $script:pipe = Get-WtaMasterPipe -App $script:app -TimeoutSec 60
        $script:listener = Start-WtEventListener -App $script:app -WaitForReady
        @{
            source_commit = $head; product_commit = $receipt.Commit; package = $script:package
            pid = $script:app.Pid; hwnd = $script:app.Hwnd; master = $script:pipe
            app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256; wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256
            copilot = $script:copilot; copilot_sha256 = (Get-FileHash -LiteralPath $script:copilot).Hash
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'build-proof.json')

        function Read-RecoverySessions {
            $snapshot = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--json', '--master', $script:pipe, '--include-status', '--origin', 'all')
            @($snapshot.sessions)
        }
        function Get-RecoverySession($Fixture) {
            @(Read-RecoverySessions | Where-Object session_id -EQ $Fixture.SessionId)
        }
        function Get-RecoveryLog {
            $directory = Join-Path $script:app.LogRootDir $script:app.Version
            $files = @(Get-ChildItem -LiteralPath $directory -Filter 'wta-main_master*.log' -File -ErrorAction SilentlyContinue)
            ($files | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw }) -join "`n"
        }
        function Get-RecoveryBindings($Fixture) {
            @((Get-RecoveryLog) -split "`r?`n" | Where-Object {
                $_.Contains($Fixture.SessionId) -and $_.Contains('recovered missing Copilot session-start')
            })
        }
        function Get-RecoveryRow($Fixture) {
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            @($window.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.Name.StartsWith($Fixture.Title, [StringComparison]::Ordinal)
            })
        }
        function Wait-RecoveryState {
            param($Fixture, [string]$Status, [switch]$Rendered)
            $row = Wait-Until -TimeoutSec 30 -IntervalSec 0.3 -Because "$($Fixture.Title) reaches $Status" -Condition {
                $rows = @(Get-RecoverySession $Fixture)
                if ($rows.Count -eq 1 -and $rows[0].status -eq $Status) { $rows[0] }
            }
            if ($Status -notin @('Ended', 'Historical', 'InUse')) {
                [guid]$row.pane_session_id | Should -Be ([guid]$Fixture.PaneSessionId)
                $row.cwd | Should -Be $Fixture.Folder
            }
            if ($Rendered) {
                Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $Fixture.PaneSessionId) | Out-Null
                $label = switch ($Status) {
                    Working { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusWorking }
                    Attention { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusAttention }
                    Idle { 'Idle|Complete' }
                }
                Wait-Until -TimeoutSec 20 -Because "the native Sidebar renders $Status and the exact cwd" -Condition {
                    $peers = @(Get-RecoveryRow $Fixture)
                    $peers.Count -eq 1 -and $peers[0].Current.Name -match $label -and
                        $peers[0].Current.Name.Contains($Fixture.Folder)
                } | Out-Null
            }
            $row | ConvertTo-Json -Depth 12 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$($Fixture.SessionId)-$Status.json")
            $row
        }
        function Send-RecoveryHook {
            param($Fixture, [string]$Event, [string]$Id = $Fixture.SessionId, [hashtable]$Extra = @{})
            $payload = @{ session_id = $Id; cwd = $Fixture.Folder; timestamp = [datetimeoffset]::UtcNow.ToUnixTimeMilliseconds() }
            foreach ($key in $Extra.Keys) { $payload[$key] = $Extra[$key] }
            $before = $script:listener.Events.Count
            $psi = [Diagnostics.ProcessStartInfo]::new($script:app.WtcliPath)
            foreach ($argument in @('agent-hook', '--cli-source', 'copilot', '--event', $Event)) {
                $psi.ArgumentList.Add($argument)
            }
            $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
            $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
            $psi.Environment['WT_SESSION'] = $Fixture.PaneSessionId
            $psi.Environment['WT_COM_CLSID'] = $script:app.ComClsid
            $process = [Diagnostics.Process]::Start($psi)
            try {
                $stdout = $process.StandardOutput.ReadToEndAsync()
                $stderr = $process.StandardError.ReadToEndAsync()
                $process.StandardInput.WriteLine(($payload | ConvertTo-Json -Depth 8 -Compress))
                $process.StandardInput.Close()
                if (-not $process.WaitForExit(15000)) { throw 'The owned hook invocation did not exit.' }
                $process.ExitCode | Should -Be 0 -Because $stderr.GetAwaiter().GetResult()
                Wait-Until -TimeoutSec 20 -Because "native $Event crosses COM for the exact session" -Condition {
                    @(Get-WtEvents -Listener $script:listener | Select-Object -Skip $before | Where-Object {
                        $_.method -eq 'agent_event' -and $_.params.event -eq $Event -and
                        $_.params.agent_session_id -eq $Id -and
                        [guid]$_.params.pane_id -eq [guid]$Fixture.PaneSessionId
                    }).Count -gt 0
                } | Out-Null
            }
            finally {
                if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit(3000) | Out-Null }
                $process.Dispose()
            }
        }
        function New-RecoveryFixture {
            param([string]$Name, [bool]$SuppressHooks = $true, [string]$InitialPrompt = '')
            $sid = [guid]::NewGuid().ToString('D')
            $folder = Join-Path $script:evidence $sid
            $config = Join-Path $folder '.github\copilot'
            New-Item -ItemType Directory -Path $config -Force | Out-Null
            @{ disableAllHooks = $SuppressHooks } | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $config 'settings.json')
            & git -C $folder init --quiet
            if ($LASTEXITCODE -ne 0) { throw 'Fixture repository initialization failed.' }
            $title = "E2E1255-$Name-$($sid.Substring(0, 8))"
            $command = "`"$script:copilot`" --session-id $sid --name $title --no-auto-update"
            if ($InitialPrompt) { $command += " -i `"$InitialPrompt`"" }
            $created = Invoke-WtCli -App $script:app -Arguments @('new-tab', '--window-id', [string]$script:app.WindowId,
                '--agent-provider', 'copilot', '--command', $command, '--cwd', $folder, '--title', $title)
            $fixture = [pscustomobject]@{
                SessionId = $sid; PaneSessionId = $created.session_id; TabId = $created.tab_id
                Folder = $folder; Title = $title; NativePid = 0; Closed = $false; SuppressHooks = $SuppressHooks
            }
            $script:fixtures.Add($fixture)
            $pane = Wait-Until -TimeoutSec 30 -Because 'the real Copilot becomes the native pane root' -Condition {
                Get-WtPanes -App $script:app -TabId ([string]$created.tab_id) -WindowId ([string]$script:app.WindowId) |
                    Where-Object { [guid]$_.session_id -eq [guid]$fixture.PaneSessionId -and $_.pid -gt 0 }
            }
            $pane.native_agent_provider_id | Should -Be 'copilot'
            $fixture.NativePid = [int]$pane.pid
            (Get-Process -Id $fixture.NativePid).ProcessName | Should -Be 'copilot'
            $lease = Join-Path $script:sessionRoot "$sid\inuse.$($fixture.NativePid).lock"
            Wait-Until -TimeoutSec 90 -Because 'the genuine Copilot session acquires its own lease' -Condition {
                if (Test-Path -LiteralPath $lease) {
                    (Get-Content -LiteralPath $lease -Raw).Trim() -eq [string]$fixture.NativePid
                }
            } | Out-Null
            Invoke-Wta -App $script:app -Arguments @('sessions', 'refresh', '--json', '--master', $script:pipe) | Out-Null
            Wait-Until -TimeoutSec 90 -Because 'ACP lists the exact genuine Copilot session' -Condition {
                @(Get-RecoverySession $fixture).Count -eq 1
            } | Out-Null
            if ($SuppressHooks) {
                $row = @(Get-RecoverySession $fixture)[0]
                $row.status | Should -BeIn @('Historical', 'InUse')
                $row.pane_session_id | Should -BeNullOrEmpty
                @(Get-WtEvents -Listener $script:listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $sid -and
                    $_.params.event -in @('agent.session.start', 'agent.session.started')
                }) | Should -HaveCount 0
            }
            $fixture | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $script:evidence "$sid-native-process.json")
            $fixture
        }
        function Close-RecoveryFixture($Fixture) {
            if ($Fixture.Closed) { return }
            Close-WtPane -App $script:app -SessionId $Fixture.PaneSessionId
            Wait-Until -TimeoutSec 15 -Because 'the created native pane and process exit' -Condition {
                -not (Get-Process -Id $Fixture.NativePid -ErrorAction SilentlyContinue)
            } | Out-Null
            $Fixture.Closed = $true
        }
        function Assert-RecoverySuppressed($Fixture) {
            $deadline = [datetimeoffset]::UtcNow.AddSeconds(3)
            do {
                $rows = @(Get-RecoverySession $Fixture)
                $rows | Should -HaveCount 1
                $rows[0].status | Should -BeIn @('Historical', 'InUse', 'Ended')
                $rows[0].pane_session_id | Should -BeNullOrEmpty
            } while ([datetimeoffset]::UtcNow -lt $deadline)
        }
        $script:guard = New-RecoveryFixture -Name guard
        Send-RecoveryHook $script:guard agent.session.start
        Wait-RecoveryState $script:guard Idle | Out-Null
    }

    AfterAll {
        try {
            if ($script:app) {
                try {
                    if ($script:guard -and -not $script:guard.Closed) {
                        Close-RecoveryFixture $script:guard
                    }
                }
                finally {
                    if ($script:listener) { Stop-WtEventListener -Listener $script:listener }
                    Stop-Terminal -App $script:app -RestoreSettings $false
                    Wait-Until -TimeoutSec 20 -Because 'all owned package processes finish shutting down before configuration restore' -Condition {
                        @(Get-WtProcessesForApp -App $script:app -IncludePackageExecutables).Count -eq 0
                    } | Out-Null
                    if (-not $script:app.ConfigBackupOwned) { throw 'The fixture has no owned configuration backup.' }
                    Restore-WtConfig -App $script:app
                }
            }
            elseif ($script:listener) { Stop-WtEventListener -Listener $script:listener }
        }
        finally {
            foreach ($path in $script:hookHashes.Keys) {
                $current = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                $current | Should -Be $script:hookHashes[$path] -Because 'tests must not install, update, or remove global hooks'
            }
        }
    }

    It 'Lost Copilot start hook recovers native status and directory' {
        $fixture = New-RecoveryFixture -Name lost-start
        try {
            foreach ($event in @('agent.tool.finished', 'agent.notification', 'agent.stop')) {
                Send-RecoveryHook $fixture $event -Extra @{ message = 'approval needed' }
                Assert-RecoverySuppressed $fixture
            }
            Send-RecoveryHook $fixture agent.prompt.submit
            $row = Wait-RecoveryState $fixture Working -Rendered
            $row.title | Should -Be $fixture.Title
            @(Get-RecoveryBindings $fixture) | Should -HaveCount 1
            Send-RecoveryHook $fixture agent.stop
            Wait-RecoveryState $fixture Idle -Rendered | Out-Null
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'recovered-native-sidebar.png') | Out-Null
            Wait-RecoveryState $script:guard Idle | Out-Null
        }
        finally { Close-RecoveryFixture $fixture }
    }

    It 'Normal Copilot start hooks preserve live state' {
        $fixture = New-RecoveryFixture -Name ordinary -SuppressHooks $false -InitialPrompt 'Reply only OK. Do not use any tools.'
        try {
            Wait-WtEvent -Listener $script:listener -TimeoutSec 90 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $fixture.SessionId -and
                $_.params.event -in @('agent.session.start', 'agent.session.started')
            } | Should -Not -BeNullOrEmpty
            Wait-WtEvent -Listener $script:listener -TimeoutSec 120 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $fixture.SessionId -and
                $_.params.event -eq 'agent.stop'
            } | Should -Not -BeNullOrEmpty
            Wait-RecoveryState $fixture Idle -Rendered | Out-Null
            Send-RecoveryHook $fixture agent.prompt.submit
            Wait-RecoveryState $fixture Working -Rendered | Out-Null
            Send-RecoveryHook $fixture agent.notification -Extra @{ message = 'approval needed'; notification_type = 'permission_prompt' }
            Wait-RecoveryState $fixture Attention -Rendered | Out-Null
            Send-RecoveryHook $fixture agent.stop
            Wait-RecoveryState $fixture Idle -Rendered | Out-Null
            @(Get-RecoveryBindings $fixture) | Should -HaveCount 0
            Wait-RecoveryState $script:guard Idle | Out-Null
        }
        finally { Close-RecoveryFixture $fixture }
    }

    It 'Copilot recovery preserves established owners and replay state' {
        $fixture = New-RecoveryFixture -Name ownership
        try {
            Send-RecoveryHook $fixture agent.prompt.submit
            Wait-RecoveryState $fixture Working -Rendered | Out-Null
            $child = [guid]::NewGuid().ToString('D')
            foreach ($event in @('agent.prompt.submit', 'agent.tool.starting', 'agent.subagent.stop')) {
                Send-RecoveryHook $fixture $event -Id $child -Extra @{ tool_name = 'read_file' }
            }
            Send-RecoveryHook $fixture agent.prompt.submit
            Wait-RecoveryState $fixture Working -Rendered | Out-Null
            @(Read-RecoverySessions | Where-Object {
                $_.session_id -eq $child -and $_.status -in @('Idle', 'Working', 'Attention', 'Error')
            }) | Should -HaveCount 0
            (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) |
                Should -Match ("ignored prompt session " + [regex]::Escape($child) + ' for already-bound pane')
            Send-RecoveryHook $fixture agent.notification -Extra @{ message = 'approval needed'; notification_type = 'permission_prompt' }
            Wait-RecoveryState $fixture Attention | Out-Null
            Send-RecoveryHook $fixture agent.session.start
            Wait-RecoveryState $fixture Attention -Rendered | Out-Null
            @(Get-RecoverySession $fixture) | Should -HaveCount 1
            Send-RecoveryHook $fixture agent.stop
            Wait-RecoveryState $fixture Idle | Out-Null
            Send-RecoveryHook $script:guard agent.prompt.submit
            Wait-RecoveryState $script:guard Working -Rendered | Out-Null
            Send-RecoveryHook $script:guard agent.stop
            Wait-RecoveryState $script:guard Idle | Out-Null
        }
        finally { Close-RecoveryFixture $fixture }
    }

    It 'Closed Copilot sessions reject delayed recovery hooks' {
        $fixture = New-RecoveryFixture -Name closed
        Send-RecoveryHook $fixture agent.prompt.submit
        Wait-RecoveryState $fixture Working | Out-Null
        Close-RecoveryFixture $fixture
        Wait-RecoveryState $fixture Ended | Out-Null
        Send-RecoveryHook $fixture agent.prompt.submit
        Assert-RecoverySuppressed $fixture
        Wait-RecoveryState $fixture Ended | Out-Null
        Send-RecoveryHook $script:guard agent.prompt.submit
        Wait-RecoveryState $script:guard Working -Rendered | Out-Null
        Send-RecoveryHook $script:guard agent.stop
        Wait-RecoveryState $script:guard Idle | Out-Null
        @(Get-WtPanes -App $script:app -TabId ([string]$script:shell.tab_id) -WindowId ([string]$script:app.WindowId) |
            Where-Object { [guid]$_.session_id -eq [guid]$script:shell.session_id }) | Should -HaveCount 1
    }
}
