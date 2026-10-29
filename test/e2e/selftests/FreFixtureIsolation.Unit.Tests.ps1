#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'FRE fixture shell integration isolation' -Tag Unit {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    }

    It '<Suite> replaces user shell profiles and disables dynamic shell profiles before launch' -ForEach @(
        @{ Suite = 'Feature.SettingsUi.Tests.ps1' }
        @{ Suite = 'Feature.KeyboardFre.Tests.ps1' }
    ) {
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot "..\tests\$Suite"), [ref]$null, [ref]$null)
        $launch = $ast.Find({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Start-Terminal'
        }, $true)
        $settingsParameter = $launch.CommandElements | Where-Object {
            $_ -is [Management.Automation.Language.CommandParameterAst] -and $_.ParameterName -eq 'Settings'
        }
        $settingsParameter | Should -HaveCount 1
        $index = [array]::IndexOf($launch.CommandElements, $settingsParameter)
        $settingsExpression = $launch.CommandElements[$index + 1]
        $settingsExpression | Should -BeOfType ([Management.Automation.Language.HashtableAst])
        $invocation = 'fixture-only'
        $fixtureSettings = & ([scriptblock]::Create($settingsExpression.Extent.Text))
        $app = [pscustomobject]@{ SettingsPath = Join-Path $TestDrive "$Suite.json" }
        @{
            theme = 'light'
            profiles = @{
                defaults = @{ commandline = 'pwsh.exe' }
                list = @(
                    @{ guid = '{11111111-1111-1111-1111-111111111111}'; commandline = 'pwsh.exe' }
                    @{ guid = '{22222222-2222-2222-2222-222222222222}'; commandline = 'wsl.exe' }
                )
            }
        } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $app.SettingsPath

        Set-WtSettings -App $app -Settings $fixtureSettings | Out-Null

        $saved = Get-WtSettingsObject -App $app
        $saved.theme | Should -Be 'light'
        $saved.autoErrorDetectionEnabled | Should -BeFalse
        $saved.defaultProfile | Should -Be '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'
        @($saved.profiles.list) | Should -HaveCount 2
        foreach ($profile in $saved.profiles.list) {
            $profile.commandline | Should -Be 'cmd.exe'
        }
        ($saved.profiles.list | Where-Object guid -EQ '{61c54bbd-c2c6-5271-96e7-009a87ff44bf}').commandline |
            Should -Be 'cmd.exe' -Because 'the built-in Windows PowerShell profile must also be overridden'
        foreach ($source in @(
            'Windows.Terminal.PowershellCore', 'Windows.Terminal.Wsl',
            'Windows.Terminal.Azure', 'Windows.Terminal.VisualStudio',
            'Windows.Terminal.SSH', 'Microsoft.WSL'
        )) {
            $saved.disabledProfileSources | Should -Contain $source
        }
    }
}
