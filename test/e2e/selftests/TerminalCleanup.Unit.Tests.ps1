#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    if (-not (Get-Module ItE2E)) { Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force }
}

Describe 'Owned terminal cleanup' -Tag 'Unit' {
    InModuleScope ItE2E {
        BeforeEach {
            $script:order = [Collections.Generic.List[string]]::new()
            $script:child = [pscustomobject]@{ Id = 41001; HasExited = $false }
            $script:hostProcess = [pscustomobject]@{
                Id = 41000; HasExited = $false; MainWindowHandle = 1; StartTime = [datetime]'2026-10-01'
                Path = 'C:\owned\WindowsTerminal.exe'
            }
            $script:hostProcess | Add-Member ScriptMethod CloseMainWindow {
                $script:order.Add('window')
                $this.HasExited = $true
                return $true
            }
            $script:app = [pscustomobject]@{
                Pid = 41000; Launched = $true; ConfigBackupOwned = $true
                OwnedProcess = $script:hostProcess; InstallLocation = 'C:\owned'
                SettingsPath = (Join-Path $TestDrive 'settings.json')
                StatePath = (Join-Path $TestDrive 'state.json')
                InputRunToken = 'unit-owned-run'; InputReceiptPath = (Join-Path $TestDrive 'owned.jsonl')
            }
            @{ pid = $script:hostProcess.Id; path = $script:hostProcess.Path
                start_utc = $script:hostProcess.StartTime.ToUniversalTime().ToString('o'); run_token = $script:app.InputRunToken } |
                ConvertTo-Json -Compress | Set-Content -LiteralPath $script:app.InputReceiptPath
            foreach ($path in @($script:app.SettingsPath, $script:app.StatePath)) {
                [IO.File]::WriteAllBytes($path, [byte[]]@(0, 255, 1, 13, 10))
            }
            Backup-WtConfig -App $script:app
            foreach ($path in @($script:app.SettingsPath, $script:app.StatePath)) {
                [IO.File]::WriteAllBytes($path, [byte[]]@(99))
            }
            Mock Write-ItLog {}
            Mock Get-DescendantWtaIds { $script:child }
            Mock Get-Process { throw 'Unexpected PID lookup during cleanup' }
            Mock Get-WtProcessesForApp { @() }
            Mock Test-Until { & $Condition }
            Mock Stop-Process -RemoveParameterType InputObject {
                $script:order.Add('listener')
                $script:child.HasExited = $true
            }
        }

        It 'accepts a confirmed exit race and stops listeners before closing the window, restoring exact bytes' {
            Mock Stop-Process -RemoveParameterType InputObject {
                $script:order.Add('listener')
                $script:child.HasExited = $true
                throw [Management.Automation.ErrorRecord]::new(
                    [ArgumentException]::new('Process not found'),
                    'NoProcessFoundForGivenId,Microsoft.PowerShell.Commands.StopProcessCommand',
                    [Management.Automation.ErrorCategory]::ObjectNotFound, 41001)
            }
            Stop-Terminal -App $script:app
            $script:order -join ',' | Should -Be 'listener,window'
            foreach ($path in @($script:app.SettingsPath, $script:app.StatePath)) {
                [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) | Should -Be 'AP8BDQo='
                Test-Path "$path.e2ebak" | Should -BeFalse
            }
            Should -Invoke Get-DescendantWtaIds -Times 1 -ParameterFilter {
                $AsProcess -and $RootStartTime -eq $script:hostProcess.StartTime -and
                [object]::ReferenceEquals($RootProcess, $script:hostProcess)
            }
        }

        It 'surfaces a real kill failure while still live, retaining backups and the original error' {
            Mock Stop-Process -RemoveParameterType InputObject { throw 'owned kill denied' }
            Mock Get-WtProcessesForApp { $script:hostProcess }
            { Stop-Terminal -App $script:app } | Should -Throw '*owned kill denied*'
            $script:order.Count | Should -Be 0
            Test-Path "$($script:app.SettingsPath).e2ebak" | Should -BeTrue
            [IO.File]::ReadAllBytes($script:app.SettingsPath)[0] | Should -Be 99
        }

        It 'retains backups for an unknown active package executable without killing it' {
            Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 999; Path = 'C:\owned\unknown.exe' } }
            { Stop-Terminal -App $script:app } | Should -Throw '*Package remains active*'
            Test-Path "$($script:app.SettingsPath).e2ebak" | Should -BeTrue
            Should -Invoke Stop-Process -Times 1 -Exactly -ParameterFilter { $InputObject.Id -eq 41001 }
        }

        It 'retains backups and surfaces failed inactivity discovery' {
            Mock Get-WtProcessesForApp { throw 'discovery denied' }
            { Stop-Terminal -App $script:app } | Should -Throw '*discovery denied*'
            Test-Path "$($script:app.SettingsPath).e2ebak" | Should -BeTrue
        }

        It 'does not restore or discover package inactivity when restoration is disabled' {
            Stop-Terminal -App $script:app -RestoreSettings $false
            Should -Invoke Get-WtProcessesForApp -Times 0
            Test-Path "$($script:app.SettingsPath).e2ebak" | Should -BeTrue
        }

        It 'does not restore without backup ownership' {
            $script:app.ConfigBackupOwned = $false
            Stop-Terminal -App $script:app
            Should -Invoke Get-WtProcessesForApp -Times 0
            Test-Path "$($script:app.SettingsPath).e2ebak" | Should -BeTrue
        }

        It 'leaves an attached app untouched and refuses restoration while it remains active' {
            $script:app.Launched = $false
            Mock Get-WtProcessesForApp { $script:hostProcess }
            { Stop-Terminal -App $script:app } | Should -Throw '*Package remains active*'
            Should -Invoke Stop-Process -Times 0
            Should -Invoke Get-DescendantWtaIds -Times 0
            $script:order.Count | Should -Be 0
            Test-Path "$($script:app.StatePath).e2ebak" | Should -BeTrue
        }

        It 'does not suppress an unrelated ArgumentException even if the child has exited' {
            Mock Stop-Process -RemoveParameterType InputObject {
                $script:child.HasExited = $true
                throw [ArgumentException]::new('invalid kill argument')
            }
            { Stop-Terminal -App $script:app } | Should -Throw '*invalid kill argument*'
            $script:order.Count | Should -Be 0
        }
        It 'rejects an exact PID with a different recorded start before closing or killing anything' {
                @{ pid = $script:hostProcess.Id; path = $script:hostProcess.Path
                    start_utc = $script:hostProcess.StartTime.AddSeconds(1).ToUniversalTime().ToString('o')
                    run_token = $script:app.InputRunToken } |
                    ConvertTo-Json -Compress | Set-Content -LiteralPath $script:app.InputReceiptPath
                Mock Get-WtProcessesForApp { $script:hostProcess }
                { Stop-Terminal -App $script:app } | Should -Throw '*stale*'
                Should -Invoke Get-DescendantWtaIds -Times 0
                Should -Invoke Stop-Process -Times 0
                $script:order.Count | Should -Be 0
            }
    }
}

