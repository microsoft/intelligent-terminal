#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Add-Type -AssemblyName WindowsBase, UIAutomationClient, UIAutomationTypes
    $ast = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
    $definition = $ast.Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Wait-CombinedHistoryObservation'
    }, $true)
    . ([scriptblock]::Create($definition.Extent.Text))
    $script:evidence = Join-Path $PSScriptRoot ("..\..\..\Generated Files\combined-readiness-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:evidence | Out-Null
    $script:app = @{ Pid = 42 }
    function New-ReadinessPeer($Id, $Name, $Bounds, $Runtime) {
        $peer = [pscustomobject]@{
            Current = @{ AutomationId = $Id; Name = $Name; BoundingRectangle = $Bounds; ProcessId = 42; IsOffscreen = $false }
            Runtime = $Runtime
        }
        $peer | Add-Member ScriptMethod GetRuntimeId { @($this.Runtime) }
        $peer
    }
    function Get-CombinedElement {
        param($Id)
        $Id | Should -Be ItemsList
        $script:icon.Current.AutomationId = if ($Mode -eq 'wrong ID') { 'generic-image' } else { 'HistoryProviderIcon' }
        $script:icon.Current.Name = if ($Mode -eq 'wrong provider') { 'Claude' } else { 'Copilot' }
        $script:icon.Current.ProcessId = if ($Mode -eq 'foreign PID') { 99 } else { 42 }
        $script:icon.Current.IsOffscreen = $Mode -eq 'offscreen' -or ($Mode -eq 'offscreen then ready' -and $script:attempt -eq 1)
        $script:icon.Current.BoundingRectangle = switch ($Mode) {
            'zero size' { [Windows.Rect]::new(5, 5, 0, 0) }
            'zero then ready' { if ($script:attempt -eq 1) { [Windows.Rect]::new(5, 5, 0, 0) } else { [Windows.Rect]::new(5, 5, 16, 16) } }
            'outside row' { [Windows.Rect]::new(90, 5, 16, 16) }
            default { [Windows.Rect]::new(5, 5, 16, 16) }
        }
        $script:list.Current.BoundingRectangle = if ($Mode -eq 'outside viewport') {
            [Windows.Rect]::new(0, 0, 50, 50)
        } else { [Windows.Rect]::new(0, 0, 100, 100) }
        $script:list
    }
    function Get-CombinedRawChildren {
        param($Element, [switch]$ContentView)
        if ($ContentView) { return 'content-from-current-row' }
        if ($Mode -eq 'missing' -or ($Mode -eq 'missing then ready' -and $script:attempt -eq 1)) { return }
        ++$script:rawReads
        if ($script:rawReads % 2 -eq 0 -and
            ($Mode -eq 'icon replaced' -or ($Mode -eq 'icon replaced then ready' -and $script:attempt -eq 1))) {
            New-ReadinessPeer 'HistoryProviderIcon' 'Copilot' ([Windows.Rect]::new(5, 5, 16, 16)) 11
            return
        }
        $script:icon
        if ($Mode -eq 'duplicate peers') { $script:icon }
    }
    function Get-CombinedRows {
        param($Kind)
        if ($Mode -eq 'stale generation' -or ($Mode -eq 'replaced then ready' -and $script:attempt -eq 1)) {
            New-ReadinessPeer '' "fixture $([char]0xB7) Copilot" ([Windows.Rect]::new(0, 0, 80, 60)) 2
        } else { $script:row }
    }
    function Wait-Until {
        param($TimeoutSec, $Because, $Condition)
        $TimeoutSec | Should -Be 5
        foreach ($script:attempt in 1..3) {
            if (& $Condition) { return $true }
        }
        throw 'bounded real-peer timeout'
    }
}

Describe 'Combined history generation-consistent real provider peer readiness' -Tag Unit {
    BeforeEach {
        $script:attempt = 0
        $script:rawReads = 0
        $script:row = New-ReadinessPeer '' "fixture $([char]0xB7) Copilot" ([Windows.Rect]::new(0, 0, 80, 60)) 1
        $script:icon = New-ReadinessPeer 'HistoryProviderIcon' 'Copilot' ([Windows.Rect]::new(5, 5, 16, 16)) 10
        $script:list = New-ReadinessPeer 'ItemsList' '' ([Windows.Rect]::new(0, 0, 100, 100)) 20
        $script:list | Add-Member ScriptMethod FindAll {
            param($Scope, $Condition)
            $script:row
            if ($Mode -eq 'duplicate rows') { $script:row }
        }
    }
    AfterAll { Remove-Item -LiteralPath $script:evidence -Recurse -Force }

    It 'observes real peers once ready: <Mode>' -ForEach @(
        @{ Mode = 'ready' }, @{ Mode = 'missing then ready' }, @{ Mode = 'offscreen then ready' },
        @{ Mode = 'zero then ready' }, @{ Mode = 'replaced then ready' }, @{ Mode = 'icon replaced then ready' }
    ) {
        $result = Wait-CombinedHistoryObservation fixture Copilot
        $result.Row | Should -Be $script:row
        $result.Icon | Should -Be $script:icon
        $result.Parts | Should -Be 'content-from-current-row'
        $script:attempt | Should -Be $(if ($Mode -eq 'ready') { 1 } else { 2 })
    }

    It 'fails closed and retains diagnostics: <Mode>' -ForEach @(
        @{ Mode = 'missing' }, @{ Mode = 'wrong ID' }, @{ Mode = 'offscreen' }, @{ Mode = 'zero size' },
        @{ Mode = 'outside row' }, @{ Mode = 'outside viewport' }, @{ Mode = 'stale generation' },
        @{ Mode = 'duplicate peers' }, @{ Mode = 'duplicate rows' }, @{ Mode = 'foreign PID' },
        @{ Mode = 'wrong provider' }, @{ Mode = 'icon replaced' }
    ) {
        { Wait-CombinedHistoryObservation fixture Copilot } | Should -Throw
        Test-Path -LiteralPath (Join-Path $script:evidence 'history-provider-readiness-failure.json') | Should -BeTrue
        if ($Mode -in @('duplicate peers', 'duplicate rows', 'foreign PID', 'wrong provider')) {
            $script:attempt | Should -Be 1
        } else { $script:attempt | Should -Be 3 }
    }
}
