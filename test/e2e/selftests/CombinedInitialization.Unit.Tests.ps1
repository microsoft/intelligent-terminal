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
        $call | Should -BeGreaterThan $text.IndexOf('$script:target = Resolve-ItApp')
        $call | Should -BeGreaterThan $text.IndexOf('Refusing to adopt or close an existing Dev process')
        $call | Should -BeGreaterThan $text.IndexOf('New-Item -ItemType Directory -Path $script:evidence')
        $call | Should -BeGreaterThan $text.IndexOf("'package-before-launch.json'")
        $call | Should -BeLessThan $text.IndexOf('$script:historyPath = Join-Path')
        $call | Should -BeLessThan $text.IndexOf('Start-Terminal -Package Dev')
    }
}
