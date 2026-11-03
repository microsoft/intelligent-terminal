#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real Terminal/SharedWta lifecycle with an isolated, deterministic native ACP fixture.

Describe 'Feature Antigravity controlled runtime lifecycle' -Tag 'Feature', 'AntigravityRuntimeLifecycle' {
    BeforeAll {
        $ErrorActionPreference = 'Stop'
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:selector = Get-ItTestPackage
        if ($script:selector -notmatch '^IntelligentTerminal\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20}_rd9vj3e6a2mbr$') {
            throw 'Use an explicitly selected, inactive private Dev package; ordinary Dev and user windows are protected.'
        }
        $script:target = Resolve-ItApp -Package $script:selector
        foreach ($entry in @(
            @{ Path = $script:target.WtaPath; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 },
            @{ Path = (Join-Path $script:target.InstallLocation 'TerminalApp.dll'); Hash = $env:ITE2E_EXPECTED_APP_SHA256 }
        )) {
            $entry.Hash | Should -Match '^[a-fA-F0-9]{64}$'
            (Get-FileHash -LiteralPath $entry.Path).Hash | Should -Be $entry.Hash
        }
        Assert-WtPackageInactive -App $script:target
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $paths = @($env:PATH, [Environment]::GetEnvironmentVariable('Path', 'User'),
            [Environment]::GetEnvironmentVariable('Path', 'Machine')) -join ';'
        foreach ($directory in $paths.Split(';')) {
            if ($directory.Trim() -and (Test-Path -LiteralPath (Join-Path ([Environment]::ExpandEnvironmentVariables($directory.Trim('" '))) 'agy_acp_server.exe'))) {
                throw 'A PATH ACP server would override the isolated deterministic fixture; no real provider will be launched by this suite.'
            }
        }
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:runId = [guid]::NewGuid().ToString('N')
        $script:evidence = Join-Path $artifactRoot ('runtime-lifecycle-' + $script:runId)
        New-Item -ItemType Directory -Path $script:evidence | Out-Null
        $script:runtimeRoot = Join-Path $env:LOCALAPPDATA "Packages\$($script:target.Package)\LocalCache\acp"
        $script:receipts = Join-Path $script:evidence 'runtime-receipts'
        New-Item -ItemType Directory -Path $script:receipts | Out-Null
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $source = Get-Content -LiteralPath (Join-Path $root 'tools\wta\src\antigravity_acp.rs') -Raw
        $version = [regex]::Match($source, 'const VERSION: &str = "([^"]+)"').Groups[1].Value
        $archiveHash = [regex]::Match($source, 'Self::WindowsX64 => "([a-f0-9]{64})"').Groups[1].Value
        if (-not $version -or -not $archiveHash) { throw 'Could not read the source standalone-install fixture contract.' }
        $script:install = Join-Path $env:LOCALAPPDATA "Packages\$($script:target.Package)\LocalCache\Local\IntelligentTerminal\agents\antigravity-acp\$version\windows-x86_64"
        if (Test-Path -LiteralPath $script:install) { throw 'The private package already has a provider installation; it will not be replaced.' }
        New-Item -ItemType Directory -Path $script:install | Out-Null
        [IO.File]::WriteAllText((Join-Path $script:install '.ite2e-owner'), $script:runId)

        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $vs = Invoke-Native -FilePath $vswhere -Arguments @(
            '-latest', '-prerelease', '-products', '*', '-requires',
            'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath') -TimeoutSec 10
        if ($vs.ExitCode -ne 0 -or -not $vs.StdOut.Trim()) { throw 'MSVC is required for the deterministic native ACP fixture.' }
        $vcvars = Join-Path $vs.StdOut.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
        $nativeSource = Join-Path $root 'test\e2e\fixtures\Mock-StandaloneAcp.cpp'
        $script:fakeServer = Join-Path $script:install 'agy_acp_server.exe'
        $command = "call `"$vcvars`" >nul && cl /nologo /EHsc /std:c++20 /MT `"$nativeSource`" /Fe:`"$script:fakeServer`" /Fo:`"$script:evidence\acp.obj`" /link /INCREMENTAL:NO"
        $build = "`$env:PATH='$((Split-Path $vswhere -Parent).Replace("'", "''"));'+`$env:PATH; & `$env:ComSpec /d /c '$($command.Replace("'", "''"))'; exit `$LASTEXITCODE"
        $encodedBuild = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($build))
        $compiled = Invoke-Native -FilePath (Get-Process -Id $PID).Path -Arguments @('-NoProfile', '-EncodedCommand', $encodedBuild) -TimeoutSec 90
        if ($compiled.ExitCode -ne 0 -or $compiled.TimedOut) { throw "Native ACP fixture build failed: $($compiled.StdOut) $($compiled.StdErr)" }
        Copy-Item -LiteralPath $script:fakeServer -Destination (Join-Path $script:install 'localharness_external.exe')
        $files = foreach ($name in @('agy_acp_server.exe', 'localharness_external.exe')) {
            $path = Join-Path $script:install $name
            @{ name = $name; bytes = (Get-Item -LiteralPath $path).Length; sha256 = (Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant() }
        }
        @{
            agent = 'antigravity'; version = $version; platform = 'windows-x86_64'
            archive_sha256 = $archiveHash; files = @($files)
        } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $script:install '.intelligent-terminal-acp.json') -Encoding utf8
        $invocation = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\agy.log' -RuntimeReceiptDirectory '$($script:receipts.Replace("'", "''"))' -ExpectedRuntimeRoot '$($script:runtimeRoot.Replace("'", "''"))'"
        [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)) |
            Set-Content -LiteralPath (Join-Path $script:install 'fixture-command.txt') -Encoding ascii
        $control = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\control.log'"
        $script:controlCommand = 'pwsh.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand ' +
            [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($control))
        Backup-WtConfig -App $script:target
        $script:ownsConfig = $true
        Set-WtState -App $script:target -Key sidebarIntroductionShown -Value $true | Out-Null
        Set-WtState -App $script:target -Key sidebarLayoutMigrationCompleted -Value $true | Out-Null
        $profile = '{a8f6a6c7-1ca3-43df-85f8-0391e12ba3c7}'
        $script:app = Start-Terminal -Package $script:selector -Backup $false -CleanSettings $false -PassFre $true -Settings @{
            acpAgent = 'custom:runtime-control'; acpCustomCommand = $script:controlCommand; acpModel = ''
            agentSessionManagementEnabled = $false; autoFixEnabled = $false; autoErrorDetectionEnabled = $false
            language = 'en-US'; tabLayout = 'horizontal'; 'warning.confirmOnClose' = 'never'
            defaultProfile = $profile; startupActions = ''
            profiles = @{
                defaults = @{ environment = @{ WT_WTCLI_PATH = $script:target.WtcliPath; WTCLI_PATH = $script:target.WtcliPath } }
                list = @(@{ guid = $profile; name = 'Runtime lifecycle shell'; commandline = 'pwsh.exe -NoLogo -NoProfile'; startingDirectory = $script:evidence })
            }
        }

        function script:Get-OwnedMaster {
            $pipe = Get-WtaMasterPipe -App $script:app
            $items = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($script:app.Pid) AND Name='wta.exe'" |
                Where-Object {
                    $_.ExecutablePath -ieq $script:app.WtaPath -and
                    $_.CommandLine -match '^\s*(?:"[^"]+"|\S+)\s+--master\s+' -and $_.CommandLine.Contains($pipe)
                })
            if ($items.Count -ne 1) { throw 'Expected exactly one master owned by the test Terminal.' }
            $process = Get-Process -Id $items[0].ProcessId
            if ($process.Path -ine $script:app.WtaPath) { throw 'The owned master executable changed.' }
            [pscustomobject]@{ ProcessId = $process.Id; StartTicks = $process.StartTime.ToUniversalTime().Ticks }
        }

        function script:Wait-AgyRuntime {
            param([uint32]$NotMaster = 0)
            Wait-Until -TimeoutSec 60 -IntervalSec 0.2 -Because 'a live fixture runtime owned by the current master' -Condition {
                $master = Get-OwnedMaster
                if ($master.ProcessId -eq $NotMaster) { return $false }
                foreach ($ready in @(Get-ChildItem -LiteralPath $script:receipts -Filter '*.json.ready' -File)) {
                    $path = $ready.FullName.Substring(0, $ready.FullName.Length - '.ready'.Length)
                    $receipt = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
                    $child = Get-CimInstance Win32_Process -Filter "ProcessId=$($receipt.pid)"
                    if (-not $child) { continue }
                    $shim = Get-CimInstance Win32_Process -Filter "ProcessId=$($child.ParentProcessId)"
                    if ($shim -and $shim.ExecutablePath -ieq $script:fakeServer -and $shim.ParentProcessId -eq $master.ProcessId) {
                        $process = Get-Process -Id $receipt.pid
                        if ($process.StartTime.ToUniversalTime().Ticks -eq $receipt.processStartTicks) {
                            return [pscustomobject]@{ Master = $master; Receipt = $receipt }
                        }
                    }
                }
                return $false
            }
        }

        function script:Start-AgyRuntime {
            Set-WtAgent -App $script:app -Agent 'antigravity' | Out-Null
            Open-AgentPane -App $script:app | Out-Null
            $runtime = Wait-AgyRuntime
            $runtime.Receipt.inExpectedRoot | Should -BeTrue
            $runtime.Receipt.parentScope | Should -Match '^[a-f0-9]{32}$'
            $runtime
        }

        function script:Assert-OldRuntimeRemoved {
            param($Old)
            Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because 'old runtime payload and ownership record to be reclaimed' -Condition {
                -not (Test-Path -LiteralPath $Old.Receipt.extraction) -and
                    -not (Test-Path -LiteralPath (Join-Path $script:runtimeRoot "$($Old.Receipt.instance).jsonl"))
            } | Out-Null
        }
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app -RestoreSettings $false -GraceSec 20 }
        if ($script:ownsConfig) {
            Assert-WtPackageInactive -App $script:target
            Restore-WtConfig -App $script:target
        }
        if ($script:install -and (Test-Path -LiteralPath (Join-Path $script:install '.ite2e-owner'))) {
            if ((Get-Content -LiteralPath (Join-Path $script:install '.ite2e-owner') -Raw) -cne $script:runId) {
                throw 'The fixture installation ownership changed; preserving it.'
            }
            Assert-WtPackageInactive -App $script:target
            Remove-Item -LiteralPath $script:install -Recurse -Force
        }
    }

    It 'Antigravity assistant restart reclaims the previous runtime' {
        $old = Start-AgyRuntime
        Send-AgentPrompt -App $script:app -Text '/restart' | Out-Null
        $next = Wait-AgyRuntime -NotMaster $old.Master.ProcessId
        Assert-OldRuntimeRemoved $old
        $next.Receipt.instance | Should -Not -Be $old.Receipt.instance
        Test-Path -LiteralPath $next.Receipt.extraction | Should -BeTrue
        (Get-Process -Id $script:app.Pid).HasExited | Should -BeFalse
    }

    It 'Antigravity settings replacement reclaims its runtime without breaking the replacement provider' {
        $old = Start-AgyRuntime
        Set-WtAgent -App $script:app -Agent 'custom:runtime-control' | Out-Null
        Wait-Until -TimeoutSec 60 -IntervalSec 0.2 -Because 'settings to replace the shared master' -Condition {
            (Get-OwnedMaster).ProcessId -ne $old.Master.ProcessId
        } | Out-Null
        Assert-OldRuntimeRemoved $old
        $marker = 'SCROLL_TURN_01_' + [guid]::NewGuid().ToString('N')
        Send-AgentPrompt -App $script:app -Text $marker | Out-Null
        Wait-Until -TimeoutSec 30 -IntervalSec 0.2 -Because 'the replacement deterministic provider to remain usable' -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 100) -match [regex]::Escape("ACK_$marker")
        } | Out-Null
    }

    It 'Terminal recovers Antigravity runtime after its owned WTA master exits' {
        $old = Start-AgyRuntime
        $process = Get-Process -Id $old.Master.ProcessId
        if ($process.Path -ine $script:app.WtaPath -or
            $process.StartTime.ToUniversalTime().Ticks -ne $old.Master.StartTicks) {
            throw 'Master identity changed before the owned exit injection.'
        }
        Stop-Process -Id $process.Id -ErrorAction Stop
        $next = Wait-AgyRuntime -NotMaster $old.Master.ProcessId
        Assert-OldRuntimeRemoved $old
        Test-Path -LiteralPath $next.Receipt.extraction | Should -BeTrue
        (Get-Process -Id $script:app.Pid).HasExited | Should -BeFalse
    }

    It 'Normal Terminal shutdown reclaims its Antigravity runtime' {
        $old = Start-AgyRuntime
        $process = $script:app.OwnedProcess
        if ($process.Id -ne $script:app.Pid -or $process.Path -ine $script:app.WindowsTerminal) {
            throw 'The owned Terminal identity changed before normal close.'
        }
        $process.CloseMainWindow() | Should -BeTrue
        $process.WaitForExit(20000) | Should -BeTrue
        Assert-OldRuntimeRemoved $old
    }
}
