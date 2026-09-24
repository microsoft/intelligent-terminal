function Save-TelemetryOwnedProcesses {
    param([Parameter(Mandatory)]$App)
    if (-not $App.Launched) { throw 'Telemetry scenarios require a test-owned window.' }
    [void]$script:ownedPids.Add([int]$App.Pid)
    foreach ($processId in @(Get-DescendantWtaIds -RootPid ([int]$App.Pid))) {
        [void]$script:ownedPids.Add([int]$processId)
    }
    # Retain the master as well as the helper even if a later setup phase aborts.
    @($script:ownedPids) | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'owned-processes.json')
}

function Stop-TelemetryOwnedTerminal {
    param([Parameter(Mandatory)]$App)
    if (-not $App.Launched) { throw 'Refusing to close a window not launched by this telemetry run.' }
    Save-TelemetryOwnedProcesses -App $App
    # Stop only this window process's WTA descendants while its COM server is
    # still alive, so shutdown callbacks cannot activate a headless replacement.
    foreach ($processId in @(Get-DescendantWtaIds -RootPid ([int]$App.Pid))) {
        $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($process -and $process.Path -eq $App.WtaPath) {
            Stop-Process -Id $processId -Force -ErrorAction Stop
        }
    }
    Stop-Terminal -App $App -RestoreSettings $false
}

function Invoke-TelemetryPhase {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][scriptblock]$Action)
    $phase = [ordered]@{ Name = $Name; StartedUtc = [DateTime]::UtcNow.ToString('o'); Data = $null; Error = $null; CleanupError = $null }
    try { $phase.Data = & $Action }
    catch { $phase.Error = $_.ToString(); $script:phaseErrors[$Name] = $_ }
    finally {
        $phase.EndedUtc = [DateTime]::UtcNow.ToString('o')
        $script:phases[$Name] = [pscustomobject]$phase
        try {
            if ($script:app) { Save-TelemetryOwnedProcesses -App $script:app }
        }
        catch {
            $phase.CleanupError = $_.ToString()
            if (-not $script:phaseErrors.ContainsKey($Name)) {
                $script:phaseErrors[$Name] = $_
                $phase.Error = $_.ToString()
            }
        }
        $script:phases[$Name] = [pscustomobject]$phase
        $script:phases | ConvertTo-Json -Depth 18 | Set-Content -LiteralPath (Join-Path $script:root 'phases.json')
    }
}

function Get-TelemetryPhaseEvents {
    param([Parameter(Mandatory)][string]$Phase, [string]$Name, [string]$Provider)
    if ($script:phaseErrors.ContainsKey($Phase)) { throw $script:phaseErrors[$Phase] }
    $bounds = $script:phases[$Phase]
    if (-not $bounds) { throw "Telemetry phase was not executed: $Phase" }
    $script:records | Where-Object {
        $time = ([DateTimeOffset]$_.Timestamp).UtcDateTime.Add($script:traceClockCorrection)
        $time -ge ([DateTimeOffset]$bounds.StartedUtc).UtcDateTime -and $time -le ([DateTimeOffset]$bounds.EndedUtc).UtcDateTime -and
        (-not $Name -or $_.Name -eq $Name) -and (-not $Provider -or $_.Provider -eq $Provider)
    }
}

function Initialize-TelemetryPhaseClock {
    param([Parameter(Mandatory)][string]$CaptureDirectory)
    # tracerpt can render a rounded local UTC offset. Calibrate against the
    # logger header's FILETIME rather than assuming its displayed offset is exact.
    [xml]$xml = Get-Content -LiteralPath (Join-Path $CaptureDirectory 'events.xml') -Raw
    $header = $xml.SelectSingleNode("//*[local-name()='Event'][*[local-name()='EventData']/*[local-name()='Data'][@Name='StartTime']]")
    if (-not $header) { throw 'ETL logger header is required for phase timing.' }
    $fileTime = [long]$header.SelectSingleNode("*[local-name()='EventData']/*[local-name()='Data'][@Name='StartTime']").InnerText
    $displayed = [DateTimeOffset]$header.SelectSingleNode("*[local-name()='System']/*[local-name()='TimeCreated']").GetAttribute('SystemTime')
    $script:traceClockCorrection = [DateTime]::FromFileTimeUtc($fileTime) - $displayed.UtcDateTime
    @{ correctionTicks = $script:traceClockCorrection.Ticks; headerFileTime = $fileTime } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'phase-clock.json')
}

