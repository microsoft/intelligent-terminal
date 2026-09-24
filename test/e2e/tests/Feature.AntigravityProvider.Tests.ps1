#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Packaged provider registration and native source discovery only; no model requests.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = [bool](Resolve-ItApp -Package (Get-ItTestPackage) -IfInstalled)
}

Describe 'Feature Antigravity provider discovery' -Tag 'Feature', 'Antigravity' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:appInfo = Resolve-ItApp -Package (Get-ItTestPackage)
        $script:wta = & (Get-Module ItE2E) {
            param($App)
            Get-RunnableWtaPath -App $App
        } $script:appInfo
        if ($env:ITE2E_EXPECTED_WTA_SHA256) {
            (Get-FileHash -LiteralPath $script:appInfo.WtaPath -Algorithm SHA256).Hash |
                Should -Be $env:ITE2E_EXPECTED_WTA_SHA256 -Because 'the packaged probe must match the intended source build'
        }
    }

    It 'Antigravity is registered as a standalone ACP provider' {
        $probe = Invoke-Native -FilePath $script:wta -Arguments @('probe-host-agents') -TimeoutSec 30
        $probe.ExitCode | Should -Be 0 -Because 'the packaged availability probe must complete'
        $snapshot = $probe.StdOut | ConvertFrom-Json -Depth 20
        $entry = @($snapshot.availability | Where-Object id -CEQ 'antigravity')
        $entry.Count | Should -Be 1 -Because 'the built-in family must cross the packaged WTA availability boundary'
        $entry[0].display_name | Should -BeExactly 'Google Antigravity'
        $entry[0].requires_npx | Should -BeFalse
        $entry[0].launch_ready | Should -Be $entry[0].native_cli_found
        @($snapshot.availability.id) | Should -Contain 'copilot'
    }

    It 'Antigravity WSL discovery requires the native Linux ACP executable' {
        $distro = $env:ITE2E_ANTIGRAVITY_WSL_DISTRO
        if ([string]::IsNullOrWhiteSpace($distro)) {
            Set-ItResult -Skipped -Because 'set ITE2E_ANTIGRAVITY_WSL_DISTRO to an installed native ACP test environment'
            return
        }
        $native = Invoke-Native -FilePath 'wsl.exe' -Arguments @(
            '-d', $distro, '--', 'bash', '-lc', 'command -v agy_acp_server.par'
        ) -TimeoutSec 30
        if ($native.ExitCode -ne 0 -or -not $native.StdOut.Trim()) {
            Set-ItResult -Skipped -Because 'the selected distro does not have the native Antigravity ACP server on PATH'
            return
        }
        $native.StdOut.Trim() | Should -Not -Match '^/mnt/' -Because 'Windows interop cannot substitute for the native Linux server'
        $probe = Invoke-Native -FilePath $script:wta -Arguments @(
            'probe-agent-sources', '--wsl-distro', $distro
        ) -TimeoutSec 45
        $probe.ExitCode | Should -Be 0
        $snapshot = $probe.StdOut | ConvertFrom-Json -Depth 20
        $snapshot.wsl_distro | Should -BeExactly $distro
        @($snapshot.agents | ForEach-Object { $_.id }) |
            Should -Contain 'antigravity' -Because 'native ACP discovery must not look for a program named after the provider ID'
    }
}
