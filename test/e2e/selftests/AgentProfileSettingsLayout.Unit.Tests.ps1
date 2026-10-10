#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Native agent profile settings layout' -Tag 'Unit' {
    BeforeAll {
        $repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $editorRoot = Join-Path $repoRoot 'src\cascadia\TerminalSettingsEditor'
        [xml]$base = Get-Content -LiteralPath (Join-Path $editorRoot 'Profiles_Base.xaml') -Raw
        [xml]$advanced = Get-Content -LiteralPath (Join-Path $editorRoot 'Profiles_Advanced.xaml') -Raw
        $xamlNamespace = 'http://schemas.microsoft.com/winfx/2006/xaml'
        $baseCode = Get-Content -LiteralPath (Join-Path $editorRoot 'Profiles_Base.cpp') -Raw
        $advancedCode = Get-Content -LiteralPath (Join-Path $editorRoot 'Profiles_Advanced.cpp') -Raw
    }

    It 'keeps auxiliary assistant selectors on the shell base page and agent advanced page' {
        foreach ($name in @('AgentPaneBackend', 'CommandPaletteAgent')) {
            $baseControls = @($base.SelectNodes('//*') | Where-Object {
                $_.GetAttribute('Name', $xamlNamespace) -eq $name
            })
            $advancedControls = @($advanced.SelectNodes('//*') | Where-Object {
                $_.GetAttribute('Name', $xamlNamespace) -eq $name
            })
            $baseControls | Should -HaveCount 1
            $advancedControls | Should -HaveCount 1
            $baseControls[0].GetAttribute('Load', $xamlNamespace) | Should -BeExactly 'false'
            $advancedControls[0].GetAttribute('Load', $xamlNamespace) | Should -BeExactly 'false'
            foreach ($attribute in @('Uid', 'ClearSettingValue', 'CurrentValue', 'CurrentValueAccessibleName', 'CurrentValueTemplate', 'HasSettingValue', 'SettingOverrideSource')) {
                $namespace = if ($attribute -eq 'Uid') { $xamlNamespace } else { '' }
                $advancedControls[0].GetAttribute($attribute, $namespace) |
                    Should -BeExactly ($baseControls[0].GetAttribute($attribute, $namespace))
            }
            $baseCode | Should -Match 'if \(!winrt::get_self<ProfileViewModel>\(_Profile\)->IsAgentProfile\(\)\)\s*\{\s*FindName\(L"AgentPaneBackend"\);\s*FindName\(L"CommandPaletteAgent"\);\s*FindName\(L"Elevate"\);\s*\}'
            $advancedCode | Should -Match 'if \(winrt::get_self<ProfileViewModel>\(_Profile\)->IsAgentProfile\(\)\)\s*\{\s*FindName\(L"AgentPaneBackend"\);\s*FindName\(L"CommandPaletteAgent"\);\s*\}'
        }
    }

    It 'does not instantiate shell-only controls for native agent profiles' {
        foreach ($name in @('ShowMarks', 'AutoMarkPrompts', 'RepositionCursorWithMouse', 'RainbowSuggestions')) {
            $controls = @($advanced.SelectNodes('//*') | Where-Object {
                $_.GetAttribute('Name', $xamlNamespace) -eq $name
            })
            $controls | Should -HaveCount 1
            $controls[0].GetAttribute('Load', $xamlNamespace) | Should -BeExactly 'false'
            $controls[0].GetAttribute('ClearSettingValue') | Should -Not -BeNullOrEmpty
            $controls[0].GetAttribute('HasSettingValue') | Should -Not -BeNullOrEmpty
        }
        $advancedCode | Should -Match 'else\s*\{\s*FindName\(L"ShowMarks"\);\s*FindName\(L"AutoMarkPrompts"\);\s*FindName\(L"RepositionCursorWithMouse"\);\s*FindName\(L"RainbowSuggestions"\);\s*\}'
    }

    It 'removes the permission selector while preserving model and argument controls' {
        $base.SelectNodes('//*') | Where-Object {
            $_.GetAttribute('Uid', $xamlNamespace) -eq 'Profile_AgentPermission'
        } | Should -BeNullOrEmpty
        foreach ($uid in @('Profile_AgentModel', 'Profile_AgentArguments')) {
            $controls = @($base.SelectNodes('//*') | Where-Object {
                $_.GetAttribute('Uid', $xamlNamespace) -eq $uid
            })
            $controls | Should -HaveCount 1
            $controls[0].GetAttribute('Visibility') | Should -BeExactly '{x:Bind Profile.IsManagedAgentProfile, Mode=OneWay}'
        }
        $settings = Get-Content -LiteralPath (Join-Path $repoRoot 'src\cascadia\TerminalSettingsModel\MTSMSettings.h') -Raw
        $settings | Should -Match 'AgentProfilePermissionMode,\s*"agentProfile\.permissionMode"'
    }

    It 'classifies copies and explicit command overrides by persistent agent identity' {
        $header = Get-Content -LiteralPath (Join-Path $editorRoot 'ProfileViewModel.h') -Raw
        $header | Should -Match 'bool IsAgentProfile\(\) const \{ return !_profile\.AgentProfileId\(\)\.empty\(\); \}'
    }

    It 'removes unused permission and split picker projections without removing JSON settings' {
        foreach ($file in @('ProfileViewModel.idl', 'ProfileViewModel.h', 'ProfileViewModel.cpp')) {
            $viewModel = Get-Content -LiteralPath (Join-Path $editorRoot $file) -Raw
            $viewModel | Should -Not -Match '\b(AgentProfilePermissionList|CurrentAgentProfilePermission|SplitProfileList|CurrentSplitProfile|AgentProfilePermissionMode|DefaultSplitProfile|_agentProfilePermissionList|_splitProfileList|_InitializeAgentProfileSettings)\b'
        }
        $settings = Get-Content -LiteralPath (Join-Path $repoRoot 'src\cascadia\TerminalSettingsModel\MTSMSettings.h') -Raw
        $settings | Should -Match 'AgentProfilePermissionMode,\s*"agentProfile\.permissionMode"'
        $settings | Should -Match 'DefaultSplitProfile,\s*"defaultSplitProfile"'
        $idl = Get-Content -LiteralPath (Join-Path $editorRoot 'ProfileViewModel.idl') -Raw
        foreach ($setting in @('AgentProfileModel', 'AgentProfileArguments')) {
            $idl | Should -Match "OBSERVABLE_PROJECTED_PROFILE_SETTING\(String, $setting\)"
        }
    }

    It 'only loads the administrator option for ordinary shell profiles' {
        $controls = @($base.SelectNodes('//*') | Where-Object {
            $_.GetAttribute('Name', $xamlNamespace) -eq 'Elevate'
        })
        $controls | Should -HaveCount 1
        $controls[0].GetAttribute('Load', $xamlNamespace) | Should -BeExactly 'false'
        $baseCode | Should -Match 'if \(!winrt::get_self<ProfileViewModel>\(_Profile\)->IsAgentProfile\(\)\)\s*\{[^}]*FindName\(L"Elevate"\);'
        $advanced.SelectNodes('//*') | Where-Object {
            $_.GetAttribute('Uid', $xamlNamespace) -eq 'Profile_Elevate'
        } | Should -BeNullOrEmpty
    }

    It 'does not add a public setting or shift existing WinRT members for page selection' {
        $idl = Get-Content -LiteralPath (Join-Path $editorRoot 'ProfileViewModel.idl') -Raw
        $idl | Should -Not -Match 'Boolean IsAgentProfile\b'
    }
}
