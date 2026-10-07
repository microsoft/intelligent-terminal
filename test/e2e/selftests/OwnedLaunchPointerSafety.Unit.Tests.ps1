BeforeDiscovery {
    if (-not (Get-Module ItE2E)) { Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force }
}
Describe 'Package membership never establishes launch or cleanup ownership' {
    InModuleScope ItE2E {
        BeforeEach {
            Mock Write-ItLog {}
            Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 991; Path = 'C:\owned\WindowsTerminal.exe' } }
            Mock Stop-Process { throw 'Must not kill an unowned process' }
            Mock Backup-WtConfig { throw 'Must not mutate configuration' }
            Mock Resolve-ItApp { [pscustomobject]@{ Package = 'Dev'; InstallLocation = 'C:\owned'; WtcliPath = 'owned' } }
            Mock New-Item {}
        }
        It 'refuses inherited cold-start callers rather than closing package members' {
            $app = Resolve-ItApp -Package Dev
            { Stop-AppInstances -App $app } | Should -Throw '*not test-owned*'
            { Stop-StaleItInstances -App $app } | Should -Throw '*not test-owned*'
            Should -Invoke Stop-Process -Times 0
        }
        It 'refuses startup before any backup or activation when an unowned process already exists' {
            { Start-Terminal -Package Dev } | Should -Throw '*not test-owned*'
            Should -Invoke Backup-WtConfig -Times 0
            Should -Invoke Stop-Process -Times 0
        }
        It 'surfaces failed inactivity discovery without assuming permission to kill or restore' {
            Mock Get-WtProcessesForApp { throw 'inactivity unknown' }
            { Stop-AppInstances -App (Resolve-ItApp -Package Dev) } | Should -Throw '*inactivity unknown*'
            Should -Invoke Stop-Process -Times 0
        }
        It 'retains a completed backup when an unowned process arrives before activation' {
            $script:probes = 0
            Mock Get-WtProcessesForApp {
                $script:probes++
                if ($script:probes -gt 1) { [pscustomobject]@{ Id = 992; Path = 'C:\owned\WindowsTerminal.exe' } }
            }
            Mock Resolve-ItApp {
                [pscustomobject]@{ Package = 'Dev'; InstallLocation = 'C:\owned'; WtcliPath = 'owned'
                    LocalStateDir = 'C:\nonexistent-ite2e-state'; SettingsPath = 'C:\nonexistent-ite2e-state\settings.json'
                    StatePath = 'C:\nonexistent-ite2e-state\state.json'; AppUserModelId = 'exact-package!App' }
            }
            Mock Initialize-LogOffsets {}
            Mock Backup-WtConfig {}
            Mock Clear-WtConfig {}
            Mock Invoke-FrePass {}
            Mock Restore-WtConfig { throw 'Must retain the backup while unknown processes remain' }
            Mock Invoke-ItTerminalActivation { throw 'Must not activate after unknown arrival' }
            { Start-Terminal -Package Dev } | Should -Throw '*not test-owned*'
            Should -Invoke Backup-WtConfig -Times 1 -Exactly
            Should -Invoke Invoke-ItTerminalActivation -Times 0
            Should -Invoke Restore-WtConfig -Times 0
            Should -Invoke Stop-Process -Times 0
        }
    }
}
Describe 'Pointer safety rejects individual hostile observations before delivery' {
    InModuleScope ItE2E {
        It 'refuses <Threat> without sending mouse input' -ForEach @(
            @{ Threat = 'held button'; Key = 'NoHeldInput' },
            @{ Threat = 'foreign cover'; Key = 'NativeHit' },
            @{ Threat = 'PID reuse'; Key = 'Lease' },
            @{ Threat = 'unknown overlay'; Key = 'NoOverlay' },
            @{ Threat = 'tooltip cover'; Key = 'NoOverlay' },
            @{ Threat = 'opaque root'; Key = 'DeepHit' },
            @{ Threat = 'stale peer'; Key = 'StablePeer' },
            @{ Threat = 'foreign foreground'; Key = 'Foreground' },
            @{ Threat = 'changed cursor'; Key = 'Cursor' },
            @{ Threat = 'missing run receipt'; Key = 'RunReceipt' }
        ) {
            $facts = @{ Lease = $true; RunReceipt = $true; NativeRoot = $true; Foreground = $true
                NativeHit = $true; DeepHit = $true; StablePeer = $true; VisiblePeer = $true
                Cursor = $true; NoHeldInput = $true; NoOverlay = $true }
            $facts[$Key] = $false
            { Assert-ItPointerFacts -Facts $facts } | Should -Throw "*$Key*"
        }
        It 'the actual mouse sender rechecks observations and cannot bypass a rejected lease' {
            Mock Get-ItOwnedPointerPeer { throw 'reused process lease' }
            { Send-ItPointerButton -App ([pscustomobject]@{}) -Flag 2 -X 1 -Y 1 } |
                Should -Throw '*reused process lease*'
            Should -Invoke Get-ItOwnedPointerPeer -Times 1 -Exactly
        }
        It 'never relays opaque winapp drag delivery' {
            { Invoke-WinAppUi -App ([pscustomobject]@{}) -UiArgs @('drag', '1,1', '2,2') } |
                Should -Throw '*Opaque winapp drag*'
        }
    }
}
