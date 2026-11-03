#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Packaged WTA -> deterministic ACP -> actual filesystem, without a window or login.

Describe 'Feature Antigravity runtime cleanup' -Tag 'Feature', 'AntigravityRuntime' {
    BeforeAll {
        $ErrorActionPreference = 'Stop'
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $selector = Get-ItTestPackage
        $family = if ($selector -eq 'Dev') { 'IntelligentTerminal_rd9vj3e6a2mbr' } else { $selector }
        if ($family -notmatch '^IntelligentTerminal(?:\.Worktree\.[A-Za-z][A-Za-z0-9]{0,20})?_rd9vj3e6a2mbr$') {
            throw 'Runtime cleanup requires Dev or an explicit isolated worktree Dev package.'
        }
        $packages = @(Get-AppxPackage | Where-Object PackageFamilyName -CEQ $family)
        $packages.Count | Should -Be 1
        $script:package = $packages[0]
        $script:binary = Join-Path $script:package.InstallLocation 'wta.exe'
        $env:ITE2E_EXPECTED_WTA_SHA256 | Should -Match '^[a-fA-F0-9]{64}$'
        (Get-FileHash -LiteralPath $script:binary).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        [xml]$manifest = Get-Content -LiteralPath (Join-Path $script:package.InstallLocation 'AppxManifest.xml') -Raw
        $aliases = @($manifest.SelectNodes(
            "//*[local-name()='Extension' and @Category='windows.appExecutionAlias' and @Executable='wta.exe']//*[local-name()='ExecutionAlias']"))
        $aliases.Count | Should -Be 1
        $script:alias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\$family\$($aliases[0].GetAttribute('Alias'))"
        Test-Path -LiteralPath $script:alias | Should -BeTrue
        $script:runtimeRoot = Join-Path $env:LOCALAPPDATA "Packages\$family\LocalCache\acp"
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path $artifactRoot ('antigravity-runtime-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence | Out-Null
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path

        function script:Invoke-RuntimeFixture {
            param([string]$Mode, [string]$AgentId = 'antigravity', [hashtable]$Environment)
            $run = Join-Path $script:evidence ([guid]::NewGuid().ToString('N'))
            New-Item -ItemType Directory -Path $run | Out-Null
            $control = Join-Path $run 'startup.json'
            @{ mode = $Mode } | ConvertTo-Json | Set-Content -LiteralPath $control -Encoding utf8
            $receipt = Join-Path $run 'runtime.json'
            $log = Join-Path $run 'requests.log'
            $invocation = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($log.Replace("'", "''"))' -StartupControlPath '$($control.Replace("'", "''"))' -RuntimeReceiptPath '$($receipt.Replace("'", "''"))' -ExpectedRuntimeRoot '$($script:runtimeRoot.Replace("'", "''"))'"
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
            $result = Invoke-Native -FilePath $script:alias -Arguments @(
                'verify-acp', '--agent-id', $AgentId, '--agent',
                "pwsh.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand $encoded"
            ) -Environment $Environment -TimeoutSec 90
            $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $run 'result.json') -Encoding utf8
            $result.TimedOut | Should -BeFalse
            [pscustomobject]@{
                Result = $result
                Receipt = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json
                Requests = Get-Content -LiteralPath $log -Raw
            }
        }

        function script:Initialize-CaptureParent {
            if ($script:captureParent) { return }
            $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
            $native = Join-Path $script:evidence ('native-parent-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
            New-Item -ItemType Directory -Path $native | Out-Null
            $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
            $vs = Invoke-Native -FilePath $vswhere -Arguments @(
                '-latest', '-prerelease', '-products', '*', '-requires',
                'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath') -TimeoutSec 10
            if ($vs.ExitCode -ne 0 -or -not $vs.StdOut.Trim()) { throw 'Native capture fixture needs the installed MSVC toolchain.' }
            $vcvars = Join-Path $vs.StdOut.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
            $targets = Get-Content -LiteralPath (Join-Path $root 'src\common.nugetversions.targets') -Raw
            $wilPackages = @([regex]::Matches($targets, 'Microsoft\.Windows\.ImplementationLibrary\.[0-9.]+') |
                ForEach-Object Value | Sort-Object -Unique)
            if ($wilPackages.Count -ne 1) { throw 'The source must select one WIL package version.' }
            $wil = Join-Path $root "packages\$($wilPackages[0])\include"
            if (-not (Test-Path -LiteralPath (Join-Path $wil 'wil\resource.h'))) {
                throw 'Restore the source WIL dependency before compiling the native capture fixture.'
            }
            $source = Join-Path $root 'test\e2e\fixtures\WtaCaptureParent.cpp'
            $output = Join-Path $native 'WtaCaptureParent.exe'
            $build = "call `"$vcvars`" >nul && cl /nologo /EHsc /std:c++20 /MT /I`"$wil`" `"$source`" /Fe:`"$output`" /Fo:`"$native\parent.obj`" /link /INCREMENTAL:NO ole32.lib oleaut32.lib runtimeobject.lib advapi32.lib"
            $installer = Split-Path -Parent $vswhere
            $buildScript = "`$env:PATH='$($installer.Replace("'", "''"));'+`$env:PATH; & `$env:ComSpec /d /c '$($build.Replace("'", "''"))'; exit `$LASTEXITCODE"
            $buildEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($buildScript))
            $result = Invoke-Native -FilePath (Get-Process -Id $PID).Path -Arguments @(
                '-NoProfile', '-EncodedCommand', $buildEncoded
            ) -WorkingDirectory $native -TimeoutSec 90
            if ($result.ExitCode -ne 0 -or $result.TimedOut) { throw "Native capture fixture did not compile: $($result.StdOut) $($result.StdErr)" }
            $script:captureWta = Join-Path $native 'wta.exe'
            Copy-Item -LiteralPath $script:binary -Destination $script:captureWta
            (Get-FileHash -LiteralPath $script:captureWta).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
            $script:captureParent = $output
        }

        function script:Invoke-CapturedRuntimeFixture {
            param([string]$Mode, [string]$AgentId = 'antigravity')
            Initialize-CaptureParent
            $runId = 'it-capture-' + [guid]::NewGuid().ToString('N').Substring(0, 12)
            $run = Join-Path ([IO.Path]::GetTempPath()) $runId
            $local = Join-Path $run 'local'
            $temp = Join-Path $run 'temp'
            New-Item -ItemType Directory -Path $local, $temp | Out-Null
            $control = Join-Path $run 'startup.json'
            @{ mode = 'hold' } | ConvertTo-Json | Set-Content -LiteralPath $control -Encoding utf8
            $receipt = Join-Path $run 'runtime.json'
            $log = Join-Path $run 'requests.log'
            $runtimeRoot = Join-Path $local 'IntelligentTerminal\acp'
            $invocation = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($log.Replace("'", "''"))' -StartupControlPath '$($control.Replace("'", "''"))' -RuntimeReceiptPath '$($receipt.Replace("'", "''"))' -ExpectedRuntimeRoot '$($runtimeRoot.Replace("'", "''"))'"
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
            $command = "verify-acp --agent-id $AgentId --agent `"pwsh.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand $encoded`""
            $ready = if ($Mode -eq 'cancel') { "$receipt.ready" } else { '-' }
            $timeout = if ($Mode -eq 'cancel') { '20000' } else { '10000' }
            $result = Invoke-Native -FilePath $script:captureParent -Arguments @(
                $script:captureWta, $command, $timeout, $ready
            ) -Environment @{ LOCALAPPDATA = $local; APPDATA = $local; TEMP = $temp; TMP = $temp } -TimeoutSec 45
            $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $run 'parent-result.json') -Encoding utf8
            $result.ExitCode | Should -Be 0
            $result.TimedOut | Should -BeFalse
            $runtime = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json
            $child = Get-Process -Id $runtime.pid -ErrorAction SilentlyContinue
            if ($child -and $child.StartTime.ToUniversalTime().Ticks -eq $runtime.processStartTicks) {
                $child.WaitForExit(5000) | Should -BeTrue -Because 'the exact fixture child must exit before fixture cleanup'
            }
            Copy-Item -LiteralPath $run -Destination (Join-Path $script:evidence $runId) -Recurse
            [pscustomobject]@{
                Result = $result.StdOut | ConvertFrom-Json
                Receipt = $runtime
                Root = $runtimeRoot
                Run = $run
            }
        }
    }

    It 'Antigravity ACP verification reclaims its dedicated temporary files' {
        $instances = [Collections.Generic.HashSet[string]]::new()
        foreach ($mode in @('ready', 'fail', 'ready', 'fail')) {
            $run = Invoke-RuntimeFixture -Mode $mode
            $response = $run.Result.StdOut | ConvertFrom-Json
            if ($mode -eq 'ready') {
                $run.Result.ExitCode | Should -Be 0
                $response.startup_verified | Should -BeTrue
            } else {
                $run.Result.ExitCode | Should -Not -Be 0
                $response.startup_failure.phase | Should -Be 'initialize'
            }
            $run.Receipt.inExpectedRoot | Should -BeTrue -Because 'the live payload must have been in this package LocalCache'
            $run.Receipt.temp | Should -BeExactly $run.Receipt.tmp
            $run.Receipt.instance | Should -Match '^[a-f0-9]{12}$'
            $instances.Add($run.Receipt.instance) | Should -BeTrue
            Test-Path -LiteralPath $run.Receipt.extraction | Should -BeFalse
            Test-Path -LiteralPath (Join-Path $script:runtimeRoot $run.Receipt.instance) | Should -BeFalse
            Test-Path -LiteralPath (Join-Path $script:runtimeRoot "$($run.Receipt.instance).jsonl") | Should -BeFalse
            $run.Requests | Should -Match '\|method\|initialize'
            $run.Requests | Should -Not -Match '\|method\|(authenticate|session/new|session/prompt)'
        }
    }

    It 'Antigravity runtime cleanup leaves custom command temporary files untouched' {
        $customTemp = Join-Path $script:evidence 'custom-temp'
        New-Item -ItemType Directory -Path $customTemp | Out-Null
        try {
            $run = Invoke-RuntimeFixture -Mode 'ready' -AgentId 'custom:antigravity' -Environment @{
                TEMP = $customTemp
                TMP = $customTemp
            }
            $run.Result.ExitCode | Should -Be 0
            $run.Receipt.temp | Should -BeExactly $customTemp
            $run.Receipt.inExpectedRoot | Should -BeFalse
            Test-Path -LiteralPath (Join-Path $run.Receipt.extraction 'payload.dll') | Should -BeTrue
        }
        finally {
            Remove-Item -LiteralPath $customTemp -Recurse -Force
        }
    }

    It 'Antigravity captured startup cancellation and timeout reclaim owned files' {
        foreach ($mode in @('cancel', 'timeout')) {
            $run = Invoke-CapturedRuntimeFixture -Mode $mode
            try {
                $run.Result.completed | Should -BeFalse
                $run.Result.cancelled | Should -Be ($mode -eq 'cancel')
                $run.Result.timedOut | Should -Be ($mode -eq 'timeout')
                $run.Receipt.inExpectedRoot | Should -BeTrue
                Test-Path -LiteralPath $run.Receipt.extraction | Should -BeFalse -Because 'a surviving native parent must retire the captured runtime before returning'
                Test-Path -LiteralPath (Join-Path $run.Root "$($run.Receipt.instance).jsonl") | Should -BeFalse
            }
            finally {
                Remove-Item -LiteralPath $run.Run -Recurse -Force
            }
        }
    }

    It 'Captured custom startup retains its original cancellation and temporary environment' {
        $run = Invoke-CapturedRuntimeFixture -Mode 'cancel' -AgentId 'custom:antigravity'
        try {
            $run.Result.cancelled | Should -BeTrue
            $run.Result.timedOut | Should -BeFalse
            $run.Result.completed | Should -BeFalse
            $run.Receipt.inExpectedRoot | Should -BeFalse
            Test-Path -LiteralPath (Join-Path $run.Receipt.extraction 'payload.dll') | Should -BeTrue
        }
        finally {
            Remove-Item -LiteralPath $run.Run -Recurse -Force
        }
    }
}