function Invoke-TelemetryConversation {
    foreach ($number in 1..2) {
        $marker = 'TELEMETRY_CHAT_' + [guid]::NewGuid().ToString('N')
        Clear-AgentInput -App $script:app -PaneSessionId $script:agent.PaneSessionId | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agent.PaneSessionId -Text $marker | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'the same ACP session completes a normal fixture prompt' -Condition {
            (Get-Content -LiteralPath $script:requestLog -Raw) -match
                ([regex]::Escape("telemetry-chat-complete|$($script:agent.AcpSessionId)|$marker"))
        } | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agent.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 15
        Start-Sleep -Milliseconds 300
    }

    @{ SessionId = $script:agent.AcpSessionId; ExplicitPrompts = 2 }
}

function Invoke-TelemetryHotPolicyFailure {
    param([Parameter(Mandatory)][bool]$Allowed, [Parameter(Mandatory)]$Listener, [int]$Baseline)
    Initialize-LogOffsets -App $script:app | Out-Null
    $marker = 'TELEMETRY_POLICY_' + [guid]::NewGuid().ToString('N')
    if ($Allowed) {
        Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id -Command "throw '$marker'" | Out-Null
        Wait-WtCommandFailure -Listener $Listener -PaneId $script:shell.session_id -TimeoutSec 20 | Out-Null
        Wait-Until -TimeoutSec 20 -Because 'the enabled helper classifies the forwarded shell failure' -Condition {
            (Get-ItLogText -App $script:app -Name "wta-main_helper-$($script:agent.HelperProcessId).log" -SinceStart) -match 'sending auto-fix prompt'
        } | Out-Null
    }
    else {
        Send-WtInput -App $script:app -SessionId $script:shell.session_id -Text "throw '$marker'"
        Send-WtKeys -App $script:app -SessionId $script:shell.session_id -Keys @('Enter')
        Assert-Pane -App $script:app -SessionId $script:shell.session_id -Match ("(?m)^\s*Exception:\s*" + [regex]::Escape($marker) + "\s*$") -TimeoutSec 20
        Get-WtCapture -App $script:app -SessionId $script:shell.session_id -MaxLines 100 |
            Set-Content -LiteralPath (Join-Path $script:root 'hot-policy-disabled-shell-failure.txt')
        Start-Sleep -Seconds 1
        @(Get-WtEvents -Listener $Listener -Predicate {
            $_.method -eq 'vt_sequence' -and $_.params.pane_id -eq $script:shell.session_id
        }) | Should -HaveCount 0 -Because 'blocked policy must suppress forwarding of the real, rendered shell failure'
        @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count |
            Should -Be $Baseline -Because 'blocked policy must neither submit the new failure nor replay an older one'
    }
    $marker
}

function Invoke-TelemetryAutoFixPolicy {
    param([Parameter(Mandatory)][bool]$Allowed)
    $listener = Start-WtEventListener -App $script:app -WaitForReady
    $baseline = @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count
    try {
        Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowAutoFix -Value ([int]$Allowed)
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Policies\Microsoft\IntelligentTerminal')
        try {
            if (-not $key) { throw 'The acknowledged policy write is absent from medium-client HKCU.' }
            $readback = Get-TelemetryPolicyValue -Key $key -Name AllowAutoFix
            $readback | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root "policy-$Allowed-readback.json")
            $readback.Exists | Should -BeTrue
            $readback.Kind | Should -Be 'DWord'
            $readback.Value | Should -Be ([int]$Allowed)
        }
        finally { if ($key) { $key.Dispose() } }
        $raw = if ($Allowed) { 'enabled' } else { 'disabled' }
        $config = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
            $_.method -eq 'agent_config_changed' -and $_.params.autofix_policy_state -eq $raw -and
            $null -ne $_.params.autofix_enabled -and $_.params.autofix_enabled -eq $Allowed
        }
        ($config.params.autofix_enabled -is [bool]) | Should -BeTrue
        ($config.params.autofix_policy_state -is [string]) | Should -BeTrue
        $config.params.autofix_enabled | Should -Be $Allowed
        $config.params.autofix_policy_state | Should -Be $raw
        Start-Sleep -Seconds 1
        @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count |
            Should -Be $baseline -Because 'a policy-only change must not replay a previous terminal error'
        $marker = Invoke-TelemetryHotPolicyFailure -Allowed $Allowed -Listener $listener -Baseline $baseline
        @{ Allowed = $Allowed; RawPolicy = $raw; Config = $config; SourcePane = $script:shell.session_id; Marker = $marker }
    }
    finally {
        try {
            ConvertTo-Json -InputObject @(Get-WtEvents -Listener $listener) -Depth 14 |
                Set-Content -LiteralPath (Join-Path $script:root "policy-$Allowed-events.json")
        }
        catch { Write-Warning "Could not retain policy event diagnostics: $_" }
        Stop-WtEventListener -Listener $listener
    }
}

