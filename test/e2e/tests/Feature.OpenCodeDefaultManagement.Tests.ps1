#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: default OpenCode session management' -Tag 'Feature', 'OpenCodeHooks' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        if ($env:ITE2E_PACKAGE -ne 'Dev') { throw 'This feature-source acceptance requires explicit Dev selection.' }
        if ($env:ITE2E_OPENCODE_EXPECTED_MAJOR -notin @('1', '2')) {
            throw 'Set ITE2E_OPENCODE_EXPECTED_MAJOR to the actual default version being tested.'
        }
        if (-not $env:ITE2E_TARGET_PFN) { throw 'Pin the exact feature-source Dev package family.' }
        $script:app = Resolve-ItApp -Package $env:ITE2E_TARGET_PFN
        $script:app.Package | Should -Not -Be 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
        foreach ($binary in @(
            @{ Path = $script:app.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 },
            @{ Path = (Join-Path $script:app.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 }
        )) {
            $binary.Hash | Should -Not -BeNullOrEmpty
            (Get-FileHash -LiteralPath $binary.Path).Hash | Should -Be $binary.Hash
        }
        $script:manifest = [xml](Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'AppxManifest.xml') -Raw)
        $classes = @($script:manifest.SelectNodes("//*[local-name()='ExeServer' and @Executable='WindowsTerminal.exe']/*[local-name()='Class']"))
        $classes | Should -HaveCount 1
        $script:app.ComClsid = '{' + $classes[0].Id + '}'
        $roots = @(Get-CimInstance Win32_Process -Filter "Name='WindowsTerminal.exe'" |
            Where-Object ExecutablePath -eq $script:app.WindowsTerminal)
        $roots | Should -HaveCount 1 -Because 'the separately launched feature-source fixture must already be running'
        $script:app.Pid = $roots[0].ProcessId
        $script:rootCreated = $roots[0].CreationDate
        $script:ownedPanes = [Collections.Generic.List[string]]::new()
        $script:baselinePanes = @(Get-WtTabs -App $script:app | ForEach-Object {
            Get-WtPanes -App $script:app -TabId $_.tab_id -WindowId $_.window_id
        })
        $script:app | Add-Member WindowId ([string](Get-WtWindows -App $script:app | Select-Object -First 1).window_id)
        $script:masterPipe = Get-WtaMasterPipe -App $script:app
        $masters = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($script:app.Pid) AND Name='wta.exe'" |
            Where-Object {
                $_.ExecutablePath -eq $script:app.WtaPath -and $_.CommandLine -match '\s--master\s' -and
                $_.CommandLine.Contains($script:masterPipe)
            })
        $masters | Should -HaveCount 1

        function Read-DefaultSessions {
            $result = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $script:masterPipe, '--json', '--include-status', '--origin', 'all'
            ) -Raw
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            @((ConvertFrom-Json -InputObject $result.StdOut).sessions)
        }

        function Test-DefaultManagement([int]$Major) {
            if ([int]$env:ITE2E_OPENCODE_EXPECTED_MAJOR -ne $Major) {
                Set-ItResult -Skipped -Because "This sequential run tests default V$env:ITE2E_OPENCODE_EXPECTED_MAJOR, not V$Major."
                return
            }
            $status = Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 60
            $opencode = @($status.clis | Where-Object name -eq 'opencode')
            $opencode | Should -HaveCount 1
            $entry = $opencode[0].binary_path
            $entry | Should -Not -BeNullOrEmpty
            $versionCall = '& ''' + $entry.Replace("'", "''") + ''' --version; exit $LASTEXITCODE'
            $versionEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($versionCall))
            $version = Invoke-Native -FilePath (Get-Process -Id $PID).Path -Arguments @(
                '-NoProfile', '-EncodedCommand', $versionEncoded
            ) -TimeoutSec 15
            $version.ExitCode | Should -Be 0 -Because $version.StdErr
            $version.StdOut.Trim() | Should -Match "^(?:opencode\s+)?v?$Major\."
            $opencode[0].plugin_enabled | Should -BeTrue
            $opencode[0].installed_version | Should -Be $opencode[0].bundle_version
            $bundle = Join-Path $script:app.InstallLocation 'wt-agent-hooks\opencode'
            $support = Join-Path $opencode[0].marketplace_path 'wt-agent-hooks'
            $installedManifest = Get-Content -LiteralPath (Join-Path $support 'plugin.json') -Raw | ConvertFrom-Json
            $installedManifest.api_variant | Should -Be "v$Major"
            $installedEntry = if ($Major -ge 2) { Join-Path $support 'wt-agent-hooks.js' }
                else { Join-Path $opencode[0].marketplace_path 'wt-agent-hooks.js' }
            (Get-FileHash -LiteralPath $installedEntry).Hash |
                Should -Be (Get-FileHash -LiteralPath (Join-Path $bundle "v$Major\wt-agent-hooks.js")).Hash
            if ($Major -ge 2) {
                foreach ($file in @('server.js', 'tui.js', 'rpc.js', 'package.json')) {
                    (Get-FileHash -LiteralPath (Join-Path $support $file)).Hash |
                        Should -Be (Get-FileHash -LiteralPath (Join-Path $bundle "v2\$file")).Hash
                }
            }
            $shell = Get-ActivePane -App $script:app
            $call = '& ''' + $entry.Replace("'", "''") + ''' session list --format json; exit $LASTEXITCODE'
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($call))
            $result = Invoke-Native -FilePath (Get-Process -Id $PID).Path -Arguments @(
                '-NoProfile', '-EncodedCommand', $encoded
            ) -WorkingDirectory $shell.cwd -TimeoutSec 30
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            $nativeIds = @((ConvertFrom-Json -InputObject $result.StdOut).id)
            $activeNativeIds = @(Get-CimInstance Win32_Process -Filter "Name='opencode.exe'" | ForEach-Object {
                $match = [regex]::Match([string]$_.CommandLine, '(?:--session|-s)(?:=|\s+)"?([A-Za-z0-9_-]+)')
                if ($match.Success) { $match.Groups[1].Value }
            })
            try {
                $history = Wait-Until -TimeoutSec 60 -Because "default V$Major history is discovered through ACP" -Condition {
                    Read-DefaultSessions | Where-Object {
                        $_.provider_id -eq 'opencode' -and $_.status -in @('Historical', 'Ended') -and
                        $_.location -eq 'Host' -and $_.session_id -in $nativeIds -and
                        $_.session_id -notin $activeNativeIds -and
                        $_.title -and $_.title -notlike 'New session*' -and (Test-Path -LiteralPath $_.cwd -PathType Container)
                    } | Select-Object -First 1
                }
            }
            catch {
                $snapshot = @(Read-DefaultSessions)
                Write-ItLog -Level ERROR -Message ("Default OpenCode history mismatch: " + (@{
                    native_ids = $nativeIds
                    active_native_ids = $activeNativeIds
                    shell_cwd = $shell.cwd
                    rows = @($snapshot | Where-Object provider_id -eq opencode |
                        Select-Object session_id, provider_id, location, status, cwd)
                    provider_ids = @($snapshot.provider_id | Sort-Object -Unique)
                } | ConvertTo-Json -Depth 6 -Compress))
                throw
            }
            $history | Should -Not -BeNullOrEmpty
            $before = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId).tab_id
            $activationId = [guid]::NewGuid().ToString('D')
            $aliasNames = @($script:manifest.SelectNodes("//*[local-name()='Application' and @Id='WtaCli']//*[local-name()='ExecutionAlias']") |
                ForEach-Object Alias)
            $aliasNames | Should -HaveCount 1
            $alias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\$($script:app.Package)\$($aliasNames[0])"
            $activated = Invoke-Native -FilePath $alias -Arguments @(
                'sessions', 'activate', '--master', $script:masterPipe,
                '--session-id', $history.session_id, '--provider', 'opencode',
                '--location', 'host', '--window-id', $script:app.WindowId, '--activation-id', $activationId, '--json'
            ) -TimeoutSec 45
            $activated.ExitCode | Should -Be 0 -Because $activated.StdErr
            $pane = Wait-Until -TimeoutSec 30 -Because 'the exact history activation creates its native CLI pane' -Condition {
                $tabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId | Where-Object tab_id -notin $before)
                foreach ($tab in $tabs) {
                    Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $tab.tab_id |
                        Where-Object native_agent_provider_id -eq 'opencode' | Select-Object -First 1
                }
            }
            $script:ownedPanes.Add($pane.session_id)
            $native = Wait-Until -TimeoutSec 60 -Because 'resume runs the default CLI with the exact historical id' -Condition {
                Get-CimInstance Win32_Process -Filter "Name='opencode.exe'" | Where-Object {
                    $_.CommandLine -match ('--session\s+' + [regex]::Escape($history.session_id) + '(?:\s|$)')
                } | Select-Object -First 1
            }
            $actualVersion = Invoke-Native -FilePath $native.ExecutablePath -Arguments @('--version') -TimeoutSec 15
            $actualVersion.ExitCode | Should -Be 0
            $actualVersion.StdOut.Trim() | Should -Match "^(?:opencode\s+)?v?$Major\."
            $restored = Wait-Until -TimeoutSec 60 -Because 'the original history title is restored by the native TUI' -Condition {
                Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId $pane.tab_id |
                    Where-Object { $_.session_id -eq $pane.session_id -and $_.title -eq "OC | $($history.title)" }
            }
            $restored | Should -Not -BeNullOrEmpty
            $row = Read-DefaultSessions | Where-Object session_id -eq $history.session_id
            $row.title | Should -Be $history.title
            $row.status | Should -Be 'Idle'
            $row.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $pane.session_id.Trim('{}').ToLowerInvariant()
            Close-WtPane -App $script:app -SessionId $pane.session_id
            $script:ownedPanes.Remove($pane.session_id) | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'closing only the test pane clears only its live registration' -Condition {
                Read-DefaultSessions | Where-Object {
                    $_.session_id -eq $history.session_id -and $_.status -in @('Ended', 'Historical') -and -not $_.pane_session_id
                }
            } | Should -Not -BeNullOrEmpty
            $base = @(Get-WtTabs -App $script:app | ForEach-Object {
                Get-WtPanes -App $script:app -TabId $_.tab_id -WindowId $_.window_id
            })
            foreach ($original in $script:baselinePanes) {
                @($base | Where-Object session_id -eq $original.session_id) | Should -HaveCount 1
            }
        }
    }

    AfterAll {
        if ($script:app) {
            $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($script:app.Pid)"
            if ($current -and $current.ExecutablePath -eq $script:app.WindowsTerminal -and
                $current.CreationDate -eq $script:rootCreated) {
                foreach ($pane in $script:ownedPanes) { Close-WtPane -App $script:app -SessionId $pane }
            }
            elseif ($script:ownedPanes.Count) { throw 'Test ownership changed; refusing to close any pane.' }
        }
    }

    It 'Default OpenCode V1 session management follows its CLI' { Test-DefaultManagement 1 }
    It 'Default OpenCode V2 session management follows its CLI' { Test-DefaultManagement 2 }
}
