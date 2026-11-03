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
}
