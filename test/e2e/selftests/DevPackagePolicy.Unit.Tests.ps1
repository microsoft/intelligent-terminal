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
                [pscustomobject]@{ ProcessId = $PID; ParentProcessId = 0 }
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
            Should -Invoke Get-WtProcessesForApp -ParameterFilter { $IncludePackageExecutables } -Times 1
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

        It 'preserves a running non-Dev Intelligent Terminal window' {
            $script:app.Package = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $script:app.PackageFullName = 'Microsoft.IntelligentTerminal_1.0.0.0_x64__8wekyb3d8bbwe'
            $script:app.InstallLocation = 'C:\StorePackage\AppX'
            $script:process = New-FakeTerminalProcess -Root $script:app.InstallLocation

            { Stop-StaleItInstances -App $script:app } | Should -Throw '*protected*'
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

        It 'refuses an ambiguous chat ancestry instead of guessing it is safe' {
            Mock Get-CimInstance {
                [pscustomobject]@{ ProcessId = $PID; ParentProcessId = $PID }
            }
            { Stop-StaleItInstances -App $script:app } | Should -Throw '*chat ancestry*'
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