function Wait-TelemetryOnlyOwnedAgent {
    param([Parameter(Mandatory)]$App)
    if (-not $App.Launched) { throw 'Helper discovery requires the freshly launched owned host.' }
    @(Get-WtWindows -App $App) | Should -HaveCount 1
    @(Get-WtTabs -App $App -WindowId ([string]$App.WindowId)) | Should -HaveCount 1
    Wait-Until -TimeoutSec 30 -Because 'exactly one new live helper belonging to the sole owned window' -Condition {
        $sessions = @(Get-AgentPaneSessions -App $App)
        if ($sessions.Count -gt 1) { throw 'Ambiguous live helper sessions; refusing newest-record selection.' }
        $owned = @(Get-DescendantWtaIds -RootPid ([int]$App.Pid))
        if ($sessions.Count -eq 1 -and [int]$sessions[0].HelperProcessId -in $owned) { $sessions[0] }
    }
}

function Invoke-TelemetryColdAutoFixPolicy {
    param([Parameter(Mandatory)][bool]$Allowed, [Parameter(Mandatory)][string]$Command)
    if ($script:app) {
        Stop-TelemetryOwnedTerminal -App $script:app
        $script:app = $null
    }
    Set-TelemetryPolicy -Transaction $script:policyTransaction -Name AllowAutoFix -Value ([int]$Allowed)
    $settings = @{
        acpAgent = 'custom:pwsh'; acpCustomCommand = $Command; acpCustomCommands = @(); acpModel = ''
        delegateAgent = ''; delegateCustomCommand = ''; delegateCustomCommands = @()
        autoFixEnabled = $true; autoErrorDetectionEnabled = $true
        tabLayout = 'horizontal'; 'agentPane.yoloMode' = $false
    }
    if (-not $Allowed) {
        $profiles = (Get-WtSettingsObject -App $script:target).profiles
        if ($profiles -is [array]) { $profiles = [pscustomobject]@{ list = $profiles } }
        if (-not $profiles) { $profiles = [pscustomobject]@{} }
        if (-not $profiles.defaults) { $profiles | Add-Member -NotePropertyName defaults -NotePropertyValue ([pscustomobject]@{}) -Force }
        $profiles.defaults | Add-Member -NotePropertyName closeOnExit -NotePropertyValue 'never' -Force
        foreach ($profile in @($profiles.list)) {
            if ($profile) { $profile | Add-Member -NotePropertyName closeOnExit -NotePropertyValue 'never' -Force }
        }
        $settings.profiles = $profiles
    }
    Invoke-TelemetryStartupCase -Name "coldAutofix-$Allowed" -Settings $settings | Out-Null
    $shell = Get-ActivePane -App $script:app
    Open-AgentPane -App $script:app | Out-Null
    $agent = if ($Allowed) {
        Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30
    }
    else {
        # The native policy intentionally blocks OSC forwarding, including the
        # normal owner-tab probe. Registry discovery is safe only in this sole,
        # fresh window with one live helper proven to descend from its process.
        Wait-TelemetryOnlyOwnedAgent -App $script:app
    }
    Wait-AgentReady -App $script:app -PaneSessionId $agent.PaneSessionId -TimeoutSec 60 | Should -BeTrue
    Save-TelemetryOwnedProcesses -App $script:app
    $listener = Start-WtEventListener -App $script:app -WaitForReady
    try {
        $before = @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count
        Initialize-LogOffsets -App $script:app | Out-Null
        $marker = 'TELEMETRY_COLD_POLICY_' + [guid]::NewGuid().ToString('N')
        $connectionEnd = $null
        if ($Allowed) {
            Invoke-RunCommand -App $script:app -SessionId $shell.session_id -Command "throw '$marker'" | Out-Null
            Wait-WtCommandFailure -Listener $listener -PaneId $shell.session_id -TimeoutSec 20 | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'the enabled helper classifies the actual forwarded shell failure' -Condition {
                (Get-ItLogText -App $script:app -Name "wta-main_helper-$($agent.HelperProcessId).log" -SinceStart) -match 'sending auto-fix prompt'
            } | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'enabled startup policy produces one real fixture Autofix prompt' -Condition {
                @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count -eq ($before + 1)
            } | Out-Null
        }
        else {
            Send-WtInput -App $script:app -SessionId $shell.session_id -Text "throw '$marker'"
            Send-WtKeys -App $script:app -SessionId $shell.session_id -Keys @('Enter')
            Assert-Pane -App $script:app -SessionId $shell.session_id -Match ("(?m)^\s*Exception:\s*" + [regex]::Escape($marker) + "\s*$") -TimeoutSec 20
            Get-WtCapture -App $script:app -SessionId $shell.session_id -MaxLines 100 |
                Set-Content -LiteralPath (Join-Path $script:root 'cold-policy-disabled-shell-failure.txt')
            @(Get-WtEvents -Listener $listener -Predicate {
                $_.method -eq 'vt_sequence' -and $_.params.pane_id -eq $shell.session_id
            }) | Should -HaveCount 0 -Because 'blocked policy intentionally suppresses OSC forwarding, despite the rendered real failure'
            @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count | Should -Be $before
            Send-WtInput -App $script:app -SessionId $shell.session_id -Text 'exit 37'
            Send-WtKeys -App $script:app -SessionId $shell.session_id -Keys @('Enter')
            $connectionEnd = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'connection_state' -and $_.params.pane_id -eq $shell.session_id -and $_.params.state -eq 'failed'
            }
            (Get-WtPaneStatus -App $script:app -SessionId $agent.PaneSessionId).state | Should -Match 'run'
            Start-Sleep -Seconds 1
            @(Get-WtEvents -Listener $listener -Predicate {
                $_.method -eq 'vt_sequence' -and $_.params.pane_id -eq $shell.session_id
            }) | Should -HaveCount 0
            @(Select-String -LiteralPath $script:requestLog -Pattern '\|session/prompt\|').Count | Should -Be $before
        }
        @{
            Allowed = $Allowed; RawPolicy = $(if ($Allowed) { 'enabled' } else { 'disabled' })
            ProcessId = $script:app.Pid; HelperPid = $agent.HelperProcessId
            SourcePane = $shell.session_id; SessionId = $agent.AcpSessionId; Marker = $marker
            StartupPolicy = $true; HotRefreshValidated = $false
            DetectionMethod = $(if ($Allowed) { 'vt_sequence' } else { 'connection_state' })
            ConnectionEnd = $connectionEnd
        }
    }
    finally {
        try {
            ConvertTo-Json -InputObject @(Get-WtEvents -Listener $listener) -Depth 14 |
                Set-Content -LiteralPath (Join-Path $script:root "cold-policy-$Allowed-events.json")
        }
        catch { Write-Warning "Could not retain startup-policy event diagnostics: $_" }
        Stop-WtEventListener -Listener $listener
    }
}

