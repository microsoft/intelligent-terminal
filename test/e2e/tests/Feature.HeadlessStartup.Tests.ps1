#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real package-local wtcli -> COM ExeServer activation, not a synthetic -Embedding launch.
# Process handles (never COM polling) observe expiry, so the oracle cannot reactivate Terminal.
# Run only with the selected package closed. No existing process is terminated by setup.

Describe 'Feature: headless COM startup lifecycle' -Tag 'Feature', 'HeadlessStartup' {
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
        $classes = @($manifest.SelectNodes("//*[local-name()='ExeServer' and @Executable='WindowsTerminal.exe']/*[local-name()='Class']"))
        $clsids = @($classes | ForEach-Object { ([guid]$_.Id).ToString('B').ToUpperInvariant() } |
            Where-Object { $_ -in $knownClsids })
        $clsids.Count | Should -Be 1 -Because 'only the selected package protocol CLSID may be activated'
        $script:app.ComClsid = $clsids[0]
        $hash = (Get-FileHash -LiteralPath $script:app.WindowsTerminal -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_TERMINAL_SHA256) {
            $hash | Should -Be $env:ITE2E_EXPECTED_TERMINAL_SHA256 -Because 'the deployed Terminal must contain the intended fix'
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path $root ("headless-startup-{0}" -f [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        @{
            package = $script:app.PackageFullName; executable = $script:app.WindowsTerminal
            sha256 = $hash; clsid = $script:app.ComClsid
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
            $processes.Count | Should -Be 1 -Because 'successful COM activation must create a real server before expiry'
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

        function Assert-HeadlessExpiry {
            param([Diagnostics.Process]$Process)
            $observation = @{ WindowSeen = $false }
            $exited = Test-Until -TimeoutSec 12 -IntervalSec 0.1 -Condition {
                if ($Process.HasExited) { return $true }
                $Process.Refresh()
                if ($Process.MainWindowHandle -ne 0) { $observation.WindowSeen = $true }
                $false
            }
            $lifetime = if ($exited) { ($Process.ExitTime - $Process.StartTime).TotalSeconds } else { $null }
            $log = Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart
            $started = [regex]::Match($log, "\[(?<time>[^\]]+)\] headless COM activation started pid=$($Process.Id) window_timeout_ms=5000")
            $ended = [regex]::Match($log, "\[(?<time>[^\]]+)\] headless COM activation exiting without a window pid=$($Process.Id)\b")
            $timerSeconds = if ($started.Success -and $ended.Success) {
                ([DateTimeOffset]::Parse($ended.Groups['time'].Value) - [DateTimeOffset]::Parse($started.Groups['time'].Value)).TotalSeconds
            } else { $null }
            Write-HeadlessEvidence -Name "expiry-$($Process.Id)" -Value @{
                pid = $Process.Id; exited = $exited; lifetimeSeconds = $lifetime
                windowSeen = $observation.WindowSeen; timerSeconds = $timerSeconds
                timerStarted = $started.Groups['time'].Value; timerExited = $ended.Groups['time'].Value
            }
            $exited | Should -BeTrue -Because 'an unused COM server must exit without a test-initiated close or kill'
            $observation.WindowSeen | Should -BeFalse
            $Process.ExitCode | Should -Be 0 -Because 'a crash must not satisfy the timeout oracle'
            $lifetime | Should -BeGreaterOrEqual 4.5 -Because 'the five-second grace period must not be an immediate COM rejection'
            $lifetime | Should -BeLessOrEqual 12 -Because 'startup overhead may vary, but the server must not leak indefinitely'
            $started.Success | Should -BeTrue
            $ended.Success | Should -BeTrue
            $timerSeconds | Should -BeGreaterOrEqual 4.5 -Because 'slow process initialization must not disguise an immediate timer expiry'
            $timerSeconds | Should -BeLessOrEqual 10
            @(Get-HeadlessProcesses).Count | Should -Be 0
        }

        function Assert-HeadlessSurvival {
            param([Diagnostics.Process]$Process, [switch]$NoWindow)
            $observation = @{ Exited = $false; WindowSeen = $false }
            Wait-Until -TimeoutSec 10 -IntervalSec 0.1 -Because 'eight seconds after the original server started' -Condition {
                if ($Process.HasExited) { $observation.Exited = $true; return $true }
                $Process.Refresh()
                if ($NoWindow -and $Process.MainWindowHandle -ne 0) { $observation.WindowSeen = $true }
                (Get-Date) -ge $Process.StartTime.AddSeconds(8)
            } | Out-Null
            Write-HeadlessEvidence -Name "survival-$($Process.Id)" -Value @{
                pid = $Process.Id; observation = $observation
                lifetimeSeconds = ((Get-Date) - $Process.StartTime).TotalSeconds
            }
            $observation.Exited | Should -BeFalse -Because 'this legitimate server must survive its original startup deadline'
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
            $windows.Count | Should -Be 2 -Because 'bare activation must restore both saved windows without adding a default or replaying them'
            $restoredTitles = @()
            foreach ($window in $windows) {
                # list-windows reports the optional window name, not the active tab title.
                $tabs = @(Get-WtTabs -App $script:app -WindowId $window.window_id)
                $tabs.Count | Should -Be 1
                $restoredTitles += $tabs[0].title
                $panes = @(Get-WtPanes -App $script:app -WindowId $window.window_id -TabId $tabs[0].tab_id)
                $panes.Count | Should -Be 1
                (Get-WtPaneStatus -App $script:app -SessionId $panes[0].session_id).state | Should -Match 'run'
            }
            Write-HeadlessEvidence -Name 'restored-tab-titles' -Value $restoredTitles
            foreach ($title in $script:titles) {
                @($restoredTitles | Where-Object { $_ -ceq $title }).Count | Should -Be 1
            }
            Wait-Until -TimeoutSec 40 -Because 'both restored tabs to connect to the deterministic ACP fixture' -Condition {
                $Process.HasExited | Should -BeFalse
                if (Test-Path -LiteralPath $script:requestLog) {
                    @([regex]::Matches((Get-Content -LiteralPath $script:requestLog -Raw), '\|session/new\|')).Count -eq 2
                }
            } | Out-Null
            (Get-Content -LiteralPath $script:requestLog -Raw) | Should -Not -Match '\|session/prompt\|'
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
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
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
        @{
            agentFreCompleted = $true
            persistedWindowLayouts = @($script:titles | ForEach-Object {
                @{ tabLayout = @(@{ action = 'newTab'; profile = $profile; tabTitle = $_; suppressApplicationTitle = $true }) }
            })
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

    It 'Headless COM startup expires without leaking a process' {
        Set-WtState -App $script:app -Key 'persistedWindowLayouts' -Value @() | Out-Null
        foreach ($cycle in 1..2) {
            $process = Start-HeadlessCom
            Assert-HeadlessExpiry -Process $process
        }
    }

    It 'Headless COM expiry preserves the saved layout' {
        foreach ($cycle in 1..2) {
            $process = Start-HeadlessCom
            Assert-HeadlessExpiry -Process $process
            Get-SavedHeadlessLayout | Should -BeExactly $script:savedLayout `
                -Because 'a headless shutdown cannot replace the previous two-window session with an empty layout'
        }
        Test-Path -LiteralPath $script:requestLog | Should -BeFalse -Because 'headless COM activation must not restore agent sessions'
    }

    It 'Interactive activation cancels the headless timeout and restores windows' {
        $process = Start-HeadlessCom
        ((Get-Date) - $process.StartTime).TotalSeconds | Should -BeLessThan 4.5 -Because 'the interactive handoff must start before expiry'
        Start-HeadlessInteractive
        Assert-RestoredHeadlessWindows -Process $process
        Assert-HeadlessSurvival -Process $process
        @(Get-HeadlessProcesses).Id | Should -Contain $process.Id -Because 'the original COM server, not a replacement process, must own the windows'
        $before = @(Get-WtWindows -App $script:app).window_id -join ','
        @(Get-WtWindows -App $script:app).window_id -join ',' | Should -BeExactly $before -Because 'repeat COM activation must not replay deferred layouts'
    }

    It 'Intentional headless compatibility remains supported' {
        Set-WtSetting -App $script:app -Key 'compatibility.allowHeadless' -Value $true | Out-Null
        $process = Start-HeadlessCom
        Assert-HeadlessSurvival -Process $process -NoWindow
        @(Get-WtWindows -App $script:app).Count | Should -Be 0
        Get-SavedHeadlessLayout | Should -BeExactly $script:savedLayout
    }

    It 'Ordinary startup still restores the previous layout' {
        Start-HeadlessInteractive
        $process = Wait-Until -TimeoutSec 15 -Because 'the direct-launch Terminal process' -Condition {
            Get-HeadlessProcesses | Select-Object -First 1
        }
        Register-HeadlessProcesses
        Assert-RestoredHeadlessWindows -Process $process
        Assert-HeadlessSurvival -Process $process
    }
}
