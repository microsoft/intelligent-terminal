#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $cases = @(
        'AgentStatus'
        'WorkingDirectory'
        'Repository'
        'Branch'
        'Changes'
    ) | ForEach-Object { @{ Key = "VerticalTabsMetadata$_" } }
}

Describe 'Sidebar metadata library resource lookups' -Tag Unit {
    BeforeAll {
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $source = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\TerminalPage.cpp') -Raw
        [xml]$resources = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\Resources\en-US\Resources.resw') -Raw
    }

    It 'uses a PRI lookup path for <Key>' -TestCases $cases {
        param($Key)

        @($resources.root.data | Where-Object name -eq "$Key.Text") |
            Should -HaveCount 1
        [regex]::Matches($source, 'RS_\(L"' + [regex]::Escape("$Key/Text") + '"\)').Count |
            Should -BeGreaterThan 0 -Because 'XAML UID properties become nested PRI resource paths'
        [regex]::Matches($source, 'RS_\(L"' + [regex]::Escape("$Key.Text") + '"\)').Count |
            Should -Be 0
    }
}
