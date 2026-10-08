#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.PaneProgress.Tests.ps1'), [ref]$null, [ref]$errors)
    if ($errors.Count) { throw 'PaneProgress fixture parse failed.' }
    $definition = $ast.Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Switch-ContextLayout'
    }, $true)
    $script:source = $definition.Extent.Text
    $owner = $definition.Find({ param($node)
        $node -is [Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -eq '$ownedRoot'
    }, $true)
    # Substitute the native owner observer only; selection, identity checks and invocation run unchanged.
    Invoke-Expression $script:source.Replace($owner.Extent.Text, '$ownedRoot = { Assert-TestLayoutOwner; $script:root }')
    function Open-TabMenu {}
    function Assert-TestLayoutOwner {}
    function Get-OwnedElements { param($Property, $Value, $Parent) }
    function Get-WtSetting { param($App, $Key) }
    function Wait-Until { param($TimeoutSec, $Because, $Condition) }
    function New-LayoutPeer {
        $peer = [pscustomobject]@{
            Current = [pscustomobject]@{
                Name = 'Switch to sidebar'; AutomationId = ''; ProcessId = 42
                ControlType = [Windows.Automation.ControlType]::MenuItem
                IsOffscreen = $false; IsEnabled = $true
                BoundingRectangle = [pscustomobject]@{ Width = 100; Height = 30 }
            }
            RuntimeId = @(42, 1234, 4, 130)
        }
        $peer | Add-Member ScriptMethod GetRuntimeId { $this.RuntimeId }
        $peer | Add-Member ScriptMethod GetCurrentPattern {
            param($pattern)
            if ($pattern -ne [Windows.Automation.InvokePattern]::Pattern) { throw 'Wrong public pattern.' }
            $script:events.Add('pattern')
            if ($script:patternFailure) { throw 'Original pattern failure.' }
            $invoker = [pscustomobject]@{}
            $invoker | Add-Member ScriptMethod Invoke {
                $script:events.Add('invoke')
                if ($script:invokeFailure) { throw 'Original invocation failure.' }
            }
            $invoker
        }
        $peer
    }
}

