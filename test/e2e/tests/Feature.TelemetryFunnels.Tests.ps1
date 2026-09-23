#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# One bounded UAC capture covers the complete deterministic scenario. ETW is
# decoded before assertions, so absent telemetry fails rather than being inferred
# from diagnostic logs. No model quota, synthetic host config, or product hooks.
Describe 'Feature: telemetry funnels' -Tag 'Feature', 'Telemetry' -Skip:($env:ITE2E_TELEMETRY -ne '1') {
    BeforeAll {
        $script:app = $null
        $script:target = $null
        $script:originalHashes = $null
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\TelemetryTrace.ps1')
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'Telemetry PR validation requires explicitly selected Dev.' }
        if (-not $env:ITE2E_EXPECTED_WTA_SHA256 -or -not $env:ITE2E_EXPECTED_APP_SHA256) {
            throw 'Supply WTA and TerminalApp.dll SHA256 values from the exact-source build receipt.'
        }
        $script:target = Resolve-ItApp -Package Dev
        @(Get-WtProcessesForApp -App $script:target) | Should -HaveCount 0 -Because 'user-owned Dev windows must not be stopped or adopted'
        (Get-FileHash -LiteralPath $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) { throw "Existing backup requires recovery: $path" }
            $script:originalHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:root = [IO.Path]::GetFullPath((Join-Path $root ("telemetry-" + [guid]::NewGuid().ToString('N'))))
        New-Item -ItemType Directory -Path $script:root -Force | Out-Null
        $script:requestLog = Join-Path $script:root 'fixture.log'
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $invoke = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
        $command = 'pwsh -NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invoke))
        $script:readyResults = @{}
        $script:slashFailure = $null
        $trace = Start-TestTelemetryTrace -Directory (Join-Path $script:root 'capture')
        try {
            @(Get-WtProcessesForApp -App $script:target) | Should -HaveCount 0
            $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
                acpAgent = 'custom:telemetry-fixture'; acpCustomCommand = $command; acpModel = ''
                delegateAgent = 'copilot'; autoFixEnabled = $false; autoErrorDetectionEnabled = $true
            }
            $script:app.Launched | Should -BeTrue
            $script:shell = Get-ActivePane -App $script:app
            Open-AgentPane -App $script:app | Out-Null
            $script:agent = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $script:shell.session_id -TimeoutSec 30
            Wait-AgentReady -App $script:app -PaneSessionId $script:agent.PaneSessionId -TimeoutSec 60 | Should -BeTrue
            @{
                appPid = $script:app.Pid; helperPid = $script:agent.HelperProcessId
                package = $script:target.Package; installLocation = $script:target.InstallLocation
                wtaSha256 = $env:ITE2E_EXPECTED_WTA_SHA256; appSha256 = $env:ITE2E_EXPECTED_APP_SHA256
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'package.json')

            try {
                Clear-AgentInput -App $script:app -PaneSessionId $script:agent.PaneSessionId | Out-Null
                Invoke-AgentMenuItem -App $script:app -PaneSessionId $script:agent.PaneSessionId -Name '/config'
                Assert-AgentPaneText -App $script:app -PaneSessionId $script:agent.PaneSessionId -Pattern 'Mode' -TimeoutSec 15
                Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Escape | Out-Null
            }
            catch { $script:slashFailure = $_ }

            $tabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $script:shell.session_id
            foreach ($hostEnabled in @($false, $true)) {
                $listener = $null
                try {
                    $listener = Start-WtEventListener -App $script:app -WaitForReady
                    Initialize-LogOffsets -App $script:app | Out-Null
                    $beforeToggle = @(Get-Content -LiteralPath $script:requestLog | Select-String '\|session/prompt\|').Count
                    if ([bool](Get-WtSettingsObject -App $script:app).autoFixEnabled -ne $hostEnabled) {
                        Set-WtSetting -App $script:app -Key autoFixEnabled -Value $hostEnabled | Out-Null
                        Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                            $_.method -eq 'agent_config_changed' -and $null -ne $_.params.autofix_enabled -and
                            $_.params.autofix_enabled -eq $hostEnabled -and -not $_.params.tab_id
                        } | Out-Null
                    }
                    if (-not $hostEnabled) {
                        # Seed a real pending old error while disabled; neither repeated
                        # readiness nor enabling the setting may submit it later.
                        $oldError = 'telemetry-old-' + [guid]::NewGuid().ToString('N')
                        Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id -Command "throw '$oldError'" | Out-Null
                        Wait-WtCommandFailure -Listener $listener -PaneId $script:shell.session_id -TimeoutSec 20 | Out-Null
                        Wait-Until -TimeoutSec 20 -Because 'the helper classifies an error while auto-suggest is disabled' -Condition {
                            (Get-ItLogText -App $script:app -Name "wta-main_helper-$($script:agent.HelperProcessId).log" -SinceStart) -match 'surfacing Detected pill'
                        } | Out-Null
                    }

                    $responses = @()
                    foreach ($repeat in 1..2) {
                        Stop-WtEventListener -Listener $listener
                        $listener = Start-WtEventListener -App $script:app -WaitForReady
                        $wrongTab = [guid]::NewGuid().ToString('B')
                        Invoke-WtCli -App $script:app -Arguments @('publish', (@{
                            method = 'agent_status'; params = @{ state = 'connected'; tab_id = $wrongTab; host_catalog_ready = $true }
                        } | ConvertTo-Json -Depth 4 -Compress)) | Out-Null
                        $marker = 'TELEMETRY_READY_REFRESH_' + [guid]::NewGuid().ToString('N')
                        Send-AgentPrompt -App $script:app -PaneSessionId $script:agent.PaneSessionId -Text $marker | Out-Null
                        $response = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                            $_.method -eq 'agent_config_changed' -and $_.params.tab_id -eq $tabId -and
                            $_.params.window_id -eq [string]$script:app.WindowId -and
                            $null -ne $_.params.autofix_enabled
                        }
                        ($response.params.autofix_enabled -is [bool]) | Should -BeTrue
                        $response.params.autofix_enabled | Should -Be $hostEnabled
                        $response.params.autofix_policy_state | Should -BeIn @('notConfigured', 'enabled', 'disabled')
                        Wait-Until -TimeoutSec 20 -Because 'the fixture completes this exact refresh prompt' -Condition {
                            (Get-Content -LiteralPath $script:requestLog -Raw) -match ([regex]::Escape("telemetry-ready-complete|$marker"))
                        } | Out-Null
                        Start-Sleep -Seconds 1
                        @(Get-Content -LiteralPath $script:requestLog | Select-String '\|session/prompt\|').Count |
                            Should -Be ($beforeToggle + $repeat) -Because 'neither enabling Autofix nor readiness may replay the old terminal error'
                        @(Get-WtEvents -Listener $listener -Predicate {
                            $_.method -eq 'agent_config_changed' -and $_.params.tab_id -eq $wrongTab
                        }) | Should -HaveCount 0
                        $responses += $response
                        Get-WtEvents -Listener $listener | ConvertTo-Json -Depth 20 |
                            Set-Content -LiteralPath (Join-Path $script:root "ready-$hostEnabled-$repeat-protocol.json")
                    }
                    # Use a fresh observer so the old failure cannot satisfy this
                    # completion check. ETW then verifies the downstream flag.
                    Stop-WtEventListener -Listener $listener
                    $listener = Start-WtEventListener -App $script:app -WaitForReady
                    Initialize-LogOffsets -App $script:app | Out-Null
                    Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id `
                        -Command ("throw 'telemetry-new-" + [guid]::NewGuid().ToString('N') + "'") | Out-Null
                    Wait-WtCommandFailure -Listener $listener -PaneId $script:shell.session_id -TimeoutSec 20 | Out-Null
                    Wait-Until -TimeoutSec 20 -Because 'the helper classifies this new failure with the current effective flag' -Condition {
                        (Get-ItLogText -App $script:app -Name "wta-main_helper-$($script:agent.HelperProcessId).log" -SinceStart) -match
                            $(if ($hostEnabled) { 'sending auto-fix prompt' } else { 'surfacing Detected pill' })
                    } | Out-Null
                    $script:readyResults[[string]$hostEnabled] = @{ Responses = $responses; Error = $null }
                }
                catch { $script:readyResults[[string]$hostEnabled] = @{ Error = $_ } }
                finally {
                    Get-ItLogText -App $script:app -Name "wta-main_helper-$($script:agent.HelperProcessId).log" -SinceStart |
                        Set-Content -LiteralPath (Join-Path $script:root "ready-$hostEnabled-helper.log")
                    if ($listener) {
                        Get-WtEvents -Listener $listener | ConvertTo-Json -Depth 20 |
                            Set-Content -LiteralPath (Join-Path $script:root "ready-$hostEnabled-final-protocol.json")
                        Stop-WtEventListener -Listener $listener
                    }
                }
            }
        }
        finally { Stop-TestTelemetryTrace -Trace $trace }
        $script:records = @(Read-TestTelemetryTrace -Directory $trace.Directory -ProcessIds @([int]$script:app.Pid, [int]$script:agent.HelperProcessId))
        ConvertTo-Json -InputObject $script:records -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'scoped-events.json')
    }

    AfterAll {
        if ($script:app -and $script:app.Launched) { Stop-Terminal -App $script:app -RestoreSettings $false }
        if ($script:target -and $script:originalHashes) {
            if (@(Get-WtProcessesForApp -App $script:target).Count) {
                throw 'Selected package remains active; configuration backups retained rather than mutating a live user window.'
            }
            Restore-WtConfig -App $script:target
            foreach ($path in $script:originalHashes.Keys) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                $actual | Should -Be $script:originalHashes[$path] -Because 'settings and state must be restored byte-for-byte'
            }
        }
    }

    It 'Window startup telemetry has one consolidated typed snapshot' {
        $created = @($script:records | Where-Object { $_.Provider -eq '24a1622f-7da7-5c77-3303-d850bd1ab2ed' -and $_.Name -eq 'AppCreated' })
        $created | Should -HaveCount 1
        $expected = @{
            TabsInTitlebar = 'Boolean'
            PrimaryProvider = 'AnsiString'
            PrimaryEffectiveProvider = 'AnsiString'
            PrimaryCustomConfiguredCount = 'UInt32'
            PrimaryCustomSelectedCommandConfigured = 'Boolean'
            DelegateProvider = 'AnsiString'
            DelegateEffectiveProvider = 'AnsiString'
            DelegateCustomConfiguredCount = 'UInt32'
            DelegateCustomSelectedCommandConfigured = 'Boolean'
            AllowedAgentsPolicy = 'AnsiString'
            AllowCustomAgentsPolicy = 'AnsiString'
            SidebarEnabled = 'Boolean'
            DefaultsFallback = 'Boolean'
        }
        @($created[0].Fields.Keys | Where-Object { $_ -ne 'PartA_PrivTags' } | Sort-Object) | Should -Be ($expected.Keys | Sort-Object)
        foreach ($field in $expected.Keys) {
            $created[0].Types[$field] | Should -Match ($expected[$field] + '$')
        }
        $created[0].Fields.PrimaryProvider | Should -Be 'custom'
        $created[0].Types.PartA_PrivTags | Should -Match 'UInt64$'
        @($script:records | Where-Object Name -in @('AgentProviderConfigured', 'CustomAgentConfigured', 'SidebarStateOnLaunch')) | Should -HaveCount 0
    }

    It 'Agent slash telemetry uses only the renamed event' {
        if ($script:slashFailure) { throw $script:slashFailure }
        $used = @($script:records | Where-Object { $_.Provider -eq '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b' -and $_.Name -eq 'AgentSlashCommandUsed' })
        $used | Should -HaveCount 1
        $used[0].Fields.command | Should -Be 'config'
        $used[0].Types.command | Should -Match 'AnsiString$'
        @($script:records | Where-Object Name -eq 'SlashCommandInvoked') | Should -HaveCount 0
    }

    It 'Connected helpers receive current native Autofix configuration' -ForEach @(
        @{ HostEnabled = $false }, @{ HostEnabled = $true }
    ) {
        $result = $script:readyResults[[string]$HostEnabled]
        if ($result.Error) { throw $result.Error }
        @($result.Responses) | Should -HaveCount 2
        $detected = @($script:records | Where-Object {
            $_.Provider -eq '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b' -and $_.Name -eq 'ErrorDetected' -and
            $_.Fields.PaneId -eq $script:shell.session_id -and
            $_.Fields.AutoFixEnabled -in $(if ($HostEnabled) { @('true', '1') } else { @('false', '0') })
        })
        $detected.Count | Should -BeGreaterThan 0
        foreach ($record in $detected) {
            $record.Fields.AllowAutoFixPolicy | Should -Be $result.Responses[-1].params.autofix_policy_state
            $record.Types.AutoFixEnabled | Should -Match 'Boolean$'
            $record.Types.AllowAutoFixPolicy | Should -Match 'AnsiString$'
        }
    }
}
