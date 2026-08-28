#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Hermetic unit tests for the ItE2E framework core. No deployed terminal required.
#   Invoke-Pester test/e2e/selftests -Tag Unit

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
}

Describe 'Invoke-Native' -Tag 'Unit' {
    It 'captures stdout and a zero exit code' {
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'echo', 'hello-ite2e')
        $r.ExitCode | Should -Be 0
        $r.StdOut | Should -Match 'hello-ite2e'
    }

    It 'propagates a non-zero exit code' {
        (Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'exit', '7')).ExitCode | Should -Be 7
    }

    It 'does NOT truncate large output (regression for the async-read race)' {
        # Produce ~500 lines; the old Register-ObjectEvent impl truncated to a few bytes.
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'for /L %i in (1,1,500) do @echo line%i')
        $r.ExitCode | Should -Be 0
        ([regex]::Matches($r.StdOut, 'line\d+')).Count | Should -Be 500
    }

    It 'times out and flags TimedOut' {
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'ping', '-n', '10', '127.0.0.1') -TimeoutSec 1
        $r.TimedOut | Should -BeTrue
        $r.ExitCode | Should -Be -1
    }
}

Describe 'Wait-Until / Test-Until' -Tag 'Unit' {
    It 'returns the truthy value once the condition holds' {
        $script:n = 0
        $v = Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Condition { $script:n++; if ($script:n -ge 3) { 'ready' } else { $null } }
        $v | Should -Be 'ready'
    }
    It 'throws on timeout' {
        { Wait-Until -TimeoutSec 1 -IntervalSec 0.2 -Condition { $false } -Because 'never' } | Should -Throw
    }
    It 'Test-Until returns a boolean and never throws' {
        Test-Until -TimeoutSec 1 -IntervalSec 0.2 -Condition { $false } | Should -BeFalse
        Test-Until -TimeoutSec 2 -IntervalSec 0.1 -Condition { $true } | Should -BeTrue
    }
}

Describe 'JSON helpers' -Tag 'Unit' {
    It 'ConvertFrom-JsonSafe returns $null on garbage' {
        ConvertFrom-JsonSafe -InputObject 'not json {' | Should -BeNullOrEmpty
        ConvertFrom-JsonSafe -InputObject '' | Should -BeNullOrEmpty
    }
    It 'ConvertFrom-JsonSafe parses valid JSON' {
        (ConvertFrom-JsonSafe -InputObject '{"a":1,"b":[2,3]}').b[1] | Should -Be 3
    }
    It 'ConvertFrom-JsonC strips // comments and trailing commas' {
        $jsonc = @"
{
    // a comment
    "acpAgent": "copilot",
    "autoFixEnabled": true,
}
"@
        $o = ConvertFrom-JsonC -Text $jsonc
        $o.acpAgent | Should -Be 'copilot'
        $o.autoFixEnabled | Should -BeTrue
    }

    It 'Set-WtSetting verifies nested object arrays structurally' {
        $settingsPath = Join-Path $TestDrive 'nested-settings.json'
        '{}' | Set-Content -LiteralPath $settingsPath -Encoding utf8
        $app = [pscustomobject]@{ SettingsPath = $settingsPath }
        $providers = @(@{
            id = 'provider-local'
            models = @(@{ id = 'test-model'; name = 'Test Model' })
        })

        Set-WtSetting -App $app -Key 'customModelProviders' -Value $providers | Out-Null

        $stored = Get-WtSetting -App $app -Key 'customModelProviders'
        @($stored) | Should -HaveCount 1
        $stored[0].id | Should -Be 'provider-local'
        $stored[0].models[0].name | Should -Be 'Test Model'
    }
}

