BeforeAll {
    . (Join-Path $PSScriptRoot '..\tests\helpers\SidebarExpansionEvents.ps1')
}
Describe 'Sidebar native event collector construction without subscribing to any window' {
    It 'compiles its real UIA callback and exposes a disposable string snapshot collector' {
        Initialize-TestSidebarExpansionEvents
        $type = 'ItE2E.SidebarExpansionEvents' -as [type]
        $type | Should -Not -BeNullOrEmpty
        [IDisposable].IsAssignableFrom($type) | Should -BeTrue
        $type.GetMethod('Snapshot').ReturnType | Should -Be ([string[]])
        $type.GetConstructors()[0].GetParameters()[0].ParameterType |
            Should -Be ([Windows.Automation.AutomationElement])
    }
}
