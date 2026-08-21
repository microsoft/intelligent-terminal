#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #418 / issue #286: packaged WTA must resolve aliases defined only in the
# user's PowerShell profile, not just commands visible to a -NoProfile probe.
#
# The test temporarily replaces the current-host profile with one unique alias,
# invokes the exact wta.exe shipped in the Dev package, and restores the original
# profile bytes in finally/AfterAll.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = $null -ne (Get-Command pwsh.exe -ErrorAction Stop)
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature: packaged WTA command resolution' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Resolve-ItApp -Package Dev
        $script:pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $script:profileState = $null

        $script:RestoreProfile = {
            if (-not $script:profileState) {
                return
            }

            if ($script:profileState.Existed) {
                [System.IO.File]::WriteAllBytes($script:profileState.Path, $script:profileState.Bytes)
            }
            elseif (Test-Path -LiteralPath $script:profileState.Path) {
                Remove-Item -LiteralPath $script:profileState.Path -Force
            }
            $script:profileState = $null
        }
    }

    AfterAll {
        & $script:RestoreProfile
    }

    It 'Profile-defined PowerShell aliases resolve through packaged WTA' {
        $profilePath = (& $script:pwsh -NoProfile -NonInteractive -Command '$PROFILE.CurrentUserCurrentHost').Trim()
        $profilePath | Should -Not -BeNullOrEmpty -Because 'pwsh must expose its current-user profile path'

        $profileExisted = Test-Path -LiteralPath $profilePath
        $script:profileState = [pscustomobject]@{
            Path = $profilePath
            Existed = $profileExisted
            Bytes = if ($profileExisted) { [System.IO.File]::ReadAllBytes($profilePath) } else { $null }
        }

        $aliasName = "ite2e-profile-alias-$([guid]::NewGuid().ToString('N'))"
        try {
            $profileDirectory = Split-Path -Parent $profilePath
            if (-not (Test-Path -LiteralPath $profileDirectory)) {
                New-Item -ItemType Directory -Path $profileDirectory -Force | Out-Null
            }
            Set-Content -LiteralPath $profilePath -Encoding utf8 -Value "Set-Alias -Name '$aliasName' -Value 'Get-ChildItem' -Scope Global"

            $withoutProfile = & $script:pwsh -NoProfile -NonInteractive -Command "Get-Command -Name '$aliasName' -ErrorAction SilentlyContinue"
            $withoutProfile | Should -BeNullOrEmpty -Because 'the fixture must exist only in the PowerShell profile'

            $result = Invoke-Wta -App $script:app -Arguments @(
                'resolve-command',
                $aliasName,
                '--shell',
                $script:pwsh,
                '--json'
            ) -TimeoutSec 15 -Raw

            $result.ExitCode | Should -Be 0 -Because "packaged wta resolve-command failed: $($result.StdErr)"
            $resolved = $result.StdOut | ConvertFrom-Json
            $resolved.status | Should -Be 'exists'

            $profileAlias = @($resolved.resolutions | Where-Object {
                    $_.type -eq 'Alias' -and
                    $_.name -eq $aliasName
                })
            $profileAlias | Should -HaveCount 1
            $profileAlias[0].target | Should -Be 'Get-ChildItem'
        }
        finally {
            & $script:RestoreProfile
        }
    }
}
