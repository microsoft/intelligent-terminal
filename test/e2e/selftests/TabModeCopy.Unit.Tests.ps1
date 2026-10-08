#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
    $settingsResources = Join-Path $repoRoot 'src\cascadia\TerminalSettingsEditor\Resources'
    $cases = @(Get-ChildItem -LiteralPath $settingsResources -Directory |
        ForEach-Object { @{ Locale = $_.Name } })
}

Describe 'Settings Tab Mode copy' -Tag 'Unit' {
    BeforeAll {
        $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $settingsResources = Join-Path $repoRoot 'src\cascadia\TerminalSettingsEditor\Resources'
    }

    It 'uses the first-run Tab Mode label in <Locale>' -TestCases $cases {
        param($Locale)

        $settingsPath = Join-Path $settingsResources "$Locale\Resources.resw"
        $frePath = Join-Path $repoRoot "src\cascadia\TerminalApp\Resources\$Locale\Resources.resw"
        (Test-Path -LiteralPath $settingsPath) | Should -BeTrue
        (Test-Path -LiteralPath $frePath) | Should -BeTrue

        [xml]$settings = Get-Content -LiteralPath $settingsPath -Raw
        [xml]$fre = Get-Content -LiteralPath $frePath -Raw
        $settingLabels = @($settings.root.data | Where-Object name -eq 'Globals_TabLayout.Header')
        $freLabels = @($fre.root.data | Where-Object name -eq 'FreOverlay_TabModeLabel.Text')
        $freLabels | Should -HaveCount 1
        $settingLabels | Should -HaveCount 1 -Because "Settings should localize Tab Mode in $Locale"
        [string]$settingLabels[0].value | Should -BeExactly ([string]$freLabels[0].value)

        foreach ($pair in @(
                @('Globals_TabLayout.HelpText', 'FreOverlay_TabModeDescription.Text'),
                @('Globals_TabLayoutVertical.Content', 'FreOverlay_TabModeSidebar'),
                @('Globals_TabLayoutHorizontal.Content', 'FreOverlay_TabModeHorizontal')
            )) {
            $settingValue = @($settings.root.data | Where-Object name -eq $pair[0])
            $freValue = @($fre.root.data | Where-Object name -eq $pair[1])
            $settingValue | Should -HaveCount 1
            $freValue | Should -HaveCount 1
            [string]$settingValue[0].value | Should -BeExactly ([string]$freValue[0].value)
        }
    }
}