function Invoke-TelemetryOffer {
    param([ValidateSet('Run', 'Insert', 'Reject')][string]$Decision)
    $marker = 'TELEMETRY_FIX_' + [guid]::NewGuid().ToString('N')
    $output = Join-Path $script:root "$marker.executed"
    @{ mode = 'offer'; marker = $marker; outputPath = $output } |
        ConvertTo-Json | Set-Content -LiteralPath $script:telemetryFixture
    $listener = Start-WtEventListener -App $script:app -WaitForReady
    try {
        Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
        Invoke-RunCommand -App $script:app -SessionId $script:shell.session_id -Command "throw '$marker'" | Out-Null
        Wait-WtCommandFailure -Listener $listener -PaneId $script:shell.session_id -TimeoutSec 20 | Out-Null
        $gate = Wait-TerminalActionProposal -App $script:app -PaneSessionId $script:agent.PaneSessionId -ReturnOnPermission -TimeoutSec 30
        if ($gate.Mode -eq 'Permission') { Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Y | Out-Null }
        Wait-Until -TimeoutSec 30 -Because 'the current Autofix recommendation is visibly presented' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agent.PaneSessionId -MaxLines 80
            $text -match (Get-RecommendationCardRegex) -and
                ($text -replace '[\s│]', '').Contains($marker)
        } | Out-Null
        foreach ($key in @('Right', 'Left', 'Right', 'Left')) {
            Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key $key | Out-Null
        }
        $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agent.PaneSessionId -MaxLines 80
        ($text -replace '[\s│]', '') | Should -Match $marker -Because 'redraw retains the same complete offer marker rather than replacing it'
        Set-Content -LiteralPath (Join-Path $script:root "offer-$Decision-card.txt") -Value $text
        (Test-Path -LiteralPath $output) | Should -BeFalse -Because 'presenting a card must not execute it'
        $decisionUtc = [DateTime]::UtcNow.ToString('o')
        if ($Decision -eq 'Reject') {
            Wait-Until -TimeoutSec 12 -Because 'Escape dismisses the current offer' -Condition {
                Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Escape | Out-Null
                (Get-AgentPaneText -App $script:app -PaneSessionId $script:agent.PaneSessionId -MaxLines 60) -notmatch (Get-RecommendationCardRegex)
            } | Out-Null
        }
        else {
            $runLabel = Get-WtaLocalizedTextRegex -Key 'recommendations.button_run_command'
            $text | Should -Match $runLabel -Because 'the localized Run action remains visible after redraw'
            if ($Decision -eq 'Insert') {
                Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Right | Out-Null
            }
            Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key Enter | Out-Null
        }
        Wait-Until -TimeoutSec 30 -Because 'the fixture records its real Session MCP proposal response' -Condition {
            (Get-Content -LiteralPath $script:requestLog -Raw) -match ([regex]::Escape("telemetry-proposal-result|$marker|"))
        } | Out-Null
        if ($Decision -eq 'Run') {
            Wait-Until -TimeoutSec 20 -Because 'the harmless queued command really executes in the source shell' -Condition {
                (Test-Path -LiteralPath $output) -and (Get-Content -LiteralPath $output -Raw).Trim() -eq $marker
            } | Out-Null
        }
        else {
            (Test-Path -LiteralPath $output) | Should -BeFalse
            if ($Decision -eq 'Insert') {
                Assert-Pane -App $script:app -SessionId $script:shell.session_id -Match $marker -TimeoutSec 10
                Send-WtKeys -App $script:app -SessionId $script:shell.session_id -Keys @('C-c') | Out-Null
            }
        }
        Start-Sleep -Seconds 1
        @{ Decision = $Decision; Marker = $marker; DecisionUtc = $decisionUtc; OutputPath = $output; Executed = Test-Path -LiteralPath $output }
    }
    catch {
        $failure = $_
        try {
            Get-AgentPaneText -App $script:app -PaneSessionId $script:agent.PaneSessionId -MaxLines 100 |
                Set-Content -LiteralPath (Join-Path $script:root "offer-$Decision-failure-card.txt")
        }
        catch { Write-Warning "Could not retain failed offer rendering: $_" }
        try { Send-WtKeys -App $script:app -SessionId $script:shell.session_id -Keys @('C-c') | Out-Null }
        catch { Write-Warning "Could not clear the failed offer's pending shell input: $_" }
        throw $failure
    }
    finally {
        Stop-WtEventListener -Listener $listener
        @{ mode = 'chat' } | ConvertTo-Json | Set-Content -LiteralPath $script:telemetryFixture
    }
}

