#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
}

Describe 'Dev-only automatic cold start' -Tag 'Unit' {
    InModuleScope ItE2E {
        BeforeAll {
            function New-FakeTerminalProcess {
                param([int]$Id = 51001, [string]$Root = 'C:\DevPackage\AppX',
                    [string]$Name = 'WindowsTerminal')
                $process = [pscustomobject]@{
                    Id = $Id
                    ProcessName = $Name
                    Path = (Join-Path $Root "$Name.exe")
                    StartTime = [datetime]'2026-10-08T00:00:00Z'
                    Handle = [IntPtr]::new(42)
                    HasExited = $false
                    MainWindowHandle = [IntPtr]::new(1)
                    Closed = $false
                }
                $process | Add-Member ScriptMethod CloseMainWindow {
                    $this.Closed = $true
                    $this.HasExited = $true
                    $true
                }
                $process
            }
        }
        BeforeEach {
            $script:devFamily = 'IntelligentTerminal_rd9vj3e6a2mbr'
            $script:devFullName = 'IntelligentTerminal_0.8.0.3_x64__rd9vj3e6a2mbr'
            $script:app = [pscustomobject]@{
                Package = $script:devFamily
                PackageFullName = $script:devFullName
                InstallLocation = 'C:\DevPackage\AppX'
                Version = '0.8.0.3'
                WtcliPath = 'wtcli.exe'
                SettingsPath = Join-Path $TestDrive 'settings.json'
                StatePath = Join-Path $TestDrive 'state.json'
                LocalStateDir = $TestDrive
                LogStartOffset = @{}
            }
            $script:process = New-FakeTerminalProcess
            Mock Get-WtProcessesForApp {
                if (-not $script:process.HasExited) { $script:process }
            }
            Mock Get-ItCreatedProcessPackage { $script:app.PackageFullName }
            Mock Get-CimInstance {
                [pscustomobject]@{
                    ProcessId = $PID
                    ParentProcessId = 0
                    CreationDate = [datetime]'2026-09-28T00:00:00Z'
                }
            }
            Mock Test-Until { & $Condition }
            Mock Start-Sleep {}
            Mock Write-ItLog {}
            Mock Stop-Process { throw 'Unexpected kill' }
        }

        It 'closes an exact Dev window automatically before a cold start' {
            Stop-StaleItInstances -App $script:app

            $script:process.Closed | Should -BeTrue
            $script:process.HasExited | Should -BeTrue
            Should -Invoke Stop-Process -Times 0
            Should -Invoke Get-WtProcessesForApp -ParameterFilter { $IncludePackageExecutables } -Times 6 -Exactly
        }

        It 'protects private worktree processes without remapping ordinary Dev cleanup permission' {
            $script:app.Package = 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr'
            $script:app.PackageFullName = 'IntelligentTerminal.Worktree.fixture_0.8.0.11_x64__rd9vj3e6a2mbr'
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*not test-owned*'
            $script:process.Closed | Should -BeFalse
            $script:ItKnownFamilies.Dev | Should -BeExactly 'IntelligentTerminal_rd9vj3e6a2mbr'
            Should -Invoke Stop-Process -Times 0
        }

        Describe 'Explicit private worktree package selection' -Tag Unit {
            InModuleScope ItE2E {
                BeforeAll {
                    $script:privateTemplate = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\..\..\build\templates\Package-DevWorktree.appxmanifest.template') -Raw
                }
                BeforeEach {
                    $script:privatePfn = 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr'
                    $script:privateClsid = '{12579AD4-2884-43B3-B558-2F5869730132}'
                    $script:registeredPrivate = [pscustomobject]@{
                        Name = 'IntelligentTerminal.Worktree.fixture'; Publisher = 'CN=Intelligent Terminal Dev'
                        PackageFamilyName = $script:privatePfn
                        PackageFullName = 'IntelligentTerminal.Worktree.fixture_0.8.0.11_x64__rd9vj3e6a2mbr'
                        Version = '0.8.0.11'; InstallLocation = 'C:\private-测试\AppX'
                    }
                    $script:privateManifest = [xml]($script:privateTemplate.Replace('__WORKTREE_TOKEN__', 'fixture').
                        Replace('__PROTOCOL_CLSID__', $script:privateClsid.Trim('{}')).
                        Replace('__DEV_PACKAGE_VERSION__', '0.8.0.11').
                        Replace('__DEV_PACKAGE_RESOURCES__', '<Resource Language="en-US" />').
                        Replace('$targetnametoken$.exe', 'WindowsTerminal.exe'))
                    Mock Get-AppxPackage { $script:registeredPrivate }
                    Mock Get-AppxPackageManifest { $script:privateManifest }
                    Mock Get-StartApps {}
                    Mock Get-Command {}
                    Mock Test-Path { $true }
                    Mock Stop-Process { throw 'Never stop private package members.' }
                    Mock Backup-WtConfig { throw 'Never back up an unverified package.' }
                    Mock Write-ItLog {}
                }
                It 'resolves the exact tokenized alias and manifest CLSID without changing ordinary descriptors' {
                    $app = Resolve-ItApp -Package $script:privatePfn
                    $app.LaunchAlias | Should -BeLike '*\IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr\wtai-fixture.exe'
                    $app.PrivateProtocolClsid | Should -BeExactly $script:privateClsid
                    $app.ComClsid | Should -BeNullOrEmpty
                    $app.WtcliPath | Should -BeExactly 'C:\private-测试\AppX\wtcli.exe'
                }
                It 'rejects malformed private selectors before package discovery' {
                    { Resolve-ItApp -Package 'IntelligentTerminal.Worktree.9bad_rd9vj3e6a2mbr' } | Should -Throw '*token*'
                    Should -Invoke Get-AppxPackage -Times 0
                }
                It 'rejects a mismatched registered manifest publisher before backup or cleanup' {
                    $script:privateManifest.Package.Identity.Publisher = 'CN=Wrong'
                    { Start-Terminal -Package $script:privatePfn } | Should -Throw '*identity/publisher*'
                    Should -Invoke Backup-WtConfig -Times 0
                    Should -Invoke Stop-Process -Times 0
                }
                It 'rejects an ordinary brand protocol CLSID' {
                    $script:privateManifest.SelectSingleNode("//*[local-name()='Class']").Id = $script:ItBrandClsids.Dev.Trim('{}')
                    { Resolve-ItApp -Package $script:privatePfn } | Should -Throw '*ordinary brand CLSID*'
                }
                It 'rejects a mismatched manifest alias' {
                    $script:privateManifest.SelectSingleNode("//*[local-name()='ExecutionAlias']").Alias = 'wtai.exe'
                    { Resolve-ItApp -Package $script:privatePfn } | Should -Throw '*tokenized alias*'
                }
                It 'rejects a missing co-located client instead of falling back to PATH' {
                    Mock Test-Path { $false } -ParameterFilter { $LiteralPath -like '*\wtcli.exe' }
                    { Resolve-ItApp -Package $script:privatePfn } | Should -Throw '*co-located binary: wtcli.exe*'
                }
                It 'rejects a protocol CLSID shared by another registered package' {
                    Mock Get-AppxPackage { @($script:registeredPrivate, [pscustomobject]@{ PackageFullName = 'another-package' }) }
                    { Resolve-ItApp -Package $script:privatePfn } | Should -Throw '*another package*'
                }
                It 'probes only the verified manifest CLSID, never any ordinary brand' {
                    $app = Resolve-ItApp -Package $script:privatePfn
                    Mock Wait-Until { & $Condition }
                    Mock Invoke-Native { [pscustomobject]@{ ExitCode = 0; StdOut = '{"windows":[]}' } }
                    Resolve-WtComClsid -App $app | Should -BeExactly $script:privateClsid
                    Should -Invoke Invoke-Native -Times 1 -Exactly -ParameterFilter { $Environment.WT_COM_CLSID -ceq $script:privateClsid }
                }
                It 'uses the per-PFN tokenized launch alias and refuses it when unavailable' {
                    $app = Resolve-ItApp -Package $script:privatePfn
                    Mock Test-Path { $false } -ParameterFilter { $LiteralPath -like '*\Microsoft\WindowsApps\*\wtai-fixture.exe' }
                    { Start-ItCreatedDevTerminal -App $app } | Should -Throw '*wtai-fixture.exe*'
                }
                It 'refuses active private processes before backup without closing them' {
                    Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 91 } }
                    Mock New-Item {}
                    { Start-Terminal -Package $script:privatePfn } | Should -Throw '*not test-owned*'
                    Should -Invoke Backup-WtConfig -Times 0
                    Should -Invoke Stop-Process -Times 0
                }
                It 'rejects a registration changed after descriptor resolution before activation' {
                    $app = Resolve-ItApp -Package $script:privatePfn
                    $script:registeredPrivate.InstallLocation = 'C:\another-layout'
                    Mock Invoke-Native { throw 'Must not probe any COM server.' }
                    { Resolve-WtComClsid -App $app } | Should -Throw '*registration changed*'
                    { Start-ItCreatedDevTerminal -App $app } | Should -Throw '*registration changed*'
                    Should -Invoke Invoke-Native -Times 0
                }
                It 'the watcher rejects <Selector> before resolution or cleanup' -TestCases @(
                    @{ Selector = 'Dev' }, @{ Selector = 'Store' }
                ) {
                    param($Selector)
                    $source = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\tests\Feature.SidebarResumeWatcher.Tests.ps1') -Raw
                    $start = $source.IndexOf('$script:package = Get-ItTestPackage', [StringComparison]::Ordinal)
                    $end = $source.IndexOf('foreach ($name in @', $start, [StringComparison]::Ordinal)
                    $preflight = [scriptblock]::Create($source.Substring($start, $end - $start))
                    Mock Get-ItTestPackage { $Selector }
                    Mock Resolve-ItApp { throw 'Must not resolve ordinary Dev or Store.' }
                    Mock Stop-StaleItInstances { throw 'Must not close any package.' }
                    { & $preflight } | Should -Throw '*explicitly selected private worktree PFN*'
                    Should -Invoke Resolve-ItApp -Times 0
                    Should -Invoke Stop-StaleItInstances -Times 0
                    Should -Invoke Stop-Process -Times 0
                }
            }
        }

        It 'also reaps a Dev embedding process that appears as the first window exits' {
            $first = $script:process
            $second = New-FakeTerminalProcess -Id 51002
            $second.MainWindowHandle = [IntPtr]::Zero
            Mock Get-WtProcessesForApp {
                if (-not $first.HasExited) { return $first }
                if (-not $second.HasExited) { return $second }
            }

            Stop-StaleItInstances -App $script:app

            $first.Closed | Should -BeTrue
            $second.Closed | Should -BeTrue
            Should -Invoke Stop-Process -Times 0
        }

        It 'reaps a Dev embedding process after an initially empty quiet-window sample' {
            $first = $script:process
            $late = New-FakeTerminalProcess -Id 51002
            $script:queries = 0
            Mock Get-WtProcessesForApp {
                $script:queries++
                if ($script:queries -eq 1) { return $first }
                if ($script:queries -eq 2) { return }
                if (-not $late.HasExited) { return $late }
            }

            Stop-StaleItInstances -App $script:app

            $first.Closed | Should -BeTrue
            $late.Closed | Should -BeTrue
            $script:queries | Should -BeGreaterThan 3
            Should -Invoke Stop-Process -Times 0
        }

        It 'closes a Dev process appearing after an initially empty discovery' {
            $late = New-FakeTerminalProcess -Id 51002
            $script:queries = 0
            Mock Get-WtProcessesForApp {
                $script:queries++
                if ($script:queries -eq 1) { return }
                if (-not $late.HasExited) { return $late }
            }

            Stop-StaleItInstances -App $script:app

            $late.Closed | Should -BeTrue
            $script:queries | Should -BeGreaterThan 2
            Should -Invoke Stop-Process -Times 0
        }

        It 'fails closed after three Dev respawns instead of retrying indefinitely' {
            $script:spawnedProcesses = @($script:process) + @(2..4 | ForEach-Object {
                New-FakeTerminalProcess -Id (51000 + $_)
            })
            Mock Get-WtProcessesForApp {
                $script:spawnedProcesses | Where-Object { -not $_.HasExited } | Select-Object -First 1
            }

            { Stop-StaleItInstances -App $script:app } |
                Should -Throw '*still active after three verified cleanup passes*'

            @($script:spawnedProcesses | Where-Object Closed).Count | Should -Be 3
            $script:spawnedProcesses[3].Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'automates the legacy cold-start entry point only for exact Dev' {
            Stop-AppInstances -App $script:app

            $script:process.Closed | Should -BeTrue
            Should -Invoke Stop-Process -Times 0
        }

        It 'closes exact Dev before Start-Terminal backs up configuration' {
            Mock Resolve-ItApp { $script:app }
            Mock Initialize-LogOffsets {}
            Mock Backup-WtConfig {
                if (-not $script:process.HasExited) { throw 'Configuration changed before Dev closed.' }
            }
            Mock Clear-WtConfig {}
            Mock Invoke-FrePass {}
            Mock Start-ItCreatedDevTerminal { throw 'owned launch sentinel' }
            Mock Restore-WtConfig {}
            $previous = $env:ITE2E_ARTIFACT_ROOT
            try {
                $env:ITE2E_ARTIFACT_ROOT = $TestDrive
                { Start-Terminal -Package Dev } | Should -Throw '*owned launch sentinel*'
            }
            finally {
                if ($null -eq $previous) { Remove-Item Env:\ITE2E_ARTIFACT_ROOT -ErrorAction SilentlyContinue }
                else { $env:ITE2E_ARTIFACT_ROOT = $previous }
            }
            $script:process.Closed | Should -BeTrue
            Should -Invoke Backup-WtConfig -Times 1
            Should -Invoke Stop-Process -Times 0
        }

        It 'force-stops a verified headless Dev helper by its exact PID' {
            $script:process = New-FakeTerminalProcess -Name wta
            $script:process.MainWindowHandle = [IntPtr]::Zero
            Mock Test-Until { $false }
            Mock Get-Process { $script:process } -ParameterFilter { $Id -eq $script:process.Id }
            Mock Stop-Process { $script:process.HasExited = $true }

            Stop-StaleItInstances -App $script:app -GraceSec 1

            $script:process.HasExited | Should -BeTrue
            Should -Invoke Stop-Process -Times 1 -ParameterFilter { $Id -eq 51001 }
        }

        It 'ignores a confirmed process exit during the initial identity snapshot' {
            Mock Get-ItCreatedProcessPackage {
                $script:process.HasExited = $true
                throw 'snapshot identity unavailable'
            }

            { Stop-StaleItInstances -App $script:app } | Should -Not -Throw

            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'retains identity failures for a process that is still alive' {
            Mock Get-ItCreatedProcessPackage { throw 'snapshot identity unavailable' }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*snapshot identity unavailable*'

            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'ignores a confirmed exit during forced-stop identity revalidation' {
            $script:process = New-FakeTerminalProcess -Name wta
            $script:identityReads = 0
            Mock Test-Until { $false }
            Mock Get-Process { $script:process } -ParameterFilter { $Id -eq 51001 }
            Mock Get-ItCreatedProcessPackage {
                $script:identityReads++
                if ($script:identityReads -gt 1) {
                    $script:process.HasExited = $true
                    throw 'snapshot identity unavailable'
                }
                $script:app.PackageFullName
            }

            { Stop-StaleItInstances -App $script:app -GraceSec 1 } | Should -Not -Throw

            Should -Invoke Stop-Process -Times 0
        }

        It 'force-stops a verified Dev helper when its managed Path is unavailable' {
            $script:process = New-FakeTerminalProcess -Name wta
            $script:process.Path = $null
            $script:live = New-FakeTerminalProcess -Name wta
            $script:live.Path = $null
            Mock Get-ItProcessImagePath { 'C:\DevPackage\AppX\wta.exe' } -ParameterFilter { $Id -eq 51001 }
            Mock Get-Process { $script:live } -ParameterFilter { $Id -eq 51001 }
            Mock Test-Until { $false }
            Mock Stop-Process { $script:process.HasExited = $true }

            Stop-StaleItInstances -App $script:app -GraceSec 1

            $script:process.HasExited | Should -BeTrue
            Should -Invoke Get-ItProcessImagePath -Times 2 -ParameterFilter { $Id -eq 51001 }
            Should -Invoke Stop-Process -Times 1 -ParameterFilter { $Id -eq 51001 }
        }

        It 'refuses a changed native path at forced shutdown even without a managed Path' {
            $script:process = New-FakeTerminalProcess -Name wta
            $script:process.Path = $null
            $script:live = New-FakeTerminalProcess -Name wta
            $script:live.Path = $null
            $script:pathLookups = 0
            Mock Get-ItProcessImagePath {
                $script:pathLookups++
                if ($script:pathLookups -eq 1) { 'C:\DevPackage\AppX\wta.exe' }
                else { 'C:\StorePackage\AppX\wta.exe' }
            } -ParameterFilter { $Id -eq 51001 }
            Mock Get-Process { $script:live } -ParameterFilter { $Id -eq 51001 }
            Mock Test-Until { $false }

            { Stop-StaleItInstances -App $script:app -GraceSec 1 } | Should -Throw '*identity changed*'

            Should -Invoke Get-ItProcessImagePath -Times 2 -ParameterFilter { $Id -eq 51001 }
            Should -Invoke Stop-Process -Times 0
        }

        It 'holds the revalidated process handle until its PID-bound stop completes' {
            $script:process = New-FakeTerminalProcess -Name wta
            $script:live = New-FakeTerminalProcess -Name wta
            [void]$script:live.PSObject.Properties.Remove('Handle')
            $script:liveHandleReads = 0
            $script:live | Add-Member ScriptProperty Handle {
                $script:liveHandleReads++
                [IntPtr]::new(42)
            }
            Mock Test-Until { $false }
            Mock Get-Process { $script:live } -ParameterFilter { $Id -eq $script:process.Id }
            Mock Stop-Process {
                $script:liveHandleReads | Should -BeGreaterThan 0
                $script:process.HasExited = $true
            }

            Stop-StaleItInstances -App $script:app -GraceSec 1

            $script:process.HasExited | Should -BeTrue
            Should -Invoke Stop-Process -Times 1 -ParameterFilter { $Id -eq 51001 }
        }

        It 'preserves a running non-Dev Intelligent Terminal window' {
            $script:app.Package = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $script:app.PackageFullName = 'Microsoft.IntelligentTerminal_1.0.0.0_x64__8wekyb3d8bbwe'
            $script:app.InstallLocation = 'C:\StorePackage\AppX'
            $script:process = New-FakeTerminalProcess -Root $script:app.InstallLocation

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*protected*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'preserves a headless non-Dev package helper before a cold start' {
            $script:app.Package = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $script:process = New-FakeTerminalProcess -Name wta -Root 'C:\StorePackage\AppX'
            $script:process.MainWindowHandle = [IntPtr]::Zero
            Mock Get-WtProcessesForApp {
                if ($IncludePackageExecutables -and -not $script:process.HasExited) { $script:process }
            }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*protected*'
            $script:process.HasExited | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
            Should -Invoke Get-WtProcessesForApp -Times 1 -ParameterFilter { $IncludePackageExecutables }
        }

        It 'waits for a protected package to remain idle throughout preflight' {
            $script:app.Package = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $script:process.HasExited = $true
            Mock Get-WtProcessesForApp {}

            Stop-StaleItInstances -App $script:app

            Should -Invoke Get-WtProcessesForApp -Times 6 -Exactly -ParameterFilter { $IncludePackageExecutables }
        }

        It 'refuses a protected package that appears during its quiet window' {
            $script:app.Package = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $script:app.InstallLocation = 'C:\StorePackage\AppX'
            $script:process = New-FakeTerminalProcess -Root $script:app.InstallLocation
            $script:queries = 0
            Mock Get-WtProcessesForApp {
                $script:queries++
                if ($script:queries -ge 3) { $script:process }
            }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*protected*'

            $script:queries | Should -Be 3
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'never treats an unverified Dev alias as the authorized family' {
            $script:app.Package = 'Dev'
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*protected*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'refuses to close the process tree hosting this chat even if it is Dev' {
            $script:process = New-FakeTerminalProcess -Id $PID
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*current chat*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'refuses to close a verified Dev ancestor hosting the current chat' {
            $script:process = New-FakeTerminalProcess -Id 51002
            $script:process.StartTime = [datetime]'2026-09-01T00:00:00Z'
            Mock Get-CimInstance {
                if ($Filter -eq "ProcessId=$PID") {
                    [pscustomobject]@{
                        ProcessId = $PID
                        ParentProcessId = 51002
                        CreationDate = [datetime]'2026-09-28T00:00:00Z'
                    }
                }
                elseif ($Filter -eq 'ProcessId=51002') {
                    [pscustomobject]@{
                        ProcessId = 51002
                        ParentProcessId = 0
                        CreationDate = [datetime]'2026-09-01T00:00:00Z'
                    }
                }
            }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*current chat process tree*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'refuses an ambiguous chat ancestry instead of guessing it is safe' {
            Mock Get-CimInstance {
                [pscustomobject]@{
                    ProcessId = $PID
                    ParentProcessId = $PID
                    CreationDate = [datetime]'2026-09-28T00:00:00Z'
                }
            }
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*chat ancestry*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'rejects a reused parent PID that started after the current chat process' {
            Mock Get-CimInstance {
                if ($Filter -eq "ProcessId=$PID") {
                    [pscustomobject]@{
                        ProcessId = $PID
                        ParentProcessId = 51002
                        CreationDate = [datetime]'2026-09-28T00:00:00Z'
                    }
                }
                elseif ($Filter -eq 'ProcessId=51002') {
                    [pscustomobject]@{
                        ProcessId = 51002
                        ParentProcessId = 0
                        CreationDate = [datetime]'2026-10-08T00:00:00Z'
                    }
                }
            }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*parent pid=51002 was reused*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'refuses a parent whose process creation time cannot be verified' {
            Mock Get-CimInstance {
                if ($Filter -eq "ProcessId=$PID") {
                    [pscustomobject]@{
                        ProcessId = $PID
                        ParentProcessId = 51002
                        CreationDate = [datetime]'2026-09-28T00:00:00Z'
                    }
                }
                elseif ($Filter -eq 'ProcessId=51002') {
                    [pscustomobject]@{ ProcessId = 51002; ParentProcessId = 0; CreationDate = $null }
                }
            }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*missing or changed process identity*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'can close a newer Dev process when an older observed chat ancestor has lost its parent' {
            Mock Get-CimInstance {
                if ($Filter -eq "ProcessId=$PID") {
                    [pscustomobject]@{
                        ProcessId = $PID
                        ParentProcessId = 111
                        CreationDate = [datetime]'2026-09-28T00:00:00Z'
                    }
                }
            }
            Stop-StaleItInstances -App $script:app
            $script:process.Closed | Should -BeTrue
        }

        It 'refuses an older Dev process when a chat parent cannot be observed' {
            $script:process.StartTime = [datetime]'2026-09-01T00:00:00Z'
            Mock Get-CimInstance {
                if ($Filter -eq "ProcessId=$PID") {
                    [pscustomobject]@{
                        ProcessId = $PID
                        ParentProcessId = 111
                        CreationDate = [datetime]'2026-09-28T00:00:00Z'
                    }
                }
            }
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*chat ancestry*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'refuses a different package identity even under the Dev directory' {
            Mock Get-ItCreatedProcessPackage { 'Microsoft.IntelligentTerminal_1.0.0.0_x64__8wekyb3d8bbwe' }
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*package identity*'
            $script:process.Closed | Should -BeFalse
            Should -Invoke Stop-Process -Times 0
        }

        It 'refuses a reused PID at forced shutdown rather than killing its replacement' {
            $script:process | Add-Member ScriptMethod CloseMainWindow {
                $this.Closed = $true
                $false
            } -Force
            $replacement = New-FakeTerminalProcess -Id $script:process.Id `
                -Root 'C:\StorePackage\AppX'
            $replacement.StartTime = $script:process.StartTime.AddMinutes(1)
            Mock Test-Until { $false }
            Mock Get-Process { $replacement } -ParameterFilter { $Id -eq $script:process.Id }

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*identity changed*'

            Should -Invoke Stop-Process -Times 0
        }

        It 'accepts a confirmed Dev exit while looking up its PID for forced cleanup' {
            $script:process | Add-Member ScriptMethod CloseMainWindow { $false } -Force
            Mock Test-Until { $false }
            Mock Get-Process {
                $script:process.HasExited = $true
                throw [Management.Automation.ErrorRecord]::new(
                    [ArgumentException]::new('Process not found'),
                    'NoProcessFoundForGivenId,Microsoft.PowerShell.Commands.GetProcessCommand',
                    [Management.Automation.ErrorCategory]::ObjectNotFound, $script:process.Id)
            } -ParameterFilter { $Id -eq $script:process.Id }

            Stop-StaleItInstances -App $script:app

            $script:process.HasExited | Should -BeTrue
            Should -Invoke Stop-Process -Times 0
        }

        It 'accepts a confirmed Dev exit after lookup but before Stop-Process' {
            $script:process | Add-Member ScriptMethod CloseMainWindow { $false } -Force
            Mock Test-Until { $false }
            Mock Get-Process { $script:process } -ParameterFilter { $Id -eq $script:process.Id }
            Mock Stop-Process {
                $script:process.HasExited = $true
                throw [Management.Automation.ErrorRecord]::new(
                    [ArgumentException]::new('Process not found'),
                    'NoProcessFoundForGivenId,Microsoft.PowerShell.Commands.StopProcessCommand',
                    [Management.Automation.ErrorCategory]::ObjectNotFound, $script:process.Id)
            }

            Stop-StaleItInstances -App $script:app

            $script:process.HasExited | Should -BeTrue
            Should -Invoke Stop-Process -Times 1 -ParameterFilter { $Id -eq 51001 }
        }

        It 'cannot select ordinary Windows Terminal as an Intelligent Terminal package' {
            Mock Get-AppxPackage {
                [pscustomobject]@{
                    PackageFamilyName = 'Microsoft.WindowsTerminal_8wekyb3d8bbwe'
                    PackageFullName = 'Microsoft.WindowsTerminal_1.0.0.0_x64__8wekyb3d8bbwe'
                    Name = 'Microsoft.WindowsTerminal'
                    Version = [version]'1.0.0.0'
                    InstallLocation = 'C:\StockPackage\AppX'
                }
            }
            Mock Get-StartApps { @() }
            Mock Get-Command { $null }
            Mock Test-Path { $true }

            { Resolve-ItApp -Package 'Microsoft.WindowsTerminal_8wekyb3d8bbwe' } |
                Should -Throw '*ordinary Windows Terminal*'
            Should -Invoke Stop-Process -Times 0
        }
    }
}

Describe 'Chat ancestry with transient Dev clients' -Tag Unit {
    InModuleScope ItE2E {
        BeforeEach {
            Mock Get-CimInstance {
                if ($Filter -eq "ProcessId=$PID") {
                    [pscustomobject]@{
                        ProcessId = $PID
                        ParentProcessId = 111
                        CreationDate = [datetime]'2026-09-28T00:00:00Z'
                    }
                }
            }
        }

        It 'ignores an exited client whose start time is no longer available' {
            $dead = [pscustomobject]@{ Id = 51002; HasExited = $true; StartTime = $null }
            $live = [pscustomobject]@{ Id = 51003; HasExited = $false; StartTime = [datetime]'2026-10-08T00:00:00Z' }

            { Assert-ItDevProcessesNotChat -Processes @($dead, $live) } | Should -Not -Throw
        }

        It 'does not require ancestry discovery when every candidate has exited' {
            Mock Get-CimInstance { throw 'No live shutdown target remains.' }
            $dead = [pscustomobject]@{ Id = 51002; HasExited = $true; StartTime = $null }

            { Assert-ItDevProcessesNotChat -Processes @($dead) } | Should -Not -Throw
            Should -Invoke Get-CimInstance -Times 0
        }

        It 'ignores a client that exits while its start time is being read' {
            $exiting = [pscustomobject]@{ Id = 51002; HasExited = $false }
            $exiting | Add-Member ScriptProperty StartTime { $this.HasExited = $true; $null }

            { Assert-ItDevProcessesNotChat -Processes @($exiting) } | Should -Not -Throw
        }

        It 'still refuses a live client with an unavailable start time' {
            $live = [pscustomobject]@{ Id = 51002; HasExited = $false; StartTime = $null }

            { Assert-ItDevProcessesNotChat -Processes @($live) } | Should -Throw '*chat ancestry*'
        }
    }
}

Describe 'Combined sidebar cold-start observation' -Tag Unit {
    BeforeAll {
        $source = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1') -Raw
        $start = $source.IndexOf('$script:package = Get-ItTestPackage', [StringComparison]::Ordinal)
        $end = $source.IndexOf('if (-not $env:ITE2E_EXPECTED_APP_SHA256', $start, [StringComparison]::Ordinal)
        if ($start -lt 0 -or $end -le $start) { throw 'Combined sidebar preflight boundaries were not found.' }
        $script:combinedPreflight = [scriptblock]::Create($source.Substring($start, $end - $start))
    }

    BeforeEach {
        $script:cleanupCompleted = $false
        $script:initialProcessCheckAt = $null
        $script:initialProcesses = $null
        Mock Get-ItTestPackage { 'Dev' }
        Mock Resolve-ItApp { [pscustomobject]@{ Package = 'IntelligentTerminal_rd9vj3e6a2mbr' } }
        Mock Stop-StaleItInstances { $script:cleanupCompleted = $true }
        Mock Get-WtProcessesForApp {
            if (-not $script:cleanupCompleted) { throw 'Inactivity was observed before Dev preflight cleanup.' }
            @()
        }
    }

    It 'records a fresh package-wide inactivity observation after Dev cleanup' {
        . $script:combinedPreflight

        $script:initialProcessCheckAt | Should -Not -BeNullOrEmpty
        $script:initialProcesses | Should -HaveCount 0
        Should -Invoke Stop-StaleItInstances -Times 1 -Exactly
        Should -Invoke Get-WtProcessesForApp -Times 1 -Exactly -ParameterFilter { $IncludePackageExecutables }
    }

    It 'passes the verified worktree identity to the existing protected-package preflight' {
        Mock Get-ItTestPackage { 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr' }
        Mock Resolve-ItApp {
            [pscustomobject]@{
                Package = 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr'
                PrivateProtocolClsid = '{12579AD4-2884-43B3-B558-2F5869730132}'
            }
        }

        . $script:combinedPreflight

        $script:initialProcesses | Should -HaveCount 0
        Should -Invoke Resolve-ItApp -Times 1 -Exactly -ParameterFilter {
            $Package -eq 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr'
        }
        Should -Invoke Stop-StaleItInstances -Times 1 -Exactly -ParameterFilter {
            $App.Package -eq 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr'
        }
    }

    It 'rejects non-Dev selections without verified worktree metadata before cleanup' -ForEach @(
        @{ Selector = 'Store' }
        @{ Selector = 'IntelligentTerminal.Worktree.fixture_rd9vj3e6a2mbr' }
    ) {
        Mock Get-ItTestPackage { $Selector }

        { . $script:combinedPreflight } | Should -Throw '*verified worktree Dev package*'

        Should -Invoke Stop-StaleItInstances -Times 0
        Should -Invoke Get-WtProcessesForApp -Times 0
    }

    It 'refuses a late Dev arrival instead of fabricating an inactive recovery receipt' {
        Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 51002 } }

        { . $script:combinedPreflight } | Should -Throw '*existing Dev process*'

        $script:initialProcesses.Id | Should -Be 51002
        Should -Invoke Stop-StaleItInstances -Times 1 -Exactly
    }

    It 'surfaces an unreadable post-cleanup package observation' {
        Mock Get-WtProcessesForApp { throw 'post-cleanup discovery unavailable' }

        { . $script:combinedPreflight } | Should -Throw '*post-cleanup discovery unavailable*'
    }
}

Describe 'Feature suites honor Dev-only cold start policy' -Tag Unit {
    It 'keeps PromptQueue failure recovery refusal-only after the preflight' {
        $path = Join-Path $PSScriptRoot '..\tests\Feature.PromptQueue.Tests.ps1'
        $tokens = $null
        $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        @($errors) | Should -HaveCount 0
        $recovery = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CatchClauseAst] -and
                $node.Extent.Text -match 'Restore-WtConfig -App \$recoveryApp'
        }, $true))
        $recovery | Should -HaveCount 1
        $commands = @($recovery[0].FindAll({
            param($node) $node -is [Management.Automation.Language.CommandAst]
        }, $true) | ForEach-Object { $_.GetCommandName() })
        $commands | Should -Contain 'Assert-WtPackageInactive'
        $commands | Should -Not -Contain 'Stop-StaleItInstances'
        [array]::IndexOf($commands, 'Assert-WtPackageInactive') |
            Should -BeLessThan ([array]::IndexOf($commands, 'Restore-WtConfig'))
    }

    It '<Suite> enters shared cleanup before its own setup' -ForEach @(
        @{ Suite = 'Feature.AcpAuthentication.Tests.ps1' }
        @{ Suite = 'Feature.AgentInputMouseCursor.Tests.ps1' }
        @{ Suite = 'Feature.AgentInputUndoRedo.Tests.ps1' }
        @{ Suite = 'Feature.AgentMouse.Tests.ps1' }
        @{ Suite = 'Feature.AgentPaneLifetime.Tests.ps1' }
        @{ Suite = 'Feature.AgentsModeActions.Tests.ps1' }
        @{ Suite = 'Feature.CombinedAgentsSidebar.Tests.ps1' }
        @{ Suite = 'Feature.FreFlow.Tests.ps1' }
        @{ Suite = 'Feature.McpDelegatedAgentIdentity.Tests.ps1' }
        @{ Suite = 'Feature.PaneProgress.Tests.ps1' }
        @{ Suite = 'Feature.Paste.Tests.ps1' }
        @{ Suite = 'Feature.PinnedTabSelection.Tests.ps1' }
        @{ Suite = 'Feature.SessionRefresh.Tests.ps1' }
        @{ Suite = 'Feature.SidebarProviderAppearance.Tests.ps1' }
        @{ Suite = 'Feature.SidebarRelativeTime.Tests.ps1' }
        @{ Suite = 'Feature.SidebarSessionScroll.Tests.ps1' }
        @{ Suite = 'Feature.SidebarTelemetry.Tests.ps1' }
        @{ Suite = 'Feature.SidebarUpgrade.Tests.ps1' }
        @{ Suite = 'Feature.TelemetryFunnels.Tests.ps1' }
        @{ Suite = 'Feature.YoloMode.Tests.ps1' }
    ) {
        $path = Join-Path $PSScriptRoot "..\tests\$Suite"
        $tokens = $null
        $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        @($errors) | Should -HaveCount 0
        $calls = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Stop-StaleItInstances'
        }, $true))
        $entryFor = {
            param($command)
            for ($parent = $command.Parent; $parent; $parent = $parent.Parent) {
                if ($parent -is [Management.Automation.Language.FunctionDefinitionAst]) { return }
                if ($parent -is [Management.Automation.Language.CommandAst] -and
                    $parent.GetCommandName() -in @('BeforeAll', 'BeforeEach', 'AfterAll', 'AfterEach', 'It')) {
                    return $parent
                }
            }
        }
        $startupCalls = @($calls | Where-Object {
            $entry = & $entryFor $_
            $entry -and $entry.GetCommandName() -in @('BeforeAll', 'BeforeEach')
        })
        $startupCalls.Count | Should -BeGreaterThan 0
        $setupCalls = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -in @(
                'Backup-WtConfig', 'Clear-WtConfig', 'Set-WtSettings', 'Set-WtSetting',
                'Set-WtState', 'Invoke-FrePass', 'Start-Terminal', 'Start-TerminalFre',
                'Initialize-TelemetryPolicyTransaction')
        }, $true))
        foreach ($cleanup in $startupCalls) {
            $entry = & $entryFor $cleanup
            if (@($startupCalls | Where-Object {
                (& $entryFor $_) -eq $entry -and $_.Extent.StartOffset -lt $cleanup.Extent.StartOffset
            }).Count) { continue }
            foreach ($setup in $setupCalls) {
                if ((& $entryFor $setup) -eq $entry) {
                    $cleanup.Extent.StartOffset | Should -BeLessThan $setup.Extent.StartOffset `
                        -Because "$Suite must close Dev before $($setup.GetCommandName())"
                }
            }
        }
    }
}
