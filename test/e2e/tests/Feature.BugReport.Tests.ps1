#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §10 (C193) — the "Report a bug (collect logs)" command palette entry
# (bugReport / Terminal.BugReport action) bundles the IntelligentTerminal logs directory into a
# timestamped zip on the Desktop (AppActionHandlers.cpp _CreateBugReportZipAsync). This drives that
# command via the command palette and asserts the produced zip actually contains agent logs
# (wta-*.log / terminal-agent-pane.log), so a bug report is useful for diagnosing agent issues.
#
# The command palette is a WT window surface reached by the Ctrl+Shift+P accelerator, so this needs
# the WT window to hold foreground (Send-WtWindowKey). In an agent-driven session the controlling
# terminal can steal foreground; when it can't be taken the case SKIPS (a foreground precondition)
# rather than failing flakily.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command pwsh -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §10 bug report zip (collect logs)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:artifactRoot = Join-Path $PSScriptRoot "..\artifacts\bug-report-$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $script:artifactRoot -Force | Out-Null
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $script:fixtureLog = Join-Path $script:artifactRoot 'fixture.log'
        $fixtureCommand = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $encodedFixture = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureCommand))
        foreach ($name in @('WTA_LOG', 'RUST_LOG')) {
            foreach ($scope in @('Process', 'User', 'Machine')) {
                [Environment]::GetEnvironmentVariable($name, $scope) | Should -BeNullOrEmpty -Because 'this suite verifies Release WTA defaults, not explicit logging overrides'
            }
        }
        $target = Resolve-ItApp -Package (Get-ItTestPackage)
        # Start-Terminal cold-starts the chosen package. Refuse to close user-owned windows.
        (& (Get-Module ItE2E) { param($App) @(Get-WtProcessesForApp -App $App).Count } $target) |
            Should -Be 0 -Because 'close existing selected-package windows normally before running this suite'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:bug-report-private-provider'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $encodedFixture"
            acpModel = ''
            autoFixEnabled = $false
            autoErrorDetectionEnabled = $true
        }
        $script:source = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        $script:agent = Wait-NewAgentPaneSession -App $script:app -TimeoutSec 45
        Wait-AgentReady -App $script:app -PaneSessionId $script:agent.PaneSessionId -TimeoutSec 60 | Should -BeTrue
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agent.PaneSessionId) | Out-Null
        $script:context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--max-lines', '0')
        $script:context.pane.session_id | Should -Be $script:source.session_id
        $script:missingSource = [guid]::NewGuid().ToString()
        { Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $script:missingSource) } | Should -Throw
        $script:sentinel = "BUG_REPORT_PRIVATE_$([guid]::NewGuid().ToString('N'))"
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agent.PaneSessionId -Text $script:sentinel | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'real helper prompt binds and reaches deterministic ACP fixture' -Condition {
            $text = Get-ItLogText -App $script:app -Name "wta-main_helper-$($script:agent.HelperProcessId).log"
            $text -match 'prompt_binding_issued' -and
                (Get-Content -LiteralPath $script:fixtureLog -Raw -ErrorAction SilentlyContinue).Contains($script:sentinel)
        } | Out-Null
        Stop-AgentPane -App $script:app | Out-Null
        $script:desktop = [Environment]::GetFolderPath('Desktop')
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
        if ($script:zip -and $env:ITE2E_RETAIN_BUG_REPORT -ne '1') {
            Remove-Item -LiteralPath $script:zip -Force -ErrorAction SilentlyContinue
        }
    }

    It 'Bug report zip includes agent logs (Report a bug collects wta/agent-pane logs into a Desktop zip)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+Shift+P command palette (competing foreground app)'; return }
        $before = @(Get-ChildItem $script:desktop -Filter 'intelligent-terminal-logs-*.zip' -ErrorAction SilentlyContinue | ForEach-Object FullName)

        # Open the command palette (Ctrl+Shift+P), run "Report a bug (collect logs)".
        $zip = $null
        for ($attempt = 0; $attempt -lt 3 -and -not $zip; $attempt++) {
            Set-WtWindowForeground -App $script:app | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x50 -Ctrl -Shift | Out-Null   # Ctrl+Shift+P
            if (-not (Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-CommandPaletteOpen -App $script:app })) { continue }
            Set-UiValue -App $script:app -Selector '_searchBox' -Value 'Report a bug' | Out-Null
            Start-Sleep -Milliseconds 800
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter -> run the command
            $zip = $null
            for ($i = 0; $i -lt 20 -and -not $zip; $i++) {
                Start-Sleep -Seconds 1
                $zip = Get-ChildItem $script:desktop -Filter 'intelligent-terminal-logs-*.zip' -ErrorAction SilentlyContinue |
                    Where-Object { $_.FullName -notin $before } | Select-Object -First 1 -ExpandProperty FullName
            }
        }
        if (-not $zip -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window could not take foreground for the command palette'; return }
        $zip | Should -Not -BeNullOrEmpty -Because 'the Report-a-bug command must create a logs zip on the Desktop'

        # Wait until the archive is fully written (the C++ side reveals it in Explorer once done),
        # then assert it contains agent logs so a bug report is actually useful for agent issues.
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $entries = @()
        $ok = Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition {
            try { $z = [System.IO.Compression.ZipFile]::OpenRead($zip); $n = @($z.Entries.FullName).Count; $z.Dispose(); $n -gt 0 }
            catch { $false }
        }
        $ok | Should -BeTrue -Because 'the zip must become readable (and non-empty) once fully written'
        # Read entries in THIS scope (assignment inside a Test-Until scriptblock would not propagate).
        $z = [System.IO.Compression.ZipFile]::OpenRead($zip)
        $entries = @($z.Entries.FullName)
        $z.Dispose()
        ($entries -join "`n") | Should -Match '(?i)(wta-[^/\\]*\.log|terminal-agent-pane\.log)' -Because 'the bug-report zip must include the agent (wta / terminal-agent-pane) logs'

        $script:zip = $zip
        Copy-Item -LiteralPath $zip -Destination (Join-Path $script:artifactRoot 'report.zip')
    }

    It 'Bug reports preserve default pane diagnostics' {
        $script:zip | Should -Not -BeNullOrEmpty -Because 'the real UI action must have produced this report'
        $archive = [System.IO.Compression.ZipFile]::OpenRead($script:zip)
        try {
            function Read-ReportEntry([string]$Name) {
                $entry = @($archive.Entries | Where-Object { $_.FullName -match "(^|[/\\])$([regex]::Escape($Name))$" }) | Select-Object -Last 1
                $entry | Should -Not -BeNullOrEmpty
                $reader = [IO.StreamReader]::new($entry.Open())
                try { $reader.ReadToEnd() } finally { $reader.Dispose() }
            }
            $json = Read-ReportEntry 'diagnostics.json'
            $report = $json | ConvertFrom-Json
            $report.schema_version | Should -Be 1
            $report.scope | Should -Be 'current_window_and_owned_agent_processes'
            $report.logs_privacy | Should -Be 'raw_logs_not_redacted'
            $json | Should -Not -Match ([regex]::Escape($script:sentinel))
            $json | Should -Not -Match 'bug-report-private-provider|Mock-AcpInteractionAgent|acpCustomCommand|"cwd"|"title"'
            $report.effective_settings.acp_agent | Should -Be 'custom'
            $report.effective_settings.auto_fix_enabled | Should -BeFalse
            $report.effective_settings.auto_error_detection_enabled | Should -BeTrue
            $settingKeys = @(
                'acp_agent', 'delegate_agent', 'agent_pane_position', 'auto_fix_enabled',
                'auto_error_detection_enabled', 'confirmation_read', 'confirmation_create',
                'confirmation_input', 'allowed_agents_policy_configured', 'custom_agent_policy_allowed',
                'auto_fix_policy_allowed', 'yolo_policy_allowed', 'coordinator_enabled', 'policy_allowed_providers'
            )
            ($report.effective_settings.PSObject.Properties.Name | Sort-Object) -join ',' |
                Should -Be (($settingKeys | Sort-Object) -join ',')
            @($report.collection_errors | Where-Object {
                $_.stage -ne 'binary_version' -or $_.code -ne 'version_resource_unavailable'
            }).Count | Should -Be 0 -Because 'missing PE version resources are explicit, but core diagnostic evidence must be collected'
            $report.expected_com_clsid.Trim('{}') | Should -Be $script:app.ComClsid.Trim('{}')
            $tab = @($report.tabs | Where-Object {
                @($_.panes | Where-Object { [guid]$_.session_id -eq [guid]$script:agent.PaneSessionId }).Count -gt 0
            })
            $tab.Count | Should -Be 1
            $tab[0].agent_stashed | Should -BeTrue
            $tab[0].stable_tab_id | Should -Match '^[{]?[0-9a-f-]{36}[}]?$'
            $source = @($tab[0].panes | Where-Object { [guid]$_.session_id -eq [guid]$script:source.session_id })
            $source.Count | Should -Be 1
            $source[0].pane_id | Should -Be $tab[0].active_pane_id
            $source[0].source_of_agent | Should -BeOfType ([bool])
            @($tab[0].panes | Where-Object { $_.kind -eq 'agent' -and $_.helper_event_ready }).Count | Should -Be 1
            foreach ($role in @('terminal', 'wta_master', 'wta_helper')) {
                $process = @($report.processes | Where-Object role -eq $role) | Select-Object -First 1
                $process.identity.status | Should -Be 'available'
                $process.identity.pid | Should -BeGreaterThan 0
                $process.identity.start_time_filetime | Should -Match '^\d{15,20}$'
                $process.identity.package_version | Should -Be ([string]$script:app.Version)
                $process.liveness | Should -Be 'running'
                $binary = $report.binaries[$process.binary_index]
                $binary.status | Should -Be 'collected'
                $binary.sha256 | Should -Match '^[0-9a-f]{64}$'
                if ($role -eq 'terminal') { $process.identity.pid | Should -Be $script:app.Pid }
                else {
                    if ($role -eq 'wta_helper') { $process.identity.pid | Should -Be $script:agent.HelperProcessId }
                    $binary.embedded_build_status | Should -Be 'collected'
                    $binary.cargo_version | Should -Match '^\d+\.\d+\.\d+'
                    $binary.build_commit | Should -Match '^[0-9a-f]{40}$'
                    $binary.sha256 | Should -Be (Get-FileHash -LiteralPath $script:app.WtaPath -Algorithm SHA256).Hash.ToLowerInvariant()
                    if ($env:ITE2E_EXPECTED_WTA_SHA256) { $binary.sha256 | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256.ToLowerInvariant() }
                }
            }
            $helper = Read-ReportEntry "wta-main_helper-$($script:agent.HelperProcessId).log"
            $master = Read-ReportEntry 'wta-main_master.log'
            foreach ($text in @($helper, $master)) {
                $startup = @($text -split "`n" | Where-Object { $_ -match 'logging_configuration' }) | Select-Object -Last 1
                $startup | Should -Match 'filter_source="default"'
                $startup | Should -Match 'effective_max_level=Some\(LevelFilter::INFO\)'
            }
            $metadata = @($helper -split "`n" | Where-Object { $_ -match 'prompt_context_binding|prompt_binding_issued|terminal_context_target_resolved|pane_context_server_provenance|pane_context_wtcli_started|helper_master_identity' }) -join "`n"
            foreach ($event in @('prompt_context_binding', 'prompt_binding_issued', 'terminal_context_target_resolved', 'pane_context_server_provenance', 'pane_context_wtcli_started', 'helper_master_identity')) {
                $metadata | Should -Match $event
            }
            $metadata | Should -Match "bound_target=$([regex]::Escape($script:source.session_id.Trim('{}')))"
            $metadata | Should -Match 'wtcli_start_time_filetime=\d+'
            $metadata | Should -Match '"request_id":\d+'
            $metadata | Should -Match '"server_pid":\d+'
            $metadata | Should -Not -Match ([regex]::Escape($script:sentinel))
            $provenanceLine = @($helper -split "`n" | Where-Object { $_ -match 'pane_context_server_provenance' }) | Select-Object -Last 1
            $provenance = ($provenanceLine -replace '^.* provenance=', '') | ConvertFrom-Json
            $provenance.server_pid | Should -Be $script:app.Pid
            $provenance.server_start_time_filetime |
                Should -Be ($report.processes | Where-Object role -eq 'terminal').identity.start_time_filetime
            $provenance.caller_pid | Should -BeGreaterThan 0
            $metadata | Should -Match "wtcli_pid=$($provenance.caller_pid)\b"
            $metadata | Should -Match "master_pid=$(($report.processes | Where-Object role -eq 'wta_master').identity.pid)\b"
            $cpp = Read-ReportEntry 'terminal-agent-pane.log'
            ($cpp -match "pane_context_com pid=$($provenance.server_pid)\b.*caller_pid=$($provenance.caller_pid)\b.*request_id=$($provenance.request_id)\b") |
                Should -BeTrue -Because 'helper child PID and server request counter must join the real COM record'
            $failedRequest = @($cpp -split "`n" | Where-Object {
                $_ -match 'WARN pane_context_com' -and $_ -match ([regex]::Escape($script:missingSource))
            }) | Select-Object -Last 1
            $failedRequest | Should -Not -BeNullOrEmpty
            $failedRequest | Should -Match 'phase=\w+ hr=0x[0-9A-F]{8} request_id=\d+'
            ($cpp -match 'pane_context_wtcli') | Should -BeTrue
            $script:context.diagnostics.server_identity.pid | Should -Be $script:app.Pid
            $script:context.diagnostics.request_id | Should -BeGreaterThan 0
            $script:context.diagnostics.caller_pid | Should -BeGreaterThan 0
            $json | Set-Content -LiteralPath (Join-Path $script:artifactRoot 'diagnostics.json')
            $metadata | Set-Content -LiteralPath (Join-Path $script:artifactRoot 'metadata.log')
            $failedRequest | Set-Content -LiteralPath (Join-Path $script:artifactRoot 'missing-source.log')
        }
        finally { $archive.Dispose() }
    }
}