function Invoke-TelemetryPalette {
    # The temporary custom delegate only exits; it cannot invoke a real model.
    Set-WtSettings -App $script:app -Settings @{
        delegateAgent = 'custom:cmd'; delegateCustomCommand = 'cmd.exe /d /c exit 0'
    } | Out-Null
    Start-Sleep -Seconds 2
    Send-WtWindowKey -App $script:app -Vk 0xBF -Alt -Shift -RequireForeground | Out-Null
    Wait-Until -TimeoutSec 10 -Because 'foreground agent palette is visible' -Condition { Test-CommandPaletteOpen -App $script:app } | Out-Null
    Set-UiValue -App $script:app -Selector '_searchBox' -Value '?telemetry abandoned' | Out-Null
    Set-UiValue -App $script:app -Selector '_searchBox' -Value '?telemetry abandoned edited' | Out-Null
    $hideUtc = [DateTime]::UtcNow.ToString('o')
    Send-WtWindowKey -App $script:app -Vk 0xBF -Alt -Shift -RequireForeground | Out-Null
    Wait-Until -TimeoutSec 10 -Because 'same shortcut hides rather than reenters the palette' -Condition { -not (Test-CommandPaletteOpen -App $script:app) } | Out-Null
    $reopenUtc = [DateTime]::UtcNow.ToString('o')
    Send-WtWindowKey -App $script:app -Vk 0xBF -Alt -Shift -RequireForeground | Out-Null
    Wait-Until -TimeoutSec 10 -Because 'foreground palette reopens' -Condition { Test-CommandPaletteOpen -App $script:app } | Out-Null
    Set-UiValue -App $script:app -Selector '_searchBox' -Value '?telemetry harmless submission' | Out-Null
    $submitUtc = [DateTime]::UtcNow.ToString('o')
    Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
    Wait-Until -TimeoutSec 10 -Because 'submission closes the palette' -Condition { -not (Test-CommandPaletteOpen -App $script:app) } | Out-Null
    Start-Sleep -Seconds 2
    $backgroundUtc = [DateTime]::UtcNow.ToString('o')
    Send-WtWindowKey -App $script:app -Vk 0x50 -Ctrl -Shift -RequireForeground | Out-Null
    Wait-Until -TimeoutSec 10 -Because 'normal palette opens for the background negative control' -Condition { Test-CommandPaletteOpen -App $script:app } | Out-Null
    Set-UiValue -App $script:app -Selector '_searchBox' -Value '&telemetry background abandoned' | Out-Null
    Send-WtWindowKey -App $script:app -Vk 0x1B -Repeat 2 -RequireForeground | Out-Null
    Wait-Until -TimeoutSec 10 -Because 'background mode is abandoned without submission' -Condition { -not (Test-CommandPaletteOpen -App $script:app) } | Out-Null
    @{ HideUtc = $hideUtc; ReopenUtc = $reopenUtc; SubmitUtc = $submitUtc; BackgroundUtc = $backgroundUtc; ExpectedEntries = 2; ExpectedSubmissions = 1 }
}

