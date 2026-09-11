#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# WT_COM_CLSID stays fixed for ordinary COM clients. Hooks use WT_COM_HOOK_CLSID
# captured from real shells to reach the process-bound ROT factory. Absence
# checks observe processes only, so the oracle cannot itself reactivate Terminal.
# Run only with the selected package closed. No existing process is terminated by setup.

Describe 'Feature: process-bound hook endpoint lifecycle' -Tag 'Feature', 'HeadlessStartup' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Resolve-ItApp -Package (Get-ItTestPackage)
        $script:backedUp = $false
        $script:owned = [Collections.Generic.List[Diagnostics.Process]]::new()
        $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
        if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
            throw 'Run this lifecycle suite unelevated: elevated-state.json is a different persistence boundary.'
        }
        $script:app.WtcliPath | Should -Be (Join-Path $script:app.InstallLocation 'wtcli.exe')
        Test-Path -LiteralPath $script:app.WindowsTerminal | Should -BeTrue
        @(Get-WtProcessesForApp -App $script:app).Count | Should -Be 0 `
            -Because 'close the selected package before running; setup must not terminate user processes'

        # Resolve identity without Resolve-WtComClsid, whose discovery probes activate COM.
        $knownClsids = & (Get-Module ItE2E) { @($script:ItBrandClsids.Values) }
        [xml]$manifest = Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'AppxManifest.xml') -Raw
        $script:manifestXml = $manifest.OuterXml
        $classes = @($manifest.SelectNodes("//*[local-name()='ExeServer' and @Executable='WindowsTerminal.exe']/*[local-name()='Class']"))
        $clsids = @($classes | ForEach-Object { ([guid]$_.Id).ToString('B').ToUpperInvariant() } |
            Where-Object { $_ -in $knownClsids })
        $clsids.Count | Should -Be 1 -Because 'only the selected package protocol CLSID may be activated'
        $script:app.ComClsid = $clsids[0]
        $hash = (Get-FileHash -LiteralPath $script:app.WindowsTerminal -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_TERMINAL_SHA256) {
            $hash | Should -Be $env:ITE2E_EXPECTED_TERMINAL_SHA256 -Because 'the deployed Terminal must contain the intended fix'
        }
        $wtcliHash = (Get-FileHash -LiteralPath $script:app.WtcliPath -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_WTCLI_SHA256) {
            $wtcliHash | Should -Be $env:ITE2E_EXPECTED_WTCLI_SHA256 -Because 'the deployed hook client must contain the intended transport'
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path $root ("headless-startup-{0}" -f [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $script:legacyScript = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\legacy-hook-bundle\send-event.ps1')).Path
        @{
            package = $script:app.PackageFullName; executable = $script:app.WindowsTerminal
            sha256 = $hash; wtcliSha256 = $wtcliHash; clsid = $script:app.ComClsid
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json') -Encoding utf8

        function Get-HeadlessProcesses {
            @(Get-WtProcessesForApp -App $script:app | Where-Object Path -eq $script:app.WindowsTerminal)
        }

        function Register-HeadlessProcesses {
            foreach ($process in @(Get-HeadlessProcesses)) {
                if ($process.StartTime -ge $script:caseStarted -and $process.Id -notin @($script:owned.Id)) {
                    $null = $process.Handle # Retain the original kernel object across exit/PID reuse.
                    $script:owned.Add($process)
                }
            }
        }

        function Write-HeadlessEvidence {
            param([string]$Name, $Value)
            $Value | ConvertTo-Json -Depth 32 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$($script:caseId)-$Name.json") -Encoding utf8
        }

        function Get-SavedHeadlessLayout {
            $state = Get-WtStateObject -App $script:app
            function ConvertTo-SortedLayout($Value) {
                if ($Value -is [pscustomobject]) {
                    $sorted = [ordered]@{}
                    foreach ($key in @($Value.PSObject.Properties.Name | Sort-Object)) {
                        $sorted[$key] = ConvertTo-SortedLayout $Value.$key
                    }
                    return $sorted
                }
                if ($Value -is [array]) { return ,@($Value | ForEach-Object { ConvertTo-SortedLayout $_ }) }
                $Value
            }
            ConvertTo-Json -InputObject (ConvertTo-SortedLayout @($state.persistedWindowLayouts)) -Depth 32 -Compress
        }

        function Start-HeadlessCom {
            @(Get-HeadlessProcesses).Count | Should -Be 0
            try {
                $response = Invoke-WtCli -App $script:app -Arguments @('list-windows') -TimeoutSec 15
            }
            finally { Register-HeadlessProcesses }
            $response.PSObject.Properties.Name | Should -Contain 'windows'
            @($response.windows).Count | Should -Be 0 -Because 'COM activation must not open or restore a window'
            $processes = @(Get-HeadlessProcesses)
            $processes.Count | Should -Be 1 -Because 'explicit package COM activation must still create a real server'
            $process = $script:owned | Where-Object Id -eq $processes[0].Id | Select-Object -Last 1
            $command = (Get-CimInstance Win32_Process -Filter "ProcessId=$($process.Id)").CommandLine
            $command | Should -Match '(?i)\s-Embedding\s*$' -Because 'the package registration, not the test, must supply -Embedding'
            Write-HeadlessEvidence -Name "activation-$($process.Id)" -Value @{
                pid = $process.Id; started = $process.StartTime; command = $command; response = $response
            }
            $process.Refresh()
            $process.MainWindowHandle | Should -Be 0
            $process
        }

        function Assert-HeadlessSurvival {
            param([Diagnostics.Process]$Process, [switch]$NoWindow)
            $observation = @{ Exited = $false; WindowSeen = $false }
            $watch = [Diagnostics.Stopwatch]::StartNew()
            Wait-Until -TimeoutSec 10 -IntervalSec 0.1 -Because 'eight seconds of legitimate COM client use' -Condition {
                if ($Process.HasExited) { $observation.Exited = $true; return $true }
                $Process.Refresh()
                if ($NoWindow -and $Process.MainWindowHandle -ne 0) { $observation.WindowSeen = $true }
                $watch.Elapsed.TotalSeconds -ge 8
            } | Out-Null
            Write-HeadlessEvidence -Name "survival-$($Process.Id)" -Value @{
                pid = $Process.Id; observation = $observation
                observationSeconds = $watch.Elapsed.TotalSeconds
                lifetimeSeconds = ((Get-Date) - $Process.StartTime).TotalSeconds
            }
            $observation.Exited | Should -BeFalse -Because 'a legitimate headless COM client must not be disconnected by a startup timeout'
            $observation.WindowSeen | Should -BeFalse
            $Process.HasExited | Should -BeFalse
        }

        function Start-HeadlessInteractive {
            try {
                # Like Start-Terminal, retain package identity through shell activation;
                # executing the package binary directly is not an interactive app launch.
                Start-Process -FilePath 'explorer.exe' -ArgumentList "shell:AppsFolder\$($script:app.AppUserModelId)" | Out-Null
            }
            finally { Register-HeadlessProcesses }
        }

        function Assert-RestoredHeadlessWindows {
            param([Diagnostics.Process]$Process)
            Wait-Until -TimeoutSec 30 -IntervalSec 0.2 -Because 'a real restored Terminal HWND' -Condition {
                if ($Process.HasExited) { throw 'The original server exited during window restoration.' }
                $Process.Refresh()
                $Process.MainWindowHandle -ne 0
            } | Out-Null
            $Process.HasExited | Should -BeFalse
            $windows = @(Get-WtWindows -App $script:app)
            Write-HeadlessEvidence -Name 'restored-windows' -Value $windows
            $windows.Count | Should -Be $script:titles.Count -Because 'bare activation must restore saved windows without adding a default or replaying them'
            $restoredTitles = @()
            $restoredPanes = @()
            foreach ($window in $windows) {
                # list-windows reports the optional window name, not the active tab title.
                $tabs = @(Get-WtTabs -App $script:app -WindowId $window.window_id)
                $tabs.Count | Should -Be 1
                $restoredTitles += $tabs[0].title
                $panes = @(Get-WtPanes -App $script:app -WindowId $window.window_id -TabId $tabs[0].tab_id)
                $panes.Count | Should -Be 1
                (Get-WtPaneStatus -App $script:app -SessionId $panes[0].session_id).state | Should -Match 'run'
                $restoredPanes += $panes
            }
            Write-HeadlessEvidence -Name 'restored-tab-titles' -Value $restoredTitles
            foreach ($title in $script:titles) {
                @($restoredTitles | Where-Object { $_ -ceq $title }).Count | Should -Be 1
            }
            Wait-Until -TimeoutSec 40 -Because 'both restored tabs to connect to the deterministic ACP fixture' -Condition {
                $Process.HasExited | Should -BeFalse
                if (Test-Path -LiteralPath $script:requestLog) {
                    @([regex]::Matches((Get-Content -LiteralPath $script:requestLog -Raw), '\|session/new\|')).Count -eq $script:expectedAcpSessions
                }
            } | Out-Null
            (Get-Content -LiteralPath $script:requestLog -Raw) | Should -Not -Match '\|session/prompt\|'
            $restoredPanes
        }

        function Start-InteractiveRuntime {
            param([Diagnostics.Process]$Process)
            Start-HeadlessInteractive
            if (-not $Process) {
                $Process = Wait-Until -TimeoutSec 15 -Because 'the interactive Terminal process' -Condition {
                    Get-HeadlessProcesses | Select-Object -First 1
                }
            }
            Register-HeadlessProcesses
            $processId = $Process.Id
            $processStart = $Process.StartTime
            $Process = $script:owned | Where-Object { $_.Id -eq $processId -and $_.StartTime -eq $processStart } | Select-Object -Last 1
            $Process | Should -Not -BeNullOrEmpty -Because 'lifecycle checks must use the retained process handle, including after exit'
            $script:expectedAcpSessions += $script:titles.Count
            $panes = @(Assert-RestoredHeadlessWindows -Process $Process)
            $hookGuids = @()
            foreach ($pane in $panes) {
                $file = Join-Path $script:evidence "$($script:caseId)-$($pane.session_id)-runtime.txt"
                Send-WtInput -App $script:app -SessionId $pane.session_id `
                    -Text "echo %WT_COM_CLSID% > `"$file`" & echo %WT_COM_HOOK_CLSID% >> `"$file`""
                Send-WtKeys -App $script:app -SessionId $pane.session_id -Keys @('Enter')
                $values = @(Wait-Until -TimeoutSec 10 -Because 'the real shell to export both inherited COM identities' -Condition {
                    if (Test-Path -LiteralPath $file) {
                        $lines = @(Get-Content -LiteralPath $file | ForEach-Object { $_.Trim() })
                        if ($lines.Count -eq 2) { $lines }
                    }
                })
                $values[0] | Should -BeExactly $script:app.ComClsid -Because 'the normal COM identity must retain its exact fixed package semantics'
                $values[1] | Should -Match '^\{[0-9a-fA-F-]{36}\}$'
                $hookGuids += ([guid]$values[1]).ToString('B').ToUpperInvariant()
            }
            @($hookGuids | Sort-Object -Unique).Count | Should -Be 1 -Because 'all panes share the process-bound hook endpoint'
            $hookGuids[0] | Should -Not -Be $script:app.ComClsid
            Test-Path -LiteralPath "Registry::HKEY_CLASSES_ROOT\CLSID\$($hookGuids[0])" | Should -BeFalse
            $script:manifestXml | Should -Not -Match ([regex]::Escape($hookGuids[0].Trim('{}')))
            (Get-Content -LiteralPath $script:runtimeLog -Tail 1) |
                Should -Match ('^\d+\|' + [regex]::Escape($script:app.ComClsid) + '\|' + [regex]::Escape($hookGuids[0]) + '$') `
                    -Because 'the real SharedWta/ACP child must inherit both product-supplied identities'
            $runtimeApp = $script:app.PSObject.Copy()
            $runtimeApp.Pid = $Process.Id
            Write-HeadlessEvidence -Name "runtime-$($Process.Id)" -Value @{
                pid = $Process.Id; hookClsid = $hookGuids[0]; packageClsid = $script:app.ComClsid
            }
            [pscustomobject]@{
                Process = $Process; App = $runtimeApp; HookClsid = $hookGuids[0]; PaneId = $panes[0].session_id
            }
        }

        function Stop-InteractiveRuntime {
            param($Runtime)
            $process = $Runtime.Process
            $closed = [Collections.Generic.HashSet[long]]::new()
            Wait-Until -TimeoutSec 20 -IntervalSec 0.2 -Because 'all test windows to close gracefully' -Condition {
                if ($process.HasExited) { return $true }
                $process.Refresh()
                if ($process.MainWindowHandle -ne 0 -and $closed.Add($process.MainWindowHandle.ToInt64())) {
                    $process.CloseMainWindow() | Out-Null
                }
                $false
            } | Out-Null
            $process.ExitCode | Should -Be 0
            @(Get-HeadlessProcesses).Count | Should -Be 0
        }

        function Invoke-RuntimeHook {
            param($Runtime, [string]$SessionId, [switch]$Legacy, [switch]$CachedLegacy, [string]$EventType = 'agent.session.end')
            $environment = @{
                WT_COM_CLSID = $Runtime.App.ComClsid; WT_COM_HOOK_CLSID = $Runtime.HookClsid; WT_SESSION = $Runtime.PaneId
            }
            $payload = @{ session_id = $SessionId; cwd = $script:evidence; reason = 'user_exit' }
            $file = Join-Path $script:evidence "$SessionId.json"
            $prefix = if ($null -eq $Runtime.HookClsid) { 'Remove-Item Env:WT_COM_HOOK_CLSID -ErrorAction SilentlyContinue; ' } else { '' }
            $wtcli = $script:app.WtcliPath.Replace("'", "''")
            if ($CachedLegacy) {
                $payload | ConvertTo-Json -Compress | Set-Content -LiteralPath $file -Encoding utf8
                $environment.PATH = $script:app.InstallLocation + ';' + $env:PATH
                $environment.WTCLI_PATH = $script:app.WtcliPath
                $environment.WTA_HOOK_LOG_DIR = $script:evidence
                $powershell = (Get-Command powershell.exe).Source.Replace("'", "''")
                $command = $prefix + "Get-Content -Raw -LiteralPath '$($file.Replace("'", "''"))' | " +
                    "& '$powershell' -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass " +
                    "-File '$($script:legacyScript.Replace("'", "''"))' -CliSource copilot '$EventType'; exit `$LASTEXITCODE"
            }
            elseif ($Legacy) {
                @{ cli_source = 'copilot'; agent_session_id = $SessionId; payload = $payload } |
                    ConvertTo-Json -Compress | Set-Content -LiteralPath $file -Encoding utf8
                $paneArgument = if ($Runtime.PaneId) { " -p '$($Runtime.PaneId)'" } else { '' }
                $command = $prefix + "`$payload=Get-Content -Raw -LiteralPath '$($file.Replace("'", "''"))'; " +
                    "& '$wtcli' --json send-event -e '$EventType'$paneArgument `$payload; exit `$LASTEXITCODE"
            }
            else {
                $payload | ConvertTo-Json -Compress | Set-Content -LiteralPath $file -Encoding utf8
                $command = $prefix + "Get-Content -Raw -LiteralPath '$($file.Replace("'", "''"))' | " +
                    "& '$wtcli' agent-hook --cli-source copilot --event '$EventType'; exit `$LASTEXITCODE"
            }
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
            $result = Invoke-Native -FilePath (Get-Command pwsh).Source -Environment $environment -TimeoutSec 10 `
                -Arguments @('-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded)
            Write-HeadlessEvidence -Name "hook-$SessionId" -Value @{
                normalClsid = $Runtime.App.ComClsid; hookClsid = $Runtime.HookClsid
                transport = if ($CachedLegacy) { 'cached-legacy' } elseif ($Legacy) { 'legacy' } else { 'native' }
                result = $result
            }
            $result.TimedOut | Should -BeFalse
            $result
        }

        function Assert-NoReplacementProcess {
            param([Diagnostics.Process]$AllowedProcess)
            $unexpected = [Collections.Generic.HashSet[int]]::new()
            $watch = [Diagnostics.Stopwatch]::StartNew()
            Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'a bounded process-only observation after hook completion' -Condition {
                foreach ($process in @(Get-HeadlessProcesses)) {
                    if (-not $AllowedProcess -or $process.Id -ne $AllowedProcess.Id -or $process.StartTime -ne $AllowedProcess.StartTime) {
                        $null = $unexpected.Add($process.Id)
                    }
                }
                $unexpected.Count -gt 0 -or $watch.Elapsed.TotalSeconds -ge 3
            } | Out-Null
            Write-HeadlessEvidence -Name 'unexpected-processes' -Value @($unexpected)
            $unexpected.Count | Should -Be 0 -Because 'missing, invalid, or stale hook endpoints must never activate the package class'
            if ($AllowedProcess) { $AllowedProcess.HasExited | Should -BeFalse }
        }

        Backup-WtConfig -App $script:app
        $script:backedUp = $true
        $script:originalHashes = @{}
        foreach ($file in @($script:app.SettingsPath, $script:app.StatePath)) {
            $script:originalHashes[$file] = if (Test-Path -LiteralPath $file) { (Get-FileHash -LiteralPath $file).Hash } else { $null }
        }
        # Restoring layouts can clean up persisted scrollback. Preserve that state too.
        $script:bufferBackup = Join-Path $script:evidence 'saved-buffers'
        New-Item -ItemType Directory -Path $script:bufferBackup | Out-Null
        $script:bufferHashes = @{}
        foreach ($file in @(Get-ChildItem -LiteralPath $script:app.LocalStateDir -Filter 'buffer_*.txt' -File)) {
            Copy-Item -LiteralPath $file.FullName -Destination $script:bufferBackup
            $script:bufferHashes[$file.Name] = (Get-FileHash -LiteralPath $file.FullName).Hash
        }
    }

    BeforeEach {
        $script:caseStarted = $null
        @(Get-HeadlessProcesses).Count | Should -Be 0
        $script:caseStarted = Get-Date
        $script:caseId = [guid]::NewGuid().ToString('N')
        $script:requestLog = Join-Path $script:evidence "$($script:caseId)-acp.log"
        $script:runtimeLog = Join-Path $script:evidence "$($script:caseId)-acp-runtime.log"
        $script:expectedAcpSessions = 0
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $invocation = "Add-Content -LiteralPath '$($script:runtimeLog.Replace("'", "''"))' -Value (`"`$PID|`" + `$env:WT_COM_CLSID + '|' + `$env:WT_COM_HOOK_CLSID); " +
            "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $profile = [guid]::NewGuid().ToString('B')
        $script:titles = @("E2E-A-$($script:caseId)", "E2E-B-$($script:caseId)")
        @{
            defaultProfile = $profile; firstWindowPreference = 'persistedLayout'
            'compatibility.allowHeadless' = $false; confirmCloseAllTabs = $false
            acpAgent = 'custom:headless-fixture'; acpCustomCommand = "pwsh -NoProfile -EncodedCommand $encoded"
            autoFixEnabled = $false; autoErrorDetectionEnabled = $false
            profiles = @{ list = @(@{
                guid = $profile; name = 'Headless lifecycle fixture'
                commandline = "`"$env:ComSpec`" /d /k echo ITE2E-restored"
                startingDirectory = $script:evidence; suppressApplicationTitle = $true
            }) }
        } | ConvertTo-Json -Depth 32 | Set-Content -LiteralPath $script:app.SettingsPath -Encoding utf8
        $script:layoutFixture = @($script:titles | ForEach-Object {
            @{ tabLayout = @(@{ action = 'newTab'; profile = $profile; tabTitle = $_; suppressApplicationTitle = $true }) }
        })
        @{
            agentFreCompleted = $true; persistedWindowLayouts = $script:layoutFixture
        } | ConvertTo-Json -Depth 32 | Set-Content -LiteralPath $script:app.StatePath -Encoding utf8
        $script:savedLayout = Get-SavedHeadlessLayout
        Initialize-LogOffsets -App $script:app | Out-Null
    }

    AfterEach {
        if ($script:caseStarted) {
            Register-HeadlessProcesses
            Write-HeadlessEvidence -Name 'processes-before-cleanup' -Value @(
                foreach ($process in $script:owned) {
                    @{
                        pid = $process.Id; started = $process.StartTime; exited = $process.HasExited
                        exitCode = if ($process.HasExited) { $process.ExitCode } else { $null }
                    }
                }
            )
            foreach ($process in $script:owned) {
                if (-not $process.HasExited) {
                    $current = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
                    if ($current -and $current.Path -eq $script:app.WindowsTerminal -and $current.StartTime -eq $process.StartTime) {
                        $cleanup = $script:app.PSObject.Copy()
                        $cleanup.Pid = $process.Id
                        Stop-Terminal -App $cleanup -RestoreSettings $false -GraceSec 2
                    }
                }
            }
            Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart |
                Set-Content -LiteralPath (Join-Path $script:evidence "$($script:caseId)-terminal.log") -Encoding utf8
            @(Get-HeadlessProcesses).Count | Should -Be 0 -Because 'test-owned processes must stop before the next fixture or config restoration'
            $script:caseStarted = $null
        }
    }

    AfterAll {
        if ($script:backedUp) {
            @(Get-HeadlessProcesses).Count | Should -Be 0 -Because 'restoring config while Terminal is alive risks overwriting the user state'
            Restore-WtConfig -App $script:app
            if ($script:bufferBackup -and (Test-Path -LiteralPath $script:bufferBackup)) {
                Get-ChildItem -LiteralPath $script:bufferBackup -File | Copy-Item -Destination $script:app.LocalStateDir -Force
                foreach ($name in $script:bufferHashes.Keys) {
                    (Get-FileHash -LiteralPath (Join-Path $script:app.LocalStateDir $name)).Hash | Should -Be $script:bufferHashes[$name]
                }
                Remove-Item -LiteralPath $script:bufferBackup -Recurse -Force
            }
            foreach ($file in $script:originalHashes.Keys) {
                if ($null -eq $script:originalHashes[$file]) { Test-Path -LiteralPath $file | Should -BeFalse }
                else { (Get-FileHash -LiteralPath $file).Hash | Should -Be $script:originalHashes[$file] }
            }
            @{
                verified = $true; configurationSha256 = $script:originalHashes; bufferSha256 = $script:bufferHashes
            } | ConvertTo-Json -Depth 8 |
                Set-Content -LiteralPath (Join-Path $script:evidence 'restoration.json') -Encoding utf8
        }
        foreach ($process in $script:owned) { $process.Dispose() }
    }

    It 'Live hooks use the owning process hook endpoint' {
        $runtime = Start-InteractiveRuntime
        $listener = Start-WtEventListener -App $runtime.App -WaitForReady
        try {
            foreach ($transport in @('native', 'legacy', 'cached')) {
                $sessionId = "live-$transport-$($script:caseId)"
                $result = Invoke-RuntimeHook -Runtime $runtime -SessionId $sessionId `
                    -Legacy:($transport -eq 'legacy') -CachedLegacy:($transport -eq 'cached')
                $result.ExitCode | Should -Be 0
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 10 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $sessionId
                }
                $event.params.event | Should -Be 'agent.session.end'
                $event.params.pane_id | Should -Be $runtime.PaneId
                $event.params.cli_source | Should -Be 'copilot'
            }
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Late shutdown hooks cannot reactivate Terminal or erase its layout' {
        $runtime = Start-InteractiveRuntime
        Stop-InteractiveRuntime -Runtime $runtime
        (Get-WtStateObject -App $script:app).persistedWindowLayouts | Should -Not -BeNullOrEmpty
        $layout = Get-SavedHeadlessLayout
        $stateHash = (Get-FileHash -LiteralPath $script:app.StatePath).Hash
        Write-HeadlessEvidence -Name 'closed-layout' -Value @{ layout = $layout; stateSha256 = $stateHash }
        foreach ($variant in @(
            @{ Name = 'stale'; Key = $runtime.HookClsid }, @{ Name = 'repeat'; Key = $runtime.HookClsid },
            @{ Name = 'missing'; Key = $null }, @{ Name = 'invalid'; Key = 'not-a-guid' }
        )) {
            $target = $runtime.PSObject.Copy()
            $target.HookClsid = $variant.Key
            $native = Invoke-RuntimeHook -Runtime $target -SessionId "closed-native-$($variant.Name)-$($script:caseId)"
            $native.ExitCode | Should -Be 0 -Because 'native hooks remain best-effort after Terminal exits'
            $native.StdOut | Should -BeNullOrEmpty
            $native.StdErr | Should -BeNullOrEmpty
            $legacy = Invoke-RuntimeHook -Runtime $target -SessionId "closed-legacy-$($variant.Name)-$($script:caseId)" -Legacy
            $legacy.ExitCode | Should -Not -Be 0 -Because 'agent.* send-event must never fall back to the valid package class'
            $cached = Invoke-RuntimeHook -Runtime $target -SessionId "closed-cached-$($variant.Name)-$($script:caseId)" -CachedLegacy
            $cached.ExitCode | Should -Be 0 -Because 'the frozen script must keep its quiet best-effort contract when its client cannot connect'
            $cached.StdOut | Should -BeNullOrEmpty
            $cached.StdErr | Should -BeNullOrEmpty
            Assert-NoReplacementProcess
            Get-SavedHeadlessLayout | Should -BeExactly $layout
            (Get-FileHash -LiteralPath $script:app.StatePath).Hash | Should -Be $stateHash
        }
    }

    It 'Restarted Terminal rejects hooks from the previous process' {
        $old = Start-InteractiveRuntime
        Stop-InteractiveRuntime -Runtime $old
        Set-WtState -App $script:app -Key 'persistedWindowLayouts' -Value $script:layoutFixture | Out-Null
        $current = Start-InteractiveRuntime
        $current.App.ComClsid | Should -BeExactly $old.App.ComClsid
        $current.HookClsid | Should -Not -Be $old.HookClsid
        $windows = @(Get-WtWindows -App $current.App).window_id -join ','
        $listener = Start-WtEventListener -App $current.App -WaitForReady
        try {
            $blockedIds = @()
            foreach ($variant in @(
                @{ Name = 'old'; Key = $old.HookClsid },
                @{ Name = 'missing'; Key = $null }, @{ Name = 'invalid'; Key = 'not-a-guid' }
            )) {
                $target = $old.PSObject.Copy()
                $target.HookClsid = $variant.Key
                # Use a current pane so stale routing cannot hide an endpoint fallback.
                $target.PaneId = $current.PaneId
                $nativeId = "$($variant.Name)-native-$($script:caseId)"
                $legacyId = "$($variant.Name)-legacy-$($script:caseId)"
                $cachedId = "$($variant.Name)-cached-$($script:caseId)"
                $blockedIds += @($nativeId, $legacyId, $cachedId)
                (Invoke-RuntimeHook -Runtime $target -SessionId $nativeId).ExitCode | Should -Be 0
                (Invoke-RuntimeHook -Runtime $target -SessionId $legacyId -Legacy).ExitCode | Should -Not -Be 0
                (Invoke-RuntimeHook -Runtime $target -SessionId $cachedId -CachedLegacy).ExitCode | Should -Be 0
            }
            $query = Invoke-Native -FilePath $script:app.WtcliPath -Arguments @('--json', 'list-windows') `
                -Environment @{ WT_COM_CLSID = $old.App.ComClsid; WT_COM_HOOK_CLSID = $old.HookClsid } -TimeoutSec 10
            $query.ExitCode | Should -Be 0 -Because 'ordinary clients retain the fixed class even when their separate hook identity is stale'
            Assert-NoReplacementProcess -AllowedProcess $current.Process
            $control = "current-$($script:caseId)"
            (Invoke-RuntimeHook -Runtime $current -SessionId $control).ExitCode | Should -Be 0
            Wait-WtEvent -Listener $listener -TimeoutSec 10 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $control
            } | Out-Null
            @(Get-WtEvents -Listener $listener -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -in $blockedIds
            }).Count | Should -Be 0 -Because 'old, missing, and invalid hook endpoints must not fall back to the live normal COM server'
            (@(Get-WtWindows -App $current.App).window_id -join ',') | Should -BeExactly $windows
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Explicit COM clients can keep a headless server alive' {
        $process = Start-HeadlessCom
        $listener = Start-WtEventListener -App $script:app -WaitForReady
        try {
            Assert-HeadlessSurvival -Process $process -NoWindow
            $listener.Process.HasExited | Should -BeFalse
            $client = [pscustomobject]@{ App = $script:app; HookClsid = 'not-a-guid'; PaneId = '' }
            foreach ($topic in @("explicit-client-$($script:caseId)", 'Agent.session.end')) {
                $sessionId = "generic-$([guid]::NewGuid().ToString('N'))"
                (Invoke-RuntimeHook -Runtime $client -SessionId $sessionId -Legacy -EventType $topic).ExitCode |
                    Should -Be 0 -Because 'generic and differently-cased topics retain normal COM routing regardless of the hook key'
                Wait-WtEvent -Listener $listener -TimeoutSec 10 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.event -ceq $topic -and $_.params.agent_session_id -eq $sessionId
                } | Out-Null
            }
            @(Get-WtWindows -App $script:app).Count | Should -Be 0
            Get-SavedHeadlessLayout | Should -BeExactly $script:savedLayout
            Test-Path -LiteralPath $script:requestLog | Should -BeFalse
        }
        finally { Stop-WtEventListener -Listener $listener }
    }

    It 'Interactive activation restores the saved windows (<Launch>)' -ForEach @(
        @{ Launch = 'ordinary' }, @{ Launch = 'deferred' }
    ) {
        $process = if ($Launch -eq 'deferred') { Start-HeadlessCom } else { $null }
        $runtime = Start-InteractiveRuntime -Process $process
        if ($process) {
            $runtime.Process.Id | Should -Be $process.Id
            $process.HasExited | Should -BeFalse -Because 'deferred activation must restore into the original COM server'
        }
        $before = @(Get-WtWindows -App $runtime.App).window_id -join ','
        (@(Get-WtWindows -App $runtime.App).window_id -join ',') | Should -BeExactly $before
    }
}