Describe 'PaneProgress exact layout MenuItem invocation' -Tag 'Unit' {
    BeforeEach {
        $script:events = [Collections.Generic.List[string]]::new()
        $script:app = @{ Hwnd = 1234; Pid = 42 }
        $script:evidence = $TestDrive
        $script:root = [pscustomobject]@{ Identity = 'original-root' }
        $script:initial = @(New-LayoutPeer)
        $script:fresh = @(New-LayoutPeer)
        $script:expectedLabel = 'Switch to sidebar'
        $script:reads = 0; $script:ownerChecks = 0
        $script:patternFailure = $false; $script:invokeFailure = $false
        Mock Open-TabMenu { $script:events.Add('open') }
        Mock Assert-TestLayoutOwner { $script:ownerChecks++; $script:events.Add('owner') }
        Mock Get-OwnedElements {
            $Property | Should -Be Name
            $Value | Should -Be $script:expectedLabel
            $Parent | Should -Be $script:root
            $script:events.Add('query')
            $script:reads++
            if ($script:reads -eq 1) { $script:initial } else { $script:fresh }
        }
        Mock Get-WtSetting { 'vertical' }
        Mock Wait-Until {
            $script:events.Add('wait')
            if (-not (& $Condition)) { throw 'Original persistence timeout.' }
        }
    }

    It 'invokes the recorded empty-ID MenuItem only after fresh identity and ownership checks' {
        Switch-ContextLayout vertical
        $script:events.ToArray() | Should -Be @('open', 'owner', 'query', 'owner', 'query', 'owner', 'pattern', 'invoke', 'wait')
        $receipt = Get-Content -Raw (Get-ChildItem $TestDrive -Filter 'layout-menu-*.json' | Select-Object -Last 1).FullName | ConvertFrom-Json
        $receipt.automationId | Should -BeExactly ''
        @($receipt.runtimeId) | Should -Be @(42, 1234, 4, 130)
        $receipt.invocationMethod | Should -Be 'UIA InvokePattern'
        Should -Invoke Wait-Until -Times 1 -ParameterFilter { $TimeoutSec -eq 10 }
    }

    It 'uses the same horizontal label and persistence contract for the outward switch' {
        $script:expectedLabel = 'Switch to horizontal tabs'
        $script:initial[0].Current.Name = $script:expectedLabel
        $script:fresh[0].Current.Name = $script:expectedLabel
        Mock Get-WtSetting { 'horizontal' }
        Switch-ContextLayout horizontal
        @($script:events | Where-Object { $_ -eq 'invoke' }).Count | Should -Be 1
        Should -Invoke Wait-Until -Times 1 -ParameterFilter { $TimeoutSec -eq 10 }
    }

    It 'rejects <Scenario> before any invocation' -ForEach @(
        @{ Scenario = 'zero peers'; Mutation = { $script:initial = @() } }
        @{ Scenario = 'multiple peers'; Mutation = { $script:initial += New-LayoutPeer } }
        @{ Scenario = 'wrong role'; Mutation = { $script:initial[0].Current.ControlType = [Windows.Automation.ControlType]::Text } }
        @{ Scenario = 'foreign PID'; Mutation = { $script:fresh[0].Current.ProcessId = 99 } }
        @{ Scenario = 'wrong label'; Mutation = { $script:fresh[0].Current.Name = 'Other action' } }
        @{ Scenario = 'hidden peer'; Mutation = { $script:fresh[0].Current.IsOffscreen = $true } }
        @{ Scenario = 'disabled peer'; Mutation = { $script:fresh[0].Current.IsEnabled = $false } }
        @{ Scenario = 'empty geometry'; Mutation = { $script:fresh[0].Current.BoundingRectangle = [pscustomobject]@{ Width = 0; Height = 0 } } }
        @{ Scenario = 'stale runtime ID'; Mutation = { $script:fresh[0].RuntimeId = @(42, 1234, 4, 131) } }
        @{ Scenario = 'changed AutomationId'; Mutation = { $script:fresh[0].Current.AutomationId = 'different' } }
        @{ Scenario = 'missing runtime ID'; Mutation = { $script:initial[0].RuntimeId = @() } }
        @{ Scenario = 'disappeared peer'; Mutation = { $script:fresh = @() } }
        @{ Scenario = 'new ambiguity'; Mutation = { $script:fresh += New-LayoutPeer } }
    ) {
        & $Mutation
        { Switch-ContextLayout vertical } | Should -Throw
        $script:events | Should -Not -Contain invoke
        Should -Invoke Wait-Until -Times 0
    }

    It 'rejects lost native/run ownership at the final boundary' {
        Mock Assert-TestLayoutOwner {
            $script:ownerChecks++
            if ($script:ownerChecks -eq 3) { throw 'Original owner lost.' }
        }
        { Switch-ContextLayout vertical } | Should -Throw '*Original owner lost*'
        $script:events | Should -Not -Contain invoke
        Should -Invoke Wait-Until -Times 0
    }

    It 'propagates <Failure> without retry, fallback or persistence polling' -ForEach @(
        @{ Failure = 'pattern failure'; Flag = 'patternFailure' }
        @{ Failure = 'invocation failure'; Flag = 'invokeFailure' }
    ) {
        Set-Variable -Scope Script -Name $Flag -Value $true
        { Switch-ContextLayout vertical } | Should -Throw "*Original $Failure*"
        Should -Invoke Wait-Until -Times 0
    }

    It 'preserves persistence failures rather than forcing settings or retrying input' {
        Mock Get-WtSetting { 'horizontal' }
        { Switch-ContextLayout vertical } | Should -Throw '*Original persistence timeout*'
        @($script:events | Where-Object { $_ -eq 'invoke' }).Count | Should -Be 1
    }

    It 'retains original native root, process/start and exact case-sensitive run receipt guards' {
        foreach ($guard in @('GetAncestor', 'GetWindowProcessId', 'IsOwnedRootOrPopup', 'GetForegroundWindow',
            'OwnedProcess.StartTime', 'WindowsTerminal.exe', 'start_utc', 'UtcDateTime.Ticks',
            'run_token -ceq', 'ITE2E_RUN_TOKEN -cne', 'ITE2E_OWNED_PROCESS_RECEIPT -cne')) {
            $script:source | Should -Match ([regex]::Escape($guard))
        }
        $script:source | Should -Not -Match 'Invoke-UiElement|Set-WtSetting|Diagnostic|Save-LayoutSwitchObservation'
    }
}