Describe 'Descendant process identity' -Tag 'Unit' {
    It 'accepts a missing CIM root only after the captured owned process confirms its exit' {
        InModuleScope ItE2E {
            $script:rootProcess = [pscustomobject]@{ Id = 41000; HasExited = $false }
            Mock Get-CimInstance { $script:rootProcess.HasExited = $true; @() }
            @(Get-DescendantWtaIds -RootPid 41000 -RootStartTime ([datetime]'2026-10-01') -AsProcess -RootProcess $script:rootProcess).Count |
                Should -Be 0
        }
    }

    It 'rejects a missing CIM root while the captured owned process remains live' {
        InModuleScope ItE2E {
            Mock Get-CimInstance { @() }
            { Get-DescendantWtaIds -RootPid 41000 -AsProcess -RootProcess ([pscustomobject]@{ Id = 41000; HasExited = $false }) } |
                Should -Throw '*without a confirmed owned exit*'
        }
    }

    It 'preserves discovery errors even when the captured owned process has exited' {
        InModuleScope ItE2E {
            Mock Get-CimInstance { throw 'CIM discovery denied' }
            { Get-DescendantWtaIds -RootPid 41000 -AsProcess -RootProcess ([pscustomobject]@{ Id = 41000; HasExited = $true }) } |
                Should -Throw '*CIM discovery denied*'
        }
    }

    It 'rejects a replacement root identity even when the captured owned process has exited' {
        InModuleScope ItE2E {
            Mock Get-CimInstance {
                [pscustomobject]@{ ProcessId = 41000; ParentProcessId = 1; CreationDate = [datetime]'2026-10-02'; Name = 'WindowsTerminal.exe' }
            }
            { Get-DescendantWtaIds -RootPid 41000 -RootStartTime ([datetime]'2026-10-01') -AsProcess -RootProcess ([pscustomobject]@{ Id = 41000; HasExited = $true }) } |
                Should -Throw '*Terminal process identity changed*'
        }
    }

    It 'accepts CIM microsecond truncation and captures only the matching descendant object' {
        InModuleScope ItE2E {
            $start = [datetime]::new(639264992174074996)
            $snapshotStart = [datetime]::new(639264992174074990)
            $script:capturedProcess = [pscustomobject]@{
                Id = 41001; Handle = 1; HasExited = $false; StartTime = $start.AddSeconds(1); Path = 'C:\owned\wta.exe'
            }

            Mock Get-CimInstance {
                [pscustomobject]@{ ProcessId = 41000; ParentProcessId = 1; CreationDate = $snapshotStart; Name = 'WindowsTerminal.exe' }
                [pscustomobject]@{ ProcessId = 41001; ParentProcessId = 41000; CreationDate = $snapshotStart.AddSeconds(1); Name = 'wta.exe'; ExecutablePath = 'C:\owned\wta.exe' }
            }
            Mock Get-Process { $script:capturedProcess }
            $result = @(Get-DescendantWtaIds -RootPid 41000 -RootStartTime $start -AsProcess)
            $result.Count | Should -Be 1
            [object]::ReferenceEquals($result[0], $script:capturedProcess) | Should -BeTrue
        }
    }

    It 'rejects PID reuse instead of returning a replacement process to kill' {
        InModuleScope ItE2E {
            $start = [datetime]'2026-10-01'
            Mock Get-CimInstance {
                [pscustomobject]@{ ProcessId = 41000; ParentProcessId = 1; CreationDate = $start; Name = 'WindowsTerminal.exe' }
                [pscustomobject]@{ ProcessId = 41001; ParentProcessId = 41000; CreationDate = $start.AddSeconds(1); Name = 'wta.exe'; ExecutablePath = 'C:\owned\wta.exe' }
            }
            Mock Get-Process {
                [pscustomobject]@{ Id = 41001; Handle = 1; HasExited = $false; StartTime = $start.AddSeconds(2); Path = 'C:\owned\wta.exe' }
            }
            { Get-DescendantWtaIds -RootPid 41000 -RootStartTime $start -AsProcess } |
                Should -Throw '*WTA process identity changed*'
        }
    }

    It 'captures bound wtcli listeners after their WTA parent but excludes other CLI commands' {
        InModuleScope ItE2E {
            $script:start = [datetime]'2026-10-01'
            Mock Get-CimInstance {
                [pscustomobject]@{ ProcessId = 41000; ParentProcessId = 1; CreationDate = $script:start; Name = 'WindowsTerminal.exe' }
                [pscustomobject]@{
                    ProcessId = 41002; ParentProcessId = 41001; CreationDate = $script:start.AddSeconds(2)
                    Name = 'wtcli.exe'; ExecutablePath = 'C:\owned\wtcli.exe'
                    CommandLine = '"C:\owned\wtcli.exe" --json listen --parent-pid 41001 --ready-token wta-41001'
                }
                [pscustomobject]@{
                    ProcessId = 41003; ParentProcessId = 41001; CreationDate = $script:start.AddSeconds(2)
                    Name = 'wtcli.exe'; ExecutablePath = 'C:\owned\wtcli.exe'; CommandLine = 'wtcli.exe --json list-windows'
                }
                [pscustomobject]@{
                    ProcessId = 41001; ParentProcessId = 41000; CreationDate = $script:start.AddSeconds(1)
                    Name = 'wta.exe'; ExecutablePath = 'C:\owned\wta.exe'
                }
            }
            Mock Get-Process {
                $isWta = $Id -eq 41001
                [pscustomobject]@{
                    Id = $Id; Handle = 1; HasExited = $false
                    StartTime = $script:start.AddSeconds($(if ($isWta) { 1 } else { 2 }))
                    Path = $(if ($isWta) { 'C:\owned\wta.exe' } else { 'C:\owned\wtcli.exe' })
                }
            }
            $result = @(Get-DescendantWtaIds -RootPid 41000 -RootStartTime $script:start -AsProcess)
            @($result.Id) -join ',' | Should -Be '41001,41002'
            Should -Invoke Get-Process -Times 0 -ParameterFilter { $Id -eq 41003 }
        }
    }
}

