#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.
# Native profile composition crosses settings -> window-targeted event -> ConPTY.
# The fixture has no hooks, ACP connection, history scan or model dependency.
BeforeDiscovery {
    $script:Ready = [bool]$env:ITE2E_PACKAGE
}

Describe 'Feature: native agent profile resume' -Tag @('Feature', 'AgentProfileResume') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:package = Get-ItTestPackage
        if ($script:package -notlike 'IntelligentTerminal.Worktree.*') {
            throw 'This suite requires an explicit inactive isolated Dev package family.'
        }
        $target = Resolve-ItApp -Package $script:package
        Assert-WtPackageInactive -App $target
        $env:PATH = "$($target.InstallLocation);$env:PATH"
        $env:WT_WTCLI_PATH = $target.WtcliPath
        $env:WTCLI_PATH = $target.WtcliPath
        $env:WT_COM_CLSID = $target.PrivateProtocolClsid
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply exact worktree build payload hashes.'
        }
        (Get-FileHash (Join-Path $target.InstallLocation 'TerminalApp.dll')).Hash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        if (-not $env:ITE2E_AGENT_PROFILE_RESUME_BUILD_RECEIPT) {
            throw 'Supply the isolated build receipt proving private-package-only hook mutation refusal.'
        }
        $receipt = Get-Content -LiteralPath $env:ITE2E_AGENT_PROFILE_RESUME_BUILD_RECEIPT -Raw | ConvertFrom-Json
        $receipt.PackageFamilyName | Should -Be $target.Package
        $receipt.LocalOverrides | Should -Contain 'private-package-only hook mutation refusal'
        $receipt.AppSha256 | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        $receipt.WtaSha256 | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:profile = '{' + [guid]::NewGuid().ToString() + '}'
        $script:unconfiguredProfile = '{' + [guid]::NewGuid().ToString() + '}'
        $shellProfile = '{' + [guid]::NewGuid().ToString() + '}'
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-NativeProfileResume.cmd')).Path
        $script:proofRoot = Join-Path $env:ITE2E_ARTIFACT_ROOT ('native-profile-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:proofRoot -Force | Out-Null
        $pwsh = (Get-Command pwsh -ErrorAction Stop).Source
        $mock = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $mockCode = "& '$($mock.Replace("'", "''"))' -LogPath '$($script:proofRoot.Replace("'", "''"))\acp.log'"
        $mockCommand = "`"$pwsh`" -NoProfile -EncodedCommand " + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($mockCode))
        $script:app = Start-Terminal -Package $script:package -PassFre $true -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
            sidebarIntroductionShown = $true; sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'
            sidebarLayoutMigrationCompleted = $true
        } -Settings @{
            language = 'en-US'; theme = 'dark'; tabLayout = 'vertical'; startupActions = ''; defaultProfile = $shellProfile
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            autoFixEnabled = $false; agentSessionManagementEnabled = $false
            acpAgent = 'custom:pwsh'; acpCustomCommand = $mockCommand
            delegateAgent = 'custom:pwsh'; delegateCustomCommand = $mockCommand
            'warning.confirmOnClose' = 'never'
            acpCustomCommands = @('fixture-adapter.exe --acp --stdio', $mockCommand)
            profiles = @{ defaults = @{ environment = @{
                PATH = "$($target.InstallLocation);$env:PATH"
                WT_WTCLI_PATH = $target.WtcliPath; WTCLI_PATH = $target.WtcliPath
                ITE2E_NATIVE_PROFILE_PROOF_DIR = $script:proofRoot
            } }; list = @(@{
                guid = $script:profile; name = 'Profile resume fixture'
                'agentProfile.id' = 'custom:profile-resume-fixture'
                commandline = "`"$script:fixture`""
                'agentProfile.model' = 'chosen-model'
                'agentProfile.arguments' = '--startup-option'
                'agentProfile.resumeArguments' = '--load-conversation'
                startingDirectory = $script:proofRoot; suppressApplicationTitle = $true
                closeOnExit = 'never'
            }, @{
                guid = $script:unconfiguredProfile; name = 'Profile resume unconfigured custom fixture'
                'agentProfile.id' = 'custom:profile-resume-fixture'; commandline = ''
                startingDirectory = ''
                closeOnExit = 'never'
            }, @{
                guid = $shellProfile; name = 'Profile resume shell'
                commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                shellIntegrationEnabled = $false; closeOnExit = 'never'
            }) }
        }
        $script:app.Launched | Should -BeTrue
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true -Force
        $script:listener = Start-WtEventListener -App $script:app -WaitForReady
        function Assert-NativeFixtureTarget([string]$SessionId) {
            $path = Join-Path $script:proofRoot ($SessionId.Trim('{}') + '.json')
            $proof = Wait-Until -TimeoutSec 20 -Because 'the native pane verifies package-local CLI discovery and routing' -Condition {
                if (Test-Path -LiteralPath $path) { Get-Content -LiteralPath $path -Raw | ConvertFrom-Json }
            }
            $proof.WtcliPath | Should -Be $script:app.WtcliPath
            $proof.WtaPath | Should -Be $script:app.WtaPath
            $proof.ComClsid.Trim('{}') | Should -Be $script:app.ComClsid.Trim('{}')
            $proof.Query.session_id.Trim('{}') | Should -Be $SessionId.Trim('{}')
            "$($proof.Query.window_id)" | Should -Be "$($script:app.WindowId)"
            $proof.PaneSessionId.Trim('{}') | Should -Be $SessionId.Trim('{}')
        }
        function Select-NativeProfilePage([string]$Name) {
            Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
            if ($script:app.OwnedProcess.HasExited) { throw 'Owned Terminal exited.' }
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $nav = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'SettingsNav'))
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem))
            $item = $nav.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            if (-not $item) { throw "Missing native profile navigation item: $Name" }
            ([Windows.Automation.ScrollItemPattern]$item.GetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern)).ScrollIntoView()
            $pattern = $null
            if ($item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) {
                ([Windows.Automation.InvokePattern]$pattern).Invoke()
            } else {
                ([Windows.Automation.SelectionItemPattern]$item.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern)).Select()
            }
        }
        function Get-NativeMasterSessions {
            $master = Get-WtaMasterPipe -App $script:app
            $snapshot = Invoke-Wta -App $script:app -Arguments @('sessions', 'list', '--json',
                '--master', $master, '--include-status', '--origin', 'all')
            [IO.File]::WriteAllText((Join-Path $script:proofRoot 'master-snapshot.json'), ($snapshot | ConvertTo-Json -Depth 20))
            $snapshot.sessions
        }
        function Get-NativeMetadataRow([string]$Title) {
            Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
            if (-not $list) { return }
            $rows = @($list.FindAll([Windows.Automation.TreeScope]::Children,
                [Windows.Automation.Condition]::TrueCondition) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.Name.Contains($Title)
            })
            if ($rows.Count -eq 1) { $rows[0].Current.Name }
        }
        function Invoke-NativeLaunchBeforeMasterSnapshot([string]$Title, [scriptblock]$Launch) {
            # Pause only this test-owned master, not the host/COM event receiver.
            # A newly allocated pane cannot have a master snapshot while it is
            # paused. The fixture emits no hooks. Always resume in finally.
            if (-not ('NativeProfileResumeMasterGate' -as [type])) {
                Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NativeProfileResumeMasterGate {
    [DllImport("ntdll.dll")] public static extern int NtSuspendProcess(IntPtr process);
    [DllImport("ntdll.dll")] public static extern int NtResumeProcess(IntPtr process);
}
'@
            }
            $pipe = Get-WtaMasterPipe -App $script:app
            $masters = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($script:app.Pid) AND Name='wta.exe'" |
                Where-Object { $_.ExecutablePath -ieq $script:app.WtaPath -and
                    $_.CommandLine -match '^\s*(?:"[^"]+"|\S+)\s+--master\s+' -and $_.CommandLine.Contains($pipe) })
            $masters.Count | Should -Be 1
            $master = Get-Process -Id $masters[0].ProcessId -ErrorAction Stop
            $master.Path | Should -Be $script:app.WtaPath
            $paused = $false
            try {
                $status = [NativeProfileResumeMasterGate]::NtSuspendProcess($master.Handle)
                if ($status -ne 0) { throw "Cannot pause owned master: NTSTATUS=$status" }
                $paused = $true
                $clock = [Diagnostics.Stopwatch]::StartNew()
                $created = & $Launch
                [IO.File]::WriteAllText((Join-Path $script:proofRoot "$Title-creation.json"),
                    ($created | ConvertTo-Json -Depth 8))
                # list-panes is active-tab scoped; use the creation receipt's
                # exact target rather than racing focus while master is paused.
                $pane = @((Invoke-WtCli -App $script:app -Arguments @(
                    'get-pane-context', '--target', $created.session_id)).pane)
                $pane.Count | Should -Be 1
                ([string]$pane[0].session_id).Trim('{}') | Should -Be ([string]$created.session_id).Trim('{}')
                $pane[0].native_agent_provider_id | Should -Be 'custom:profile-resume-fixture'
                $pane[0].cwd | Should -Be $script:proofRoot
                "$($pane[0].window_id)" | Should -Be "$($script:app.WindowId)"
                $row = Wait-Until -TimeoutSec 5 -Because 'creation metadata is visible before any hook or new-pane master snapshot' -Condition {
                    $name = Get-NativeMetadataRow $Title
                    if ($name -match '\bIdle\b' -and $name.Contains($script:proofRoot)) { $name }
                }
                $clock.Elapsed.TotalSeconds | Should -BeLessThan 5
                [IO.File]::WriteAllText((Join-Path $script:proofRoot "$Title-initial-metadata.json"), (@{
                    pane = $pane[0]; row = $row; elapsedMs = $clock.ElapsedMilliseconds
                    masterPid = $master.Id; masterPaused = $true; hookEmitted = $false
                } | ConvertTo-Json -Depth 8))
                $created
            }
            finally {
                if ($paused) {
                    $status = [NativeProfileResumeMasterGate]::NtResumeProcess($master.Handle)
                    if ($status -ne 0) { throw "Cannot resume owned master: NTSTATUS=$status" }
                }
                $master.Dispose()
            }
        }
    }

    AfterAll {
        if ($script:listener) {
            [IO.File]::WriteAllText((Join-Path $script:proofRoot 'events.json'), (@(Get-WtEvents -Listener $script:listener) | ConvertTo-Json -Depth 20))
        }
        if ($script:listener) { Stop-WtEventListener -Listener $script:listener }
        if ($script:app) {
            Stop-Terminal -App $script:app -RestoreSettings $false
            Wait-Until -TimeoutSec 10 -Because 'owned package clients finish teardown before restoring private configuration' -Condition {
                @(Get-WtProcessesForApp -App $script:app -IncludePackageExecutables).Count -eq 0
            } | Out-Null
            if ($script:app.ConfigBackupOwned) { Restore-WtConfig -App $script:app }
        }
    }

    It 'Native profile resume uses effective launch and resume arguments only when requested' {
        foreach ($tracking in @($false, $true)) {
            Set-WtSetting -App $script:app -Key agentSessionManagementEnabled -Value $tracking | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'the host has reloaded the requested Sessions mode' -Condition {
                (Invoke-WtCli -App $script:app -Arguments @('get-settings')).agentSessionManagementEnabled -eq $tracking
            } | Out-Null
            $ordinaryTitle = 'metadata-normal-' + $tracking + '-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
            $ordinary = Invoke-NativeLaunchBeforeMasterSnapshot $ordinaryTitle {
                Invoke-WtCli -App $script:app -Arguments @(
                    'new-tab', '-p', $script:profile, '-n', $ordinaryTitle, '--window-id', "$($script:app.WindowId)"
                )
            }
            $text = Wait-Until -TimeoutSec 20 -Because 'ordinary profile fixture starts' -Condition {
                $capture = Get-WtCapture -App $script:app -SessionId $ordinary.session_id
                if ($capture -match 'PROFILE-RESUME-ARGS') { $capture }
            }
            $text | Should -Match '--model.*chosen-model.*--startup-option'
            $text | Should -Not -Match '--load-conversation'
            Assert-NativeFixtureTarget $ordinary.session_id
            $knownId = 'fixture-' + [guid]::NewGuid().ToString('N')
            $requestId = [guid]::NewGuid().ToString()
            $resumeTitle = 'metadata-resume-' + $tracking + '-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
            $request = @{
                window_id = "$($script:app.WindowId)"; agent_id = 'custom:profile-resume-fixture'
                agent_source = 'host'; resume_target = 'native'
                profile_id = $script:profile; session_id = $knownId
                request_id = $requestId; title = $resumeTitle
            } | ConvertTo-Json -Compress
            $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = ($request | ConvertFrom-Json) } | ConvertTo-Json -Compress
            $resumed = Invoke-NativeLaunchBeforeMasterSnapshot $resumeTitle {
                Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
                $script:metadataReceipt = Wait-WtEvent -Listener $script:listener -TimeoutSec 5 -Predicate {
                    $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $requestId
                }
                $script:metadataReceipt.params.success | Should -BeTrue
                [pscustomobject]@{ session_id = $script:metadataReceipt.params.result.session_id }
            }
            $receipt = $script:metadataReceipt
            $receipt.params.success | Should -BeTrue
            $resumed.session_id | Should -Not -BeNullOrEmpty
            $text = Wait-Until -TimeoutSec 20 -Because 'resumed fixture receives the effective arguments and automatic ID' -Condition {
                $capture = Get-WtCapture -App $script:app -SessionId $resumed.session_id
                if ($capture -match [regex]::Escape($knownId)) { $capture }
            }
            $text | Should -Match "--model.*chosen-model.*--startup-option.*--load-conversation.*$knownId"
            $text | Should -Not -Match '--acp|--stdio'
            Assert-NativeFixtureTarget $resumed.session_id
            $receipt.params.result.session_id.Trim('{}') | Should -Be $resumed.session_id.Trim('{}')

            $panes = @(Get-WtPanes -App $script:app)
            $panes.Count | Should -BeGreaterThan 0
            $count = $panes.Count
            $rejectedId = [guid]::NewGuid().ToString()
            $mismatch = @{
                window_id = "$($script:app.WindowId)"; agent_id = 'custom:wrong-provider'
                agent_source = 'host'; resume_target = 'native'; profile_id = $script:profile
                session_id = $knownId; request_id = $rejectedId
            } | ConvertTo-Json -Compress
            $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = ($mismatch | ConvertFrom-Json) } | ConvertTo-Json -Compress
            Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
            $rejected = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $rejectedId
            }
            $rejected.params.success | Should -BeFalse
            $rejected.params.error | Should -BeExactly 'No corresponding native agent profile was found. Check the selected profile and its agentProfile.id.'
            @(Get-WtPanes -App $script:app).Count | Should -Be $count

            $missingProfileRequestId = [guid]::NewGuid().ToString()
            $missingProfile = @{
                window_id = "$($script:app.WindowId)"; agent_id = 'custom:profile-resume-fixture'
                agent_source = 'host'; resume_target = 'native'
                profile_id = '{' + [guid]::NewGuid().ToString() + '}'
                session_id = $knownId; request_id = $missingProfileRequestId
            } | ConvertTo-Json -Compress
            $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = ($missingProfile | ConvertFrom-Json) } | ConvertTo-Json -Compress
            Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
            $missingProfileResult = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $missingProfileRequestId
            }
            $missingProfileResult.params.request_id | Should -BeExactly $missingProfileRequestId
            $missingProfileResult.params.success | Should -BeFalse
            $missingProfileResult.params.error | Should -BeExactly 'No corresponding native agent profile was found. Check the selected profile and its agentProfile.id.'
            @(Get-WtPanes -App $script:app).Count | Should -Be $count

            foreach ($invalid in @(
                @{ agent = 'custom: invalid'; profile = $script:profile; error = 'The native agent provider is invalid or blocked by policy.' }
                @{ agent = 'custom:profile-resume-fixture'; profile = 'not-a-guid'; error = 'The requested native agent profile_id is not a valid GUID.' }
            )) {
                $invalidRequestId = [guid]::NewGuid().ToString()
                $invalidRequest = @{
                    window_id = "$($script:app.WindowId)"; agent_id = $invalid.agent
                    agent_source = 'host'; resume_target = 'native'; profile_id = $invalid.profile
                    session_id = $knownId; request_id = $invalidRequestId
                } | ConvertTo-Json -Compress
                $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = ($invalidRequest | ConvertFrom-Json) } | ConvertTo-Json -Compress
                Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
                $invalidResult = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $invalidRequestId
                }
                $invalidResult.params.request_id | Should -BeExactly $invalidRequestId
                $invalidResult.params.success | Should -BeFalse
                $invalidResult.params.error | Should -BeExactly $invalid.error
                @(Get-WtPanes -App $script:app).Count | Should -Be $count
            }

            $unconfiguredRequestId = [guid]::NewGuid().ToString()
            $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = @{
                window_id = "$($script:app.WindowId)"; agent_id = 'custom:profile-resume-fixture'
                agent_source = 'host'; resume_target = 'native'; profile_id = $script:unconfiguredProfile
                session_id = 'unconfigured-' + [guid]::NewGuid().ToString('N'); request_id = $unconfiguredRequestId
            } } | ConvertTo-Json -Compress -Depth 6
            Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
            $unconfiguredResult = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $unconfiguredRequestId
            }
            $unconfiguredResult.params.request_id | Should -BeExactly $unconfiguredRequestId
            $unconfiguredResult.params.success | Should -BeFalse
            $unconfiguredResult.params.error | Should -BeExactly 'Configure the native interactive command for this custom agent profile.'
            @(Get-WtPanes -App $script:app).Count | Should -Be $count

            # Controlled legacy-hook transport, never an installed hook mutation.
            # Repeat the same update to protect owner binding/deduplication.
            $hook = @{
                agent = 'custom:profile-resume-fixture'; cli_source = 'custom:profile-resume-fixture'
                agent_session_id = $knownId; event = 'agent.prompt.submit'; pane_id = $resumed.session_id
                payload = @{ cwd = $script:proofRoot; message = 'controlled metadata update' }
            } | ConvertTo-Json -Depth 5 -Compress
            [IO.File]::WriteAllText((Join-Path $script:proofRoot "$resumeTitle-hook-input.json"), $hook)
            foreach ($repeat in 1..2) {
                Invoke-WtCli -App $script:app -Arguments @('send-event', '-p', $resumed.session_id,
                    '-e', 'agent.prompt.submit', $hook) | Out-Null
            }
            $expected = if ($tracking) { 'Working' } else { 'Idle' }
            $updated = Wait-Until -TimeoutSec 10 -Because 'controlled hook enriches the same owner only when Sessions is enabled' -Condition {
                $name = Get-NativeMetadataRow $resumeTitle
                if ($name -cmatch "(?:^|,\s*)Agent status:\s*$expected(?:,|$)") { $name }
            }
            (Get-NativeMetadataRow $ordinaryTitle) | Should -Match 'Agent status: Idle(?:,|$)'
            @(Get-WtPanes -App $script:app).Count | Should -Be $count
            if ($tracking) {
                $bound = Wait-Until -TimeoutSec 10 -Because 'duplicate hooks retain one conversation bound to the resumed pane' -Condition {
                    $rows = @(Get-NativeMasterSessions | Where-Object session_id -EQ $knownId)
                    if ($rows.Count -eq 1 -and $rows[0].status -eq 'Working' -and
                        ([string]$rows[0].pane_session_id).Trim('{}') -eq $resumed.session_id.Trim('{}')) { $rows[0] }
                }
                ([string]$bound.pane_session_id).Trim('{}') | Should -Be $resumed.session_id.Trim('{}')
                $bound.provider_id | Should -Be 'custom:profile-resume-fixture'
                $bound.status | Should -BeExactly 'Working'
                [IO.File]::WriteAllText((Join-Path $script:proofRoot "$resumeTitle-authoritative-hook.json"),
                    ($bound | ConvertTo-Json -Depth 12))
                @(Get-NativeMasterSessions | Where-Object {
                    ([string]$_.pane_session_id).Trim('{}') -eq $ordinary.session_id.Trim('{}')
                }).Count | Should -Be 0 -Because 'the ordinary sibling has no hook or fabricated conversation ID'
            }
            [IO.File]::WriteAllText((Join-Path $script:proofRoot "$resumeTitle-after-hook.json"),
                (@{ row = $updated; pane = $resumed.session_id; tracking = $tracking } | ConvertTo-Json))
        }
    }

    It 'Master native history activation uses edited profile settings and preserves its receipt' {
        Set-WtSetting -App $script:app -Key agentSessionManagementEnabled -Value $true | Out-Null
        $provider = 'custom:fixture-adapter'
        $generated = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
            Where-Object agent_id -EQ $provider) | Select-Object -First 1
        $generated | Should -Not -BeNullOrEmpty
        $guid = '{' + ([string]$generated.id).Trim('{}') + '}'
        $settings = Get-WtSettingsObject -App $script:app
        $others = @($settings.profiles.list | Where-Object { ([string]$_.guid).Trim('{}') -ne $guid.Trim('{}') })
        $profile = @{
            guid = $guid; source = 'IntelligentTerminal.AgentProfiles'; name = 'fixture-adapter configured'
            commandline = "`"$script:fixture`""; 'agentProfile.model' = 'master-model'
            'agentProfile.arguments' = '--master-startup'; 'agentProfile.resumeArguments' = '--load-master'
            startingDirectory = $env:SystemRoot; closeOnExit = 'never'
        }
        $settings.profiles.list = $others + @($profile)
        Set-WtSetting -App $script:app -Key profiles -Value $settings.profiles | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'the generated original receives its independent native command' -Condition {
            @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                Where-Object { $_.name -eq 'fixture-adapter configured' }).Count -eq 1
        } | Out-Null
        $knownId = 'master-fixture-' + [guid]::NewGuid().ToString('N')
        $requestId = [guid]::NewGuid().ToString()
        $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = @{
            window_id = "$($script:app.WindowId)"; agent_id = $provider; agent_source = 'host'
            resume_target = 'native'; profile_id = $guid; session_id = $knownId; request_id = $requestId
        } } | ConvertTo-Json -Compress -Depth 6
        Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
        $seed = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
            $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $requestId
        }
        $seed.params.success | Should -BeTrue
        Assert-NativeFixtureTarget $seed.params.result.session_id
        Wait-Until -TimeoutSec 20 -Because 'the actual native pane binding reaches the owning master' -Condition {
            @(Get-NativeMasterSessions | Where-Object session_id -EQ $knownId).Count -eq 1
        } | Out-Null
        Close-WtPane -App $script:app -SessionId $seed.params.result.session_id
        Wait-Until -TimeoutSec 20 -Because 'closing the fixture leaves an unbound resumable history row' -Condition {
            $row = @(Get-NativeMasterSessions | Where-Object session_id -EQ $knownId) | Select-Object -First 1
            $row -and -not $row.pane_session_id
        } | Out-Null
        $profile.name = 'fixture-adapter'
        $profile['agentProfile.model'] = 'edited-model'
        $profile['agentProfile.resumeArguments'] = '--load-edited-master'
        $settings.profiles.list = $others + @($profile)
        Set-WtSetting -App $script:app -Key profiles -Value $settings.profiles | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'resume reads the newly edited profile rather than its old launch' -Condition {
            @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                Where-Object { $_.name -eq 'fixture-adapter' }).Count -eq 1
        } | Out-Null
        $pipe = Get-WtaMasterPipe -App $script:app
        $activation = [guid]::NewGuid().ToString()
        $arguments = @('sessions', 'activate', '--json', '--master', $pipe, '--session-id', $knownId,
            '--provider', $provider, '--location', 'host', '--window-id', "$($script:app.WindowId)", '--activation-id', $activation)
        $receipt = Invoke-Wta -App $script:app -Arguments $arguments
        $receipt.accepted | Should -BeTrue
        $receipt.launched | Should -BeTrue
        $pane = Get-ActivePane -App $script:app
        $pane.session_id.Trim('{}') | Should -Not -Be $seed.params.result.session_id.Trim('{}')
        Assert-NativeFixtureTarget $pane.session_id
        $text = Get-WtCapture -App $script:app -SessionId $pane.session_id
        $text | Should -Match "--model.*edited-model.*--master-startup.*--load-edited-master.*$knownId"
        $text | Should -Not -Match '--load-master'
        $replay = Invoke-Wta -App $script:app -Arguments $arguments
        $replay | ConvertTo-Json -Depth 8 -Compress | Should -Be ($receipt | ConvertTo-Json -Depth 8 -Compress)
        (Get-ActivePane -App $script:app).session_id | Should -Be $pane.session_id

        # The original GUID came from custom-agent generation, not a manual orphan.
        # A retained-source JSON copy is deliberately first in AllProfiles.
        $retainedCopy = @{
            guid = '{' + [guid]::NewGuid().ToString() + '}'
            source = 'IntelligentTerminal.AgentProfiles'; name = 'fixture-adapter retained-source copy'
            'agentProfile.id' = $provider; commandline = "`"$script:fixture`""
            'agentProfile.model' = 'retained-copy-model'; 'agentProfile.arguments' = '--retained-copy-startup'
            'agentProfile.resumeArguments' = '--load-retained-copy'
            startingDirectory = $script:proofRoot; closeOnExit = 'never'
        }
        $plainCopy = @{
            guid = '{' + [guid]::NewGuid().ToString() + '}'; name = 'fixture-adapter plain copy'
            'agentProfile.id' = $provider; commandline = "`"$script:fixture`""
            'agentProfile.model' = 'plain-copy-model'; 'agentProfile.arguments' = '--plain-copy-startup'
            'agentProfile.resumeArguments' = '--load-plain-copy'
            startingDirectory = $script:proofRoot; closeOnExit = 'never'
        }
        $savedProfiles = $settings.profiles
        $savedCommands = @($settings.acpCustomCommands)
        $fixtureProfiles = $savedProfiles | ConvertTo-Json -Depth 64 | ConvertFrom-Json
        $fixtureProfiles.list = @($retainedCopy, $plainCopy, $profile) + $others
        function Invoke-SelectionFixture([hashtable]$Expected, [bool]$ExplicitGuid) {
            # A fresh conversation has no known-pane binding. Do not let a
            # profile lookup fill profile_id on the no-GUID request.
            $conversation = 'selection-' + [guid]::NewGuid().ToString('N')
            $selectionRequest = [guid]::NewGuid().ToString()
            $parameters = @{
                window_id = "$($script:app.WindowId)"; agent_id = $provider; agent_source = 'host'
                resume_target = 'native'; session_id = $conversation; request_id = $selectionRequest
            }
            if ($ExplicitGuid) { $parameters.profile_id = $Expected.guid }
            else { $parameters.ContainsKey('profile_id') | Should -BeFalse }
            $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = $parameters } |
                ConvertTo-Json -Compress -Depth 6
            Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
            $result = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $selectionRequest
            }
            $result.params.request_id | Should -BeExactly $selectionRequest
            if (-not $Expected) {
                $result.params.success | Should -BeFalse
                $result.params.error | Should -BeExactly 'No corresponding native agent profile was found. Check the selected profile and its agentProfile.id.'
                return
            }
            $result.params.success | Should -BeTrue
            $selectedPane = $result.params.result.session_id
            Assert-NativeFixtureTarget $selectedPane
            $context = (Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $selectedPane)).pane
            $context.profile | Should -BeExactly $Expected.name
            $context.native_agent_provider_id | Should -BeExactly $provider
            # The CLI exposes the pane's profile name; resolve that unique name
            # back to its GUID, never feed the resolved GUID into the request.
            $configured = @(Get-WtSettingsObject -App $script:app |
                ForEach-Object { $_.profiles.list } | Where-Object name -EQ $context.profile)
            $configured.Count | Should -Be 1
            ([string]$configured[0].guid).Trim('{}') | Should -Be ([string]$Expected.guid).Trim('{}')
            $capture = Wait-Until -TimeoutSec 20 -Because 'selection uses this profile command and its independent argv' -Condition {
                $text = Get-WtCapture -App $script:app -SessionId $selectedPane
                if ($text -match [regex]::Escape($conversation)) { $text }
            }
            $argv = '--model.*' + [regex]::Escape($Expected['agentProfile.model']) + '.*' +
                [regex]::Escape($Expected['agentProfile.arguments']) + '.*' +
                [regex]::Escape($Expected['agentProfile.resumeArguments']) + '.*' + [regex]::Escape($conversation)
            $capture | Should -Match $argv
            [IO.File]::WriteAllText((Join-Path $script:proofRoot "$selectionRequest-selection.json"),
                (@{ request = $parameters; receipt = $result.params; pane = $context
                    profileGuid = $configured[0].guid; capture = $capture } | ConvertTo-Json -Depth 12))
            Close-WtPane -App $script:app -SessionId $selectedPane
        }
        try {
            Set-WtSetting -App $script:app -Key profiles -Value $fixtureProfiles | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'original and plain copy are active after the retained-source orphan is ordered first' -Condition {
                $rows = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                    Where-Object agent_id -EQ $provider)
                $rows.Count -eq 2 -and @($rows | Where-Object name -EQ $profile.name).Count -eq 1 -and
                    @($rows | Where-Object name -EQ $plainCopy.name).Count -eq 1
            } | Out-Null
            Invoke-SelectionFixture $profile $false
            Invoke-SelectionFixture $retainedCopy $true
            Invoke-SelectionFixture $plainCopy $true

            # Remove both the generator input and original override. Copies keep
            # explicit commands, so an accidental fallback would launch them.
            Set-WtSetting -App $script:app -Key acpCustomCommands -Value @(
                $savedCommands | Where-Object { $_ -ne 'fixture-adapter.exe --acp --stdio' }
            ) | Out-Null
            $fixtureProfiles.list = @($retainedCopy, $plainCopy) + $others
            Set-WtSetting -App $script:app -Key profiles -Value $fixtureProfiles | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'the generated original is unavailable while the plain copy remains active' -Condition {
                $rows = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                    Where-Object agent_id -EQ $provider)
                $rows.Count -eq 1 -and $rows[0].name -eq $plainCopy.name
            } | Out-Null
            $before = (Get-ActivePane -App $script:app).session_id
            Invoke-SelectionFixture $null $false
            (Get-ActivePane -App $script:app).session_id | Should -Be $before
            Invoke-SelectionFixture $retainedCopy $true
            Invoke-SelectionFixture $plainCopy $true
        }
        finally {
            Set-WtSetting -App $script:app -Key acpCustomCommands -Value $savedCommands | Out-Null
            $expectedCommands = ConvertTo-Json -InputObject $savedCommands -Depth 20 -Compress
            Wait-Until -TimeoutSec 15 -Because 'the application finishes regenerating the original before restoring profile overrides' -Condition {
                $runtime = Invoke-WtCli -App $script:app -Arguments @('get-settings')
                $commands = ConvertTo-Json -InputObject @($runtime.acpCustomCommands) -Depth 20 -Compress
                $commands -eq $expectedCommands -and
                    @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                        Where-Object id -EQ $guid).Count -eq 1
            } | Out-Null
            Set-WtSetting -App $script:app -Key profiles -Value $savedProfiles | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'the original fixture is restored for the settings UI case' -Condition {
                @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                    Where-Object { $_.id -eq $guid -and $_.name -eq $profile.name }).Count -eq 1
            } | Out-Null
        }
    }

    It 'Agent profiles default to home and migrate empty startup folders only once' {
        $savedProfiles = Get-WtSetting -App $script:app -Key profiles
        $panes = @()
        try {
            $generated = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                Where-Object agent_id -EQ 'custom:fixture-adapter') | Select-Object -First 1
            $generated | Should -Not -BeNullOrEmpty
            $profiles = $savedProfiles | ConvertTo-Json -Depth 64 | ConvertFrom-Json
            $entry = @($profiles.list | Where-Object guid -EQ $generated.id)[0]
            $entry | Add-Member -NotePropertyName commandline -NotePropertyValue "`"$script:fixture`"" -Force
            $entry | Add-Member -NotePropertyName suppressApplicationTitle -NotePropertyValue $true -Force
            $entry.PSObject.Properties.Remove('startingDirectory')
            Set-WtSetting -App $script:app -Key profiles -Value $profiles | Out-Null
            $normal = Invoke-WtCli -App $script:app -Arguments @('new-tab', '-w', "$($script:app.WindowId)", '-p', $generated.id)
            $panes += $normal.session_id
            Assert-NativeFixtureTarget $normal.session_id
            $proofPath = Join-Path $script:proofRoot ($normal.session_id.Trim('{}') + '.json')
            (Get-Content -LiteralPath $proofPath -Raw | ConvertFrom-Json).WorkingDirectory | Should -Be $env:USERPROFILE

            $settings = Get-WtSettingsObject -App $script:app
            $settings.'internal.agentProfileStartingDirectoryMigrationCompleted' | Should -BeTrue
            $entry = @($settings.profiles.list | Where-Object guid -EQ $script:unconfiguredProfile)[0]
            $entry.startingDirectory | Should -BeExactly '%USERPROFILE%' -Because 'the explicit-empty legacy fixture was migrated during cold startup'
            $entry | Add-Member -NotePropertyName commandline -NotePropertyValue "`"$script:fixture`"" -Force
            Set-WtSetting -App $script:app -Key profiles -Value $settings.profiles | Out-Null
            $upgraded = Invoke-WtCli -App $script:app -Arguments @('new-tab', '-w', "$($script:app.WindowId)", '-p', $script:unconfiguredProfile)
            $panes += $upgraded.session_id
            Assert-NativeFixtureTarget $upgraded.session_id
            $proofPath = Join-Path $script:proofRoot ($upgraded.session_id.Trim('{}') + '.json')
            (Get-Content -LiteralPath $proofPath -Raw | ConvertFrom-Json).WorkingDirectory | Should -Be $env:USERPROFILE

            $profiles = Get-WtSetting -App $script:app -Key profiles
            @($profiles.list | Where-Object guid -EQ $script:unconfiguredProfile)[0].startingDirectory = ''
            Set-WtSetting -App $script:app -Key profiles -Value $profiles | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'a subsequent deliberate empty directory remains empty after reload' -Condition {
                $current = Invoke-WtCli -App $script:app -Arguments @('get-settings')
                $profile = @($current.profiles.list | Where-Object guid -EQ $script:unconfiguredProfile)[0]
                $current.'internal.agentProfileStartingDirectoryMigrationCompleted' -and
                    $profile.PSObject.Properties.Name -contains 'startingDirectory' -and $profile.startingDirectory -eq ''
            } | Out-Null
        } finally {
            foreach ($pane in $panes) {
                Invoke-WtCli -App $script:app -Arguments @('kill-pane', '-t', $pane) | Out-Null
            }
            Set-WtSetting -App $script:app -Key profiles -Value $savedProfiles | Out-Null
        }
    }

    It 'Agent profile icon edits update live tabs and recent sessions without changing identity' {
        Add-Type -AssemblyName System.Drawing, UIAutomationClient, UIAutomationTypes
        $savedProfiles = Get-WtSetting -App $script:app -Key profiles
        $panes = @()
        function Get-ProfileIconParts($Element) {
            $walker = [Windows.Automation.TreeWalker]::RawViewWalker
            $child = $walker.GetFirstChild($Element)
            while ($child) {
                $child
                Get-ProfileIconParts $child
                $child = $walker.GetNextSibling($child)
            }
        }
        function Assert-ProfileIconRendering([string]$Marker, [Drawing.Color]$Color, [string]$Phase, [string]$MonochromeTheme = '') {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $searchToggle = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTabsButton'))
            $searchToggle | Should -Not -BeNullOrEmpty
            $toggle = [Windows.Automation.TogglePattern]$searchToggle.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
            if ($toggle.Current.ToggleState -ne [Windows.Automation.ToggleState]::On) {
                Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            }
            Wait-UiElement -App $script:app -Selector SearchTextBox -TimeoutSec 15 | Out-Null
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $Marker | Out-Null
            $imagePath = Join-Path $script:proofRoot "$Phase-icons.png"
            Wait-Until -TimeoutSec 15 -Because 'both live and recent icons render the configured profile artwork' -Condition {
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
                $parts = @()
                $liveCount = 0
                $historyCount = 0
                foreach ($row in $list.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition)) {
                    if ($row.Current.IsOffscreen -or -not $row.Current.Name.StartsWith($Marker, [StringComparison]::Ordinal)) { continue }
                    $rowParts = @(Get-ProfileIconParts $row | Where-Object {
                        -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0
                    })
                    $history = @($rowParts | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' })
                    if ($history.Count -eq 1) {
                        $parts += @{ row = $row; icon = $history[0]; kind = 'recent' }
                        $historyCount++
                    } else {
                        # The decorative live ContentPresenter exposes its Image child, not its XAML name.
                        $images = @($rowParts | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::Image })
                        if ($images.Count -eq 1) {
                            $parts += @{ row = $row; icon = $images[0]; kind = 'live' }
                            $liveCount++
                        }
                    }
                }
                if ($liveCount -ne 1 -or $historyCount -ne 1) { return $false }
                Save-UiScreenshot -App $script:app -Path $imagePath | Out-Null
                foreach ($part in $parts) {
                    $rowImagePath = Join-Path $script:proofRoot "$Phase-$($part.kind)-row.png"
                    $rowImages = @($rowImagePath)
                    if ($MonochromeTheme) {
                        $hoverImagePath = Join-Path $script:proofRoot "$Phase-$($part.kind)-hover-row.png"
                        $hoverApp = $script:app
                        $rowSelector = $part.row.Current.Name
                        $duringHover = {
                            $capture = Invoke-WinAppUi -App $hoverApp -UiArgs @(
                                'screenshot', $rowSelector, '--output', $hoverImagePath, '--json')
                            if ($capture.ExitCode -ne 0) { throw "Owned hover capture failed: $($capture.StdErr)" }
                        }.GetNewClosure()
                        $hoverSelector = if ($part.kind -eq 'recent') { 'HistoryProviderIcon' } else { 'TabCloseButton' }
                        Invoke-ItOwnedHover -App $script:app -Selector $hoverSelector -DwellMs 1200 -DuringHover $duringHover
                        $rowImages += $hoverImagePath
                    }
                    if ($MonochromeTheme) {
                        $captureApp = $script:app
                        $captureSelector = $part.row.Current.Name
                        $afterHover = {
                            $capture = Invoke-WinAppUi -App $captureApp -UiArgs @(
                                'screenshot', $captureSelector, '--output', $rowImagePath, '--json')
                            if ($capture.ExitCode -ne 0) { throw "Owned post-hover capture failed: $($capture.StdErr)" }
                        }.GetNewClosure()
                        Invoke-ItOwnedHover -App $script:app -Selector SearchTextBox -DwellMs 250 -DuringHover $afterHover
                    } else {
                        $capture = Invoke-WinAppUi -App $script:app -UiArgs @(
                            'screenshot', $part.row.Current.Name, '--output', $rowImagePath, '--json')
                        if ($capture.ExitCode -ne 0) { throw "Owned row capture failed: $($capture.StdErr)" }
                    }
                    foreach ($rowImagePath in $rowImages) {
                    $bitmap = [Drawing.Bitmap]::new($rowImagePath)
                    try {
                        $rowBounds = $part.row.Current.BoundingRectangle
                        $sx = $bitmap.Width / $rowBounds.Width
                        $sy = $bitmap.Height / $rowBounds.Height
                        $rect = $part.icon.Current.BoundingRectangle
                        $left = [Math]::Max(0, [int][Math]::Floor(($rect.Left - $rowBounds.Left) * $sx))
                        $top = [Math]::Max(0, [int][Math]::Floor(($rect.Top - $rowBounds.Top) * $sy))
                        $right = [Math]::Min($bitmap.Width, [int][Math]::Ceiling(($rect.Right - $rowBounds.Left) * $sx))
                        $bottom = [Math]::Min($bitmap.Height, [int][Math]::Ceiling(($rect.Bottom - $rowBounds.Top) * $sy))
                        $matches = 0
                        for ($y = $top; $y -lt $bottom; $y++) {
                            for ($x = $left; $x -lt $right; $x++) {
                                $pixel = $bitmap.GetPixel($x, $y)
                                if ($MonochromeTheme -eq 'dark') {
                                    if ($pixel.R -ge 180 -and $pixel.G -ge 180 -and $pixel.B -ge 180) { $matches++ }
                                } elseif ($MonochromeTheme -eq 'light') {
                                    if ($pixel.R -le 75 -and $pixel.G -le 75 -and $pixel.B -le 75) { $matches++ }
                                } elseif ([Math]::Abs($pixel.R - $Color.R) -le 5 -and
                                    [Math]::Abs($pixel.G - $Color.G) -le 5 -and
                                    [Math]::Abs($pixel.B - $Color.B) -le 5) { $matches++ }
                            }
                        }
                        if ($matches -lt 4) { return $false }
                    } finally { $bitmap.Dispose() }
                    }
                }
                return $true
            } | Out-Null
        }
        try {
            Set-WtSetting -App $script:app -Key agentSessionManagementEnabled -Value $true | Out-Null
            Set-WtSetting -App $script:app -Key sidebarShowRecentAgentSessions -Value $true | Out-Null
            $builtin = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                Where-Object agent_id -EQ 'copilot') | Select-Object -First 1
            $builtin | Should -Not -BeNullOrEmpty
            $custom = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
                Where-Object agent_id -EQ 'custom:fixture-adapter') | Select-Object -First 1
            $custom | Should -Not -BeNullOrEmpty
            foreach ($target in @(
                @{ id = $custom.id; provider = 'custom:fixture-adapter' },
                @{ id = $builtin.id; provider = 'copilot' }
            )) {
                $marker = 'profile-icon-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
                $conversation = [guid]::NewGuid().ToString()
                $historyCwd = Join-Path $script:proofRoot $marker
                New-Item -ItemType Directory -Path $historyCwd -Force | Out-Null
                foreach ($phase in @(
                    @{ name = 'before'; color = [Drawing.Color]::FromArgb(245, 39, 131) },
                    @{ name = 'after'; color = [Drawing.Color]::FromArgb(23, 197, 173) }
                )) {
                    $iconPath = Join-Path $script:proofRoot "$marker-$($phase.name).png"
                    $icon = [Drawing.Bitmap]::new(16, 16)
                    $graphics = [Drawing.Graphics]::FromImage($icon)
                    try {
                        $graphics.Clear($phase.color)
                        $icon.Save($iconPath, [Drawing.Imaging.ImageFormat]::Png)
                    } finally { $graphics.Dispose(); $icon.Dispose() }
                    $profiles = Get-WtSetting -App $script:app -Key profiles
                    $entry = @($profiles.list | Where-Object guid -EQ $target.id)[0]
                    $entry | Add-Member -NotePropertyName icon -NotePropertyValue $iconPath -Force
                    $entry | Add-Member -NotePropertyName commandline -NotePropertyValue "`"$script:fixture`"" -Force
                    $entry.name = "Renamed $($target.provider) $($phase.name)"
                    Set-WtSetting -App $script:app -Key profiles -Value $profiles | Out-Null
                    if ($phase.name -eq 'before') {
                        $requestId = [guid]::NewGuid().ToString()
                        $event = @{ type = 'event'; method = 'resume_in_new_agent_tab'; params = @{
                            window_id = "$($script:app.WindowId)"; agent_id = $target.provider; agent_source = 'host'
                            resume_target = 'native'; profile_id = $target.id; session_id = $conversation
                            request_id = $requestId; title = $marker; cwd = $historyCwd
                        } } | ConvertTo-Json -Compress -Depth 6
                        Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $event) | Out-Null
                        $receipt = Wait-WtEvent -Listener $script:listener -TimeoutSec 20 -Predicate {
                            $_.method -eq 'native_profile_resume_result' -and $_.params.request_id -eq $requestId
                        }
                        $receipt.params.success | Should -BeTrue
                        $seed = $receipt.params.result.session_id
                        $panes += $seed
                        Assert-NativeFixtureTarget $seed
                        $hook = @{
                            cli_source = $target.provider; agent_session_id = $conversation
                            event = 'agent.session.start'; pane_id = $seed
                            payload = @{ cwd = $historyCwd }
                        } | ConvertTo-Json -Depth 5 -Compress
                        Invoke-WtCli -App $script:app -Arguments @('send-event', '-p', $seed, '-e', 'agent.session.start', $hook) | Out-Null
                        Wait-Until -TimeoutSec 15 -Because 'the owned conversation reaches the recent-session model' -Condition {
                            @(Get-NativeMasterSessions | Where-Object {
                                $_.session_id -eq $conversation -and $_.provider_id -eq $target.provider -and $_.cwd -eq $historyCwd
                            }).Count -eq 1
                        } | Out-Null
                        Close-WtPane -App $script:app -SessionId $seed
                        $panes = @($panes | Where-Object { $_ -ne $seed })
                        Wait-Until -TimeoutSec 20 -Because 'the distinct historical row is no longer represented by a live pane' -Condition {
                            $row = @(Get-NativeMasterSessions | Where-Object session_id -EQ $conversation) | Select-Object -First 1
                            $row -and -not $row.pane_session_id
                        } | Out-Null
                        $created = Invoke-WtCli -App $script:app -Arguments @('new-tab', '-w', "$($script:app.WindowId)", '-p', $target.id, '-n', $marker)
                        $panes += $created.session_id
                        Assert-NativeFixtureTarget $created.session_id
                        @(Get-NativeMasterSessions | Where-Object {
                            ([string]$_.pane_session_id).Trim('{}') -eq $created.session_id.Trim('{}')
                        }).Count | Should -Be 0 -Because 'the fresh live pane must not claim the historical conversation'
                    }
                    Assert-ProfileIconRendering $marker $phase.color "$marker-$($phase.name)"
                    @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles | Where-Object {
                        $_.id -eq $target.id -and $_.agent_id -eq $target.provider
                    }).Count | Should -Be 1
                }
                if ($target.provider -eq 'copilot') {
                    $profiles = Get-WtSetting -App $script:app -Key profiles
                    @($profiles.list | Where-Object guid -EQ $target.id)[0].icon = 'ms-appx:///AgentIcons/copilot.svg'
                    Set-WtSetting -App $script:app -Key profiles -Value $profiles | Out-Null
                    $themePhase = 0
                    foreach ($theme in @('dark', 'light', 'dark')) {
                        Set-WtSetting -App $script:app -Key theme -Value $theme | Out-Null
                        Assert-ProfileIconRendering $marker ([Drawing.Color]::Empty) "$marker-mono-$themePhase-$theme" $theme
                        $themePhase++
                    }
                }
                Invoke-WtCli -App $script:app -Arguments @('kill-pane', '-t', $created.session_id) | Out-Null
                $panes = @($panes | Where-Object { $_ -ne $created.session_id })
            }
        } finally {
            foreach ($pane in $panes) {
                Invoke-WtCli -App $script:app -Arguments @('kill-pane', '-t', $pane) | Out-Null
            }
            Set-WtSetting -App $script:app -Key profiles -Value $savedProfiles | Out-Null
            Set-WtSetting -App $script:app -Key theme -Value 'dark' | Out-Null
            if (Test-UiElementExists -App $script:app -Selector SearchTextBox -TimeoutSec 1) {
                Set-UiValue -App $script:app -Selector SearchTextBox -Value '' | Out-Null
                Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            }
        }
    }

    It 'Agent profile settings expose an indicator and automatic session ID guidance' {
        Open-WtSettings -App $script:app | Out-Null
        [IO.File]::WriteAllText((Join-Path $script:proofRoot 'settings-navigation.txt'), (Get-UiTree -App $script:app -Depth 20))
        Select-NativeProfilePage 'Profile resume fixture'
        Test-UiElementExists -App $script:app -Selector 'Agent profile' -TimeoutSec 10 | Should -BeTrue
        Test-UiElementExists -App $script:app -Selector 'Resume arguments' -TimeoutSec 10 | Should -BeTrue
        $tree = Get-UiTree -App $script:app -Depth 15
        Test-UiElementExists -App $script:app -Selector 'Added after the normal startup arguments only when resuming a session. The session ID is appended automatically.' -TimeoutSec 10 | Should -BeTrue
        $tree | Should -Match 'Delete profile'
        $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
        $deleteCondition = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::AutomationIdProperty, 'DeleteButton')
        $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $deleteCondition).Current.IsEnabled | Should -BeTrue
        $original = @((Invoke-WtCli -App $script:app -Arguments @('list-profiles')).profiles |
            Where-Object agent_id -EQ 'custom:fixture-adapter') | Select-Object -First 1
        Select-NativeProfilePage $original.name
        $generated = Get-UiTree -App $script:app -Depth 15
        $generated | Should -Match 'Agent profile'
        $generated | Should -Match 'Delete profile'
        $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $deleteCondition).Current.IsEnabled | Should -BeFalse
        $generated | Should -Match 'Appearance'
    }
}
