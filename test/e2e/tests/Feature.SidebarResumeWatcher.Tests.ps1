#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: Sidebar resume watcher' -Tag @('Feature', 'SidebarResumeWatcher') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:package = Get-ItTestPackage
        if ($script:package -cnotmatch '^IntelligentTerminal\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20}_rd9vj3e6a2mbr$') {
            throw 'Watcher validation requires one explicitly selected private worktree PFN, not regular Dev/Store.'
        }
        $script:target = Resolve-ItApp -Package $script:package
        Assert-WtPackageInactive -App $script:target
        foreach ($name in @('ITE2E_EXPECTED_APP_SHA256', 'ITE2E_EXPECTED_WTA_SHA256', 'ITE2E_SOURCE_COMMIT')) {
            if (-not [Environment]::GetEnvironmentVariable($name)) { throw "Supply exact build evidence: $name" }
        }
        $script:head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        $env:ITE2E_SOURCE_COMMIT | Should -Be $script:head
        $script:wtaRevision = if ($env:ITE2E_WTA_SOURCE_COMMIT) { $env:ITE2E_WTA_SOURCE_COMMIT } else { $script:head }
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:app = $null
        $script:originalEnvironment = @{}
        $script:globalHashes = @{}
        foreach ($path in @((Join-Path $env:USERPROFILE '.copilot\settings.json'))) {
            $script:globalHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash $path).Hash } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('resume-watcher-' + [guid]::NewGuid().ToString('N'))
        $script:sandboxHome = Join-Path $script:evidence 'home'
        $script:bin = Join-Path $script:evidence 'bin'
        New-Item -ItemType Directory -Path $script:sandboxHome, $script:bin -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $script:sandboxHome 'AppData\Local\Packages'),
            (Join-Path $script:sandboxHome 'AppData\Roaming'),
            (Join-Path $script:sandboxHome 'Documents') -Force | Out-Null
        New-Item -ItemType Junction -Path (Join-Path $script:sandboxHome "AppData\Local\Packages\$($script:target.Package)") `
            -Target (Split-Path $script:target.LocalStateDir -Parent) | Out-Null
        $script:sid = [guid]::NewGuid().ToString()
        $script:folder = Join-Path $script:evidence ('watcher-' + $script:sid)
        New-Item -ItemType Directory -Path $script:folder -Force | Out-Null
        $script:title = Split-Path $script:folder -Leaf
        $script:launchLog = Join-Path $script:folder 'launch.jsonl'
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-WatcherResumeCli.ps1')).Path
        $config = @{
            ITE2E_SHIM_PWSH = $pwsh; ITE2E_SHIM_FIXTURE = $fixture
            ITE2E_SHIM_LOG = $script:launchLog; ITE2E_SHIM_RUN = $script:sid
            ITE2E_SHIM_WTCLI = $script:target.WtcliPath; ITE2E_SHIM_RESUME_SESSION = $script:sid
        }
        $header = Join-Path $script:bin 'config.h'
        @($config.Keys | ForEach-Object { "#define $_ LR`"ite2e($($config[$_]))ite2e`"" }) |
            Set-Content -LiteralPath $header -Encoding utf8BOM
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $vs = Invoke-Native -FilePath $vswhere -Arguments @('-latest', '-products', '*',
            '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath')
        $vs.ExitCode | Should -Be 0
        $vcvars = Join-Path $vs.StdOut.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
        $source = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-CopilotDelegate.cpp')).Path
        $builtShim = Join-Path $script:bin 'copilot.exe'
        $build = "call `"$vcvars`" >nul && cl /nologo /utf-8 /EHsc /std:c++17 /FI`"$header`" `"$source`" /Fe:`"$builtShim`" /Fo:`"$script:bin\copilot.obj`" /link /INCREMENTAL:NO"
        $output = & $env:ComSpec /d /c $build 2>&1
        if ($LASTEXITCODE -ne 0) { throw "Native fixture build failed: $output" }
        $script:shim = Join-Path $script:target.InstallLocation 'copilot.exe'
        if (Test-Path -LiteralPath $script:shim) { throw 'Never replace an existing native executable.' }
        # CreateProcess searches the host executable's directory before PATH.
        # This new test-owned file affects only this private package's launches.
        Assert-WtPackageInactive -App $script:target
        [IO.File]::Copy($builtShim, $script:shim, $false)
        $script:shimHash = (Get-FileHash $script:shim).Hash
        $script:shimCreated = $true
        $profilePath = "$script:bin;$($script:target.InstallLocation);$(Split-Path $pwsh -Parent);" +
            "$env:SystemRoot\System32;$env:SystemRoot;$env:LOCALAPPDATA\Microsoft\WindowsApps"
        $environment = @{
            USERPROFILE = $script:sandboxHome; HOME = $script:sandboxHome; COPILOT_HOME = (Join-Path $script:sandboxHome '.copilot')
            CLAUDE_CONFIG_DIR = (Join-Path $script:sandboxHome '.claude'); CODEX_HOME = (Join-Path $script:sandboxHome '.codex')
            PATH = $profilePath; WTCLI_PATH = $script:target.WtcliPath; WT_WTCLI_PATH = $script:target.WtcliPath
        }
        foreach ($key in $environment.Keys) {
            $script:originalEnvironment[$key] = [Environment]::GetEnvironmentVariable($key)
            [Environment]::SetEnvironmentVariable($key, $environment[$key])
        }
        New-Item -ItemType Directory -Path (Join-Path $script:sandboxHome '.copilot\session-state') -Force | Out-Null
        $profileId = '{907421A8-B17A-469F-8837-9FC25EE81151}'
        $script:app = Start-Terminal -Package $script:package -TimeoutSec 90 -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; tabLayoutVerticalWidth = 420
            sidebarShowRecentAgentSessions = $true
            acpAgent = 'copilot'; autoFixEnabled = $false
            startupActions = ''; firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            defaultProfile = $profileId; 'warning.confirmOnClose' = 'never'; actions = @()
            profiles = @{ defaults = @{ environment = $environment; agentPaneBackend = '' }; list = @(@{
                guid = $profileId; name = 'Watcher E2E'; commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                startingDirectory = $script:evidence; agentPaneBackend = ''
            }) }
        }
        $script:app.Launched | Should -BeTrue
        $script:app | Add-Member RequireOwnedForeground $true -Force
        $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
        $settingsError = [Windows.Automation.PropertyCondition]::new(
            [Windows.Automation.AutomationElement]::NameProperty, 'Failed to reload settings')
        $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $settingsError) |
            Should -BeNullOrEmpty -Because 'a settings-error fallback is not a valid test configuration'
        Get-WtSettingsObject -App $script:app | ConvertTo-Json -Depth 32 |
            Set-Content (Join-Path $script:evidence 'settings-after-launch.json')
        Invoke-WtCli -App $script:app -Arguments @('get-settings') | ConvertTo-Json -Depth 32 |
            Set-Content (Join-Path $script:evidence 'settings-product.json')
        $pipePath = Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt'
        $script:pipe = Wait-Until -TimeoutSec 60 -Because 'the owned master publishes its pipe independently of ACP readiness' -Condition {
            if (Test-Path $pipePath) {
                $name = (Get-Content $pipePath -Raw).Trim()
                if ($name) { $name }
            }
        }
        $masters = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object {
            $_.ExecutablePath -eq $script:app.WtaPath -and
                $_.CommandLine.Contains('--master') -and $_.CommandLine.Contains($script:pipe)
        })
        $masters | Should -HaveCount 1
        $loadedApp = @((Get-Process -Id $script:app.Pid).Modules |
            Where-Object ModuleName -EQ TerminalApp.dll)
        $loadedApp | Should -HaveCount 1
        $loadedApp[0].FileName | Should -Be (Join-Path $script:app.InstallLocation 'TerminalApp.dll')
        $script:shell = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId |
            Where-Object { -not $_.is_agent_pane })[0].session_id
        @{
            host_source_commit = $script:head; wta_source_commit = $script:wtaRevision
            package = $script:app.Package; pid = $script:app.Pid
            hwnd = $script:app.Hwnd; install_location = $script:app.InstallLocation
            app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256; wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256
            isolated_home = $script:sandboxHome; shim = $script:shim
            master_pid = $masters[0].ProcessId; master_path = $masters[0].ExecutablePath
            loaded_app_path = $loadedApp[0].FileName
        } | ConvertTo-Json | Set-Content (Join-Path $script:evidence 'build-proof.json')

        function Read-WatcherSessions {
            $snapshot = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $script:pipe, '--origin', 'all', '--json', '--include-status')
            @($snapshot.sessions)
        }
        function Get-WatcherElement {
            param([string]$Id)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        }
        function Get-WatcherRow {
            $list = Get-WatcherElement ItemsList
            if (-not $list -or $list.Current.IsOffscreen) { return }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            @($list.FindAll([Windows.Automation.TreeScope]::Children, $condition) | Where-Object {
                -not $_.Current.IsOffscreen -and $_.Current.Name.Contains($script:title)
            })
        }
        function Send-WatcherHook {
            param([string]$Event, [string]$Pane = $script:shell)
            $json = @{ session_id = $script:sid; cwd = $script:folder; message = 'Waiting for input' } | ConvertTo-Json -Compress
            $code = "'$($json.Replace("'", "''"))' | & '$($script:app.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event $Event; exit `$LASTEXITCODE"
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
            $result = Invoke-Native -FilePath $pwsh -Arguments @('-NoProfile', '-EncodedCommand', $encoded) `
                -Environment @{ WT_SESSION = $Pane; WT_COM_CLSID = $script:app.ComClsid } -TimeoutSec 15
            if ($result.ExitCode -ne 0) { throw "Native hook failed: $($result.StdErr)" }
        }
        function Wait-WatcherStatus {
            param([string]$Status, [string]$Pane, [switch]$Rendered)
            $row = Wait-Until -TimeoutSec 20 -IntervalSec 0.25 -Because "authoritative resumed session reaches $Status" -Condition {
                $rows = @(Read-WatcherSessions | Where-Object session_id -EQ $script:sid)
                if ($rows.Count -eq 1 -and $rows[0].status -eq $Status) { $rows[0] }
            }
            if ($Pane) { $row.pane_session_id.Trim('{}') | Should -Be $Pane.Trim('{}') }
            if ($Rendered) {
                $label = switch ($Status) {
                    Working { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusWorking }
                    Attention { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusAttention }
                    Idle { Get-WtReswTextRegex -Key VerticalTabsHistoryStatusIdle }
                }
                $label | Should -Not -BeNullOrEmpty
                Wait-Until -TimeoutSec 15 -Because "the live Sidebar row renders $Status" -Condition {
                    $peers = @(Get-WatcherRow)
                    $peers.Count -eq 1 -and $peers[0].Current.Name -match $label
                } | Out-Null
                $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
                $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $settingsError) |
                    Should -BeNullOrEmpty -Because 'visible state must not be accepted through a settings-error fallback'
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "$script:generation-$Status.png") | Out-Null
            }
            $row | ConvertTo-Json -Depth 12 |
                Set-Content (Join-Path $script:evidence "$script:generation-$Status.json")
            $row
        }
        function Resume-WatcherHistory {
            param([int]$Generation)
            $script:generation = $Generation
            Invoke-UiElement -App $script:app -Selector FilterTabsButton | Out-Null
            $item = Wait-Until -TimeoutSec 10 -Condition {
                $element = Get-WatcherElement RecentAgentSessionsFilterMenuItem
                if ($element -and -not $element.Current.IsOffscreen) { $element }
            }
            $toggle = $item.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern)
            if ($toggle.Current.ToggleState -eq [Windows.Automation.ToggleState]::Off) { $toggle.Toggle() }
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
            $peer = Wait-Until -TimeoutSec 20 -Because 'the exact ended native session is visible in Sidebar History' -Condition {
                $rows = @(Get-WatcherRow)
                if ($rows.Count -eq 1) { $rows[0] }
            }
            $peer.Current.ProcessId | Should -Be $script:app.Pid
            Set-WtWindowForeground -App $script:app -Attempts 3 -DelayMs 150 | Should -BeTrue
            $peer.SetFocus()
            Send-WtWindowKey -App $script:app -Vk 0x0D -RequireForeground | Out-Null
            $receipt = Wait-Until -TimeoutSec 30 -Because 'Sidebar Enter launches the real owned native resume process' -Condition {
                if (Test-Path $script:launchLog) {
                    $records = @(Get-Content $script:launchLog | ForEach-Object { $_ | ConvertFrom-Json })
                    if ($records.Count -eq $Generation) { $records[-1] }
                }
            }
            $receipt.session_id | Should -Be $script:sid
            $receipt.transcript | Should -Be (Join-Path $script:sandboxHome ".copilot\session-state\$script:sid\events.jsonl")
            (Get-Process -Id $receipt.native_pid).Path | Should -Be $script:shim
            $receipt.pane_session_id | Should -Not -BeNullOrEmpty
            (Get-ActivePane -App $script:app).session_id.Trim('{}') | Should -Be $receipt.pane_session_id.Trim('{}')
            Wait-WatcherStatus Idle $receipt.pane_session_id -Rendered | Out-Null
            $receipt | Add-Member Sequence 0 -Force
            $receipt
        }
        function Send-WatcherRecord {
            param($Receipt, [string]$Command)
            $Receipt.Sequence++
            Send-WtInput -App $script:app -SessionId $Receipt.pane_session_id -Text $Command
            Send-WtKeys -App $script:app -SessionId $Receipt.pane_session_id -Keys Enter
            $stage = Join-Path $script:folder "stage-$($Receipt.native_pid)-$($Receipt.Sequence).json"
            Wait-Until -TimeoutSec 10 -Because 'the native CLI confirms its actual transcript append' -Condition {
                Test-Path -LiteralPath $stage
            } | Out-Null
            (Get-Content $stage -Raw | ConvertFrom-Json).command | Should -Be $Command
        }
    }

    AfterAll {
        try {
            if ($script:app) { Stop-Terminal -App $script:app }
            if ($script:shimCreated) {
                if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                    throw 'Keep the fixture executable while its private package is active.'
                }
                (Get-FileHash $script:shim).Hash | Should -Be $script:shimHash
                Remove-Item -LiteralPath $script:shim -ErrorAction Stop
            }
        }
        finally {
            foreach ($key in $script:originalEnvironment.Keys) {
                [Environment]::SetEnvironmentVariable($key, $script:originalEnvironment[$key])
            }
            foreach ($path in $script:globalHashes.Keys) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash $path).Hash } else { $null }
                $actual | Should -Be $script:globalHashes[$path] -Because "user configuration must remain untouched: $path"
            }
        }
    }

    It 'Native history resume tracks activity without hooks' {
        Send-WatcherHook agent.session.start
        Send-WatcherHook agent.session.end
        Wait-WatcherStatus Ended | Out-Null
        $receipt = Resume-WatcherHistory 1
        foreach ($case in @(
            @{ Command = 'working'; Status = 'Working' },
            @{ Command = 'attention'; Status = 'Attention' },
            @{ Command = 'idle'; Status = 'Idle' }
        )) {
            Send-WatcherRecord $receipt $case.Command
            Wait-WatcherStatus $case.Status $receipt.pane_session_id -Rendered | Out-Null
        }
        $before = @(Read-WatcherSessions | Where-Object session_id -EQ $script:sid)[0]
        Send-WatcherRecord $receipt unrelated
        Start-Sleep -Seconds 2
        $unrelated = @(Read-WatcherSessions | Where-Object session_id -EQ $receipt.unrelated_session_id)
        foreach ($row in $unrelated) {
            $row.status | Should -BeIn @('Historical', 'Ended') -Because 'history discovery may list the file, but watcher activity must not make it live'
            $row.pane_session_id | Should -BeNullOrEmpty
        }
        $after = @(Read-WatcherSessions | Where-Object session_id -EQ $script:sid)[0]
        $after.status | Should -Be $before.status
        $after.pane_session_id | Should -Be $before.pane_session_id

        Send-WatcherHook agent.notification $receipt.pane_session_id
        Wait-WatcherStatus Attention $receipt.pane_session_id -Rendered | Out-Null
        Send-WatcherRecord $receipt idle
        Start-Sleep -Seconds 2
        Wait-WatcherStatus Attention $receipt.pane_session_id -Rendered | Out-Null
        Send-WatcherHook agent.stop $receipt.pane_session_id
        Wait-WatcherStatus Idle $receipt.pane_session_id -Rendered | Out-Null
        Close-WtPane -App $script:app -SessionId $receipt.pane_session_id
        Wait-WatcherStatus Ended | Out-Null

        $resumed = Resume-WatcherHistory 2
        $resumed.pane_session_id | Should -Not -Be $receipt.pane_session_id
        Send-WatcherRecord $resumed working
        Wait-WatcherStatus Working $resumed.pane_session_id -Rendered | Out-Null
        Send-WatcherRecord $resumed idle
        Wait-WatcherStatus Idle $resumed.pane_session_id -Rendered | Out-Null
    }
}
