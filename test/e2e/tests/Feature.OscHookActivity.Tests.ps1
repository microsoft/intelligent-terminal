#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: native hook and OSC activity arbitration' -Tag @('Feature', 'OscHookActivity') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $script:app = $null
        $script:listener = $null
        $script:fixtures = [Collections.Generic.List[object]]::new()
        $script:package = Get-ItTestPackage
        if ($script:package -cnotmatch '^IntelligentTerminal\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20}_rd9vj3e6a2mbr$') {
            throw 'Select an inactive private Dev PFN built from the intended PR head; never use Store or ordinary Dev.'
        }
        $script:target = Resolve-ItApp -Package $script:package
        Assert-WtPackageInactive -App $script:target
        $script:root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $head = (& git -C $script:root rev-parse HEAD).Trim()
        if ($env:ITE2E_SOURCE_COMMIT -ne $head -or -not $env:ITE2E_OSC_ACTIVITY_BUILD_RECEIPT) {
            throw 'Supply the current source commit and private-package build receipt.'
        }
        $receipt = Get-Content -LiteralPath $env:ITE2E_OSC_ACTIVITY_BUILD_RECEIPT -Raw | ConvertFrom-Json
        $receipt.PackageFamilyName | Should -BeExactly $script:package
        $receipt.LocalOverrides | Should -Contain 'private-package-only hook mutation refusal'
        & git -C $script:root diff --quiet $receipt.Commit HEAD -- tools\wta src build\templates
        if ($LASTEXITCODE -ne 0) { throw 'Product sources differ from the package build receipt.' }
        & git -C $script:root diff --quiet HEAD -- tools\wta src build\templates
        if ($LASTEXITCODE -ne 0) { throw 'Restore temporary product overrides before live verification.' }
        foreach ($entry in @(
            @{ Path = (Join-Path $script:target.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 },
            @{ Path = $script:target.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 }
        )) {
            $entry.Hash | Should -Not -BeNullOrEmpty
            (Get-FileHash -LiteralPath $entry.Path).Hash | Should -Be $entry.Hash
        }
        (Get-FileHash -LiteralPath $receipt.SourceMsix).Hash | Should -Be $receipt.SourceMsixSha256
        $zip = [IO.Compression.ZipFile]::OpenRead($receipt.SourceMsix)
        try {
            foreach ($binary in @('TerminalApp.dll', 'WindowsTerminal.exe', 'wta.exe', 'wtcli.exe')) {
                $entry = $zip.GetEntry($binary)
                if (-not $entry) { throw "Missing MSIX payload: $binary" }
                $stream = $entry.Open()
                try { $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)) }
                finally { $stream.Dispose() }
                (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation $binary)).Hash | Should -Be $hash
            }
        }
        finally { $zip.Dispose() }
        $script:hookHashes = @{}
        foreach ($relative in @('.copilot\settings.json', '.copilot\plugins\installed_plugins.json',
                '.claude\settings.json', '.claude\plugins\known_marketplaces.json',
                '.claude\plugins\installed_plugins.json', '.codex\config.toml')) {
            $path = Join-Path $env:USERPROFILE $relative
            $script:hookHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
        }
        $script:evidence = Join-Path $env:ITE2E_ARTIFACT_ROOT ('osc-hook-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $script:hookHashes | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'global-hook-hashes-before.json')
        $script:pwsh = (Get-Command pwsh.exe -ErrorAction Stop | Select-Object -First 1).Source
        $script:emitter = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Emit-OscActivity.ps1')).Path
        $mock = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $code = "& '$($mock.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\acp.log'"
        $profile = '{' + [guid]::NewGuid().ToString('D') + '}'
        $environment = @{
            PATH = "$($script:target.InstallLocation);$env:PATH"
            WTCLI_PATH = $script:target.WtcliPath
            WT_WTCLI_PATH = $script:target.WtcliPath
        }
        $script:app = Start-Terminal -Package $script:package -PassFre $true -TimeoutSec 90 -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
            sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; tabLayoutVerticalWidth = 500
            agentSessionManagementEnabled = $true; autoErrorDetectionEnabled = $false; autoFixEnabled = $false
            startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            acpAgent = 'custom:osc-hook-fixture'
            acpCustomCommand = "`"$script:pwsh`" -NoProfile -EncodedCommand " +
                [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
            actions = @(@{ command = 'restartConnection'; keys = 'ctrl+alt+shift+r' })
            'warning.confirmOnClose' = 'never'; defaultProfile = $profile
            profiles = @{ defaults = @{
                environment = $environment; shellIntegrationEnabled = $false
                'compatibility.reloadEnvironmentVariables' = $false
            }; list = @(@{
                guid = $profile; name = 'OSC activity control'
                commandline = "`"$script:pwsh`" -NoLogo -NoProfile -NoExit"
                startingDirectory = $script:evidence
            }) }
        }
        $script:app.Launched | Should -BeTrue
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true -Force
        (Get-WtSetting -App $script:app -Key tabLayout) | Should -Be vertical
        $uiaRoot = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
        @{
            root = @{ name = $uiaRoot.Current.Name; class = $uiaRoot.Current.ClassName; offscreen = $uiaRoot.Current.IsOffscreen }
            children = @($uiaRoot.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition) |
                Select-Object -First 250 | ForEach-Object { @{
                    name = $_.Current.Name; id = $_.Current.AutomationId; class = $_.Current.ClassName
                    type = $_.Current.ControlType.ProgrammaticName; offscreen = $_.Current.IsOffscreen
                } })
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence 'initial-ui.json')
        $script:pipe = Get-WtaMasterPipe -App $script:app -TimeoutSec 60
        $script:listener = Start-WtEventListener -App $script:app -WaitForReady
        @{
            source_commit = $head; product_commit = $receipt.Commit; package = $script:package
            pid = $script:app.Pid; hwnd = $script:app.Hwnd; master = $script:pipe
            app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256; wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'build-proof.json')

        function Read-ActivitySnapshot {
            $result = Invoke-Wta -App $script:app -Raw -Arguments @(
                'sessions', 'list', '--json', '--master', $script:pipe, '--include-status', '--origin', 'all')
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            $snapshot = ConvertFrom-Json -InputObject $result.StdOut -NoEnumerate -ErrorAction Stop
            $snapshot.PSObject.Properties.Name | Should -Contain pane_statuses
            $snapshot
        }
        function Get-ActivityRow($Fixture) {
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            $rows = @($window.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                -not $_.Current.IsOffscreen
            })
            if ($Fixture) { @($rows | Where-Object { $_.Current.Name.StartsWith($Fixture.Title, [StringComparison]::Ordinal) }) }
            else { $rows }
        }
        function Get-ActivityRings($Row) {
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ProgressBar)
            @($Row.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0
            })
        }
        function Wait-Activity {
            param($Fixture, [string]$Status, [Nullable[bool]]$Spinner = $null)
            try {
                $result = Wait-Until -TimeoutSec 30 -IntervalSec 0.3 -Because "$($Fixture.Title) reaches $Status in the shared read model and Sidebar" -Condition {
                    $snapshot = Read-ActivitySnapshot
                    $sessions = @($snapshot.sessions | Where-Object {
                        $_.session_id -eq $Fixture.SessionId -and $_.provider_id -eq 'copilot'
                    })
                    $panes = @($snapshot.pane_statuses | Where-Object { [guid]$_.pane_id -eq [guid]$Fixture.Pane })
                    $allRows = @(Get-ActivityRow)
                    $rows = @($allRows | Where-Object { $_.Current.Name.StartsWith($Fixture.Title, [StringComparison]::Ordinal) })
                    $script:activityDiagnostic = @{
                        expected = $Status; session = $Fixture.SessionId; pane = $Fixture.Pane
                        sessions = $sessions; panes = $panes
                        rows = @($allRows | ForEach-Object { @{
                            name = $_.Current.Name; id = $_.Current.AutomationId
                            rings = @(Get-ActivityRings $_).Count
                        } })
                    }
                    if ($sessions.Count -ne 1 -or $sessions[0].status -ne $Status) { return $false }
                    if ($rows.Count -ne 1) { return $false }
                    $name = $rows[0].Current.Name
                    if ($Status -eq 'Ended') {
                        if ($sessions[0].pane_session_id -or @($panes | Where-Object status -In @('Working', 'Idle', 'Attention', 'Error')).Count) { return $false }
                        if ($name -match '(?<![\w\\/-])(Working|Idle|Complete|Waiting for input|Error)(?![\w\\/-])') { return $false }
                    }
                    else {
                        if ($panes.Count -ne 1 -or $panes[0].status -ne $Status -or
                            [guid]$sessions[0].pane_session_id -ne [guid]$Fixture.Pane) { return $false }
                        $label = switch ($Status) {
                            Working { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusWorking }
                            Attention { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusAttention }
                            Error { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusError }
                            Idle { '(?<![\w\\/-])(Idle|Complete)(?![\w\\/-])' }
                        }
                        if ($name -notmatch $label) { return $false }
                    }
                    if ($null -ne $Spinner -and ((@(Get-ActivityRings $rows[0]).Count -gt 0) -ne $Spinner)) { return $false }
                    [pscustomobject]@{ snapshot = $snapshot; name = $name; state = $Status }
                }
            }
            catch {
                $script:activityDiagnostic | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (
                    Join-Path $script:evidence "$($Fixture.SessionId)-failure.json")
                throw
            }
            $result | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (
                Join-Path $script:evidence "$($Fixture.SessionId)-$Status-$([guid]::NewGuid().ToString('N')).json")
            $result
        }
        function Invoke-ActivityClient {
            param([string[]]$Arguments, $InputObject, [string]$Pane, [switch]$ExpectInvalidObservation)
            $psi = [Diagnostics.ProcessStartInfo]::new($script:app.WtcliPath)
            foreach ($argument in $Arguments) { $psi.ArgumentList.Add($argument) }
            $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
            $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
            $psi.Environment['WT_SESSION'] = $Pane
            $psi.Environment['WT_COM_CLSID'] = $script:app.ComClsid
            $process = [Diagnostics.Process]::Start($psi)
            try {
                $stdout = $process.StandardOutput.ReadToEndAsync()
                $stderr = $process.StandardError.ReadToEndAsync()
                $process.StandardInput.WriteLine(($InputObject | ConvertTo-Json -Depth 20 -Compress))
                $process.StandardInput.Close()
                if (-not $process.WaitForExit(15000)) { throw 'Owned protocol fixture did not exit.' }
                $errorText = $stderr.GetAwaiter().GetResult()
                if ($ExpectInvalidObservation) {
                    $process.ExitCode | Should -Be 1
                    $errorText | Should -Match '0x80070057' -Because 'host observations are not accepted as external SendEvent broadcasts'
                    @{ exit_code = $process.ExitCode; stderr = $errorText; event = $InputObject } |
                        ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $script:evidence 'rejected-old-observation.json')
                }
                else { $process.ExitCode | Should -Be 0 -Because $errorText }
                $stdout.GetAwaiter().GetResult() | Out-Null
            }
            finally {
                if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit(3000) | Out-Null }
                $process.Dispose()
            }
        }
        function Send-ActivityHook {
            param($Fixture, [string]$Event, [string]$Id = $Fixture.SessionId, [hashtable]$Extra = @{})
            $payload = @{ session_id = $Id; cwd = $script:evidence; timestamp = [datetimeoffset]::UtcNow.ToUnixTimeMilliseconds() }
            foreach ($key in $Extra.Keys) { $payload[$key] = $Extra[$key] }
            $before = $script:listener.Events.Count
            Invoke-ActivityClient -Arguments @('agent-hook', '--cli-source', 'copilot', '--event', $Event) -InputObject $payload -Pane $Fixture.Pane
            Wait-Until -TimeoutSec 15 -Because "the native $Event hook crosses COM for the exact pane/session" -Condition {
                @(Get-WtEvents -Listener $script:listener | Select-Object -Skip $before | Where-Object {
                    $_.method -eq 'agent_event' -and $_.params.event -eq $Event -and
                    $_.params.agent_session_id -eq $Id -and [guid]$_.params.pane_id -eq [guid]$Fixture.Pane
                }).Count -gt 0
            } | Out-Null
        }
        function New-ActivityFixture {
            param([string]$Name, [switch]$Unbound)
            $title = "OSC1265-$Name-" + [guid]::NewGuid().ToString('N').Substring(0, 8)
            $inbox = Join-Path $script:evidence $title
            New-Item -ItemType Directory -Path $inbox | Out-Null
            $ready = Join-Path $inbox 'ready.json'
            $command = "`"$script:pwsh`" -NoLogo -NoProfile -File `"$script:emitter`" -Inbox `"$inbox`" -Ready `"$ready`" -Title $title"
            $arguments = @('new-tab', '--window-id', [string]$script:app.WindowId, '-c', $command, '-n', $title, '-d', $script:evidence)
            if (-not $Unbound) { $arguments += @('--agent-provider', 'copilot') }
            $created = Invoke-WtCli -App $script:app -Arguments $arguments
            $fixture = [pscustomobject]@{
                Pane = [string]$created.session_id; Tab = [string]$created.tab_id
                Title = $title; Inbox = $inbox; Ready = $ready
                SessionId = [guid]::NewGuid().ToString('D')
            }
            $script:fixtures.Add($fixture)
            $boot = Wait-Until -TimeoutSec 20 -Because 'the owned native child reports its exact package-local routing' -Condition {
                if (Test-Path -LiteralPath $ready) {
                    $value = Get-Content -LiteralPath $ready -Raw | ConvertFrom-Json
                    if ([guid]$value.pane -eq [guid]$fixture.Pane) { $value }
                }
            }
            [guid]$boot.clsid | Should -Be ([guid]$script:app.ComClsid)
            $boot.wtcli | Should -Be $script:app.WtcliPath
            $boot.wta | Should -Be $script:app.WtaPath
            if (-not $Unbound) { Send-ActivityHook $fixture 'agent.session.start' }
            $fixture
        }
        function Send-ActivityOsc {
            param($Fixture, [int]$State, [switch]$Noop, [switch]$Unbound)
            $token = [guid]::NewGuid().ToString('N')
            $request = @{ token = $token; kind = $(if ($Noop) { 'noop' } else { 'progress' }); state = $State }
            $temp = Join-Path $Fixture.Inbox "$token.pending"
            $path = Join-Path $Fixture.Inbox "$token.request.json"
            $before = $script:listener.Events.Count
            $request | ConvertTo-Json | Set-Content -LiteralPath $temp
            Move-Item -LiteralPath $temp -Destination $path
            Wait-Until -TimeoutSec 15 -Because 'the native child completed the requested stdout action' -Condition {
                Test-Path -LiteralPath (Join-Path $Fixture.Inbox "$token.reply.json")
            } | Out-Null
            if (-not $Noop -and -not $Unbound) {
                $event = Wait-Until -TimeoutSec 15 -Because 'parsed OSC crosses the native observation channel for the emitting pane' -Condition {
                    @(Get-WtEvents -Listener $script:listener | Select-Object -Skip $before | Where-Object {
                        $_.method -eq 'native_agent_observation' -and $_.params.kind -eq 'progress' -and
                        [guid]$_.params.pane_id -eq [guid]$Fixture.Pane -and $_.params.progress_state -eq $State
                    }) | Select-Object -Last 1
                }
                return $event
            }
        }
        function Assert-ActivityStable {
            param($Fixture, [string]$Status)
            $watch = [Diagnostics.Stopwatch]::StartNew()
            do {
                $rows = @((Read-ActivitySnapshot).sessions | Where-Object session_id -EQ $Fixture.SessionId)
                $rows | Should -HaveCount 1
                $rows[0].status | Should -Be $Status
                Start-Sleep -Milliseconds 150
            } while ($watch.Elapsed.TotalSeconds -lt 1)
            Wait-Activity $Fixture $Status | Out-Null
        }
    }

    AfterAll {
        if ($script:listener) {
            $script:listener.Events | ConvertTo-Json -Depth 20 |
                Set-Content -LiteralPath (Join-Path $script:evidence 'events.json')
            Stop-WtEventListener -Listener $script:listener
        }
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($script:target) { Assert-WtPackageInactive -App $script:target }
        foreach ($path in $script:hookHashes.Keys) {
            $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
            $actual | Should -Be $script:hookHashes[$path] -Because 'private E2E must never modify global agent hooks'
        }
        if ($script:evidence) {
            @{ verified = $true; files = $script:hookHashes.Keys } | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $script:evidence 'global-hooks-unchanged.json')
        }
    }

    It 'OSC Working overrides hook Idle' -Tag 'OscHookWorking' {
        $a = New-ActivityFixture busy
        $b = New-ActivityFixture sibling
        Send-ActivityHook $a 'agent.prompt.submit'
        Send-ActivityOsc $a 3 | Out-Null
        Send-ActivityHook $a 'agent.stop'
        Wait-Activity $a Working -Spinner $true | Out-Null
        Wait-Activity $b Idle -Spinner $false | Out-Null
        Send-ActivityHook $a 'agent.stop'
        Assert-ActivityStable $a Working
        Send-ActivityOsc $a 0 | Out-Null
        Wait-Activity $a Idle -Spinner $false | Out-Null
    }

    It 'OSC Idle overrides hook Working' {
        $a = New-ActivityFixture clear
        Send-ActivityHook $a 'agent.prompt.submit'
        Send-ActivityOsc $a 0 -Noop
        Wait-Activity $a Working -Spinner $false | Out-Null
        Send-ActivityOsc $a 3 | Out-Null
        Wait-Activity $a Working -Spinner $true | Out-Null
        Send-ActivityOsc $a 0 | Out-Null
        Wait-Activity $a Idle -Spinner $false | Out-Null
        Send-ActivityHook $a 'agent.tool.finished'
        Assert-ActivityStable $a Idle
        Send-ActivityOsc $a 0 | Out-Null
        Wait-Activity $a Idle -Spinner $false | Out-Null
    }

    It 'OSC activity preserves attention errors and ended sessions' {
        $a = New-ActivityFixture guarded
        Send-ActivityHook $a 'agent.prompt.submit'
        Send-ActivityHook $a 'agent.notification' -Extra @{ notification_type = 'permission_prompt'; message = 'E2E approval required' }
        foreach ($state in @(0, 3)) {
            Send-ActivityOsc $a $state | Out-Null
            Wait-Activity $a Attention | Out-Null
        }
        Send-ActivityHook $a 'agent.error' -Extra @{ error = 'E2E controlled agent error' }
        foreach ($state in @(0, 3)) {
            Send-ActivityOsc $a $state | Out-Null
            Wait-Activity $a Error | Out-Null
        }
        Send-ActivityHook $a 'agent.session.end'
        Send-ActivityOsc $a 3 | Out-Null
        Wait-Activity $a Ended | Out-Null
        Assert-ActivityStable $a Ended
    }

    It 'OSC activity remains pane and connection scoped' {
        $a = New-ActivityFixture restart
        $b = New-ActivityFixture guard
        Send-ActivityHook $a 'agent.prompt.submit'
        $old = Send-ActivityOsc $a 0
        Wait-Activity $a Idle | Out-Null
        Send-ActivityHook $a 'agent.prompt.submit' -Id ([guid]::NewGuid().ToString('D'))
        Wait-Activity $a Idle | Out-Null
        $oldPane = $a.Pane
        $oldSession = $a.SessionId
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $oldPane) | Out-Null
        Set-WtWindowForeground -App $script:app | Should -BeTrue
        Send-WtWindowKey -App $script:app -Vk 0x52 -Ctrl -Alt -Shift -RequireForeground | Out-Null
        $replacement = Wait-Until -TimeoutSec 20 -Because 'public restart replaces the connection GUID in the same tab' -Condition {
            $panes = @(Get-WtPanes -App $script:app -TabId $a.Tab -WindowId $script:app.WindowId)
            if ($panes.Count -eq 1 -and [guid]$panes[0].session_id -ne [guid]$oldPane) { $panes[0] }
        }
        $a.Pane = [string]$replacement.session_id
        $a.SessionId = [guid]::NewGuid().ToString('D')
        Wait-Until -TimeoutSec 20 -Because 'the restarted native child acquired its new connection identity' -Condition {
            if (Test-Path -LiteralPath $a.Ready) {
                $boot = Get-Content -LiteralPath $a.Ready -Raw | ConvertFrom-Json
                [guid]$boot.pane -eq [guid]$a.Pane
            }
        } | Out-Null
        Send-ActivityHook $a 'agent.session.start'
        Send-ActivityHook $a 'agent.prompt.submit'
        Wait-Activity $a Working -Spinner $false | Out-Null
        $old.method | Should -BeExactly native_agent_observation
        [guid]$old.params.pane_id | Should -Be ([guid]$oldPane)
        $old.params.progress_state | Should -Be 0
        Invoke-ActivityClient -Arguments @('publish', '--existing-only', '--stdin') -InputObject $old -Pane $oldPane -ExpectInvalidObservation
        Send-ActivityHook ([pscustomobject]@{ Pane = $oldPane; SessionId = $oldSession }) 'agent.prompt.submit'
        Send-ActivityOsc $a 0 -Noop
        Assert-ActivityStable $a Working
        Wait-Activity $b Idle | Out-Null
        Send-ActivityOsc $a 0 | Out-Null
        Wait-Activity $a Idle -Spinner $false | Out-Null
        $plain = New-ActivityFixture plain -Unbound
        Send-ActivityOsc $plain 3 -Unbound
        Wait-Until -TimeoutSec 15 -Because 'the unbound shell progress reaches its real rendered spinner' -Condition {
            $rows = @(Get-ActivityRow $plain)
            $rows.Count -eq 1 -and @(Get-ActivityRings $rows[0]).Count -eq 1
        } | Out-Null
        $snapshot = Read-ActivitySnapshot
        @($snapshot.pane_statuses | Where-Object { [guid]$_.pane_id -eq [guid]$plain.Pane }) | Should -HaveCount 0
        $rows = @(Get-ActivityRow $plain)
        $rows | Should -HaveCount 1
        $rows[0].Current.Name | Should -Not -Match '(?<![\w\\/-])(Working|Idle|Complete|Waiting for input|Error)(?![\w\\/-])'
        Wait-Activity $a Idle | Out-Null
        Wait-Activity $b Idle | Out-Null
    }
}
