#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# No package, terminal, UIA, desktop or deployment is accessed by this suite.
Describe 'Harness: progress leading-slot geometry oracle' -Tag 'Harness' {
    BeforeAll {
        . (Join-Path $PSScriptRoot 'helpers\ProgressIconSlot.ps1')
        function New-TestBounds {
            param([double]$X, [double]$Y, [double]$Width, [double]$Height)
            @{ X = $X; Y = $Y; Width = $Width; Height = $Height }
        }
    }
    It 'accepts a centered 15-DIP replacement at <Scale> scale' -TestCases @(
        @{ Scale = 1 }; @{ Scale = 1.25 }; @{ Scale = 2 }
    ) {
        param($Scale)
        $icon = New-TestBounds (10 * $Scale) (20 * $Scale) (16 * $Scale) (16 * $Scale)
        $ring = New-TestBounds (10.5 * $Scale) (20.5 * $Scale) (15 * $Scale) (15 * $Scale)
        { Assert-ProgressIconSlotGeometry $icon $ring $Scale -TitleOffset 40 -ProgressTitleOffset 40 } | Should -Not -Throw
    }
    It 'rejects the original separate trailing progress column' {
        $icon = New-TestBounds 10 20 16 16
        $ring = New-TestBounds 38.5 20.5 15 15
        { Assert-ProgressIconSlotGeometry $icon $ring 1 } | Should -Throw
    }
    It 'rejects a vertically miscentered spinner' {
        $icon = New-TestBounds 10 20 16 16
        $ring = New-TestBounds 10.5 24 15 15
        { Assert-ProgressIconSlotGeometry $icon $ring 1 } | Should -Throw
    }
    It 'rejects a second column that shifts the title despite a centered spinner' {
        $icon = New-TestBounds 10 20 16 16
        $ring = New-TestBounds 10.5 20.5 15 15
        { Assert-ProgressIconSlotGeometry $icon $ring 1 -TitleOffset 40 -ProgressTitleOffset 60 } | Should -Throw
    }
    It 'rejects an oversized or empty progress widget' -TestCases @(
        @{ Size = 0 }; @{ Size = 20 }
    ) {
        param($Size)
        $icon = New-TestBounds 10 20 16 16
        $ring = New-TestBounds (18 - $Size / 2) (28 - $Size / 2) $Size $Size
        { Assert-ProgressIconSlotGeometry $icon $ring 1 } | Should -Throw
    }
}
