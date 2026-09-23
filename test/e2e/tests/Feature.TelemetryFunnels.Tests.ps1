#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# One bounded UAC capture covers the complete deterministic scenario. ETW is
# decoded before assertions, so absent telemetry fails rather than being inferred
# from diagnostic logs. No model quota, synthetic host config, or product hooks.
Describe 'Feature: telemetry funnels' -Tag 'Feature', 'Telemetry' -Skip:($env:ITE2E_TELEMETRY -ne '1') {
    BeforeAll {
        $script:app = $null
        $script:target = $null
        $script:originalHashes = $null
        $script:policyTransaction = $null
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\TelemetryTrace.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TelemetryFunnels.Scenarios.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TelemetryPolicy.ps1')
        $script:ownedPids = [Collections.Generic.HashSet[int]]::new()
        $script:phases = [ordered]@{}
        $script:phaseErrors = @{}
        $script:startupCases = @{}
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
        foreach ($package in @(Get-AppxPackage | Where-Object {
            $_.Name -like '*IntelligentTerminal*' -and $_.PackageFamilyName -ne $script:target.Package
        })) {
            $other = Resolve-ItApp -Package $package.PackageFamilyName
            @(Get-WtProcessesForApp -App $other) | Should -HaveCount 0 -Because 'HKCU policy changes must not affect another running Intelligent Terminal package'
        }
        $script:policyTransaction = Initialize-TelemetryPolicyTransaction -Directory $script:root
        $script:requestLog = Join-Path $script:root 'fixture.log'
        $script:telemetryFixture = Join-Path $script:root 'telemetry-fixture.json'
        @{ mode = 'chat' } | ConvertTo-Json | Set-Content -LiteralPath $script:telemetryFixture
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $invoke = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))' -TelemetryFixturePath '$($script:telemetryFixture.Replace("'", "''"))'"
        $command = 'pwsh -NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invoke))
        $script:readyResults = @{}
        $script:slashFailure = $null
        $trace = Start-TestTelemetryTrace -Directory (Join-Path $script:root 'capture')
        try {
            @(Get-WtProcessesForApp -App $script:target) | Should -HaveCount 0
            foreach ($name in @('AllowAutoFix', 'AllowedAgents', 'AllowCustomAgents')) {
                Set-TelemetryPolicy -Transaction $script:policyTransaction -Name $name -Value $null
            }
            $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
                acpAgent = 'custom:telemetry-fixture'; acpCustomCommand = $command; acpModel = ''
                delegateAgent = 'copilot'; autoFixEnabled = $false; autoErrorDetectionEnabled = $true
                tabLayout = 'horizontal'; 'agentPane.yoloMode' = $false
            }
            $script:app.Launched | Should -BeTrue
            $script:shell = Get-ActivePane -App $script:app
            Open-AgentPane -App $script:app | Out-Null
            $script:agent = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $script:shell.session_id -TimeoutSec 30
            Wait-AgentReady -App $script:app -PaneSessionId $script:agent.PaneSessionId -TimeoutSec 60 | Should -BeTrue
            $script:primaryAppPid = [int]$script:app.Pid
            $script:startupCases['initial'] = $script:primaryAppPid
            Save-TelemetryOwnedProcesses -App $script:app
            Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
            Send-WtWindowKey -App $script:app -Vk 0x10 -RequireForeground | Out-Null
            $script:initialReadyUtc = [DateTime]::UtcNow
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
            Invoke-TelemetryPhase -Name conversation -Action { Invoke-TelemetryConversation }
            foreach ($decision in @('Run', 'Insert', 'Reject')) {
                Invoke-TelemetryPhase -Name "offer-$decision" -Action { Invoke-TelemetryOffer -Decision $decision }
            }
            foreach ($allowed in @($false, $true)) {
                Invoke-TelemetryPhase -Name "autofix-policy-$allowed" -Action { Invoke-TelemetryAutoFixPolicy -Allowed $allowed }
            }
            Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowAutoFix -Value $null
            Invoke-TelemetryPhase -Name second-window -Action { Invoke-TelemetrySecondWindow }
            @(Get-WtWindows -App $script:app) | Should -HaveCount 1 -Because 'later settings changes must only reach the retained primary window'
            Invoke-TelemetryPhase -Name palette -Action { Invoke-TelemetryPalette }
            Invoke-TelemetryPhase -Name provider-changes -Action { Invoke-TelemetryProviderChanges }
            foreach ($allowed in @($false, $true)) {
                Invoke-TelemetryPhase -Name "cold-autofix-policy-$allowed" -Action {
                    Invoke-TelemetryColdAutoFixPolicy -Allowed $allowed -Command $command
                }
            }
            if ($script:app) {
                Stop-TelemetryOwnedTerminal -App $script:app
                $script:app = $null
            }
            Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowAutoFix -Value $null
            Invoke-TelemetryPhase -Name startup-inventory -Action {
                Invoke-TelemetryStartupCase -Name inventory -Settings @{
                    acpAgent = 'custom:pwsh'; acpCustomCommand = $command
                    acpCustomCommands = @($command, "$command -duplicate", 'cmd.exe /d /c exit 0')
                    delegateAgent = 'custom:cmd'; delegateCustomCommand = 'cmd.exe /d /c exit 0'
                    delegateCustomCommands = @('cmd.exe /d /c exit 0', 'pwsh -NoProfile -Command exit')
                    tabLayout = 'vertical'; autoFixEnabled = $false
                }
            }
            Invoke-TelemetryPhase -Name startup-no-agent -Action {
                Invoke-TelemetryStartupCase -Name noAgent -Settings @{
                    acpAgent = ''; acpCustomCommand = ''; acpCustomCommands = @()
                    delegateAgent = ''; delegateCustomCommand = ''; delegateCustomCommands = @()
                    tabLayout = 'horizontal'; autoFixEnabled = $false
                }
            }
            Invoke-TelemetryPhase -Name startup-policy-blocked -Action {
                Save-TelemetryOwnedProcesses -App $script:app
                Stop-TelemetryOwnedTerminal -App $script:app
                $script:app = $null
                Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowedAgents -Value ([string[]]@())
                Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowCustomAgents -Value 0
                Invoke-TelemetryStartupCase -Name policyBlocked -Settings @{
                    acpAgent = 'custom:pwsh'; acpCustomCommand = $command; acpCustomCommands = @('cmd.exe /d /c exit 0')
                    delegateAgent = 'custom:cmd'; delegateCustomCommand = 'cmd.exe /d /c exit 0'; delegateCustomCommands = @($command)
                    tabLayout = 'vertical'; autoFixEnabled = $false
                }
            }
            Invoke-TelemetryPhase -Name startup-policy-allowlist -Action {
                Save-TelemetryOwnedProcesses -App $script:app
                Stop-TelemetryOwnedTerminal -App $script:app
                $script:app = $null
                Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowedAgents -Value ([string[]]@('copilot'))
                Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowCustomAgents -Value 1
                Invoke-TelemetryStartupCase -Name policyAllowlist -Settings @{
                    acpAgent = 'custom:pwsh'; acpCustomCommand = $command; acpCustomCommands = @()
                    delegateAgent = 'claude'; delegateCustomCommand = ''; delegateCustomCommands = @()
                    tabLayout = 'horizontal'; autoFixEnabled = $false
                }
            }
        }
        finally { Stop-TestTelemetryTrace -Trace $trace }
        $script:records = @(Read-TestTelemetryTrace -Directory $trace.Directory -ProcessIds @($script:ownedPids))
        Initialize-TelemetryPhaseClock -CaptureDirectory $trace.Directory
        @($script:ownedPids) | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'owned-processes.json')
        ConvertTo-Json -InputObject $script:records -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'scoped-events.json')
    }

    AfterAll {
        try {
            if ($script:app -and $script:app.Launched) { Stop-TelemetryOwnedTerminal -App $script:app }
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
        finally { if ($script:policyTransaction) { Restore-TelemetryPolicy -Transaction $script:policyTransaction } }
    }

    It 'Window startup telemetry has one consolidated typed snapshot' {
        $created = @($script:records | Where-Object {
            $_.ProcessId -eq $script:primaryAppPid -and $_.Name -eq 'AppCreated' -and
            ([DateTimeOffset]$_.Timestamp).UtcDateTime.Add($script:traceClockCorrection) -le $script:initialReadyUtc
        })
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

    It 'Each same-process window emits one startup snapshot without activation duplicates' {
        $created = @(Get-TelemetryPhaseEvents -Phase second-window -Name AppCreated)
        $created | Should -HaveCount 1
        $created[0].ProcessId | Should -Be $script:primaryAppPid
        ([DateTimeOffset]$created[0].Timestamp).UtcDateTime.Add($script:traceClockCorrection) |
            Should -BeLessThan ([DateTimeOffset]$script:phases['second-window'].Data.ActivationUtc).UtcDateTime
        @($script:records | Where-Object { $_.ProcessId -eq $script:primaryAppPid -and $_.Name -eq 'AppCreated' }) |
            Should -HaveCount 2 -Because 'initial and secondary windows each emit once; activation and settings reload do not'
        @($created[0].Fields.Keys | Where-Object { $_ -ne 'PartA_PrivTags' }) | Should -HaveCount 13
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
            $_.Fields.AllowAutoFixPolicy -eq $result.Responses[-1].params.autofix_policy_state -and
            $_.Fields.AutoFixEnabled -in $(if ($HostEnabled) { @('true', '1') } else { @('false', '0') })
        })
        $detected.Count | Should -BeGreaterThan 0
        foreach ($record in $detected) {
            $record.Fields.AllowAutoFixPolicy | Should -Be $result.Responses[-1].params.autofix_policy_state
            $record.Types.AutoFixEnabled | Should -Match 'Boolean$'
            $record.Types.AllowAutoFixPolicy | Should -Match 'AnsiString$'
        }
    }

    It 'Interactive sessions and connections expose local adoption sources' {
        $interactive = @($script:records | Where-Object { $_.ProcessId -eq $script:primaryAppPid -and $_.Name -eq 'SessionBecameInteractive' })
        $connections = @($script:records | Where-Object { $_.ProcessId -eq $script:primaryAppPid -and $_.Name -eq 'ConnectionCreated' })
        $interactive.Count | Should -BeGreaterThan 0
        $connections.Count | Should -BeGreaterThan 0
        foreach ($record in @($interactive) + @($connections)) {
            $record.Types.Count | Should -Be $record.Fields.Count
            $record.Types.Count | Should -BeGreaterThan 0
            $record.Types.PartA_PrivTags | Should -Match 'UInt64$'
        }
        $interactive[0].Fields.Branding | Should -Be '0'
        $interactive[0].Fields.Distribution | Should -Be '2'
        $interactive[0].Types.Branding | Should -Match 'UInt8$'
        $interactive[0].Types.Distribution | Should -Match 'UInt8$'
        $sourceConnections = @($connections | Where-Object { $_.Fields.SessionGuid.Trim('{}') -eq $script:shell.session_id.Trim('{}') })
        $sourceConnections | Should -HaveCount 1
        foreach ($field in @('ConnectionTypeGuid', 'ProfileGuid', 'SessionGuid')) {
            $sourceConnections[0].Types[$field] | Should -Match 'GUID$'
        }
        # ConnectionCreated reports Profile.ConnectionType, whose unset local
        # default is GUID_NULL; it does not report the ConPTY runtime class GUID.
        [guid]::Parse($sourceConnections[0].Fields.ConnectionTypeGuid) | Should -Be ([guid]::Empty)
        [guid]::Parse($sourceConnections[0].Fields.ProfileGuid) | Should -Not -Be ([guid]::Empty)
        [guid]::Parse($sourceConnections[0].Fields.SessionGuid) | Should -Be ([guid]$script:shell.session_id)
        @($sourceConnections[0].Fields.Keys | Sort-Object) |
            Should -Be @('ConnectionTypeGuid', 'PartA_PrivTags', 'ProfileGuid', 'SessionGuid')
    }

    It 'ACP session creation and completed ordinary prompts expose engagement' {
        $created = @($script:records | Where-Object {
            $_.Name -eq 'AcpNewSessionComplete' -and $_.Fields.SessionId -eq $script:agent.AcpSessionId
        })
        $created.Fields.Route | Should -Contain 'MasterForward'
        @($created | Where-Object { $_.Fields.Route -like 'Helper*' }).Count | Should -BeGreaterThan 0
        foreach ($record in $created) {
            $record.Fields.Success | Should -BeIn @('true', '1')
            $record.Types.Success | Should -Match 'Boolean$'
            $record.Types.DurationMs | Should -Match 'Double$'
        }
        $prompts = @(Get-TelemetryPhaseEvents -Phase conversation -Name AgentPromptSent)
        $prompts | Should -HaveCount 2
        foreach ($record in $prompts) {
            $record.Fields.SessionId | Should -Be $script:agent.AcpSessionId
            $record.Fields.IsAutofix | Should -BeIn @('false', '0')
            $record.Fields.TemplateKind | Should -Not -Be 'AgentCommand'
            $record.Types.SessionId | Should -Match 'AnsiString$'
            $record.Types.PromptLengthBytes | Should -Match 'UInt32$'
        }
        $completed = @(Get-TelemetryPhaseEvents -Phase conversation -Name AgentResponseComplete | Where-Object {
            $_.Fields.SessionId -eq $script:agent.AcpSessionId
        })
        $completed.Count | Should -BeGreaterOrEqual 2
        foreach ($record in $completed) {
            $record.Fields.Success | Should -BeIn @('true', '1')
            $record.Types.TotalDurationMs | Should -Match 'Double$'
        }
    }

    It 'Concrete Autofix offers count once and only Run accepts' -ForEach @(
        @{ Decision = 'Run' }, @{ Decision = 'Insert' }, @{ Decision = 'Reject' }
    ) {
        $offered = @(Get-TelemetryPhaseEvents -Phase "offer-$Decision" -Name ErrorFixOffered)
        $accepted = @(Get-TelemetryPhaseEvents -Phase "offer-$Decision" -Name ErrorFixAccepted)
        $offered | Should -HaveCount 1 -Because 'redrawing the same concrete card must not double count it'
        $offered[0].Types.OfferId | Should -Match 'AnsiString$'
        [guid]::Parse($offered[0].Fields.OfferId) | Should -Not -Be ([guid]::Empty)
        $phase = $script:phases["offer-$Decision"].Data
        if ($Decision -eq 'Run') {
            $accepted | Should -HaveCount 1
            $accepted[0].Fields.OfferId | Should -Be $offered[0].Fields.OfferId
            $accepted[0].Types.OfferId | Should -Match 'AnsiString$'
            ([DateTimeOffset]$accepted[0].Timestamp).UtcDateTime.Add($script:traceClockCorrection) |
                Should -BeGreaterOrEqual ([DateTimeOffset]$phase.DecisionUtc).UtcDateTime
            $phase.Executed | Should -BeTrue
        }
        else {
            $accepted | Should -HaveCount 0
            $phase.Executed | Should -BeFalse
        }
    }

    It 'Visible foreground palette entry is distinct from hiding and submission' {
        $entries = @(Get-TelemetryPhaseEvents -Phase palette -Name CommandPaletteAgentPromptEntered)
        $submissions = @(Get-TelemetryPhaseEvents -Phase palette -Name CommandPaletteDispatchedAgentPrompt)
        $entries | Should -HaveCount 2
        $submissions | Should -HaveCount 1
        $phase = $script:phases.palette.Data
        $first = ([DateTimeOffset]$entries[0].Timestamp).UtcDateTime.Add($script:traceClockCorrection)
        $second = ([DateTimeOffset]$entries[1].Timestamp).UtcDateTime.Add($script:traceClockCorrection)
        $first | Should -BeLessThan ([DateTimeOffset]$phase.HideUtc).UtcDateTime
        $second | Should -BeGreaterOrEqual ([DateTimeOffset]$phase.ReopenUtc).UtcDateTime
        $second | Should -BeLessThan ([DateTimeOffset]$phase.SubmitUtc).UtcDateTime
        $submissions[0].Fields.IsBackgroundMode | Should -BeIn @('false', '0')
    }

    It 'Provider changes expose bounded roles and transitions without unchanged reloads' {
        $actual = @(Get-TelemetryPhaseEvents -Phase provider-changes -Name AgentProviderChanged)
        $expected = @($script:phases['provider-changes'].Data.Expected)
        $actual | Should -HaveCount $expected.Count
        for ($i = 0; $i -lt $expected.Count; $i++) {
            $actual[$i].Fields.role | Should -Be $expected[$i].Role
            $actual[$i].Fields.from | Should -Be $expected[$i].From
            $actual[$i].Fields.to | Should -Be $expected[$i].To
            foreach ($field in @('role', 'from', 'to')) { $actual[$i].Types[$field] | Should -Match 'AnsiString$' }
        }
        @($actual | Where-Object {
            ([DateTimeOffset]$_.Timestamp).UtcDateTime.Add($script:traceClockCorrection) -ge
                ([DateTimeOffset]$script:phases['provider-changes'].Data.UnchangedUtc).UtcDateTime
        }) | Should -HaveCount 0
        $barrier = $script:phases['provider-changes'].Data
        $barrier.UnchangedApplied.method | Should -Be 'agent_config_changed'
        ($barrier.UnchangedApplied.params.autofix_enabled -is [bool]) | Should -BeTrue
        $barrier.UnchangedApplied.params.autofix_enabled | Should -Be $barrier.UnchangedAutofix
        @(Get-TelemetryPhaseEvents -Phase provider-changes -Name JsonSettingsChanged | Where-Object {
            ([DateTimeOffset]$_.Timestamp).UtcDateTime.Add($script:traceClockCorrection) -ge
                ([DateTimeOffset]$script:phases['provider-changes'].Data.UnchangedUtc).UtcDateTime
        }).Count | Should -BeGreaterThan 0 -Because 'an unrelated setting proves the unchanged-provider reload actually completed'
    }

    It 'Startup snapshots include inventory sidebar and no-agent states' {
        foreach ($phase in @('startup-inventory', 'startup-no-agent')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name AppCreated) | Should -HaveCount 1
        }
        $inventory = @(Get-TelemetryPhaseEvents -Phase startup-inventory -Name AppCreated)[0]
        $none = @(Get-TelemetryPhaseEvents -Phase startup-no-agent -Name AppCreated)[0]
        foreach ($role in @('Primary', 'Delegate')) {
            $inventory.Fields["${role}Provider"] | Should -Be 'custom'
            $inventory.Fields["${role}EffectiveProvider"] | Should -Be 'custom'
            $inventory.Fields["${role}CustomConfiguredCount"] | Should -Be '2'
            $inventory.Fields["${role}CustomSelectedCommandConfigured"] | Should -BeIn @('true', '1')
            $none.Fields["${role}Provider"] | Should -Be 'none'
            $none.Fields["${role}EffectiveProvider"] | Should -Be 'none'
            $none.Fields["${role}CustomConfiguredCount"] | Should -Be '0'
            $none.Fields["${role}CustomSelectedCommandConfigured"] | Should -BeIn @('false', '0')
        }
        $inventory.Fields.SidebarEnabled | Should -BeIn @('true', '1')
        $none.Fields.SidebarEnabled | Should -BeIn @('false', '0')
        @($script:records | Where-Object { $_.ProcessId -eq $script:startupCases.noAgent -and $_.Name -eq 'AgentSessionStarted' }) |
            Should -HaveCount 0
        foreach ($snapshot in @($inventory, $none)) {
            @($snapshot.Fields.Keys | Where-Object { $_ -ne 'PartA_PrivTags' }) | Should -HaveCount 13
            $snapshot.Fields.AllowedAgentsPolicy | Should -Be 'not_configured'
            $snapshot.Fields.AllowCustomAgentsPolicy | Should -Be 'not_configured'
            $snapshot.Fields.DefaultsFallback | Should -BeIn @('false', '0')
        }
    }

    It 'Raw Autofix policy stays distinct from effective helper state' -ForEach @(
        @{ Allowed = $false }, @{ Allowed = $true }
    ) {
        $events = @(Get-TelemetryPhaseEvents -Phase "autofix-policy-$Allowed" -Name ErrorDetected | Where-Object {
            $_.Provider -eq '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b' -and $_.Fields.PaneId -eq $script:shell.session_id
        })
        $data = $script:phases["autofix-policy-$Allowed"].Data
        ($data.Config.params.autofix_enabled -is [bool]) | Should -BeTrue
        $data.Config.params.autofix_enabled | Should -Be $Allowed
        $data.Config.params.autofix_policy_state | Should -Be $(if ($Allowed) { 'enabled' } else { 'disabled' })
        if ($Allowed) {
            $events.Count | Should -BeGreaterThan 0
            foreach ($event in $events) {
                $event.Fields.AllowAutoFixPolicy | Should -Be 'enabled'
                $event.Fields.AutoFixEnabled | Should -BeIn @('true', '1')
                $event.Fields.Method | Should -Be 'vt_sequence'
                $event.Types.AllowAutoFixPolicy | Should -Match 'AnsiString$'
                $event.Types.AutoFixEnabled | Should -Match 'Boolean$'
            }
        }
        else {
            $events | Should -HaveCount 0 -Because 'blocked policy suppresses the shell VT event before helper classification'
            @(Get-TelemetryPhaseEvents -Phase "autofix-policy-$Allowed" | Where-Object {
                $_.ProcessId -eq $script:agent.HelperProcessId -and
                $_.Name -in @('AgentPromptSent', 'ErrorFixOffered', 'ErrorFixAccepted')
            }) | Should -HaveCount 0 -Because 'neither the new blocked failure nor an older error may start Autofix'
        }
    }

    It 'Startup Autofix policy labels match effective helper state (Allowed=<Allowed>)' -ForEach @(
        @{ Allowed = $false }, @{ Allowed = $true }
    ) {
        $phaseName = "cold-autofix-policy-$Allowed"
        $events = @(Get-TelemetryPhaseEvents -Phase $phaseName -Name ErrorDetected -Provider '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b')
        $data = $script:phases[$phaseName].Data
        $data.StartupPolicy | Should -BeTrue
        $data.HotRefreshValidated | Should -BeFalse
        $events = @($events | Where-Object {
            $_.ProcessId -eq $data.HelperPid -and $_.Fields.PaneId -eq $data.SourcePane
        })
        $events | Should -HaveCount 1 -Because 'the enabled OSC failure or disabled shell connection failure must be classified exactly once'
        $events[0].Fields.AllowAutoFixPolicy | Should -Be $(if ($Allowed) { 'enabled' } else { 'disabled' })
        $events[0].Fields.AutoFixEnabled | Should -BeIn $(if ($Allowed) { @('true', '1') } else { @('false', '0') })
        $events[0].Types.AllowAutoFixPolicy | Should -Match 'AnsiString$'
        $events[0].Types.AutoFixEnabled | Should -Match 'Boolean$'
        $events[0].Types.PartA_PrivTags | Should -Match 'UInt64$'
        $events[0].Fields.Method | Should -Be $(if ($Allowed) { 'vt_sequence' } else { 'connection_state' })
        $events[0].Types.Method | Should -Match 'AnsiString$'
        $events[0].Fields.Method | Should -Be $data.DetectionMethod
        if (-not $Allowed) {
            $data.ConnectionEnd.params.state | Should -Be 'failed'
            $data.ConnectionEnd.params.pane_id | Should -Be $data.SourcePane
            @(Get-TelemetryPhaseEvents -Phase $phaseName | Where-Object {
                $_.ProcessId -eq $data.HelperPid -and $_.Name -in @('ErrorFixOffered', 'ErrorFixAccepted')
            }) | Should -HaveCount 0 -Because 'connection failure is notification-only, never an Autofix offer'
        }
        $prompts = @(Get-TelemetryPhaseEvents -Phase $phaseName -Name AgentPromptSent | Where-Object {
            $_.ProcessId -eq $data.HelperPid -and $_.Fields.SessionId -eq $data.SessionId
        })
        $prompts | Should -HaveCount $(if ($Allowed) { 1 } else { 0 })
        if ($Allowed) {
            $prompts[0].Fields.IsAutofix | Should -BeIn @('true', '1')
            $prompts[0].Types.IsAutofix | Should -Match 'Boolean$'
        }
    }

    It 'Startup policy buckets retain configured inventory and effective provider gates' {
        $blocked = @(Get-TelemetryPhaseEvents -Phase startup-policy-blocked -Name AppCreated)
        $allowlist = @(Get-TelemetryPhaseEvents -Phase startup-policy-allowlist -Name AppCreated)
        $blocked | Should -HaveCount 1
        $allowlist | Should -HaveCount 1
        $blocked[0].Fields.AllowedAgentsPolicy | Should -Be 'empty'
        $blocked[0].Fields.AllowCustomAgentsPolicy | Should -Be 'blocked'
        foreach ($role in @('Primary', 'Delegate')) {
            $blocked[0].Fields["${role}Provider"] | Should -Be 'custom'
            $blocked[0].Fields["${role}EffectiveProvider"] | Should -Be 'none'
            $blocked[0].Fields["${role}CustomConfiguredCount"] | Should -Be '2'
            $blocked[0].Fields["${role}CustomSelectedCommandConfigured"] | Should -BeIn @('true', '1')
        }
        $blocked[0].Fields.SidebarEnabled | Should -BeIn @('true', '1')
        @($script:records | Where-Object { $_.ProcessId -eq $script:startupCases.policyBlocked -and $_.Name -eq 'AgentSessionStarted' }) |
            Should -HaveCount 0
        $allowlist[0].Fields.AllowedAgentsPolicy | Should -Be 'allowlist'
        $allowlist[0].Fields.AllowCustomAgentsPolicy | Should -Be 'allowed'
        $allowlist[0].Fields.PrimaryProvider | Should -Be 'custom'
        $allowlist[0].Fields.PrimaryEffectiveProvider | Should -Be 'custom'
        $allowlist[0].Fields.DelegateProvider | Should -Be 'claude'
        $allowlist[0].Fields.DelegateEffectiveProvider | Should -Be 'none'
    }
}
