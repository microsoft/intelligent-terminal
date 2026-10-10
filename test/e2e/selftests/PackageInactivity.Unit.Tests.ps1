Describe 'Package inactivity limited identity query' -Tag Unit {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\ItE2E\Public\Harness.ps1')
        if (-not (Get-Command Get-ItProcessImagePath -ErrorAction SilentlyContinue)) {
            function Get-ItProcessImagePath { param([int]$Id) throw 'Fallback not implemented.' }
        }
        $script:app = [pscustomobject]@{ InstallLocation = 'C:\owned-dev\AppX' }
    }

    BeforeEach {
        Mock Get-ChildItem { [pscustomobject]@{ BaseName = 'OpenConsole' } }
        Mock Get-Process { [pscustomobject]@{ Id = 123; ProcessName = 'OpenConsole'; Path = $null; HasExited = $false } }
    }

    It 'Does not confuse a proven stock console with the selected Dev package' {
        Mock Get-ItProcessImagePath { 'C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_test\OpenConsole.exe' }
        @(Get-WtProcessesForApp -App $script:app -IncludePackageExecutables).Count | Should -Be 0
        Should -Invoke Get-ItProcessImagePath -Exactly 1
    }

    It 'Still refuses a proven active Dev process' {
        Mock Get-ItProcessImagePath { 'C:\owned-dev\AppX\OpenConsole.exe' }
        @(Get-WtProcessesForApp -App $script:app -IncludePackageExecutables).Count | Should -Be 1
    }

    It 'refuses a late Dev process during recovery without shutting it down' {
        Mock Get-ItProcessImagePath { 'C:\owned-dev\AppX\OpenConsole.exe' }
        Mock Stop-Process { throw 'Recovery must not terminate a late arrival.' }

        { Assert-WtPackageInactive -App $script:app } | Should -Throw '*pre-existing or unknown package processes*'

        Should -Invoke Stop-Process -Times 0
    }

    It 'Still fails closed if the read-only identity cannot be established' {
        Mock Get-ItProcessImagePath { throw 'Access denied.' }
        { Get-WtProcessesForApp -App $script:app -IncludePackageExecutables } |
            Should -Throw '*Cannot establish package inactivity*'
    }

    It 'Keeps legacy descendant ID discovery independent of optional root timestamps' {
        Mock Get-CimInstance {
            @(
                [pscustomobject]@{ ProcessId=123; ParentProcessId=0; Name='WindowsTerminal.exe'; CreationDate=[datetime]'2026-10-08T00:00:00Z' }
                [pscustomobject]@{ ProcessId=456; ParentProcessId=123; Name='wta.exe'; CreationDate=[datetime]'2026-10-08T00:00:01Z' }
            )
        }
        @(Get-DescendantWtaIds -RootPid 123) | Should -Be @(456)
    }

    It 'Requires the root timestamp when returning creation-proven process objects' {
        Mock Get-CimInstance {
            [pscustomobject]@{ ProcessId=123; ParentProcessId=0; Name='WindowsTerminal.exe'; CreationDate=[datetime]'2026-10-08T00:00:00Z' }
        }
        { Get-DescendantWtaIds -RootPid 123 -AsProcess } | Should -Throw '*RootStartTime*'
    }
}