function Invoke-TelemetryProviderChanges {
    $changes = @(
        @{ Key = 'acpAgent'; Value = 'custom:pwsh'; Role = 'primary'; From = 'custom'; To = 'custom' }
        @{ Key = 'delegateAgent'; Value = 'claude'; Role = 'delegate'; From = 'custom'; To = 'claude' }
        @{ Key = 'delegateAgent'; Value = 'custom:cmd'; Role = 'delegate'; From = 'claude'; To = 'custom' }
        @{ Key = 'delegateAgent'; Value = 'custom:pwsh'; Role = 'delegate'; From = 'custom'; To = 'custom' }
        @{ Key = 'acpAgent'; Value = ''; Role = 'primary'; From = 'custom'; To = 'none' }
    )
    foreach ($change in $changes) {
        Set-WtSetting -App $script:app -Key $change.Key -Value $change.Value | Out-Null
        Start-Sleep -Seconds 2
        Save-TelemetryOwnedProcesses -App $script:app
    }
    $listener = Start-WtEventListener -App $script:app -WaitForReady
    try {
        $unchangedUtc = [DateTime]::UtcNow.ToString('o')
        $autofix = -not [bool](Get-WtSettingsObject -App $script:app).autoFixEnabled
        Set-WtSetting -App $script:app -Key delegateAgent -Value 'custom:pwsh' | Out-Null
        Set-WtSetting -App $script:app -Key copyOnSelect -Value (-not [bool](Get-WtSettingsObject -App $script:app).copyOnSelect) | Out-Null
        # The runtime-config diff follows native settings application. A disk
        # readback or a fixed sleep cannot prove the provider baseline was used.
        Set-WtSetting -App $script:app -Key autoFixEnabled -Value $autofix | Out-Null
        $applied = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
            $_.method -eq 'agent_config_changed' -and -not $_.params.tab_id -and
            [string]$_.params.window_id -eq [string]$script:app.WindowId -and
            $null -ne $_.params.autofix_enabled -and $_.params.autofix_enabled -eq $autofix
        }
        @{
            Expected = $changes; UnchangedUtc = $unchangedUtc
            UnchangedApplied = $applied; UnchangedAutofix = $autofix
        }
    }
    finally { Stop-WtEventListener -Listener $listener }
}