Describe 'Affirmative package inactivity discovery' -Tag 'Unit' {
    InModuleScope ItE2E {
        BeforeEach {
            $script:install = Join-Path $TestDrive 'package'
            New-Item -ItemType Directory -Path $script:install -Force | Out-Null
            [IO.File]::WriteAllBytes((Join-Path $script:install 'WindowsTerminal.exe'), [byte[]]@())
            [IO.File]::WriteAllBytes((Join-Path $script:install 'wta.exe'), [byte[]]@())
            $script:target = [pscustomobject]@{ InstallLocation = $script:install }
        }

        It 'refuses quiescence when an actual helper finds a WindowsTerminal with an unknown path' {
            Mock Get-Process { [pscustomobject]@{ Id = 41000; ProcessName = 'WindowsTerminal'; Path = $null } }
            { Get-WtProcessesForApp -App $script:target -IncludePackageExecutables } |
                Should -Throw '*executable path unavailable*'
        }

        It 'does not block on protected unrelated System processes with unknown paths' {
            Mock Get-Process { [pscustomobject]@{ Id = 4; ProcessName = 'System'; Path = $null } }
            @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count | Should -Be 0
        }

        It 'returns a package process and excludes a known executable outside the package' {
            Mock Get-Process {
                [pscustomobject]@{ Id = 41000; ProcessName = 'WindowsTerminal'; Path = (Join-Path $script:install 'WindowsTerminal.exe') }
                [pscustomobject]@{ Id = 24036; ProcessName = 'WindowsTerminal'; Path = 'C:\Store\WindowsTerminal.exe' }
            }
            $result = @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables)
            $result.Count | Should -Be 1
            $result[0].Id | Should -Be 41000
        }

        It 'retains exact backup bytes when real inactivity discovery encounters an unknown path' {
            $app = [pscustomobject]@{
                Launched = $false; Pid = 41000; ConfigBackupOwned = $true; InstallLocation = $script:install
                SettingsPath = (Join-Path $TestDrive 'unknown-settings.json')
                StatePath = (Join-Path $TestDrive 'unknown-state.json')
            }
            foreach ($path in @($app.SettingsPath, $app.StatePath)) {
                [IO.File]::WriteAllBytes("$path.e2ebak", [byte[]]@(0, 255, 1, 13, 10))
                [IO.File]::WriteAllBytes($path, [byte[]]@(99))
            }
            Mock Write-ItLog {}
            Mock Get-Process { [pscustomobject]@{ Id = 41000; ProcessName = 'WindowsTerminal'; Path = $null } }
            Mock Stop-Process { throw 'Must not kill an attached or unknown process' }
            { Stop-Terminal -App $app } | Should -Throw '*executable path unavailable*'
            foreach ($path in @($app.SettingsPath, $app.StatePath)) {
                [Convert]::ToBase64String([IO.File]::ReadAllBytes("$path.e2ebak")) | Should -Be 'AP8BDQo='
                [IO.File]::ReadAllBytes($path)[0] | Should -Be 99
            }
            Should -Invoke Stop-Process -Times 0
        }
    }
}

