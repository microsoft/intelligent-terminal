#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: restored session startup' -Tag 'Feature', 'RestoredSessionStartup' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:package = Get-ItTestPackage
        if ($script:package -cnotmatch '^IntelligentTerminal\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20}_rd9vj3e6a2mbr$') {
            throw 'Startup validation requires an explicitly selected inactive private Dev package.'
        }
        $script:target = Resolve-ItApp -Package $script:package
        Assert-WtPackageInactive -App $script:target
        $script:head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        foreach ($name in @('ITE2E_SOURCE_COMMIT', 'ITE2E_EXPECTED_APP_SHA256', 'ITE2E_EXPECTED_WTA_SHA256')) {
            if (-not [Environment]::GetEnvironmentVariable($name)) { throw "Supply exact build evidence: $name" }
        }
        $env:ITE2E_SOURCE_COMMIT | Should -Be $script:head
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:app = $null
        $script:originalEnvironment = @{}
        $script:pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $script:queueFixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpQueueAgent.ps1')).Path
        $script:resumeFixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-WatcherResumeCli.ps1')).Path
        $script:shimSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-CopilotDelegate.cpp')).Path
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $vs = Invoke-Native -FilePath $vswhere -Arguments @('-latest', '-products', '*',
            '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath')
        if ($vs.ExitCode -ne 0) { throw "Cannot resolve native fixture compiler: $($vs.StdErr)" }
        $script:vcvars = Join-Path $vs.StdOut.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else {
            Join-Path $PSScriptRoot '..\..\..\Generated Files\restored-session-startup'
        }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('startup-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null

        function Read-StartupRecords {
            @(Get-ChildItem -LiteralPath $script:records -Filter '*.json' -File |
                ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json })
        }

        function Read-StartupSnapshot {
            Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--json', '--master', $script:pipe, '--include-status', '--origin', 'all')
        }

        function Get-StartupRowName {
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList'))
            if (-not $list -or $list.Current.IsOffscreen) {
                $script:lastRowNames = @('Owned Sidebar ItemsList is absent or offscreen')
                return
            }
            $allRows = @($list.FindAll([Windows.Automation.TreeScope]::Children,
                [Windows.Automation.Condition]::TrueCondition))
            $script:lastRowNames = @($allRows | ForEach-Object {
                @{ name = $_.Current.Name; offscreen = $_.Current.IsOffscreen }
            })
            $rows = @($allRows | Where-Object {
                    -not $_.Current.IsOffscreen -and $_.Current.Name.Contains($script:sid)
                })
            if ($rows.Count -eq 1) { $rows[0].Current.Name }
        }
    }

    AfterAll {
        try {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
        finally {
            foreach ($key in $script:originalEnvironment.Keys) {
                [Environment]::SetEnvironmentVariable($key, $script:originalEnvironment[$key], 'Process')
            }
        }
    }

    It 'Restored CLI sessions show Idle before agent initialization' {
        foreach ($sessionsEnabled in @($true, $false)) {
            Assert-WtPackageInactive -App $script:target
            $case = Join-Path $script:evidence "sessions-$sessionsEnabled"
            $sandboxHome = Join-Path $case 'home'
            $bin = Join-Path $case 'bin'
            $nativeBin = Join-Path $case 'native'
            $control = Join-Path $case 'control'
            $script:records = Join-Path $case 'records'
            $script:sid = [guid]::NewGuid().ToString()
            $folder = Join-Path $case ('watcher-' + $script:sid)
            $script:title = 'Restore-' + $script:sid
            $launchLog = Join-Path $folder 'launch.jsonl'
            foreach ($directory in @($sandboxHome, $bin, $nativeBin, $control, $script:records, $folder,
                    (Join-Path $sandboxHome 'AppData\Local\Packages'), (Join-Path $sandboxHome 'AppData\Roaming'))) {
                New-Item -ItemType Directory -Path $directory -Force | Out-Null
            }
            New-Item -ItemType Junction -Path (Join-Path $sandboxHome "AppData\Local\Packages\$script:package") `
                -Target (Split-Path $script:target.LocalStateDir -Parent) | Out-Null

            $rejectSource = Join-Path $bin 'Reject.cs'
            'class Reject { static int Main() { return 3; } }' | Set-Content -LiteralPath $rejectSource
            $compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
            $reject = Join-Path $bin 'Reject.exe'
            $built = Invoke-Native -FilePath $compiler -Arguments @('/nologo', '/target:exe', "/out:$reject", $rejectSource)
            if ($built.ExitCode -ne 0) { throw "Provider blocker compilation failed: $($built.StdErr)" }
            foreach ($provider in @('copilot', 'claude', 'codex', 'gemini', 'opencode', 'npx', 'agy', 'agy_acp_server')) {
                Copy-Item -LiteralPath $reject -Destination (Join-Path $bin "$provider.exe")
            }
            $config = @{
                ITE2E_SHIM_PWSH = $script:pwsh; ITE2E_SHIM_FIXTURE = $script:resumeFixture
                ITE2E_SHIM_LOG = $launchLog; ITE2E_SHIM_RUN = $script:sid
                ITE2E_SHIM_WTCLI = $script:target.WtcliPath; ITE2E_SHIM_RESUME_SESSION = $script:sid
            }
            $header = Join-Path $nativeBin 'config.h'
            @($config.Keys | ForEach-Object { "#define $_ LR`"ite2e($($config[$_]))ite2e`"" }) |
                Set-Content -LiteralPath $header -Encoding utf8BOM
            $shim = Join-Path $nativeBin 'copilot.exe'
            $build = "call `"$script:vcvars`" >nul && cl /nologo /utf-8 /EHsc /std:c++17 /FI`"$header`" `"$script:shimSource`" /Fe:`"$shim`" /Fo:`"$nativeBin\copilot.obj`" /link /INCREMENTAL:NO"
            $output = & $env:ComSpec /d /c $build 2>&1
            if ($LASTEXITCODE -ne 0) { throw "Native resume fixture compilation failed: $output" }

            $environment = @{
                USERPROFILE = $sandboxHome; HOME = $sandboxHome; COPILOT_HOME = (Join-Path $sandboxHome '.copilot')
                CLAUDE_CONFIG_DIR = (Join-Path $sandboxHome '.claude'); CODEX_HOME = (Join-Path $sandboxHome '.codex')
                GEMINI_CLI_HOME = (Join-Path $sandboxHome '.gemini'); XDG_CONFIG_HOME = (Join-Path $sandboxHome '.config')
                OPENCODE_CONFIG_DIR = (Join-Path $sandboxHome '.config\opencode')
                PATH = "$bin;$(Split-Path $script:pwsh);$($script:target.InstallLocation);$env:WINDIR\System32;$env:WINDIR;$env:LOCALAPPDATA\Microsoft\WindowsApps"
                WTCLI_PATH = $script:target.WtcliPath; WT_WTCLI_PATH = $script:target.WtcliPath
            }
            foreach ($key in $environment.Keys) {
                if (-not $script:originalEnvironment.ContainsKey($key)) {
                    $script:originalEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
                }
                [Environment]::SetEnvironmentVariable($key, $environment[$key], 'Process')
            }
            foreach ($key in @('COPILOT_HOME', 'CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'GEMINI_CLI_HOME',
                    'XDG_CONFIG_HOME', 'OPENCODE_CONFIG_DIR')) {
                New-Item -ItemType Directory -Path $environment[$key] -Force | Out-Null
            }
            New-Item -ItemType Directory -Path (Join-Path $sandboxHome '.copilot\session-state') -Force | Out-Null
            $nativeEnvironment = $environment.Clone()
            $nativeEnvironment.PATH = "$nativeBin;$($environment.PATH)"
            $environmentProbe = Join-Path $control 'agent-environment.json'
            $probe = "@{ userprofile = `$env:USERPROFILE; home = `$env:HOME; copilot_home = `$env:COPILOT_HOME } | ConvertTo-Json | Set-Content -LiteralPath '$($environmentProbe.Replace("'", "''"))'"
            $invocation = "$probe; & '$($script:queueFixture.Replace("'", "''"))' -ControlDirectory '$($control.Replace("'", "''"))' -RecordDirectory '$($script:records.Replace("'", "''"))' -HoldInitialize"
            $command = 'pwsh -NoLogo -NoProfile -EncodedCommand ' +
                [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
            $nativeProfile = '{' + [guid]::NewGuid().ToString() + '}'
            $guardProfile = '{' + [guid]::NewGuid().ToString() + '}'
            $pane = '{' + [guid]::NewGuid().ToString() + '}'
            $savedHelper = '"wta.exe" --initial-view chat --agent-backend host:custom:restore-startup --agent-custom-command "' + $command + '"'
            $state = @{
                featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
                sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
                persistedWindowLayouts = @(@{ tabLayout = @(
                    @{ action = 'newTab'; profile = $nativeProfile; sessionId = $pane
                        commandline = 'cmd.exe /d /s /c "copilot --resume ' + $script:sid + '"'
                        startingDirectory = $folder; tabTitle = $script:title }
                    @{ action = 'splitPane'; type = 'agentStashed'; split = 'right'; size = 0.25
                        commandline = $savedHelper; startingDirectory = $folder }
                    @{ action = 'newTab'; profile = $guardProfile; tabTitle = 'Ordinary guard' }
                ) })
            }
            try {
                $script:app = Start-Terminal -Package $script:package -TimeoutSec 45 -State $state -Settings @{
                    language = 'en-US'; tabLayout = 'vertical'; tabLayoutVerticalWidth = 420
                    sidebarShowRecentAgentSessions = $false; firstWindowPreference = 'persistedLayoutAndContent'
                    startupActions = ''; windowingBehavior = 'useNew'; defaultProfile = $guardProfile
                    agentSessionManagementEnabled = $sessionsEnabled; autoErrorDetectionEnabled = $false; autoFixEnabled = $false
                    acpAgent = 'custom:restore-startup'; acpCustomCommand = $command; acpCustomCommands = @($command)
                    'warning.confirmOnClose' = 'never'; actions = @()
                    disabledProfileSources = @('Windows.Terminal.PowershellCore', 'Windows.Terminal.Wsl',
                        'Windows.Terminal.Azure', 'Windows.Terminal.VisualStudio', 'Windows.Terminal.SSH', 'Microsoft.WSL')
                    profiles = @{ defaults = @{ shellIntegrationEnabled = $false; reloadEnvironmentVariables = $false }; list = @(
                        @{ guid = $nativeProfile; name = 'Restored native'; commandline = 'cmd.exe'
                            tabTitle = $script:title; suppressApplicationTitle = $true; environment = $nativeEnvironment }
                        @{ guid = $guardProfile; name = 'Ordinary guard'; commandline = 'cmd.exe /d'
                            environment = $environment; suppressApplicationTitle = $true }
                    ) }
                }
                $script:app.Launched | Should -BeTrue
                $effectiveSettings = Invoke-WtCli -App $script:app -Arguments @('get-settings')
                $effectiveSettings.tabLayout | Should -Be 'vertical'
                $effectiveSettings.agentSessionManagementEnabled | Should -Be $sessionsEnabled
                $effectiveSettings.defaultProfile | Should -Be $guardProfile
                (Get-Process -Id $script:app.Pid).Path | Should -Be $script:app.WindowsTerminal
                @((Get-Process -Id $script:app.Pid).Modules | Where-Object ModuleName -EQ TerminalApp.dll)[0].FileName |
                    Should -Be (Join-Path $script:target.InstallLocation 'TerminalApp.dll')
                Wait-Until -TimeoutSec 10 -Because 'the native resume command actually starts' -Condition {
                    Test-Path -LiteralPath $launchLog
                } | Out-Null
                $launch = Get-Content -LiteralPath $launchLog | Select-Object -First 1 | ConvertFrom-Json
                $launch.session_id | Should -Be $script:sid
                $launch.pane_session_id.Trim('{}') | Should -Be $pane.Trim('{}')
                $launch.userprofile | Should -Be $sandboxHome
                (Get-Process -Id $launch.native_pid).Path | Should -Be $shim
                Wait-Until -TimeoutSec 10 -Because 'ACP initialization is held by the real fixture process' -Condition {
                    @(Read-StartupRecords | Where-Object kind -EQ initialize).Count -gt 0
                } | Out-Null
                $agentEnvironment = Get-Content -LiteralPath $environmentProbe -Raw | ConvertFrom-Json
                $agentEnvironment.userprofile | Should -Be $sandboxHome
                $agentEnvironment.home | Should -Be $sandboxHome
                $agentEnvironment.copilot_home | Should -Be (Join-Path $sandboxHome '.copilot')
                $script:pipe = Get-WtaMasterPipe -App $script:app
                $masters = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object {
                    $_.ExecutablePath -eq $script:app.WtaPath -and $_.CommandLine.Contains('--master') -and $_.CommandLine.Contains($script:pipe)
                })
                $masters | Should -HaveCount 1
                $masters[0].ParentProcessId | Should -Be $script:app.Pid
                $snapshot = Wait-Until -TimeoutSec 10 -Because 'restored native session becomes canonically Idle without ACP' -Condition {
                    $current = Read-StartupSnapshot
                    $rows = @($current.sessions | Where-Object session_id -EQ $script:sid)
                    if ($rows.Count -eq 1 -and $rows[0].status -eq 'Idle') { $current }
                }
                @($snapshot.sessions | Where-Object session_id -EQ $script:sid)[0].pane_session_id.Trim('{}') |
                    Should -Be $pane.Trim('{}')
                Wait-Until -TimeoutSec 10 -Because 'the visible restored Sidebar tab renders Idle without ACP' -Condition {
                    (Get-StartupRowName) -match '\bIdle\b'
                } | Out-Null
                @(Read-StartupRecords | Where-Object kind -In @('initialized', 'session')).Count | Should -Be 0
                foreach ($initialization in @(Read-StartupRecords | Where-Object kind -EQ initialize)) {
                    (Get-Process -Id $initialization.pid -ErrorAction Stop).HasExited | Should -BeFalse
                    (Get-CimInstance Win32_Process -Filter "ProcessId=$($initialization.pid)").ParentProcessId |
                        Should -Be $masters[0].ProcessId
                }
                $log = Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log'
                $log | Should -Match ("publishing restored session " + [regex]::Escape($script:sid) + ' receiver=\d+ consumer=(master|helper)')
                $log | Should -Match ("agent_status_projection: source=snapshot session=" + [regex]::Escape($script:sid) + ' .*status=Idle')
                $guardTabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId |
                    Where-Object title -EQ 'Ordinary guard')
                $guardTabs | Should -HaveCount 1
                $guard = @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId -TabId ([string]$guardTabs[0].tab_id))
                $guard | Should -HaveCount 1
                $guardIds = @($guard | ForEach-Object { $_.session_id.Trim('{}') })
                @($snapshot.pane_statuses | Where-Object { $_.pane_id.Trim('{}') -in $guardIds }) |
                    Should -HaveCount 0 -Because 'ordinary shells must not acquire a fabricated agent status'
                @{ source_commit = $script:head; package = $script:app.Package; host_pid = $script:app.Pid
                    master_pid = $masters[0].ProcessId; hwnd = $script:app.Hwnd; sessions_enabled = $sessionsEnabled
                    session_id = $script:sid; pane_id = $pane; rendered_name = Get-StartupRowName
                    app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256; wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256 } |
                    ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $case 'idle-before-initialize.json')
                Save-UiScreenshot -App $script:app -Path (Join-Path $case 'idle-before-initialize.png') | Out-Null
                foreach ($step in @(@{ Command = 'working'; Status = 'Working'; Label = 'Active' },
                        @{ Command = 'idle'; Status = 'Idle'; Label = 'Idle' })) {
                    Send-WtInput -App $script:app -SessionId $pane.Trim('{}') -Text $step.Command
                    Send-WtKeys -App $script:app -SessionId $pane.Trim('{}') -Keys @('Enter')
                    Wait-Until -TimeoutSec 10 -Because "resumed native activity still reaches $($step.Status)" -Condition {
                        $rows = @((Read-StartupSnapshot).sessions | Where-Object session_id -EQ $script:sid)
                        $rows.Count -eq 1 -and $rows[0].status -eq $step.Status -and
                            (Get-StartupRowName) -match ('\b' + $step.Label + '\b')
                    } | Out-Null
                }
                @(Read-StartupRecords | Where-Object kind -EQ initialized).Count | Should -Be 0
                New-Item -ItemType File -Path (Join-Path $control 'initialize.release') | Out-Null
                Wait-Until -TimeoutSec 15 -Because 'the held ACP fixture can finish normally' -Condition {
                    @(Read-StartupRecords | Where-Object kind -EQ session_ready).Count -gt 0
                } | Out-Null
                @((Read-StartupSnapshot).sessions | Where-Object session_id -EQ $script:sid) | Should -HaveCount 1
            }
            catch {
                $original = $_
                if ($script:app -and -not $script:app.OwnedProcess.HasExited) {
                    try {
                        @{ error = $original.ToString(); rows = $script:lastRowNames
                            snapshot = Read-StartupSnapshot } | ConvertTo-Json -Depth 20 |
                            Set-Content -LiteralPath (Join-Path $case 'failure.json')
                        Save-UiScreenshot -App $script:app -Path (Join-Path $case 'failure.png') | Out-Null
                    }
                    catch { Write-ItLog -Level WARN -Message "Startup failure diagnostics unavailable: $_" }
                }
                throw $original
            }
            finally {
                if ($script:app) {
                    Stop-Terminal -App $script:app
                    $script:app = $null
                }
                foreach ($key in $script:originalEnvironment.Keys) {
                    [Environment]::SetEnvironmentVariable($key, $script:originalEnvironment[$key], 'Process')
                }
            }
        }
    }
}