function Invoke-TelemetrySecondWindow {
    $before = @(Get-WtWindows -App $script:app)
    $beforeIds = @($before | ForEach-Object { [string]$_.window_id })
    $beforeIds | Should -Contain ([string]$script:app.WindowId)
    $hwnds = @(Get-WtWindowHwnds -App $script:app | ForEach-Object { [string]$_.hwnd })
    $secondId = $null
    try {
        # Use the shipped newWindow action through real input. Do not launch a
        # packaged EXE directly or use Start-Terminal's cold-start behavior.
        Send-WtWindowKey -App $script:app -Vk 0x4E -Ctrl -Shift -RequireForeground | Out-Null
        $secondId = Wait-Until -TimeoutSec 20 -Because 'Ctrl+Shift+N creates exactly one new packaged window' -Condition {
            $created = @(Get-WtWindows -App $script:app | Where-Object { [string]$_.window_id -notin $beforeIds })
            if ($created.Count -eq 1) { [string]$created[0].window_id }
        }
        $newHwnd = Wait-Until -TimeoutSec 15 -Because 'the second window belongs to the same owned process' -Condition {
            Get-WtWindowHwnds -App $script:app | Where-Object {
                [int]$_.pid -eq [int]$script:app.Pid -and [string]$_.hwnd -notin $hwnds
            } | Select-Object -First 1
        }
        $second = $script:app.PSObject.Copy()
        $second.Hwnd = $newHwnd.hwnd
        $second.WindowId = $secondId
        $activationUtc = [DateTime]::UtcNow.ToString('o')
        foreach ($context in @($script:app, $second, $script:app)) {
            Send-WtWindowKey -App $context -Vk 0x10 -RequireForeground | Out-Null
        }
        @(Get-WtWindows -App $script:app).Count | Should -Be ($before.Count + 1)
        Save-TelemetryOwnedProcesses -App $script:app
        @{ ProcessId = $script:app.Pid; SecondWindowId = $secondId; ActivationUtc = $activationUtc }
    }
    finally {
        if ($secondId) {
            foreach ($tab in @(Get-WtTabs -App $script:app -WindowId $secondId)) {
                foreach ($pane in @(Get-WtPanes -App $script:app -WindowId $secondId -TabId ([string]$tab.tab_id))) {
                    Close-WtPane -App $script:app -SessionId $pane.session_id
                }
            }
            Wait-Until -TimeoutSec 15 -Because 'only the second window closes before later settings phases' -Condition {
                $ids = @(Get-WtWindows -App $script:app | ForEach-Object { [string]$_.window_id })
                $secondId -notin $ids -and [string]$script:app.WindowId -in $ids
            } | Out-Null
        }
        Set-WtPaneFocus -App $script:app -SessionId $script:shell.session_id
        Set-WtWindowForeground -App $script:app | Out-Null
    }
}

function Invoke-TelemetryStartupCase {
    param([Parameter(Mandatory)][string]$Name, [hashtable]$Settings)
    if ($script:app) {
        Save-TelemetryOwnedProcesses -App $script:app
        Stop-TelemetryOwnedTerminal -App $script:app
        $script:app = $null
    }
    @(Get-WtProcessesForApp -App $script:target) | Should -HaveCount 0
    $script:app = Start-Terminal -Package Dev -Backup $false -CleanSettings $false -PassFre $true -Settings $Settings
    Save-TelemetryOwnedProcesses -App $script:app
    $script:startupCases[$Name] = [int]$script:app.Pid
    Start-Sleep -Seconds 2
    @{ Name = $Name; ProcessId = $script:app.Pid }
}