Describe 'Combined fixture bounded headless recovery' -Tag 'Unit' {
    BeforeAll {
        $tokens = $null
        $errors = $null
        $path = Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'
        $ast = [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        if ($errors) { throw 'Combined fixture source could not be parsed.' }
        foreach ($name in @('Initialize-CombinedCleanupNative', 'Get-CombinedVisibleProcessIds',
                'Start-CombinedCleanupTab', 'Invoke-CombinedHeadlessRecovery')) {
            $definition = $ast.Find({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
            }, $true)
            . ([scriptblock]::Create($definition.Extent.Text))
        }
        $script:combinedStartTab = (Get-Command Start-CombinedCleanupTab).ScriptBlock
        $cleanup = $ast.Find({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'AfterAll'
        }, $true).CommandElements[1].ScriptBlock.Extent.Text
        $script:combinedAfterAll = [scriptblock]::Create($cleanup.Substring(1, $cleanup.Length - 2))
    }

    BeforeEach {
        $script:recovered = $false
        $script:target = [pscustomobject]@{
            Package = 'Dev-fixture'; AppUserModelId = 'Dev-fixture!App'
            WindowsTerminal = (Join-Path $TestDrive 'WindowsTerminal.exe')
            SettingsPath = (Join-Path $TestDrive 'combined-settings.json')
            StatePath = (Join-Path $TestDrive 'combined-state.json')
        }
        $script:app = [pscustomobject]@{
            Pid = 41000; Package = $script:target.Package; AppUserModelId = $script:target.AppUserModelId
            Launched = $true
            OwnedProcess = [pscustomobject]@{ Id = 41000; HasExited = $true; StartTime = [datetime]'2026-10-01' }
        }
        $script:headless = [pscustomobject]@{
            Id = 41088; Handle = 1; HasExited = $false; MainWindowHandle = 0
            Path = $script:target.WindowsTerminal; StartTime = [datetime]'2026-10-01T00:00:01'
        }
        $script:ownsConfig = $true
        $script:initialProcessCheckAt = 'verified before launch'
        $script:initialProcesses = @()
        $script:evidence = $TestDrive
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            [IO.File]::WriteAllBytes("$path.e2ebak", [byte[]]@(0, 255, 1, 13, 10))
            $script:originalHashes[$path] = (Get-FileHash -LiteralPath "$path.e2ebak").Hash
            [IO.File]::WriteAllBytes($path, [byte[]]@(99))
        }
        Mock Get-WtProcessesForApp { if (-not $script:recovered) { $script:headless } }
        Mock Get-CombinedVisibleProcessIds { @() }
        Mock Get-CimInstance {
            [pscustomobject]@{
                ProcessId = 41088; Name = 'WindowsTerminal.exe'; ExecutablePath = $script:target.WindowsTerminal
                CreationDate = $script:headless.StartTime
                CommandLine = "`"$($script:target.WindowsTerminal)`" -Embedding"
            }
        }
        Mock Start-CombinedCleanupTab { $script:recovered = $true; 24680 }
        Mock Test-Until { & $Condition }
        Mock Start-Sleep {}
        Mock Stop-Terminal {}
        Mock Stop-Process { throw 'Headless processes must not be force-stopped' }
        Mock Write-ItLog {}
    }

    It 'runs the actual AfterAll recovery once and restores original configuration bytes only after package inactivity' {
        & $script:combinedAfterAll
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            (Get-FileHash -LiteralPath $path).Hash | Should -Be $script:originalHashes[$path]
            Test-Path "$path.e2ebak" | Should -BeFalse
        }
        Should -Invoke Start-CombinedCleanupTab -Times 1 -Exactly -ParameterFilter { $AppUserModelId -eq 'Dev-fixture!App' }
        Should -Invoke Stop-Process -Times 0
        Should -Invoke Start-Sleep -Times 1 -ParameterFilter { $Seconds -eq 3 }
    }

    It 'refuses recovery when initial package inactivity was not confirmed' {
        { Invoke-CombinedHeadlessRecovery -App $script:app -Target $script:target -InitiallyInactive $false } |
            Should -Throw '*initial inactivity*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
    }

    It 'refuses recovery before the captured owned GUI host has exited' {
        $script:app.OwnedProcess.HasExited = $false
        { Invoke-CombinedHeadlessRecovery -App $script:app -Target $script:target -InitiallyInactive $true } |
            Should -Throw '*confirmed owned Dev host exit*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
    }

    It 'refuses a visible app window even when MainWindowHandle is zero' {
        Mock Get-CombinedVisibleProcessIds { 41088 }
        { & $script:combinedAfterAll } | Should -Throw '*refuses visible*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }

    It 'refuses unknown package membership or a non-host executable' {
        $script:headless.Path = Join-Path $TestDrive 'wta.exe'
        { & $script:combinedAfterAll } | Should -Throw '*non-host package processes*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }

    It 'refuses unverified CIM identity without creating a task tab' {
        Mock Get-CimInstance { @() }
        { & $script:combinedAfterAll } | Should -Throw '*identity-bound headless Dev COM server*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }

    It 'retains backups and surfaces failure when the single task tab does not quiesce Dev' {
        Mock Start-CombinedCleanupTab { 24680 }
        Mock Test-Until { $false }
        { & $script:combinedAfterAll } | Should -Throw '*did not quiesce Dev*'
        Should -Invoke Start-CombinedCleanupTab -Times 1 -Exactly
        Should -Invoke Stop-Process -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }

    It 'does not recover or hide a real owned termination failure' {
        Mock Stop-Terminal { throw 'real owned termination denied' }
        { & $script:combinedAfterAll } | Should -Throw '*real owned termination denied*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }

    It 'bounds native AUMID activation and surfaces timeout instead of retrying' {
        Mock Invoke-Native { [pscustomobject]@{ TimedOut = $true; ExitCode = -1; StdErr = 'timeout' } }
        { & $script:combinedStartTab -AppUserModelId 'Dev-fixture!App' } | Should -Throw '*activation failed*'
        Should -Invoke Invoke-Native -Times 1 -Exactly -ParameterFilter { $TimeoutSec -eq 15 }
    }

    It 'preserves a real termination error when subsequent process observation also fails' {
        $script:discoveryCalls = 0
        Mock Get-WtProcessesForApp {
            $script:discoveryCalls++
            if ($script:discoveryCalls -eq 1) { $script:headless } else { throw 'later discovery failed' }
        }
        Mock Stop-Terminal { throw 'real owned termination denied' }
        { & $script:combinedAfterAll } | Should -Throw '*real owned termination denied*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }

    It 'refuses changed package membership immediately before activation' {
        $script:discoveryCalls = 0
        Mock Get-WtProcessesForApp {
            $script:discoveryCalls++
            if ($script:discoveryCalls -eq 1) { $script:headless }
            else { [pscustomobject]@{ Id = 41089; StartTime = $script:headless.StartTime } }
        }
        { Invoke-CombinedHeadlessRecovery -App $script:app -Target $script:target -InitiallyInactive $true } |
            Should -Throw '*membership changed*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
    }

    It 'refuses an invisible host that is not the exact Embedding activation' {
        Mock Get-CimInstance {
            [pscustomobject]@{
                ProcessId = 41088; Name = 'WindowsTerminal.exe'; ExecutablePath = $script:target.WindowsTerminal
                CreationDate = $script:headless.StartTime; CommandLine = "`"$($script:target.WindowsTerminal)`" new-tab"
            }
        }
        { & $script:combinedAfterAll } | Should -Throw '*identity-bound headless Dev COM server*'
        Should -Invoke Start-CombinedCleanupTab -Times 0
        Test-Path "$($script:target.SettingsPath).e2ebak" | Should -BeTrue
    }
}
