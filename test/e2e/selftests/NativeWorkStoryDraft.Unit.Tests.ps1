#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Native Work demo composer extraction' {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\tests\helpers\NativeWorkStoryDemo.ps1')
    }

    It 'reads a centered composer independently of changing keyboard help' {
        $frame = @'
    Work story demo | Fix Issue #4821
    Earlier conversation

    ┌────────────────────────────────────┐
    │ > Continue fixing issue #4821      │
    └────────────────────────────────────┘
    F4 Actions   F5 Details   Enter Send
'@
        Get-NativeWorkStoryDraftText -Frame $frame | Should -BeExactly 'Continue fixing issue #4821'
    }

    It 'preserves meaningful leading spaces and explicit draft newlines' {
        $frame = @'
  ┌────────────────────┐
  │ > first line       │
  │     indented       │
  └────────────────────┘
  Enter Send
'@
        Get-NativeWorkStoryDraftText -Frame $frame | Should -BeExactly "first line`n  indented"
    }

    It 'recognizes the shared empty-draft placeholder' {
        $frame = @'
 ┌────────────────────────────────────┐
 │ > Ask anything, / for commands..   │
 └────────────────────────────────────┘
'@
        Get-NativeWorkStoryDraftText -Frame $frame | Should -BeExactly ''
    }

    It 'does not confuse an earlier card with the bottom composer' {
        $frame = @'
┌──────────────────┐
│ > old message    │
└──────────────────┘

┌──────────────────┐
│ > new draft      │
└──────────────────┘
Enter Send
'@
        Get-NativeWorkStoryDraftText -Frame $frame | Should -BeExactly 'new draft'
    }

    It 'rejects a non-composer bordered card rather than treating it as empty input' {
        $frame = @'
 ┌────────────────────┐
 │ Budget 20K         │
 └────────────────────┘
'@
        { Get-NativeWorkStoryDraftText -Frame $frame } | Should -Throw '*malformed draft row*'
    }

    It 'rejects missing composer borders' {
        { Get-NativeWorkStoryDraftText -Frame 'Only conversation history' } | Should -Throw '*closing border*'
    }

    It 'recognizes the legacy and explicit Work graph demo markers' {
        Test-NativeWorkStoryFrame -Frame 'Work story demo | Fix Issue #4821' | Should -BeTrue
        Test-NativeWorkStoryFrame -Frame @'
Saved Work relationships
Scripted prototype | Independent contexts and budgets | Simulated tokens, NOT provider quota
'@ | Should -BeTrue
    }

    It 'rejects ordinary Work UI and incomplete prototype markers' {
        foreach ($frame in @(
            'Saved Work relationships',
            'Scripted prototype',
            'Scripted prototype | Independent contexts and budgets | Simulated tokens, NOT provider quota',
            'Main agent / Coordinate'
        )) {
            Test-NativeWorkStoryFrame -Frame $frame | Should -BeFalse
        }
    }
}