Describe 'Agent settings cleanup' -Tag 'Unit' {
    It 'removes showTokenUsageAndCost while preserving profiles' {
        $settingsPath = Join-Path $TestDrive 'settings.json'
        @{
            defaultProfile = '{6239a42c-1111-49a3-80bd-e8fdd045185c}'
            profiles = @(@{
                name = 'p0'
                guid = '{6239a42c-1111-49a3-80bd-e8fdd045185c}'
            })
            acpAgent = 'copilot'
            showTokenUsageAndCost = $true
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $settingsPath -Encoding utf8
        $app = [pscustomobject]@{ SettingsPath = $settingsPath }

        Clear-WtConfig -App $app

        $settings = Get-WtSettingsObject -App $app
        $settings.PSObject.Properties.Name | Should -Not -Contain 'showTokenUsageAndCost'
        $settings.PSObject.Properties.Name | Should -Not -Contain 'acpAgent'
        $settings.profiles[0].name | Should -Be 'p0'
    }
}

Describe 'Configuration backup and restore' -Tag 'Unit' {
    It 'restores existing configuration content' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'existing-settings.json'
            StatePath = Join-Path $TestDrive 'existing-state.json'
        }
        '{"original":"settings"}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{"original":"state"}' | Set-Content -LiteralPath $app.StatePath -Encoding utf8

        Backup-WtConfig -App $app
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{}' | Set-Content -LiteralPath $app.StatePath -Encoding utf8
        Restore-WtConfig -App $app

        Get-Content -LiteralPath $app.SettingsPath -Raw | Should -Match '"original":"settings"'
        Get-Content -LiteralPath $app.StatePath -Raw | Should -Match '"original":"state"'
    }

    It 'removes files created when the original configuration was absent' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'missing-settings.json'
            StatePath = Join-Path $TestDrive 'missing-state.json'
        }

        Backup-WtConfig -App $app
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{}' | Set-Content -LiteralPath $app.StatePath -Encoding utf8
        Restore-WtConfig -App $app

        Test-Path -LiteralPath $app.SettingsPath | Should -BeFalse
        Test-Path -LiteralPath $app.StatePath | Should -BeFalse
    }

    It 'recovers a stale missing-file marker before taking a new snapshot' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'stale-settings.json'
            StatePath = Join-Path $TestDrive 'stale-state.json'
        }
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        [System.IO.File]::WriteAllBytes("$($app.SettingsPath).e2ebak.missing", [byte[]]::new(0))

        Backup-WtConfig -App $app

        Test-Path -LiteralPath $app.SettingsPath | Should -BeFalse
        Test-Path -LiteralPath "$($app.SettingsPath).e2ebak.missing" | Should -BeTrue
        Restore-WtConfig -App $app
    }

    It 'recovers a stale backup before taking a new snapshot' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'crashed-settings.json'
            StatePath = Join-Path $TestDrive 'crashed-state.json'
        }
        '{"test":"contaminated"}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{"original":"settings"}' | Set-Content -LiteralPath "$($app.SettingsPath).e2ebak" -Encoding utf8

        Backup-WtConfig -App $app

        Get-Content -LiteralPath $app.SettingsPath -Raw | Should -Match '"original":"settings"'
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        Restore-WtConfig -App $app
        Get-Content -LiteralPath $app.SettingsPath -Raw | Should -Match '"original":"settings"'
    }
}

Describe 'Resolve-ItApp' -Tag 'Unit' {
    It 'resolves a descriptor with the expected shape when a package is installed' {
        $installed = Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }
        if (-not $installed) { Set-ItResult -Skipped -Because 'no IT package installed'; return }
        $app = Resolve-ItApp -Package Auto
        $app.Package | Should -Match 'IntelligentTerminal'
        $app.AppUserModelId | Should -Match '!'
        $app.SettingsPath | Should -Match 'LocalState\\settings\.json$'
        $app.WtcliPath | Should -Not -BeNullOrEmpty
    }
}

Describe 'Feature suite package selection' -Tag 'Unit' {
    It 'resolves the Yolo suite package once and uses it for every app operation' {
        $suitePath = Join-Path $PSScriptRoot '..\tests\Feature.YoloMode.Tests.ps1'
        $suite = Get-Content -LiteralPath $suitePath -Raw

        ([regex]::Matches($suite, '\bGet-ItTestPackage\b')).Count | Should -Be 1
        ([regex]::Matches($suite, '(?m)^Describe ')).Count | Should -Be 4
        ([regex]::Matches($suite, '(?m)^Describe .* -ForEach \$script:PackageCase\b')).Count | Should -Be 4
        $suite | Should -Not -Match '-Package\s+Dev\b'
        $suite | Should -Not -Match 'Resolve-ItApp\s+-Package\s+(?!\$(?:script:Package|Package)\b)'
        $suite | Should -Not -Match 'Start-Terminal\s+-Package\s+(?!\$Package\b)'
    }
}

