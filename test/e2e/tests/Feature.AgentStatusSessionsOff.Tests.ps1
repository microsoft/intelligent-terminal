#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real provider-labelled COM creation -> child stdout -> ConPTY parser -> Sidebar metadata.
# No hooks or status injection; list guesses remain separate from confirmed identity.
# Known resume uses separately seeded real Copilot history; this suite sends no ACP prompts.
# Without Sessions/hooks, command completion or retained connection state cannot
# prove that the native agent ended; its pane ID must disappear.
Describe 'Feature: native agent status with Sessions off' -Tag @('Feature', 'AgentStatusSessionsOff') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # Cache the absolute UI tool before provider containment narrows PATH.
        & (Get-Module ItE2E) { Get-WinAppPath } | Out-Null
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $script:app = $null
        $script:ownsConfig = $false
        $script:suiteClock = [Diagnostics.Stopwatch]::StartNew()
        $script:fixtures = @()
        $script:runnerEnvironment = @{}
        $script:hookIsolationVerified = $false
        $script:sidebarScopeToRestore = $null
        if (-not $env:ITE2E_PACKAGE -or $env:ITE2E_PACKAGE -in @('Auto', 'Dev', 'Store')) {
            throw 'Pin the owned isolated Dev package family; ordinary Dev and Store are forbidden.'
        }
        $script:target = Resolve-ItApp -Package $env:ITE2E_PACKAGE
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'The selected private package must be inactive; never adopt or close existing hosts.'
        }
        $head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or $env:ITE2E_SOURCE_COMMIT -ne $head) {
            throw 'Supply ITE2E_SOURCE_COMMIT from the exact feature-head build receipt.'
        }
        foreach ($entry in @(
            @{ Path = (Join-Path $script:target.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 },
            @{ Path = $script:target.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 }
        )) {
            if (-not $entry.Hash) { throw 'Supply exact-source deployed App and WTA SHA256 receipts.' }
            (Get-FileHash -LiteralPath $entry.Path).Hash | Should -Be $entry.Hash
        }
        if (-not $env:ITE2E_COPILOT_SEED_RECEIPT) {
            throw 'Supply the isolated real Copilot seeded-acp-history-receipt.json; never use personal history.'
        }
        $script:copilotSeed = Get-Content -LiteralPath $env:ITE2E_COPILOT_SEED_RECEIPT -Raw |
            ConvertFrom-Json -ErrorAction Stop
        $script:copilotSeed.Verdict | Should -Be TWO_REAL_SEEDED_IDS_VERIFIED_IN_ACP_HISTORY
        $script:copilotSeed.AgentInfo.name | Should -Be Copilot
        $script:copilotSeed.ExecutionSource | Should -Match '^Host\b'
        $script:copilotSeed.UnexpectedHistoryCount | Should -Be 0
        $script:copilotSeed.ExpectedRows | Should -HaveCount 2
        @($script:copilotSeed.ExpectedRows.sessionId | Select-Object -Unique) | Should -HaveCount 2
        $script:copilotSeed.USERPROFILE | Should -Match '(?i)automation|smoke|probe|test-runs'
        [IO.Path]::GetFullPath($script:copilotSeed.USERPROFILE) |
            Should -Not -Be ([IO.Path]::GetFullPath($env:USERPROFILE))
        foreach ($row in $script:copilotSeed.ExpectedRows) {
            [guid]::Parse($row.sessionId) | Should -Not -Be ([guid]::Empty)
            $row.title | Should -Match '^auto:smoke:status-off:'
            Test-Path -LiteralPath $row.cwd -PathType Container | Should -BeTrue
        }
        [IO.Path]::GetFullPath($script:copilotSeed.COPILOT_HOME) |
            Should -Be ([IO.Path]::GetFullPath((Join-Path $script:copilotSeed.USERPROFILE '.copilot')))
        (Get-FileHash -LiteralPath $script:copilotSeed.Executable).Hash | Should -Be $script:copilotSeed.SHA256
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Recover existing backup before testing: $path"
            }
            $script:originalHashes[$path] = if (Test-Path $path) { (Get-FileHash $path).Hash } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('sessions-off-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence | Out-Null
        Copy-Item -LiteralPath $env:ITE2E_COPILOT_SEED_RECEIPT -Destination (Join-Path $script:evidence 'copilot-seed-receipt.json')
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $script:wrapperDir = Join-Path $script:evidence 'hook-cli-wrappers'
        New-Item -ItemType Directory -Path $script:wrapperDir | Out-Null
        $script:providers = @('copilot', 'claude', 'codex', 'gemini', 'opencode')
        foreach ($provider in $script:providers) {
            $receipt = Join-Path $script:evidence "$provider-wrapper.log"
            @("@echo off", ">>`"$receipt`" echo $provider fixture invoked", 'exit /b 1') |
                Set-Content (Join-Path $script:wrapperDir "$provider.cmd") -Encoding ascii
        }
        # These shims isolate shell lookup and hook reconciliation. WTA refreshes
        # child PATH from the registry; its real ACP children use automation homes.
        $blockerSource = Join-Path $script:evidence 'Reject-Provider.cs'
        @'
using System;
using System.IO;
class RejectProvider {
    static int Main() {
        string name = Path.GetFileNameWithoutExtension(Environment.GetCommandLineArgs()[0]);
        File.AppendAllText(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "..", name + "-wrapper.log"), "fixture invoked\r\n");
        return 3;
    }
}
'@ | Set-Content $blockerSource
        $compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
        $blocker = Join-Path $script:evidence 'Reject-Provider.exe'
        (Invoke-Native -FilePath $compiler -Arguments @('/nologo', '/target:exe', "/out:$blocker", $blockerSource) -TimeoutSec 20).ExitCode |
            Should -Be 0
        $script:blockerHash = (Get-FileHash $blocker).Hash
        foreach ($provider in @($script:providers) + @('npx', 'antigravity')) {
            Copy-Item $blocker (Join-Path $script:wrapperDir "$provider.exe")
        }
        foreach ($provider in @('npx', 'antigravity')) {
            @('@echo off', 'exit /b 3') | Set-Content (Join-Path $script:wrapperDir "$provider.cmd") -Encoding ascii
        }
        $automationHome = $script:copilotSeed.USERPROFILE
        $script:isolatedEnvironment = @{
            USERPROFILE = $automationHome; HOME = $automationHome
            XDG_CONFIG_HOME = (Join-Path $script:evidence 'automation-config')
            COPILOT_HOME = (Join-Path $automationHome '.copilot')
            CLAUDE_CONFIG_DIR = (Join-Path $automationHome '.claude')
            CODEX_HOME = (Join-Path $automationHome '.codex')
            GEMINI_CLI_HOME = (Join-Path $automationHome '.gemini')
            OPENCODE_CONFIG_DIR = (Join-Path $script:evidence 'automation-config\opencode')
            PATH = (@($script:wrapperDir, (Split-Path $pwsh), $script:target.InstallLocation,
                (Join-Path $env:WINDIR 'System32'), $env:WINDIR) -join ';')
            WT_WTCLI_PATH = $script:target.WtcliPath; WTCLI_PATH = $script:target.WtcliPath
        }
        foreach ($key in $script:isolatedEnvironment.Keys) {
            $script:runnerEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            [Environment]::SetEnvironmentVariable($key, $script:isolatedEnvironment[$key], 'Process')
        }
        foreach ($key in @('USERPROFILE', 'HOME', 'XDG_CONFIG_HOME', 'COPILOT_HOME', 'CLAUDE_CONFIG_DIR',
            'CODEX_HOME', 'GEMINI_CLI_HOME', 'OPENCODE_CONFIG_DIR')) {
            New-Item -ItemType Directory -Path $script:isolatedEnvironment[$key] -Force | Out-Null
        }
        # Unredirected known-folder queries expand USERPROFILE, whereas packaged
        # settings stay in the registered user's LocalState.
        New-Item -ItemType Directory -Path (Join-Path $automationHome 'AppData\Local'),
            (Join-Path $automationHome 'AppData\Roaming') -Force | Out-Null
        $script:fixture = Join-Path $script:evidence 'Emit-NativeStatus.ps1'
        @'
param([string]$Signal, [string]$Receipt, [string]$Title, [int]$LifetimeSec = 540)
[Console]::Write([char]27 + ']0;' + $Title + [char]27 + '\')
$last = ''
$clock = [Diagnostics.Stopwatch]::StartNew()
while ($clock.Elapsed.TotalSeconds -lt $LifetimeSec) {
    $next = Get-ChildItem -LiteralPath ([IO.Path]::GetDirectoryName($Signal)) `
        -Filter ([IO.Path]::GetFileName($Signal) + '.*.request') -File | Select-Object -First 1
    if ($next) {
        $stream = [IO.File]::Open($next.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read,
            [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
        $reader = [IO.StreamReader]::new($stream)
        try { $line = $reader.ReadToEnd() } finally { $reader.Dispose() }
        if ($line -and $line -ne $last) {
            $last = $line
            $request = $line | ConvertFrom-Json
            if ($request.kind -eq 'close') { exit 0 }
            if ($request.kind -eq 'bell') { [Console]::Write([char]7) }
            else { [Console]::Write([char]27 + ']9;4;' + $request.state + ';50' + [char]27 + '\') }
            [Console]::Out.Flush()
            [IO.File]::WriteAllText($Receipt, $request.token)
            Remove-Item -LiteralPath $next.FullName
        }
    }
    Start-Sleep -Milliseconds 50
}
@{ status = 'timed-out'; pid = $PID; lifetime_seconds = $LifetimeSec } |
    ConvertTo-Json | Set-Content -LiteralPath "$Receipt.failure.json"
exit 124
'@ | Set-Content -LiteralPath $script:fixture
        $acp = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $history = Join-Path $script:evidence 'acp-history.json'
        $script:historyPath = $history
        @{ sessions = @() } | ConvertTo-Json | Set-Content -LiteralPath $history
        $code = "& '$($acp.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\acp.log' -HistoryPath '$($history.Replace("'", "''"))'"
        $profile = '{' + [guid]::NewGuid().ToString() + '}'
        $script:ownsConfig = $true
        $script:app = Start-Terminal -Package $env:ITE2E_PACKAGE -PassFre $true -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
            sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            agentSessionManagementEnabled = $false; autoErrorDetectionEnabled = $false; autoFixEnabled = $false
            acpAgent = 'custom:sessions-off-fixture'
            acpCustomCommand = 'pwsh -NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
            'warning.confirmOnClose' = 'never'; defaultProfile = $profile
            profiles = @{ list = @(@{
                guid = $profile; name = 'Sessions-off control'; commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                tabTitle = 'Sessions-off control'; suppressApplicationTitle = $true
                shellIntegrationEnabled = $false
                reloadEnvironmentVariables = $false
                environment = $script:isolatedEnvironment
            }) }
        }
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true
        $script:app.Launched | Should -BeTrue
        $script:window = [string]$script:app.WindowId
        $settingsReadByHost = Invoke-WtCli -App $script:app -Arguments @('get-settings')
        $settingsReadByHost.defaultProfile | Should -Be $profile
        $settingsReadByHost.agentSessionManagementEnabled | Should -BeFalse
        $settingsReadByHost.tabLayout | Should -Be vertical
        $initialTabs = @(Get-WtTabs -App $script:app -WindowId $script:window)
        $initialTabs | Should -HaveCount 1
        $initialTabs[0].title | Should -Be 'Sessions-off control' -Because 'the actual host must load the fixture profile, not fallback defaults'
        $settingsReadByHost | ConvertTo-Json -Depth 12 |
            Set-Content (Join-Path $script:evidence 'settings-read-by-host.json')
        # Profile overrides prove pane-side isolation separately. They cannot
        # prove the host environment used for C++ hook reconciliation.
        $probeFile = Join-Path $script:evidence 'host-hook-isolation.json'
        $probeCode = @"
try {
`$names = @('copilot', 'claude', 'codex', 'gemini', 'opencode')
`$envNames = @('USERPROFILE', 'HOME', 'XDG_CONFIG_HOME', 'COPILOT_HOME', 'CLAUDE_CONFIG_DIR',
    'CODEX_HOME', 'GEMINI_CLI_HOME', 'OPENCODE_CONFIG_DIR', 'PATH')
`$environment = @{}
foreach (`$name in `$envNames) { `$environment[`$name] = [Environment]::GetEnvironmentVariable(`$name, 'Process') }
`$paths = @{}
foreach (`$name in `$names) {
    `$paths[`$name] = (Get-Command `$name -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    & `$paths[`$name] --version 2>`$null | Out-Null
}
`$statusText = & '$($script:target.WtaPath.Replace("'", "''"))' hooks status --json
if (`$LASTEXITCODE -ne 0) { throw 'Read-only hook status probe failed' }
@{ environment = `$environment; paths = `$paths; hook_status = (`$statusText -join "`n" | ConvertFrom-Json) } |
    ConvertTo-Json -Depth 12 | Set-Content -LiteralPath '$($probeFile.Replace("'", "''"))'
}
catch {
    @{ error = `$_.ToString(); stack = `$_.ScriptStackTrace; environment = `$environment; paths = `$paths } | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath '$($probeFile.Replace("'", "''")).failure.json'
    throw
}
"@
        $probeCode | Set-Content (Join-Path $script:evidence 'host-hook-isolation-probe.ps1')
        $probeTokens = $null
        $probeErrors = $null
        [void][Management.Automation.Language.Parser]::ParseInput($probeCode, [ref]$probeTokens, [ref]$probeErrors)
        if ($probeErrors.Count) { throw ($probeErrors.Message -join '; ') }
        $probeEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probeCode))
        $probePane = New-WtTab -App $script:app -Command "`"$pwsh`" -NoProfile -NoExit -EncodedCommand $probeEncoded"
        try {
            Wait-Until -TimeoutSec 30 -Because 'owned host inheritance and read-only hook resolver proof' -Condition {
                (Test-Path -LiteralPath $probeFile) -or (Test-Path -LiteralPath "$probeFile.failure.json")
            } | Out-Null
            if (Test-Path -LiteralPath "$probeFile.failure.json") {
                throw (Get-Content -LiteralPath "$probeFile.failure.json" -Raw)
            }
        }
        finally {
            Get-WtCapture -App $script:app -SessionId $probePane.session_id -MaxLines 100 |
                Set-Content (Join-Path $script:evidence 'host-hook-isolation-probe.txt')
        }
        $probe = Get-Content -LiteralPath $probeFile -Raw | ConvertFrom-Json
        foreach ($key in @('USERPROFILE', 'HOME', 'XDG_CONFIG_HOME', 'COPILOT_HOME', 'CLAUDE_CONFIG_DIR',
            'CODEX_HOME', 'GEMINI_CLI_HOME', 'OPENCODE_CONFIG_DIR')) {
            $probe.environment.$key | Should -Be $script:isolatedEnvironment[$key]
        }
        ($probe.environment.PATH -split ';') | Should -Contain $script:wrapperDir
        foreach ($provider in $script:providers) {
            $expected = Join-Path $script:wrapperDir "$provider.exe"
            $probe.paths.$provider | Should -Be $expected
            $cli = @($probe.hook_status.clis | Where-Object name -EQ $provider)
            $cli | Should -HaveCount 1
            $cli[0].binary_path | Should -Be $expected
            $cli[0].plugin_installed | Should -BeFalse
            (Test-Path (Join-Path $script:evidence "$provider-wrapper.log")) | Should -BeTrue
        }
        $creator = $script:app.OwnedLauncherProcess
        $directHost = $creator -and -not $creator.HasExited -and
            $creator.Id -eq $script:app.Pid -and
            $creator.Path -eq (Join-Path $script:target.InstallLocation 'WindowsTerminal.exe') -and
            $creator.StartTime -eq $script:app.OwnedProcess.StartTime
        # The verified wtai creator launches its adjacent host with a null
        # environment block (src/cascadia/wt/shim.cpp), preserving inheritance.
        $shimHost = $creator -and
            $script:app.PSObject.Properties.Name -contains 'CreatedTerminalParentPid' -and
            $script:app.CreatedTerminalParentPid -eq $creator.Id -and
            $script:app.OwnedProcess.Path -eq (Join-Path $script:target.InstallLocation 'WindowsTerminal.exe')
        @{
            pane_isolation_verified = $true
            direct_inherited_host_verified = [bool]$directHost
            shim_inherited_host_verified = [bool]$shimHost
            created_pid = if ($creator) { $creator.Id } else { $null }
            actual_host_pid = $script:app.Pid
            required_image = (Join-Path $script:target.InstallLocation 'WindowsTerminal.exe')
            reason = 'Direct creation or the identity-verified wtai creator chain preserves the isolated runner environment.'
        } | ConvertTo-Json | Set-Content (Join-Path $script:evidence 'host-activation-isolation.json')
        $script:hookIsolationVerified = [bool]($directHost -or $shimHost)
        function Assert-StatusDeadline {
            if ($script:suiteClock.Elapsed.TotalSeconds -ge 480) {
                throw 'Sessions-off suite exceeded its eight-minute outer deadline.'
            }
            foreach ($fixture in $script:fixtures) {
                if (Test-Path "$($fixture.Receipt).failure.json") {
                    throw "Native fixture timed out: $($fixture.Receipt).failure.json"
                }
            }
        }
        function New-StatusFixture {
            param([string]$Kind)
            Assert-StatusDeadline
            $title = "IT-status-$Kind-" + [guid]::NewGuid().ToString('N').Substring(0, 8)
            $signal = Join-Path $script:evidence "$title.signal.json"
            $receipt = Join-Path $script:evidence "$title.receipt.txt"
            $command = "`"$pwsh`" -NoProfile -File `"$script:fixture`" -Signal `"$signal`" -Receipt `"$receipt`" -Title $title -LifetimeSec 540"
            $args = @('new-tab', '-c', $command, '-n', $title, '-d', $script:evidence)
            if ($Kind -eq 'native') { $args += @('--agent-provider', 'copilot') }
            $createdAfterMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            if ($Kind -in @('delegate', 'delegate-prompt')) {
                $previousPane = Get-ActivePane -App $script:app
                $delegate = @('delegate', '--agent', 'copilot', '--delegate-agent', $command,
                    '--delegate-agent-id', 'custom:Sessions-Off-Fixture', '--delegate-source', 'host',
                    '--cwd', $script:evidence)
                if ($Kind -eq 'delegate-prompt') { $delegate += 'controlled startup prompt' }
                $result = Invoke-Wta -App $script:app -Arguments $delegate -Raw -TimeoutSec 20
                $result.ExitCode | Should -Be 0 -Because $result.StdErr
                $pane = Get-ActivePane -App $script:app
                $pane.session_id | Should -Not -Be $previousPane.session_id
                $actual = @(Get-WtPanes -App $script:app -TabId $pane.tab_id |
                    Where-Object session_id -EQ $pane.session_id)
                $actual | Should -HaveCount 1
                $actual[0].native_agent_provider_id | Should -Be 'custom:sessions-off-fixture'
            }
            else {
                $pane = Invoke-WtCli -App $script:app -Arguments $args
            }
            $createdBeforeMs = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            $pane.session_id | Should -Not -BeNullOrEmpty
            $fixture = [pscustomobject]@{
                Pane = $pane; Title = $title; Signal = $signal; Receipt = $receipt; Kind = $Kind
                CreatedAfterMs = $createdAfterMs; CreatedBeforeMs = $createdBeforeMs; Creation = $null
            }
            $script:fixtures += $fixture
            $fixture
        }
        function Get-StatusRowParts {
            param($Element)
            $walker = [Windows.Automation.TreeWalker]::RawViewWalker
            $child = $walker.GetFirstChild($Element)
            while ($child) {
                $child
                Get-StatusRowParts $child
                $child = $walker.GetNextSibling($child)
            }
        }
        function Get-StatusText {
            param($Fixture)
            Assert-StatusDeadline
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
            if (-not $list -or $list.Current.IsOffscreen) { throw 'Owned Sidebar ItemsList is not visible.' }
            $expectedTitle = if ($Fixture.PSObject.Properties['StatusTitle']) { $Fixture.StatusTitle } else { $Fixture.Title }
            $rows = @($list.FindAll([Windows.Automation.TreeScope]::Children,
                [Windows.Automation.Condition]::TrueCondition) | Where-Object {
                $parts = @(Get-StatusRowParts $_)
                -not $_.Current.IsOffscreen -and $_.Current.Name.Contains($expectedTitle) -and
                    @($parts | Where-Object { $_.Current.AutomationId -in @('HistoryProviderIcon', 'HistoryHeaderButton') }).Count -eq 0
            })
            if ($rows.Count -ne 1) { return $null }
            # The top-level tab peer contains first-party metadata. Descendant
            # progress-ring peers also say Error/Paused for ordinary shells.
            $rows[0].Current.Name
        }
        function Save-StatusDiagnostic {
            param($Fixture, [string]$Phase)
            $fileTitle = $Fixture.Title -replace '[<>:"/\\|?*]', '_'
            try {
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
                $rows = if ($list) {
                    @($list.FindAll([Windows.Automation.TreeScope]::Children,
                        [Windows.Automation.Condition]::TrueCondition) | ForEach-Object {
                        @{ name = $_.Current.Name; automation_id = $_.Current.AutomationId
                            offscreen = $_.Current.IsOffscreen; control_type = $_.Current.ControlType.ProgrammaticName }
                    })
                } else { @() }
                @{
                    utc = [DateTimeOffset]::UtcNow; phase = $Phase; expected_title = $Fixture.Title
                    host_pid = $root.Current.ProcessId; list_found = [bool]$list
                    list_offscreen = if ($list) { $list.Current.IsOffscreen } else { $null }
                    title_matched_rows = @($rows | Where-Object { $_.name.Contains($Fixture.Title) })
                    all_rows = $rows; fixture_pane = $Fixture.Pane
                    actual_panes = @(Get-WtPanes -App $script:app -WindowId $script:window -TabId $Fixture.Pane.tab_id)
                    settings_disk = Invoke-WtCli -App $script:app -Arguments @('get-settings')
                    registry = try { Get-StatusRegistrySnapshot } catch { @{ diagnostic_error = $_.ToString() } }
                } | ConvertTo-Json -Depth 16 |
                    Set-Content (Join-Path $script:evidence "$fileTitle-$Phase-diagnostic.json")
            }
            catch {
                $_ | Out-String | Set-Content (Join-Path $script:evidence "$fileTitle-$Phase-diagnostic-error.txt")
            }
        }
        function Send-StatusSignal {
            param($Fixture, [string]$Kind = 'progress', [int]$State = 0)
            if ($Kind -ne 'close') { Assert-StatusDeadline }
            if ($Kind -eq 'progress' -and $State -in @(0, 4)) {
                Save-StatusDiagnostic $Fixture "before-OSC$State"
            }
            $token = [guid]::NewGuid().ToString('N')
            $request = @{ kind = $Kind; state = $State; token = $token } | ConvertTo-Json -Compress
            $staging = "$($Fixture.Signal).next"
            $destination = "$($Fixture.Signal).$token.request"
            [IO.File]::WriteAllText($staging, $request)
            [IO.File]::Move($staging, $destination)
            if ($Kind -ne 'close') {
                Wait-Until -TimeoutSec 10 -Because 'the real child emitted the exact signal' -Condition {
                    Assert-StatusDeadline
                    (Test-Path $Fixture.Receipt) -and (Get-Content $Fixture.Receipt -Raw) -eq $token
                } | Out-Null
            }
        }
        function Assert-Status {
            param($Fixture, [string]$Expected)
            try {
                $registryStatus = @{ Active = 'Working'; Idle = 'Idle'; Error = 'Error'; 'Waiting for input' = 'Attention' }[$Expected]
                Assert-RegistryObservation $Fixture $registryStatus
                $text = Wait-Until -TimeoutSec 10 -Because "native metadata reaches $Expected" -Condition {
                    $value = Get-StatusText $Fixture
                    if ($value -match "(?m)(^|[ ,·])$([regex]::Escape($Expected))($|[ ,·])") { $value }
                }
            }
            catch {
                Save-StatusDiagnostic $Fixture "timeout-$Expected"
                throw
            }
            $text | Should -Not -Match 'Attention requested'
            $text | Set-Content (Join-Path $script:evidence "$($Fixture.Title)-$Expected.txt")
        }
        function Assert-NoAgentStatus {
            param($Fixture)
            $deadline = [DateTime]::UtcNow.AddSeconds(2)
            do {
                $text = Get-StatusText $Fixture
                $text | Should -Not -BeNullOrEmpty -Because 'suppression requires a realized owned row'
                $text | Should -Not -Match '(?m)(^|[ ,·])(Active|Idle|Error|Paused|Attention requested|Waiting for input)($|[ ,·])'
                Start-Sleep -Milliseconds 100
            } while ([DateTime]::UtcNow -lt $deadline)
            $snapshot = Get-StatusRegistrySnapshot
            @($snapshot.native_pane_observations | Where-Object {
                ([string]$_.pane_id).Trim('{}') -eq ([string]$Fixture.Pane.session_id).Trim('{}')
            }) | Should -HaveCount 0 -Because 'shell output and Sessions-ON native output must not create OFF observations'
        }
        function Assert-BellRequestsInput {
            param($Fixture)
            Send-StatusSignal $Fixture -Kind bell
            Assert-Status $Fixture 'Waiting for input'
            $deadline = [DateTime]::UtcNow.AddSeconds(2)
            do {
                $text = Get-StatusText $Fixture
                $text | Should -Match '(?m)(^|[ ,·])Waiting for input($|[ ,·])'
                $text | Should -Not -Match 'Paused|Attention requested'
                Start-Sleep -Milliseconds 100
            } while ([DateTime]::UtcNow -lt $deadline)
        }
        function Get-StatusRegistrySnapshot {
            Assert-StatusDeadline
            $pipe = (Get-Content (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
            $pipe | Should -Not -BeNullOrEmpty
            # --include-status returns SessionsListResponse, not default JSON-lines.
            $result = Invoke-Wta -App $script:app -TimeoutSec 10 -Raw -Arguments @(
                'sessions', 'list', '--master', $pipe, '--json', '--include-status', '--origin', 'all')
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            $result.StdOut | Should -Not -BeNullOrEmpty
            $snapshot = ConvertFrom-Json -InputObject $result.StdOut -NoEnumerate -ErrorAction Stop
            $snapshot | Should -BeOfType [pscustomobject]
            $snapshot.PSObject.Properties.Name | Should -Contain sessions
            $snapshot.PSObject.Properties.Name | Should -Contain pane_statuses
            $snapshot.PSObject.Properties.Name | Should -Contain history_status
            ($snapshot.sessions -is [array]) | Should -BeTrue -Because 'including an empty sessions array is mandatory'
            ($snapshot.pane_statuses -is [array]) | Should -BeTrue
            @($snapshot.pane_statuses | ForEach-Object { $_.pane_id } | Select-Object -Unique) |
                Should -HaveCount (@($snapshot.pane_statuses).Count)
            foreach ($paneStatus in $snapshot.pane_statuses) {
                [guid]::Parse($paneStatus.pane_id) | Should -Not -Be ([guid]::Empty)
                $paneStatus.provider_id | Should -Not -BeNullOrEmpty
                $paneStatus.status | Should -Not -BeNullOrEmpty
            }
            # History discovery failures are independent of this successful
            # full-registry response; they do not invalidate live pane bindings.
            $snapshot.history_status | Should -BeIn @('loading', 'ready', 'error')
            if ($snapshot.PSObject.Properties.Name -contains 'native_pane_observations') {
                ($snapshot.native_pane_observations -is [array]) | Should -BeTrue
                foreach ($observation in $snapshot.native_pane_observations) {
                    foreach ($key in @('pane_id', 'provider_id', 'window_id', 'tab_id', 'mode_epoch',
                        'pane_created_at_ms', 'pane_creation_sequence', 'status', 'last_activity_at_ms')) {
                        $observation.PSObject.Properties.Name | Should -Contain $key
                    }
                    [guid]::Parse($observation.pane_id) | Should -Not -Be ([guid]::Empty)
                    $observation.provider_id | Should -Not -BeNullOrEmpty
                    $observation.tab_id | Should -BeOfType [string]
                    $observation.tab_id | Should -Not -BeNullOrEmpty
                    $observation.mode_epoch | Should -BeGreaterOrEqual 0
                    $observation.last_activity_at_ms | Should -BeGreaterThan 0
                    $observation.pane_created_at_ms | Should -BeGreaterThan 0
                    $observation.pane_creation_sequence | Should -BeGreaterThan 0
                }
            }
            foreach ($session in $snapshot.sessions) {
                $session.PSObject.Properties.Name | Should -Contain session_id
                $session.PSObject.Properties.Name | Should -Contain pane_session_id
                $session.session_id | Should -Not -BeNullOrEmpty
            }
            $snapshot
        }
        function Assert-RegistryObservation {
            param($Fixture, [string]$Status)
            $captured = @{ Snapshot = $null }
            try {
                $observation = Wait-Until -TimeoutSec 10 -Because "master owns this exact native pane's $Status observation" -Condition {
                    $captured.Snapshot = Get-StatusRegistrySnapshot
                    $rows = @($captured.Snapshot.native_pane_observations | Where-Object {
                        ([string]$_.pane_id).Trim('{}') -eq ([string]$Fixture.Pane.session_id).Trim('{}')
                    })
                    $paneStatuses = @($captured.Snapshot.pane_statuses | Where-Object {
                        ([string]$_.pane_id).Trim('{}') -eq ([string]$Fixture.Pane.session_id).Trim('{}')
                    })
                    if ($rows.Count -eq 1 -and $rows[0].status -eq $Status -and
                        $paneStatuses.Count -eq 1 -and $paneStatuses[0].status -eq $Status) { $rows[0] }
                }
                $paneStatus = @($captured.Snapshot.pane_statuses | Where-Object {
                    ([string]$_.pane_id).Trim('{}') -eq ([string]$Fixture.Pane.session_id).Trim('{}')
                })[0]
                $paneStatus.agent_session_id | Should -BeNullOrEmpty
                $paneStatus.provider_id | Should -Be copilot
                $observation.provider_id | Should -Be copilot
                [string]$observation.window_id | Should -Be $script:window
                $observation.agent_session_id | Should -BeNullOrEmpty -Because 'a new native pane has no known conversation ID'
                $observation.location | Should -BeNullOrEmpty
                $observation.pane_created_at_ms | Should -BeGreaterOrEqual $Fixture.CreatedAfterMs
                $observation.pane_created_at_ms | Should -BeLessOrEqual $Fixture.CreatedBeforeMs
                if ($Fixture.Creation) {
                    $observation.pane_created_at_ms | Should -Be $Fixture.Creation.Time
                    $observation.pane_creation_sequence | Should -Be $Fixture.Creation.Sequence
                }
                else {
                    $Fixture.Creation = @{
                        Time = $observation.pane_created_at_ms; Sequence = $observation.pane_creation_sequence
                    }
                }
                @($captured.Snapshot.sessions | Where-Object {
                    ([string]$_.pane_session_id).Trim('{}') -eq ([string]$Fixture.Pane.session_id).Trim('{}')
                }) | Should -HaveCount 0 -Because 'pane observations are not invented ACP sessions'
            }
            finally {
                $captured.Snapshot | ConvertTo-Json -Depth 16 |
                    Set-Content (Join-Path $script:evidence "$($Fixture.Title)-registry-$Status.json")
            }
        }
        function Assert-NoFabricatedBinding {
            param($Fixture)
            $snapshot = Get-StatusRegistrySnapshot
            (Get-Content (Join-Path $script:evidence 'acp.log') -Raw) | Should -Match '(?m)^\d+\|list\|' -Because 'the fixture must exercise real ACP session discovery'
            $snapshot | ConvertTo-Json -Depth 16 | Set-Content (Join-Path $script:evidence "$($Fixture.Title)-binding.json")
            @($snapshot.sessions | Where-Object {
                ([string]$_.pane_session_id).Trim('{}') -eq ([string]$Fixture.Pane.session_id).Trim('{}')
            }) | Should -HaveCount 0
        }
        $script:native = New-StatusFixture native
        $script:otherNative = New-StatusFixture native
        $script:shell = New-StatusFixture shell
        foreach ($fixture in $script:fixtures) {
            Wait-Until -TimeoutSec 15 -Because 'the real fixture row has its unique title' -Condition {
                Get-StatusText $fixture
            } | Out-Null
        }
        foreach ($fixture in @($script:native, $script:otherNative)) {
            $panes = @(Get-WtPanes -App $script:app -WindowId $script:window -TabId $fixture.Pane.tab_id)
            ($panes | Where-Object session_id -EQ $fixture.Pane.session_id).native_agent_provider_id | Should -Be copilot
            Assert-RegistryObservation $fixture Idle
        }
        $script:otherNative.Creation.Sequence | Should -BeGreaterThan $script:native.Creation.Sequence
    }
    AfterEach {
        if ($script:sidebarScopeToRestore) {
            $scope = $script:sidebarScopeToRestore
            Set-TestSidebarScope -App $script:app -AgentsOnly $scope.AgentsOnly -Recent $scope.Recent
            Wait-Until -TimeoutSec 15 -Because 'the Sidebar preference save completes before the next settings mutation' -Condition {
                $disk = Get-Content -LiteralPath $script:app.SettingsPath -Raw | ConvertFrom-Json
                $hostSettings = Invoke-WtCli -App $script:app -Arguments @('get-settings')
                [bool]$disk.sidebarShowAgentsOnly -eq $scope.AgentsOnly -and
                    [bool]$disk.sidebarShowRecentAgentSessions -eq $scope.Recent -and
                    [bool]$hostSettings.sidebarShowAgentsOnly -eq $scope.AgentsOnly -and
                    [bool]$hostSettings.sidebarShowRecentAgentSessions -eq $scope.Recent
            } | Should -BeTrue
            $script:sidebarScopeToRestore = $null
        }
    }
    AfterAll {
        try {
        if ($script:app) {
            $owned = $script:app.OwnedProcess
            if (-not $script:app.Launched -or -not $owned -or $owned.Id -ne $script:app.Pid) {
                throw 'Cleanup lacks owned host receipt; retain backups.'
            }
            try {
                foreach ($fixture in $script:fixtures) {
                    Send-StatusSignal $fixture -Kind close
                }
            }
            finally {
                Stop-Terminal -App $script:app -GraceSec 8 -RestoreSettings $false
                Wait-Until -TimeoutSec 5 -IntervalSec 0.25 -Because 'owned shutdown publications finish before restoring configuration' -Condition {
                    -not @(Get-WtProcessesForApp -App $script:app -IncludePackageExecutables).Count
                } | Out-Null
                Restore-WtConfig -App $script:app
            }
        }
        elseif ($script:ownsConfig -and $script:target -and
            -not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            Restore-WtConfig -App $script:target
        }
        if ($script:ownsConfig -and -not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            foreach ($path in $script:originalHashes.Keys) {
                $actual = if (Test-Path $path) { (Get-FileHash $path).Hash } else { $null }
                $actual | Should -Be $script:originalHashes[$path]
            }
        }
        foreach ($fixture in $script:fixtures) {
            (Test-Path "$($fixture.Receipt).failure.json") | Should -BeFalse -Because 'fixture timeout is a test failure'
        }
        }
        finally {
            foreach ($key in $script:runnerEnvironment.Keys) {
                [Environment]::SetEnvironmentVariable($key, $script:runnerEnvironment[$key], 'Process')
            }
        }
    }
    It 'Sessions off native OSC and BEL report truthful status without shell false positives' {
        Send-StatusSignal $script:otherNative -State 4
        Assert-Status $script:otherNative Idle
        foreach ($step in @(@{ State = 1; Text = 'Active' }, @{ State = 3; Text = 'Active' },
            @{ State = 2; Text = 'Error' }, @{ State = 4; Text = 'Idle' }, @{ State = 0; Text = 'Idle' })) {
            Send-StatusSignal $script:native -State $step.State
            Assert-Status $script:native $step.Text
            Assert-Status $script:otherNative Idle
            Send-StatusSignal $script:shell -State $step.State
            Assert-NoAgentStatus $script:shell
        }
        Assert-BellRequestsInput $script:native
        Assert-Status $script:otherNative Idle
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'sessions-off-waiting-for-input-and-idle.png') | Out-Null
        Send-StatusSignal $script:otherNative -State 2
        Assert-Status $script:otherNative Error
        Assert-Status $script:native 'Waiting for input'
        Assert-BellRequestsInput $script:otherNative
        Send-StatusSignal $script:shell -Kind bell
        Assert-NoAgentStatus $script:shell
        $panes = @(Get-WtPanes -App $script:app -WindowId $script:window -TabId $script:shell.Pane.tab_id)
        ($panes | Where-Object session_id -EQ $script:shell.Pane.session_id).native_agent_provider_id | Should -BeNullOrEmpty
        Assert-NoFabricatedBinding $script:native
        Assert-NoFabricatedBinding $script:otherNative
        Assert-NoFabricatedBinding $script:shell
        Send-StatusSignal $script:native -State 1
        Assert-Status $script:native Active
        Assert-BellRequestsInput $script:native
        Send-StatusSignal $script:native -State 0
        Assert-Status $script:native Idle
        Send-StatusSignal $script:otherNative -State 0
        Assert-Status $script:otherNative Idle
    }
    It 'Sessions off new native panes retain creation metadata and infer list identity without control ownership' {
        # Real launch-only and prompt delegate creation share the same immutable
        # pane record; only SDK discovery supplies the separately marked guess.
        $delegates = @(New-StatusFixture delegate; New-StatusFixture delegate-prompt)
        try {
            $rows = foreach ($fixture in $delegates) {
                $row = @(Wait-Until -TimeoutSec 10 -Because 'the registry merges actual launch source with its admitted pane' -Condition {
                    $snapshot = Get-StatusRegistrySnapshot
                    $observations = @($snapshot.native_pane_observations | Where-Object {
                        ([string]$_.pane_id).Trim('{}') -eq ([string]$fixture.Pane.session_id).Trim('{}')
                    })
                    if ($observations.Count -eq 1 -and $observations[0].location -eq 'Host') { $observations[0] }
                })
                $row | Should -HaveCount 1
                $row[0].location | Should -Be Host
                $row[0].pane_created_at_ms | Should -BeGreaterOrEqual $fixture.CreatedAfterMs
                $row[0].pane_created_at_ms | Should -BeLessOrEqual $fixture.CreatedBeforeMs
                $row[0].agent_session_id | Should -BeNullOrEmpty
                $fixture.Creation = @{ Time = $row[0].pane_created_at_ms; Sequence = $row[0].pane_creation_sequence }
                Send-StatusSignal $fixture -State 1
                @{
                    sessionId = [guid]::NewGuid().ToString()
                    title = "controlled inferred $($fixture.Kind)"; cwd = $script:evidence
                    updatedAt = [DateTimeOffset]::FromUnixTimeMilliseconds($row[0].pane_created_at_ms + 1).ToString('o')
                }
            }
            Wait-Until -TimeoutSec 20 -Because 'an existing empty discovery completes after both launches before publishing fresh history' -Condition {
                $complete = @(Get-Content (Join-Path $script:evidence 'acp.log') | Where-Object {
                    $parts = $_ -split '\|'
                    $parts.Count -ge 3 -and $parts[1] -eq 'list-complete' -and
                        [DateTimeOffset]::Parse($parts[2]).ToUnixTimeMilliseconds() -gt $delegates[-1].CreatedBeforeMs
                })
                $complete.Count -gt 0
            } | Out-Null
            $stage = "$($script:historyPath).next"
            $unrelated = @{
                sessionId = [guid]::NewGuid().ToString(); title = 'controlled unrelated Recent sentinel'
                cwd = $script:evidence
                updatedAt = [DateTimeOffset]::FromUnixTimeMilliseconds($delegates[0].Creation.Time - 1000).ToString('o')
            }
            @{ sessions = @($unrelated, $rows[1], $rows[0]); pageSize = 1 } |
                ConvertTo-Json -Depth 8 | Set-Content $stage
            [IO.File]::Move($stage, $script:historyPath, $true)
            $matched = Wait-Until -TimeoutSec 20 -Because 'existing ACP discovery enriches ordered pending panes without control bindings' -Condition {
                $snapshot = Get-StatusRegistrySnapshot
                $found = @($snapshot.native_pane_observations | Where-Object inferred_agent_session_id -In $rows.sessionId)
                $historyRows = @($snapshot.sessions | Where-Object session_id -In $rows.sessionId)
                if ($found.Count -eq 2 -and $historyRows.Count -eq 2 -and
                    @($found | Where-Object status -NE Working).Count -eq 0) { $snapshot }
            }
            $matched | ConvertTo-Json -Depth 16 | Set-Content (Join-Path $script:evidence 'delegate-list-enrichment.json')
            @(Get-Content (Join-Path $script:evidence 'acp.log') | Where-Object { $_ -match '\|list-page\|[^|]+\|2\|1$' }).Count |
                Should -BeGreaterThan 0 -Because 'the inferred native sessions require the completed three-page inventory'
            for ($i = 0; $i -lt $delegates.Count; $i++) {
                $row = @($matched.native_pane_observations | Where-Object {
                    ([string]$_.pane_id).Trim('{}') -eq ([string]$delegates[$i].Pane.session_id).Trim('{}')
                })
                $row | Should -HaveCount 1
                $row[0].inferred_agent_session_id | Should -Be $rows[$i].sessionId
                $row[0].agent_session_id | Should -BeNullOrEmpty
                $row[0].pane_created_at_ms | Should -Be $delegates[$i].Creation.Time
                $row[0].pane_creation_sequence | Should -Be $delegates[$i].Creation.Sequence
                $row[0].status | Should -Be Working
                $sessions = @($matched.sessions | Where-Object session_id -EQ $rows[$i].sessionId)
                $sessions | Should -HaveCount 1
                $sessions[0].pane_session_id |
                    Should -BeNullOrEmpty -Because 'a guess never authorizes live control or SDK status routing'
                $paneStatuses = @($matched.pane_statuses | Where-Object {
                    ([string]$_.pane_id).Trim('{}') -eq ([string]$delegates[$i].Pane.session_id).Trim('{}')
                })
                $paneStatuses | Should -HaveCount 1
                $paneStatuses[0].agent_session_id | Should -BeNullOrEmpty
                $paneStatuses[0].inferred_history_key.session_id | Should -Be $rows[$i].sessionId
            }
            $scope = Invoke-WtCli -App $script:app -Arguments @('get-settings')
            $script:sidebarScopeToRestore = @{
                AgentsOnly = [bool]$scope.sidebarShowAgentsOnly
                Recent = [bool]$scope.sidebarShowRecentAgentSessions
            }
            Set-TestSidebarScope -App $script:app -AgentsOnly $true -Recent $true
            Wait-Until -TimeoutSec 15 -Because 'inferred delegate conversations are hidden from Recent while unrelated history remains visible' -Condition {
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
                $historyRows = @($list.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition) |
                    Where-Object {
                        -not $_.Current.IsOffscreen -and
                        @((Get-StatusRowParts $_) | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count -gt 0
                    })
                if (@($historyRows | Where-Object { $_.Current.Name.Contains($unrelated.title) }).Count -eq 1 -and
                    @($historyRows | Where-Object { $_.Current.Name.Contains('controlled inferred ') }).Count -eq 0) { $true }
            } | Should -BeTrue
        }
        finally {
            foreach ($fixture in $delegates) { Close-WtPane -App $script:app -SessionId $fixture.Pane.session_id }
        }
        Wait-Until -TimeoutSec 10 -Because 'actual pane retirement removes inferred live records' -Condition {
            $snapshot = Get-StatusRegistrySnapshot
            -not @($snapshot.native_pane_observations | Where-Object {
                ([string]$_.pane_id).Trim('{}') -in @($delegates.Pane.session_id | ForEach-Object { $_.Trim('{}') })
            }).Count
        } | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'closing delegate panes restores their conversations to Recent' -Condition {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
            $historyRows = @($list.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition) |
                Where-Object {
                    -not $_.Current.IsOffscreen -and
                    @((Get-StatusRowParts $_) | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count -gt 0
                })
            if (@($historyRows | Where-Object { $_.Current.Name.Contains('controlled inferred ') }).Count -eq 2) { $true }
        } | Should -BeTrue
    }
    It 'Sessions on without hooks suppresses native OSC and BEL fallback' {
        if (-not $script:hookIsolationVerified) {
            throw 'Actual-host automation environment inheritance is unverified; ON reconciliation is unsafe.'
        }
        foreach ($fixture in @($script:native, $script:otherNative)) {
            Send-StatusSignal $fixture -State 0
            Assert-Status $fixture Idle
        }
        $beforeModeChange = Get-StatusRegistrySnapshot
        $initialEpochs = @{}
        foreach ($fixture in @($script:native, $script:otherNative)) {
            $observation = @($beforeModeChange.native_pane_observations | Where-Object {
                ([string]$_.pane_id).Trim('{}') -eq ([string]$fixture.Pane.session_id).Trim('{}')
            })
            $observation | Should -HaveCount 1
            $initialEpochs[$fixture.Pane.session_id] = $observation[0].mode_epoch
        }
        $beforeReceipts = @{}
        foreach ($provider in @('copilot', 'claude', 'codex', 'gemini')) {
            $beforeReceipts[$provider] = @(Get-Content (Join-Path $script:evidence "$provider-wrapper.log")).Count
        }
        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtSetting -App $script:app -Key agentSessionManagementEnabled -Value $true | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'the product clears OFF metadata on the effective mode change' -Condition {
            $text = Get-StatusText $script:native
            $otherText = Get-StatusText $script:otherNative
            $statusPattern = '(?m)(^|[ ,·])(Active|Idle|Error|Paused|Attention requested|Waiting for input)($|[ ,·])'
            $text -and $otherText -and $text -notmatch $statusPattern -and $otherText -notmatch $statusPattern
        } | Out-Null
        $onSnapshot = Get-StatusRegistrySnapshot
        foreach ($fixture in @($script:native, $script:otherNative)) {
            @($onSnapshot.native_pane_observations | Where-Object {
                ([string]$_.pane_id).Trim('{}') -eq ([string]$fixture.Pane.session_id).Trim('{}')
            }) | Should -HaveCount 0 -Because 'loaded ON mode must clear master-owned OFF observations'
        }
        $onSnapshot | ConvertTo-Json -Depth 16 | Set-Content (Join-Path $script:evidence 'registry-sessions-on.json')
        Wait-Until -TimeoutSec 30 -Because 'ALL-provider reconciliation invokes only failing fixture CLIs' -Condition {
            $complete = $true
            foreach ($provider in $beforeReceipts.Keys) {
                if (@(Get-Content (Join-Path $script:evidence "$provider-wrapper.log")).Count -le $beforeReceipts[$provider]) {
                    $complete = $false
                }
            }
            $complete
        } | Out-Null
        $hookLog = Get-ItLogText -App $script:app -Name 'wta-install-hooks*.log' -SinceStart
        $hookLog | Should -Not -BeNullOrEmpty -Because 'capture real WTA reconciliation diagnostics alongside wrapper receipts'
        $hookLog | Set-Content (Join-Path $script:evidence 'hook-reconciliation.txt')
        Assert-NoAgentStatus $script:otherNative
        foreach ($state in @(1, 2, 3, 4, 0)) {
            Send-StatusSignal $script:native -State $state
            Assert-NoAgentStatus $script:native
        }
        Send-StatusSignal $script:native -Kind bell
        Assert-NoAgentStatus $script:native
        Set-WtSetting -App $script:app -Key agentSessionManagementEnabled -Value $false | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'the product reloads Sessions OFF before fresh output' -Condition {
            $text = Get-StatusText $script:native
            $otherText = Get-StatusText $script:otherNative
            $idlePattern = '(?m)(^|[ ,·])Idle($|[ ,·])'
            $text -match $idlePattern -and $otherText -match $idlePattern
        } | Out-Null
        foreach ($fixture in @($script:native, $script:otherNative)) {
            Assert-RegistryObservation $fixture Idle
        }
        $afterModeChange = Get-StatusRegistrySnapshot
        foreach ($fixture in @($script:native, $script:otherNative)) {
            $observation = @($afterModeChange.native_pane_observations | Where-Object {
                ([string]$_.pane_id).Trim('{}') -eq ([string]$fixture.Pane.session_id).Trim('{}')
            })
            $observation | Should -HaveCount 1
            $observation[0].mode_epoch | Should -BeGreaterThan $initialEpochs[$fixture.Pane.session_id]
        }
        Send-StatusSignal $script:native -State 1
        Assert-Status $script:native Active
        Send-StatusSignal $script:native -State 0
        Assert-Status $script:native Idle
        Assert-NoFabricatedBinding $script:native
    }
    It 'Known native resume shares canonical status across tabs Recent and session list' {
        $seedRows = @($script:copilotSeed.ExpectedRows | Sort-Object title)
        $sid = $seedRows[0].sessionId
        $otherSid = $seedRows[1].sessionId
        # The resumed native tab deliberately takes the unrelated history title.
        # Exact conversation identity, not the displayed title, must deduplicate it.
        $title = $seedRows[1].title
        $selectedTitle = $seedRows[0].title
        $launchPath = Join-Path $script:evidence 'known-native-launch.json'
        $signal = Join-Path $script:evidence 'known-native.signal.json'
        $receipt = Join-Path $script:evidence 'known-native.receipt.txt'
        $otherCwd = Split-Path $seedRows[1].cwd -Leaf
        $dispatcher = Join-Path $script:evidence 'Dispatch-CopilotFixture.ps1'
        $dispatchCode = @'
$ErrorActionPreference = 'Stop'
$cli = @($args)
if ($cli.Count -eq 2 -and $cli[0] -ceq '--resume' -and $cli[1] -ceq '__SID__' -and $env:WT_SESSION) {
    @{ args = $cli; session_id = $cli[1]; pane_session_id = $env:WT_SESSION; pid = $PID; provider = 'copilot'; location = 'Host' } |
        ConvertTo-Json -Compress | Set-Content '__LAUNCH__'
    & '__EMITTER__' -Signal '__SIGNAL__' -Receipt '__RECEIPT__' -Title '__TITLE__'
    exit $LASTEXITCODE
}
[Console]::Error.WriteLine('Fixture rejects unsupported Copilot arguments')
exit 1
'@
        foreach ($replacement in @{
            '__SID__' = $sid
            '__LAUNCH__' = $launchPath; '__EMITTER__' = $script:fixture; '__SIGNAL__' = $signal
            '__RECEIPT__' = $receipt; '__TITLE__' = $title
        }.GetEnumerator()) {
            $dispatchCode = $dispatchCode.Replace($replacement.Key, $replacement.Value.Replace("'", "''"))
        }
        $dispatchCode | Set-Content $dispatcher
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $launcherSource = @'
using System;
using System.Diagnostics;
class CopilotFixture {
    static int Main(string[] args) {
        bool resume = args.Length == 2 && args[0] == "--resume" && args[1] == "__SID__";
        if (!resume) return 3;
        var start = new ProcessStartInfo(@"__PWSH__");
        start.UseShellExecute = false;
        start.Arguments = "-NoProfile -File \"__DISPATCH__\" " + args[0] + " " + args[1];
        start.EnvironmentVariables["ITE2E_COPILOT_LAUNCHER_PID"] = Process.GetCurrentProcess().Id.ToString();
        using (var child = Process.Start(start)) {
            if (!child.WaitForExit(330000)) {
                child.CloseMainWindow();
                if (!child.WaitForExit(5000)) child.Kill();
                if (!child.WaitForExit(5000)) return 124;
                return 124;
            }
            return child.ExitCode;
        }
    }
}
'@
        $launcherSource = $launcherSource.Replace('__SID__', $sid).Replace('__PWSH__', $pwsh.Replace('"', '""')).
            Replace('__DISPATCH__', $dispatcher.Replace('\', '\\').Replace('"', '\"'))
        $sourcePath = Join-Path $script:evidence 'CopilotFixture.cs'
        $launcherSource | Set-Content $sourcePath
        $ownedExe = Join-Path $script:wrapperDir 'copilot.exe'
        (Get-FileHash $ownedExe).Hash | Should -Be $script:blockerHash
        @(Get-CimInstance Win32_Process -Filter "Name='copilot.exe'" |
            Where-Object ExecutablePath -eq $ownedExe) | Should -HaveCount 0
        $compiledExe = Join-Path $script:evidence 'Known-Copilot.exe'
        if (Test-Path $compiledExe) { throw 'Owned compiler output already exists.' }
        $compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
        (Invoke-Native -FilePath $compiler -Arguments @('/nologo', '/target:exe', "/out:$compiledExe", $sourcePath) -TimeoutSec 20).ExitCode |
            Should -Be 0
        [IO.File]::Move($compiledExe, $ownedExe, $true)
        (Invoke-Native -FilePath $ownedExe -Arguments @('--resume', $otherSid) -TimeoutSec 5).ExitCode | Should -Be 3
        (Invoke-Native -FilePath $ownedExe -Arguments @('--version') -TimeoutSec 5).ExitCode | Should -Be 3
        (Get-Command copilot.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source |
            Should -Be $ownedExe
        $resolver = Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 10
        @($resolver.clis | Where-Object name -eq copilot)[0].binary_path | Should -Be $ownedExe
        @{ path = $ownedExe; sha256 = (Get-FileHash $ownedExe).Hash; wrong_resume_exit = 3; unsupported_exit = 3 } |
            ConvertTo-Json | Set-Content (Join-Path $script:evidence 'known-owned-executable.json')
        Set-WtSetting -App $script:app -Key acpModel -Value '' | Out-Null
        Set-WtSetting -App $script:app -Key acpAgent -Value copilot | Out-Null
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 30 | Should -BeTrue
        $knownRows = Wait-Until -TimeoutSec 20 -Because 'real Copilot/Host ACP history contains both exact fixture IDs' -Condition {
            $snapshot = Get-StatusRegistrySnapshot
            $rows = @($snapshot.sessions | Where-Object { $_.session_id -in @($sid, $otherSid) })
            if ($rows.Count -eq 2 -and @($rows | Where-Object { $_.provider_id -ne 'copilot' -or $_.location -ne 'Host' }).Count -eq 0) { $rows }
        }
        $otherBefore = @($knownRows | Where-Object session_id -eq $otherSid)[0]
        $pipe = (Get-Content (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
        $masters = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object {
            $_.ExecutablePath -eq $script:target.WtaPath -and
            $_.CommandLine -match '\s--master\s' -and $_.CommandLine.Contains($pipe)
        })
        $masters | Should -HaveCount 1
        $masters[0].ParentProcessId | Should -Be $script:app.Pid
        (Get-FileHash -LiteralPath $masters[0].ExecutablePath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $realChildren = @(Get-CimInstance Win32_Process -Filter "Name='copilot.exe'" | Where-Object {
            $_.ParentProcessId -eq $masters[0].ProcessId -and $_.CommandLine -match '\s--acp\b' -and
            $_.ExecutablePath -ne $ownedExe -and
            (Get-FileHash -LiteralPath $_.ExecutablePath).Hash -eq $script:copilotSeed.SHA256
        })
        $realChildren | Should -Not -BeNullOrEmpty -Because 'actual Copilot ACP, not the native-only emitter, must own the seeded history; pooled chat and isolated history queries may coexist'
        @{
            actual_acp_children = @($realChildren | Select-Object ProcessId, ExecutablePath, CreationDate, CommandLine)
            child_sha256 = $script:copilotSeed.SHA256
            parent_pid = $masters[0].ProcessId; parent_path = $masters[0].ExecutablePath
            parent_start = $masters[0].CreationDate; pipe = $pipe
            seeded_home = $script:isolatedEnvironment.COPILOT_HOME
            provider = 'copilot'; location = 'Host'; verified_history = $knownRows
        } | ConvertTo-Json -Depth 12 | Set-Content (Join-Path $script:evidence 'known-real-acp-launch.json')
        $rpc = [IO.Pipes.NamedPipeClientStream]::new('.', ($pipe -replace '^\\\\\.\\pipe\\', ''), [IO.Pipes.PipeDirection]::InOut)
        $reader = $null
        $writer = $null
        try {
            $rpc.Connect(5000)
            $reader = [IO.StreamReader]::new($rpc)
            $writer = [IO.StreamWriter]::new($rpc)
            $writer.AutoFlush = $true
            foreach ($request in @(
                @{ jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{
                    protocolVersion = 1; clientCapabilities = @{}; clientInfo = @{ name = 'known-status-test'; version = '1' }
                    _meta = @{ wta = @{ control_client = 'sidebar-history-v1' } }
                } },
                @{ jsonrpc = '2.0'; id = 2; method = '_intellterm.wta/session/activate'; params = @{
                    identity = @{ session_id = $sid; history_key = @{ session_id = $sid; provider_id = 'copilot'; location = 'Host' } }
                    window_id = [long]$script:window; activation_id = [guid]::NewGuid().ToString()
                } }
            )) {
                $writer.WriteLine(($request | ConvertTo-Json -Depth 10 -Compress))
                $clock = [Diagnostics.Stopwatch]::StartNew()
                do {
                    $read = $reader.ReadLineAsync()
                    if (-not $read.Wait(10000)) { throw 'Known resume RPC timed out.' }
                    $response = $read.GetAwaiter().GetResult() | ConvertFrom-Json -ErrorAction Stop
                    if ($clock.Elapsed.TotalSeconds -gt 15) { throw 'Known resume response deadline exceeded.' }
                } while ($response.id -ne $request.id)
                if ($response.error) { throw ($response.error | ConvertTo-Json -Depth 8 -Compress) }
            }
            $response.result.accepted | Should -BeTrue
        }
        finally {
            if ($writer) { $writer.Dispose() }
            if ($reader) { $reader.Dispose() }
            $rpc.Dispose()
        }
        Wait-Until -TimeoutSec 15 -Because 'the native CLI receives the exact resume ID in a real Terminal connection' -Condition {
            Test-Path $launchPath
        } | Out-Null
        $launch = Get-Content $launchPath -Raw | ConvertFrom-Json
        $launch.session_id | Should -BeExactly $sid
        ($launch.args -join ' ') | Should -BeExactly "--resume $sid"
        $launch.pane_session_id | Should -Not -BeNullOrEmpty
        $context = Invoke-WtCli -App $script:app -Arguments @('get-pane-context', '--target', $launch.pane_session_id)
        [string]$context.pane.window_id | Should -Be $script:window
        $native = [pscustomobject]@{
            Pane = $context.pane; Title = $title; StatusTitle = $selectedTitle
            Signal = $signal; Receipt = $receipt; Kind = 'native'
        }
        $script:fixtures += $native
        foreach ($step in @(@{ State = 1; Status = 'Working'; Text = 'Active' }, @{ State = 0; Status = 'Idle'; Text = 'Idle' },
            @{ State = -1; Status = 'Attention'; Text = 'Waiting for input' })) {
            if ($step.State -eq -1) { Send-StatusSignal $native -Kind bell } else { Send-StatusSignal $native -State $step.State }
            $current = Wait-Until -TimeoutSec 10 -Because 'one canonical Copilot/Host binding and native observation share status' -Condition {
                $snapshot = Get-StatusRegistrySnapshot
                $rows = @($snapshot.sessions | Where-Object session_id -eq $sid)
                $observations = @($snapshot.native_pane_observations | Where-Object {
                    ([string]$_.pane_id).Trim('{}') -eq ([string]$launch.pane_session_id).Trim('{}')
                })
                $paneStatuses = @($snapshot.pane_statuses | Where-Object {
                    ([string]$_.pane_id).Trim('{}') -eq ([string]$launch.pane_session_id).Trim('{}')
                })
                if ($rows.Count -eq 1 -and $rows[0].status -eq $step.Status -and
                    ([string]$rows[0].pane_session_id).Trim('{}') -eq ([string]$launch.pane_session_id).Trim('{}') -and
                    $observations.Count -eq 1 -and $observations[0].agent_session_id -eq $sid -and
                    $observations[0].status -eq $step.Status -and $paneStatuses.Count -eq 1 -and
                    $paneStatuses[0].agent_session_id -eq $sid -and $paneStatuses[0].status -eq $step.Status) { $snapshot }
            }
            $bound = @($current.sessions | Where-Object session_id -eq $sid)[0]
            $bound.provider_id | Should -Be copilot
            $bound.location | Should -Be Host
            $bound.title | Should -BeExactly $selectedTitle
            $nativeObservation = @($current.native_pane_observations | Where-Object agent_session_id -eq $sid)
            $nativeObservation | Should -HaveCount 1
            $nativeObservation[0].provider_id | Should -Be copilot
            $nativeObservation[0].location | Should -Be Host
            [string]$nativeObservation[0].window_id | Should -Be $script:window
            $other = @($current.sessions | Where-Object session_id -eq $otherSid)
            $other | Should -HaveCount 1
            $other[0].pane_session_id | Should -BeNullOrEmpty
            $other[0].status | Should -Be $otherBefore.status
            $current | ConvertTo-Json -Depth 16 | Set-Content (Join-Path $script:evidence "known-$($step.Status)-registry.json")
            Wait-Until -TimeoutSec 10 -Because 'the bound tab renders the same canonical status' -Condition {
                $text = Get-StatusText $native
                if ($text -match [regex]::Escape($step.Text)) { $text }
            } | Set-Content (Join-Path $script:evidence "known-$($step.Status)-tab.txt")
            $hookEvents = switch ($step.Status) {
                Working { @('agent.stop') }
                Idle { @('agent.tool.starting', 'agent.notification') }
                default { @() }
            }
            foreach ($hookEvent in $hookEvents) {
                $hook = @{
                    type = 'event'; method = 'agent_event'; params = @{
                        event = $hookEvent; pane_id = $launch.pane_session_id
                        agent_session_id = $sid; cli_source = 'copilot'; agent = 'copilot'
                        payload = @{ tool_name = 'auto:smoke:stale-hook'; message = 'auto:smoke:stale-hook' }
                    }
                } | ConvertTo-Json -Depth 8 -Compress
                Invoke-WtCli -App $script:app -Arguments @('publish', '--existing-only', $hook) | Out-Null
                Wait-Until -TimeoutSec 5 -Because 'the real master processes this exact leftover COM hook' -Condition {
                    $log = Get-ItLogText -App $script:app -Name 'wta-main_master*.log' -SinceStart
                    @($log -split "`n" | Where-Object {
                        $_.Contains('processed COM agent hook') -and $_.Contains($sid) -and $_.Contains($hookEvent)
                    }).Count -gt 0
                } | Out-Null
                $deadline = [datetime]::UtcNow.AddSeconds(2)
                do {
                    $protected = Get-StatusRegistrySnapshot
                    @($protected.sessions | Where-Object session_id -EQ $sid)[0].status | Should -Be $step.Status
                    @($protected.native_pane_observations | Where-Object agent_session_id -EQ $sid)[0].status |
                        Should -Be $step.Status -Because 'Sessions OFF ignores unqualified COM activity, not qualified watcher evidence'
                    Start-Sleep -Milliseconds 100
                } while ([datetime]::UtcNow -lt $deadline)
            }
        }
        Set-TestSidebarScope -App $script:app -AgentsOnly $true -Recent $true
        $recent = Wait-Until -TimeoutSec 10 -Because 'only the unrelated same-title history remains in Recent' -Condition {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
            $visibleRows = @($list.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition) |
                Where-Object { -not $_.Current.IsOffscreen })
            $historyRows = @($visibleRows | Where-Object {
                @((Get-StatusRowParts $_) | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count -gt 0 -and
                    $_.Current.Name.Contains($title)
            })
            $selectedHistoryRows = @($visibleRows | Where-Object {
                @((Get-StatusRowParts $_) | Where-Object { $_.Current.AutomationId -eq 'HistoryProviderIcon' }).Count -gt 0 -and
                    $_.Current.Name.Contains($selectedTitle)
            })
            $liveRows = @($visibleRows | Where-Object {
                @((Get-StatusRowParts $_) | Where-Object { $_.Current.AutomationId -in @('HistoryProviderIcon', 'HistoryHeaderButton') }).Count -eq 0 -and
                    $_.Current.Name.Contains($selectedTitle)
            })
            $rows = @($liveRows) + @($historyRows)
            if ($rows.Count -eq 2 -and $liveRows.Count -eq 1 -and $historyRows.Count -eq 1 -and $selectedHistoryRows.Count -eq 0) {
                $parts = @($historyRows[0].FindAll([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name })
                if (($parts -join ' ') -match [regex]::Escape($otherCwd)) {
                    @($rows | ForEach-Object { $_.Current.Name }); $parts
                }
            }
        }
        $recent | Set-Content (Join-Path $script:evidence 'known-recent-dedup.txt')
        $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
        $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
        $list | Should -Not -BeNullOrEmpty
        $selectedHistory = @($list.FindAll([Windows.Automation.TreeScope]::Children,
            [Windows.Automation.Condition]::TrueCondition) | Where-Object {
            -not $_.Current.IsOffscreen -and $_.Current.Name.Contains($selectedTitle) -and
            $_.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'HistoryProviderIcon'))
        })
        $selectedHistory | Should -HaveCount 0 -Because 'the resumed exact identity must disappear from Recent despite its changed native title'
        Open-SessionList -App $script:app -TimeoutSec 15 | Out-Null
        $helper = Get-AgentPaneSession -App $script:app
        Wait-Until -TimeoutSec 10 -Because 'classic sessions renders both real identities with one attention badge' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $helper.PaneSessionId -MaxLines 100
            $selectedRows = @($text -split "`n" | Where-Object { $_.Contains($selectedTitle) })
            $otherRows = @($text -split "`n" | Where-Object { $_.Contains($title) })
            if ($selectedRows.Count -eq 1 -and $selectedRows[0] -match 'Waiting for input' -and
                $otherRows.Count -eq 1 -and $otherRows[0] -notmatch 'Waiting for input') { $text }
        } | Set-Content (Join-Path $script:evidence 'known-classic-sessions.txt')
        $final = Get-StatusRegistrySnapshot
        @($final.sessions | Where-Object { $_.session_id -eq $sid -and $_.status -eq 'Attention' -and $_.provider_id -eq 'copilot' -and $_.location -eq 'Host' }) |
            Should -HaveCount 1
    }

    It 'Full trust Settings remains usable during native agent metadata refresh' {
        Assert-StatusDeadline
        $settingsHash = (Get-FileHash $script:app.SettingsPath).Hash
        $beforeTabs = @(Get-WtTabs -App $script:app -WindowId $script:window | ForEach-Object tab_id)
        $settingsTab = $null
        try {
            Send-WtWindowKey -App $script:app -Vk 0xBC -Ctrl -RequireForeground | Out-Null
            Open-WtSettings -App $script:app -TimeoutSec 15 | Out-Null
            $added = @(Get-WtTabs -App $script:app -WindowId $script:window |
                Where-Object { $_.tab_id -notin $beforeTabs })
            $added | Should -HaveCount 1 -Because 'only the newly opened Settings tab may be closed'
            $settingsTab = $added[0].tab_id
            Invoke-SettingsNav -App $script:app -NavItem AIAgentsNavItem | Out-Null
            foreach ($selector in @('AcpAgent', 'ShowTokenUsageAndCostToggle')) {
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $root.Current.ProcessId | Should -Be $script:app.Pid
                $peer = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $selector))
                $peer | Should -Not -BeNullOrEmpty
                if ($peer.Current.IsOffscreen) {
                    $pattern = $null
                    if ($peer.TryGetCurrentPattern([Windows.Automation.ScrollItemPattern]::Pattern, [ref]$pattern)) {
                        ([Windows.Automation.ScrollItemPattern]$pattern).ScrollIntoView()
                    }
                    else {
                        $ancestor = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($peer)
                        while ($ancestor -and $ancestor.Current.ProcessId -eq $script:app.Pid) {
                            if ($ancestor.TryGetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern, [ref]$pattern)) { break }
                            $ancestor = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($ancestor)
                        }
                        $pattern | Should -Not -BeNullOrEmpty -Because 'the Settings control must have a supported scroll owner'
                        $scroll = [Windows.Automation.ScrollPattern]$pattern
                        for ($step = 0; $step -lt 12 -and $peer.Current.IsOffscreen; $step++) {
                            $amount = if ($peer.Current.BoundingRectangle.Top -lt $ancestor.Current.BoundingRectangle.Top) {
                                [Windows.Automation.ScrollAmount]::SmallDecrement
                            } else { [Windows.Automation.ScrollAmount]::SmallIncrement }
                            $scroll.Scroll([Windows.Automation.ScrollAmount]::NoAmount, $amount)
                            Start-Sleep -Milliseconds 100
                        }
                    }
                    Wait-Until -TimeoutSec 3 -Because "owned Settings control $selector is scrolled into view" -Condition {
                        -not $peer.Current.IsOffscreen
                    } | Out-Null
                }
                $element = Get-UiElement -App $script:app -Selector $selector
                $element | Should -Not -BeNullOrEmpty
                $element.isOffscreen | Should -BeFalse
                $element.isEnabled | Should -BeTrue
            }
            $toggle = $peer.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
            $originalToggleState = $toggle.Current.ToggleState
            Invoke-UiElement -App $script:app -Selector ShowTokenUsageAndCostToggle -TimeoutSec 5 | Out-Null
            Wait-Until -TimeoutSec 3 -Because 'the visible Settings toggle responds to the real UI action' -Condition {
                $toggle.Current.ToggleState -ne $originalToggleState
            } | Out-Null
            Get-UiElement -App $script:app -Selector ShowTokenUsageAndCostToggle |
                ConvertTo-Json -Depth 8 | Set-Content (Join-Path $script:evidence 'fulltrust-settings-toggled.json')
            Send-StatusSignal $script:native -State 1
            Assert-Status $script:native Active
            Send-StatusSignal $script:native -Kind bell
            Assert-Status $script:native 'Waiting for input'
            Invoke-UiElement -App $script:app -Selector ShowTokenUsageAndCostToggle -TimeoutSec 5 | Out-Null
            Wait-Until -TimeoutSec 3 -Because 'the real UI action restores the original Settings toggle state' -Condition {
                $toggle.Current.ToggleState -eq $originalToggleState
            } | Out-Null
            Get-UiElement -App $script:app -Selector ShowTokenUsageAndCostToggle |
                ConvertTo-Json -Depth 8 | Set-Content (Join-Path $script:evidence 'fulltrust-settings-toggle-restored.json')
            Invoke-SettingsNav -App $script:app -NavItem AppearanceNavItem | Out-Null
            Invoke-SettingsNav -App $script:app -NavItem AIAgentsNavItem | Out-Null
            Test-UiElementExists -App $script:app -Selector AcpAgent -TimeoutSec 5 | Should -BeTrue
            $script:app.OwnedProcess.HasExited | Should -BeFalse
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $errors = @($root.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.Condition]::TrueCondition) | Where-Object {
                -not $_.Current.IsOffscreen -and
                ($_.Current.ClassName -match 'ContentDialog' -or
                    ($_.Current.ControlType -eq [Windows.Automation.ControlType]::Window -and $_.Current.Name -eq 'Error'))
            })
            $errors | Should -HaveCount 0 -Because 'Settings must not display an error dialog'
            (Get-FileHash $script:app.SettingsPath).Hash | Should -Be $settingsHash -Because 'navigation must not save settings'
        }
        finally {
            Get-UiTree -App $script:app -Depth 22 |
                Set-Content (Join-Path $script:evidence 'fulltrust-settings-ui.txt')
            if ($settingsTab -and -not $script:app.OwnedProcess.HasExited -and
                (Test-UiElementExists -App $script:app -Selector SettingsNav -TimeoutSec 1)) {
                Send-WtWindowKey -App $script:app -Vk 0x57 -Ctrl -Shift -RequireForeground | Out-Null
                Wait-Until -TimeoutSec 5 -Because 'only the new Settings tab closes' -Condition {
                    $tabs = @(Get-WtTabs -App $script:app -WindowId $script:window | ForEach-Object tab_id)
                    $tabs.Count -eq $beforeTabs.Count -and -not @($beforeTabs | Where-Object { $_ -notin $tabs }).Count
                } | Out-Null
            }
        }
        Assert-Status $script:native 'Waiting for input'
    }
}
