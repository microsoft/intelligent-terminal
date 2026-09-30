#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
Describe 'Telemetry funnel scenario helpers' -Tag Unit {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\tests\helpers\TelemetryFunnels.Scenarios.ps1')
        . (Join-Path $PSScriptRoot '..\tests\helpers\TelemetryPolicy.ps1')
        $script:root = Join-Path $PSScriptRoot ('..\artifacts\telemetry-scenario-unit-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:root -Force | Out-Null
    }
    AfterAll { Remove-Item -LiteralPath $script:root -Recurse -Force }

    It 'Calibrates tracerpt display offsets against logger FILETIME' {
        $utc = [DateTime]::SpecifyKind([DateTime]'2026-09-23T04:00:00', [DateTimeKind]::Utc)
        "<Events><Event><System><TimeCreated SystemTime='2026-09-23T12:00:00+07:59'/></System><EventData><Data Name='StartTime'>$($utc.ToFileTimeUtc())</Data></EventData></Event></Events>" |
            Set-Content -LiteralPath (Join-Path $script:root 'events.xml')
        Initialize-TelemetryPhaseClock -CaptureDirectory $script:root
        $script:traceClockCorrection.TotalSeconds | Should -Be -60
        $script:phaseErrors = @{}
        $script:phases = @{ example = @{ StartedUtc = '2026-09-23T04:00:00Z'; EndedUtc = '2026-09-23T04:00:10Z' } }
        $script:records = @(
            @{ Timestamp = '2026-09-23T12:00:05+07:59'; Name = 'inside'; Provider = 'fixture' }
            @{ Timestamp = '2026-09-23T12:00:11+07:59'; Name = 'outside'; Provider = 'fixture' }
        )
        @(Get-TelemetryPhaseEvents -Phase example).Name | Should -Be @('inside')
    }

    It 'Never turns an incomplete phase into zero-event negative evidence' {
        $script:phaseErrors = @{ failed = 'deliberate phase failure' }
        { Get-TelemetryPhaseEvents -Phase failed } | Should -Throw '*deliberate phase failure*'
    }

    It 'The reused ACP fixture completes two ordinary prompts without model requests' {
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $log = Join-Path $script:root 'fixture.log'
        $first = 'TELEMETRY_CHAT_' + [guid]::NewGuid().ToString('N')
        $second = 'TELEMETRY_CHAT_' + [guid]::NewGuid().ToString('N')
        $messages = @(
            @{ jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{} }
            @{ jsonrpc = '2.0'; id = 2; method = 'session/prompt'; params = @{ sessionId = 'offline-fixture'; prompt = @(@{ type = 'text'; text = $first }) } }
            @{ jsonrpc = '2.0'; id = 3; method = 'session/prompt'; params = @{ sessionId = 'offline-fixture'; prompt = @(@{ type = 'text'; text = $second }) } }
        ) | ForEach-Object { $_ | ConvertTo-Json -Depth 8 -Compress }
        $reply = @($messages | & (Get-Command pwsh).Source -NoProfile -File $fixture -LogPath $log | ForEach-Object { $_ | ConvertFrom-Json })
        $LASTEXITCODE | Should -Be 0
        @($reply | Where-Object { $_.result.stopReason -eq 'end_turn' }) | Should -HaveCount 2
        $evidence = Get-Content -LiteralPath $log -Raw
        $evidence | Should -Match ([regex]::Escape("telemetry-chat-complete|offline-fixture|$first"))
        $evidence | Should -Match ([regex]::Escape("telemetry-chat-complete|offline-fixture|$second"))
    }

    It 'Policy preparation refuses unapproved modification before opening registry keys' {
        $saved = $env:ITE2E_TELEMETRY_POLICY_APPROVED
        try {
            $env:ITE2E_TELEMETRY_POLICY_APPROVED = ''
            { Initialize-TelemetryPolicyTransaction -Directory $script:root } | Should -Throw '*explicit approval*'
            { Set-TelemetryPolicy -Transaction @{} -Name AllowAutoFix -Value 0 } | Should -Throw '*not approved*'
            Test-Path -LiteralPath (Join-Path $script:root 'hkcu-policy-original.clixml') | Should -BeFalse
        }
        finally { $env:ITE2E_TELEMETRY_POLICY_APPROVED = $saved }
    }

    It 'All scenario entry points exist immediately after dot-sourcing' {
        foreach ($name in @(
            'Invoke-TelemetryConversation', 'Invoke-TelemetryAutoFixPolicy', 'Invoke-TelemetryHotPolicyFailure', 'Invoke-TelemetryOffer',
            'Invoke-TelemetryPalette', 'Invoke-TelemetryProviderChanges', 'Invoke-TelemetryStartupCase',
            'Invoke-TelemetrySecondWindow', 'Invoke-TelemetryColdAutoFixPolicy', 'Wait-TelemetryOnlyOwnedAgent'
        )) {
            Get-Command $name -CommandType Function -ErrorAction Stop | Should -Not -BeNullOrEmpty
        }
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Get-Command Get-DescendantWtaIds -Module ItE2E -ErrorAction Stop | Should -Not -BeNullOrEmpty
    }

    It 'Phase process-evidence cleanup preserves an earlier scenario failure' {
        $script:app = @{ Launched = $true; Pid = 123 }
        $script:phases = [ordered]@{}
        $script:phaseErrors = @{}
        Mock Save-TelemetryOwnedProcesses { throw 'secondary evidence failure' }
        try {
            { Invoke-TelemetryPhase -Name test-failure -Action { throw 'original scenario failure' } } | Should -Not -Throw
            $script:phaseErrors['test-failure'].ToString() | Should -Match 'original scenario failure'
            $script:phases['test-failure'].CleanupError | Should -Match 'secondary evidence failure'
            (Get-Content -LiteralPath (Join-Path $script:root 'phases.json') -Raw) | Should -Match 'original scenario failure'
        }
        finally { $script:app = $null }
    }

    It 'Persists owned master and helper process IDs before later phases can fail' {
        Mock Get-DescendantWtaIds { @(101, 102) }
        $script:ownedPids = [Collections.Generic.HashSet[int]]::new()
        Save-TelemetryOwnedProcesses -App @{ Launched = $true; Pid = 100 }
        @(Get-Content (Join-Path $script:root 'owned-processes.json') -Raw | ConvertFrom-Json | Sort-Object) |
            Should -Be @(100, 101, 102)
    }

    It 'Stops only owned exact-path WTA descendants before the host without post-close COM probes' {
        $script:shutdownOrder = [Collections.Generic.List[string]]::new()
        Mock Save-TelemetryOwnedProcesses {}
        Mock Get-DescendantWtaIds { @(101, 102) }
        Mock Get-Process {
            @{ Path = $(if ($Id -eq 101) { 'C:\owned\wta.exe' } else { 'C:\unrelated\wta.exe' }) }
        }
        Mock Stop-Process { $script:shutdownOrder.Add("wta:$Id") }
        Mock Stop-Terminal { $script:shutdownOrder.Add('host') }
        Mock Get-WtWindows { throw 'post-close COM activation is forbidden' }
        Stop-TelemetryOwnedTerminal -App @{ Launched = $true; Pid = 100; WtaPath = 'C:\owned\wta.exe' }
        @($script:shutdownOrder) | Should -Be @('wta:101', 'host')
        Should -Invoke Get-WtWindows -Times 0 -Exactly
        { Stop-TelemetryOwnedTerminal -App @{ Launched = $false } } | Should -Throw '*not launched*'
    }

    It 'Sets startup policy before launch without claiming hot refresh for <Allowed>' -ForEach @(
        @{ Allowed = $false }, @{ Allowed = $true }
    ) {
        $script:coldAllowed = $Allowed
        $script:coldOrder = [Collections.Generic.List[string]]::new()
        $script:app = @{ Launched = $true; Pid = 100 }
        $script:policyTransaction = @{}
        $script:target = @{}
        $script:requestLog = Join-Path $script:root 'cold-fixture.log'
        Set-Content -LiteralPath $script:requestLog -Value ''
        Mock Stop-TelemetryOwnedTerminal { $script:coldOrder.Add('stop') }
        Mock Set-TelemetryPolicy {
            $script:app | Should -BeNullOrEmpty
            $Name | Should -Be 'AllowAutoFix'
            $Value | Should -Be ([int]$script:coldAllowed)
            $script:coldOrder.Add('policy')
        }
        Mock Invoke-TelemetryStartupCase {
            $Settings.autoFixEnabled | Should -BeTrue
            if (-not $script:coldAllowed) {
                $Settings.profiles.defaults.closeOnExit | Should -Be 'never'
                $Settings.profiles.list[0].closeOnExit | Should -Be 'never'
            }
            $script:coldOrder.Add('start')
            $script:app = @{ Launched = $true; Pid = 200 }
        }
        Mock Get-ActivePane { @{ session_id = 'cold-source' } }
        Mock Get-WtSettingsObject {
            [pscustomobject]@{ profiles = [pscustomobject]@{
                defaults = [pscustomobject]@{}
                list = @([pscustomobject]@{ guid = 'profile'; closeOnExit = 'always' })
            } }
        }
        Mock Open-AgentPane {}
        Mock Wait-NewAgentPaneSession { @{ PaneSessionId = 'cold-agent'; HelperProcessId = 201; AcpSessionId = 'cold-session' } }
        Mock Wait-TelemetryOnlyOwnedAgent { @{ PaneSessionId = 'cold-agent'; HelperProcessId = 201; AcpSessionId = 'cold-session' } }
        Mock Wait-AgentReady { $true }
        Mock Save-TelemetryOwnedProcesses {}
        Mock Start-WtEventListener { @{ Events = @() } }
        Mock Initialize-LogOffsets {}
        Mock Invoke-RunCommand {
            $script:coldOrder.Add('failure')
            if ($script:coldAllowed) { Add-Content -LiteralPath $script:requestLog -Value 'fixture|session/prompt|mock' }
        }
        Mock Wait-WtCommandFailure {}
        Mock Send-WtInput {
            $script:coldOrder.Add($(if ($Text -eq 'exit 37') { 'exit' } else { 'failure' }))
        }
        Mock Send-WtKeys {}
        Mock Assert-Pane { $Match | Should -Match 'Exception:' }
        Mock Get-WtCapture { 'rendered exception fixture' }
        Mock Wait-WtEvent { @{ method = 'connection_state'; params = @{ pane_id = 'cold-source'; state = 'failed' } } }
        Mock Get-WtPaneStatus { @{ state = 'running' } }
        Mock Get-ItLogText { 'surfacing Detected pill; sending auto-fix prompt' }
        Mock Wait-Until { & $Condition }
        Mock Start-Sleep {}
        Mock Get-WtEvents { @() }
        Mock Stop-WtEventListener {}
        try {
            $result = Invoke-TelemetryColdAutoFixPolicy -Allowed $Allowed -Command 'fixture'
            $expectedOrder = @('stop', 'policy', 'start', 'failure')
            if (-not $Allowed) { $expectedOrder += 'exit' }
            @($script:coldOrder) | Should -Be $expectedOrder
            $result.RawPolicy | Should -Be $(if ($Allowed) { 'enabled' } else { 'disabled' })
            $result.HotRefreshValidated | Should -BeFalse
            $result.SourcePane | Should -Be 'cold-source'
            $result.HelperPid | Should -Be 201
            $result.DetectionMethod | Should -Be $(if ($Allowed) { 'vt_sequence' } else { 'connection_state' })
            Should -Invoke Wait-WtCommandFailure -Times $(if ($Allowed) { 1 } else { 0 }) -Exactly
            Should -Invoke Wait-NewAgentPaneSession -Times $(if ($Allowed) { 1 } else { 0 }) -Exactly
            Should -Invoke Wait-TelemetryOnlyOwnedAgent -Times $(if ($Allowed) { 0 } else { 1 }) -Exactly
        }
        finally { $script:app = $null }
    }

    It 'Non-VT discovery requires one fresh live helper in the owned process and refuses ambiguity' {
        $script:discoverySessions = @(@{ PaneSessionId = 'fresh'; HelperProcessId = 201 })
        $script:discoveryOwned = @(201)
        Mock Get-WtWindows { @{ window_id = 1 } }
        Mock Get-WtTabs { @{ tab_id = 0 } }
        Mock Get-AgentPaneSessions { $script:discoverySessions }
        Mock Get-DescendantWtaIds { $script:discoveryOwned }
        Mock Resolve-AgentOwnerTabId { throw 'VT probing is forbidden' }
        Mock Wait-Until {
            $result = & $Condition
            if (-not $result) { throw 'no matched owned helper' }
            $result
        }
        $app = @{ Launched = $true; Pid = 200; WindowId = 1 }
        (Wait-TelemetryOnlyOwnedAgent -App $app).HelperProcessId | Should -Be 201
        $script:discoveryOwned = @(999)
        { Wait-TelemetryOnlyOwnedAgent -App $app } | Should -Throw '*no matched owned helper*'
        $script:discoverySessions += @{ PaneSessionId = 'another'; HelperProcessId = 202 }
        { Wait-TelemetryOnlyOwnedAgent -App $app } | Should -Throw '*Ambiguous*'
        Should -Invoke Resolve-AgentOwnerTabId -Times 0 -Exactly
    }

    It 'Hot-policy post-barrier failure uses the policy-permitted oracle for <Allowed>' -ForEach @(
        @{ Allowed = $false }, @{ Allowed = $true }
    ) {
        $script:app = @{}
        $script:shell = @{ session_id = 'hot-source' }
        $script:agent = @{ HelperProcessId = 201 }
        $script:requestLog = Join-Path $script:root 'hot-policy-fixture.log'
        Set-Content -LiteralPath $script:requestLog -Value ''
        $script:forwardedHotVt = $false
        Mock Initialize-LogOffsets {}
        Mock Invoke-RunCommand {}
        Mock Wait-WtCommandFailure {}
        Mock Wait-Until { & $Condition }
        Mock Get-ItLogText { 'sending auto-fix prompt' }
        Mock Send-WtInput { $script:hotCommand = $Text }
        Mock Send-WtKeys {}
        Mock Assert-Pane {
            $script:hotCommand | Should -Match "^throw 'TELEMETRY_POLICY_[a-f0-9]{32}'$"
            $marker = [regex]::Match($script:hotCommand, 'TELEMETRY_POLICY_[a-f0-9]{32}').Value
            "Exception: $marker" | Should -Match $Match
            $script:hotCommand | Should -Not -Match $Match -Because 'echoed input is not proof the exception executed'
        }
        Mock Get-WtCapture { 'rendered exception fixture' }
        Mock Start-Sleep {}
        Mock Get-WtEvents {
            if ($script:forwardedHotVt) { @{ method = 'vt_sequence'; params = @{ pane_id = 'hot-source' } } }
        }
        try {
            $marker = Invoke-TelemetryHotPolicyFailure -Allowed $Allowed -Listener @{} -Baseline 0
            $marker | Should -Match '^TELEMETRY_POLICY_[a-f0-9]{32}$'
            Should -Invoke Wait-WtCommandFailure -Times $(if ($Allowed) { 1 } else { 0 }) -Exactly
            Should -Invoke Invoke-RunCommand -Times $(if ($Allowed) { 1 } else { 0 }) -Exactly
            Should -Invoke Assert-Pane -Times $(if ($Allowed) { 0 } else { 1 }) -Exactly
            if (-not $Allowed) {
                $script:forwardedHotVt = $true
                { Invoke-TelemetryHotPolicyFailure -Allowed $false -Listener @{} -Baseline 0 } |
                    Should -Throw '*blocked policy must suppress*'
            }
        }
        finally { $script:app = $null }
    }
}