Describe 'Start-Terminal startup ordering' -Tag 'Unit' {
    It 'waits for the first window before probing COM' {
        InModuleScope ItE2E {
            $script:startupOrder = @()
            $root = Join-Path $env:TEMP "ite2e-startup-order-$PID"
            $fakeApp = [pscustomobject]@{
                Package = 'IntelligentTerminal_rd9vj3e6a2mbr'
                Version = '0.0.0.0'
                WtcliPath = 'wtcli.exe'
                LocalStateDir = $root
                SettingsPath = Join-Path $root 'settings.json'
                StatePath = Join-Path $root 'state.json'
                AppUserModelId = 'IntelligentTerminal_rd9vj3e6a2mbr!App'
                LaunchAlias = $null
                ComClsid = $null
                Pid = $null
                Hwnd = $null
            }

            Mock Resolve-ItApp { $fakeApp }
            Mock Stop-StaleItInstances
            Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 4242 } }
            Mock Start-Process
            Mock Get-WtWindowHwnds {
                $script:startupOrder += 'hwnd'
                [pscustomobject]@{ pid = 4242; hwnd = 9001; title = 'PowerShell' }
            }
            Mock Resolve-WtComClsid {
                $script:startupOrder += 'com'
                '{D5B7C9E1-4F6A-4B8C-D9E0-F1A2B3C4D5E6}'
            }
            Mock Get-ActivePane { [pscustomobject]@{ window_id = 1 } }
            Mock Initialize-LogOffsets
            Mock Write-ItLog

            $app = Start-Terminal -Package Dev -PassFre $false -Backup $false -CleanSettings $false

            $app.Hwnd | Should -Be 9001
            $script:startupOrder | Should -Be @('hwnd', 'com')
        }
    }
}

Describe 'Agent readiness log boundary' -Tag 'Unit' {
    It 'checks failures only in the current launch log slice' {
        InModuleScope ItE2E {
            $script:observedOffset = $null
            $app = [pscustomobject]@{
                LogStartOffset = @{ 'wta-main_helper-old.log' = 200 }
                PreLaunchLogStartOffset = @{ 'wta-main_helper-old.log' = 100 }
            }

            Mock Open-AgentPane
            Mock Get-AgentConnectedPlaceholderRegex { 'connected-never-matches' }
            Mock Get-AgentPaneText { '' }
            Mock Get-ItLogText {
                param($App)
                $script:observedOffset = $App.LogStartOffset['wta-main_helper-old.log']
                'class=auth_required'
            }
            Mock Write-ItLog

            Wait-AgentReady -App $app -TimeoutSec 1 | Should -BeFalse
            $script:observedOffset | Should -Be 100
        }
    }

    It 'matches native Yolo updates by launch slice and ACP session' {
        InModuleScope ItE2E {
            $script:observedOffset = $null
            $app = [pscustomobject]@{
                LogStartOffset = @{ 'wta-main_helper-old.log' = 200 }
                PreLaunchLogStartOffset = @{ 'wta-main_helper-old.log' = 100 }
            }
            Mock Get-ItLogText {
                param($App)
                $script:observedOffset = $App.LogStartOffset['wta-main_helper-old.log']
                'session_id=session-a provider-native Yolo updated for live session enabled=true'
            }

            Test-AgentNativeYoloUpdate -App $app -AcpSessionId 'session-a' -Enabled $true | Should -BeTrue
            Test-AgentNativeYoloUpdate -App $app -AcpSessionId 'session-b' -Enabled $true | Should -BeFalse
            Test-AgentNativeYoloUpdate -App $app -AcpSessionId 'session-a' -Enabled $false | Should -BeFalse
            $script:observedOffset | Should -Be 100
        }
    }
}
