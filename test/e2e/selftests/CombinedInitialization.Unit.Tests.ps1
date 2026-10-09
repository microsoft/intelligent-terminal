#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:ast = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
}

Describe 'Combined sidebar initialization order' -Tag 'Unit' {
    It 'file-level BeforeAll defines helpers without accessing package paths' {
        $parent = $script:ast.EndBlock.Statements | Where-Object {
            $_ -is [Management.Automation.Language.PipelineAst] -and $_.PipelineElements[0].GetCommandName() -eq 'BeforeAll'
        } | Select-Object -First 1
        $body = $parent.PipelineElements[0].CommandElements[1].ScriptBlock
        @($body.EndBlock.Statements | Where-Object {
            $_ -isnot [Management.Automation.Language.FunctionDefinitionAst]
        }).Count | Should -Be 0
        $script:target = $null
        $script:evidence = $null
        $text = $body.Extent.Text
        { . ([scriptblock]::Create($text.Substring(1, $text.Length - 2))) } | Should -Not -Throw
    }

    It 'snapshots runtime state after provenance but before any fixture or app mutation' {
        $text = $script:ast.Extent.Text
        $call = $text.IndexOf("`n        Initialize-CombinedRuntimeBackup")
        $call | Should -BeGreaterOrEqual 0
        foreach ($marker in @(
            '$script:target = Resolve-ItApp',
            'Refusing to adopt or close an existing Dev process',
            'New-Item -ItemType Directory -Path $script:evidence',
            "'package-before-launch.json'"
        )) {
            $position = $text.IndexOf($marker)
            $position | Should -BeGreaterOrEqual 0 -Because "the prerequisite marker must exist: $marker"
            $call | Should -BeGreaterThan $position
        }
        foreach ($marker in @('$script:historyPath = Join-Path', 'Start-Terminal -Package Dev')) {
            $position = $text.IndexOf($marker)
            $position | Should -BeGreaterOrEqual 0 -Because "the mutation marker must exist: $marker"
            $call | Should -BeLessThan $position
        }
    }
}
