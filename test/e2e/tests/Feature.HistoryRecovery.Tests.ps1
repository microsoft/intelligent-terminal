#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real private-package history recovery and process cleanup; no model requests.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
}

Describe 'Feature: History recovery' -Tag 'Feature', 'HistoryRecovery' -Skip:($env:ITE2E_PACKAGE -notlike 'IntelligentTerminal.Worktree.*') {
    BeforeAll {
        $ErrorActionPreference = 'Stop'
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        if ($env:ITE2E_HOOK_MUTATIONS_DISABLED -cne '1') {
            throw 'History recovery acceptance requires an isolated build with hook mutations disabled.'
        }
        $script:recoveryTarget = Resolve-ItApp -Package (Get-ItTestPackage)
        foreach ($entry in @(
            @{ Path = $script:recoveryTarget.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 },
            @{ Path = (Join-Path $script:recoveryTarget.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 }
        )) {
            if ($entry.Hash -notmatch '^[a-fA-F0-9]{64}$') { throw 'Exact private build hashes are required.' }
            (Get-FileHash -LiteralPath $entry.Path).Hash | Should -Be $entry.Hash
        }
        Assert-WtPackageInactive -App $script:recoveryTarget
        Backup-WtConfig -App $script:recoveryTarget
        $script:recoveryOwnsConfig = $true
        $script:recoveryConfigHashes = @{}
        foreach ($file in @($script:recoveryTarget.SettingsPath, $script:recoveryTarget.StatePath)) {
            if (Test-Path -LiteralPath "$file.e2ebak") {
                $script:recoveryConfigHashes[$file] = (Get-FileHash -LiteralPath "$file.e2ebak").Hash
            }
        }
        $script:recoveryEvidence = Join-Path $env:ITE2E_ARTIFACT_ROOT ('history-recovery-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Force -Path $script:recoveryEvidence | Out-Null
        $script:recoveryRequests = Join-Path $script:recoveryEvidence 'requests.log'
        $script:recoveryHistory = Join-Path $script:recoveryEvidence 'history.json'
        $script:recoveryControl = Join-Path $script:recoveryEvidence 'history-control.json'
        $script:recoveryStartup = Join-Path $script:recoveryEvidence 'startup.json'
        $script:recoverySavedId = 'history-recovery-' + [guid]::NewGuid().ToString('N')
        @{ mode = 'ready' } | ConvertTo-Json | Set-Content -LiteralPath $script:recoveryControl -Encoding utf8
        @{ mode = 'ready' } | ConvertTo-Json | Set-Content -LiteralPath $script:recoveryStartup -Encoding utf8
        @{ sessions = @(@{
            sessionId = $script:recoverySavedId
            cwd = $script:recoveryEvidence
            title = 'History cached before failure'
            updatedAt = [DateTimeOffset]::UtcNow.ToString('o')
        }) } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $script:recoveryHistory -Encoding utf8
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:recoveryRequests.Replace("'", "''"))' -HistoryPath '$($script:recoveryHistory.Replace("'", "''"))' -HistoryControlPath '$($script:recoveryControl.Replace("'", "''"))' -StartupControlPath '$($script:recoveryStartup.Replace("'", "''"))'"
        $script:recoveryEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $command = "pwsh.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand $($script:recoveryEncoded)"
        $script:recoveryEnvironment = @{}
        foreach ($name in @('PATH','WT_WTCLI_PATH','WTCLI_PATH','WT_COM_CLSID')) {
            $script:recoveryEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        }
        $env:PATH = "$($script:recoveryTarget.InstallLocation);$env:PATH"
        $env:WT_WTCLI_PATH = $script:recoveryTarget.WtcliPath
        $env:WTCLI_PATH = $script:recoveryTarget.WtcliPath
        $env:WT_COM_CLSID = $script:recoveryTarget.PrivateProtocolClsid
        $profile = '{84a4c418-c123-4def-bcdd-cd6c9c641264}'
        @{
            acpAgent = 'custom:pwsh'
            acpCustomCommand = $command
            acpModel = ''
            autoFixEnabled = $false
            agentSessionManagementEnabled = $true
            language = 'en-US'
            defaultProfile = $profile
            profiles = @{
                defaults = @{ environment = @{
                    PATH = $env:PATH
                    WT_WTCLI_PATH = $script:recoveryTarget.WtcliPath
                    WTCLI_PATH = $script:recoveryTarget.WtcliPath
                } }
                list = @(@{
                    guid = $profile
                    name = 'History recovery acceptance'
                    commandline = 'cmd.exe'
                    startingDirectory = $script:recoveryEvidence
                })
            }
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $script:recoveryTarget.SettingsPath -Encoding utf8
        @{ agentFreCompleted = $true; sidebarIntroductionShown = $true; sidebarLayoutMigrationCompleted = $true } |
            ConvertTo-Json | Set-Content -LiteralPath $script:recoveryTarget.StatePath -Encoding utf8
        $script:recoveryApp = Start-Terminal -Package (Get-ItTestPackage) -Backup $false -CleanSettings $false -PassFre $false
        function script:Get-RecoveryRows {
            $app = $script:recoveryApp
            $master = Get-WtaMasterPipe -App $app
            (Invoke-Wta -App $app -Arguments @(
                'sessions','list','--master',$master,'--json','--include-status','--origin','all'
            )).sessions
        }
    }

    AfterAll {
        try {
            if ($script:recoveryApp) { Stop-Terminal -App $script:recoveryApp -RestoreSettings $false }
        }
        finally {
            if ($script:recoveryOwnsConfig) {
                Assert-WtPackageInactive -App $script:recoveryTarget
                Restore-WtConfig -App $script:recoveryTarget
                foreach ($file in $script:recoveryConfigHashes.Keys) {
                    (Get-FileHash -LiteralPath $file).Hash | Should -Be $script:recoveryConfigHashes[$file]
                }
            }
            foreach ($name in $script:recoveryEnvironment.Keys) {
                [Environment]::SetEnvironmentVariable($name, $script:recoveryEnvironment[$name], 'Process')
            }
        }
    }

    It 'History recovery pauses after repeated failed query cycles' {
        $app = $script:recoveryApp
        Wait-Until -TimeoutSec 90 -Because 'the initial dedicated history query to populate its cached row' -Condition {
            @(Get-RecoveryRows | Where-Object session_id -eq $script:recoverySavedId).Count -eq 1
        } | Out-Null
        $mainId = [int](Wait-Until -TimeoutSec 30 -Because 'the fixture chat session to be created' -Condition {
            $entry = Get-Content -LiteralPath $script:recoveryRequests |
                Where-Object { $_ -match '^\d+\|new\|' } | Select-Object -First 1
            if ($entry) { [int]$entry.Split('|')[0] }
        })
        $main = Get-Process -Id $mainId -ErrorAction Stop
        $null = $main.Handle
        $mainStart = $main.StartTime.ToUniversalTime().Ticks
        $mainPath = $main.Path
        @{ mode = 'fail'; spawnChild = $true } | ConvertTo-Json |
            Set-Content -LiteralPath $script:recoveryControl -Encoding utf8
        Wait-Until -TimeoutSec 90 -Because 'three failed complete history cycles to pause recovery' -Condition {
            (Get-ItLogText -App $app -Name 'wta-main_master.log' -SinceStart) -match
                '(?m)^(?=.*automatic history recovery paused)(?=.*custom:pwsh).*$'
        } | Out-Null
        $failed = @(Get-Content -LiteralPath $script:recoveryRequests |
            Where-Object { $_ -match '^\d+\|history-failed\|' })
        $failed.Count | Should -Be 3 -Because 'retirement and cleanup cannot spend another query-cycle slot'
        @($failed | ForEach-Object { $_.Split('|')[0] } | Select-Object -Unique).Count | Should -Be 3
        $children = @(Get-Content -LiteralPath $script:recoveryRequests |
            Where-Object { $_ -match '^\d+\|child\|' })
        $children.Count | Should -Be 3
        Wait-Until -TimeoutSec 15 -Because 'all failed history descendants to exit before acceptance' -Condition {
            foreach ($line in $children) {
                $record = $line.Split('|',5)
                $child = Get-Process -Id ([int]$record[2]) -ErrorAction SilentlyContinue
                if ($child -and $child.StartTime.ToUniversalTime().Ticks -eq [long]$record[3] -and $child.Path -eq $record[4]) {
                    return $false
                }
            }
            $true
        } | Out-Null
        $main.HasExited | Should -BeFalse -Because 'history failures cannot retire the healthy chat process'
        $main.StartTime.ToUniversalTime().Ticks | Should -Be $mainStart
        $main.Path | Should -Be $mainPath
        @(Get-RecoveryRows | Where-Object session_id -eq $script:recoverySavedId)[0].title |
            Should -Be 'History cached before failure'
        $master = Get-WtaMasterPipe -App $app
        Invoke-Wta -App $app -Arguments @('sessions','refresh','--master',$master,'--json') | Out-Null
        $observationClock = [Diagnostics.Stopwatch]::StartNew()
        $observation = Wait-Until -TimeoutSec 70 -IntervalSec 1 -Because 'paused history to remain stable across timer ticks' -Condition {
            $count = @(Get-Content -LiteralPath $script:recoveryRequests |
                Where-Object { $_ -match '^\d+\|history-failed\|' }).Count
            if ($count -gt 3 -or $observationClock.Elapsed.TotalSeconds -ge 65) {
                @{ count = $count }
            }
        }
        $observation.count | Should -Be 3
        @(Get-Content -LiteralPath $script:recoveryRequests | Where-Object { $_ -match '^\d+\|history-failed\|' }).Count |
            Should -Be 3 -Because 'ordinary refresh and timer ticks cannot reopen the breaker'

        $current = Get-CimInstance Win32_Process -Filter "ProcessId=$mainId"
        $main.HasExited | Should -BeFalse
        $current.CreationDate.ToUniversalTime().Ticks | Should -Be ($mainStart - $mainStart % 10)
        $ownedMaster = Get-CimInstance Win32_Process -Filter "ProcessId=$($current.ParentProcessId)"
        $current.ExecutablePath | Should -Be $mainPath
        $current.CommandLine | Should -Match ([regex]::Escape($script:recoveryEncoded))
        $ownedMaster.ExecutablePath | Should -Be $app.WtaPath
        $ownedMaster.ParentProcessId | Should -Be $app.Pid
        $ownedMaster.CommandLine | Should -Match '^\s*(?:"[^"]+"|\S+)\s+--master\s+'
        $offset = (Get-ItLogText -App $app -Name 'wta-main_master.log').Length
        Stop-Process -Id $mainId -ErrorAction Stop
        Wait-Until -TimeoutSec 30 -Because 'the exact old custom chat agent to be reaped' -Condition {
            $log = Get-ItLogText -App $app -Name 'wta-main_master.log'
            $log.Substring($offset) -match '(?m)^(?=.*dead agent reaped)(?=.*custom:pwsh)(?=.*pool_entry_removed=true).*$'
        } | Out-Null
        @{ sessions = @(@{
            sessionId = $script:recoverySavedId
            cwd = $script:recoveryEvidence
            title = 'History recovered after retirement'
            updatedAt = [DateTimeOffset]::UtcNow.ToString('o')
        }) } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $script:recoveryHistory -Encoding utf8
        @{ mode = 'ready' } | ConvertTo-Json | Set-Content -LiteralPath $script:recoveryControl -Encoding utf8
        Invoke-Wta -App $app -Arguments @('sessions','refresh','--master',$master,'--json','--retry-history') | Out-Null
        New-WtTab -App $app -Command 'cmd.exe' -Title 'History recovery after retirement' | Out-Null
        Wait-Until -TimeoutSec 90 -Because 'the retained gate to produce fresh history after explicit retry' -Condition {
            @(Get-RecoveryRows | Where-Object {
                $_.session_id -eq $script:recoverySavedId -and $_.title -eq 'History recovered after retirement'
            }).Count -eq 1
        } | Out-Null
        @(Get-Content -LiteralPath $script:recoveryRequests | Where-Object { $_ -match '^\d+\|prompt\|' }).Count |
            Should -Be 0
        @(Get-Content -LiteralPath $script:recoveryRequests | Where-Object { $_ -match '\|method\|session/prompt$' }).Count |
            Should -Be 0
        @{ old_chat_pid = $mainId; failed_history_processes = $failed; descendants = $children;
            paused_cycles = 3; no_model_prompt = $true; retired_gate_recovered = $true } |
            ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $script:recoveryEvidence 'acceptance.json') -Encoding utf8
    }
}
