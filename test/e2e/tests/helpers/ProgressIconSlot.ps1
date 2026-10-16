# Pure geometry oracle shared by live OSC regression tests and non-live harness tests.
function Assert-ProgressIconSlotGeometry {
    param(
        [Parameter(Mandatory)]$IconBounds,
        [Parameter(Mandatory)]$ProgressBounds,
        [Parameter(Mandatory)][ValidateRange(0.5, 8)][double]$Scale,
        [Nullable[double]]$TitleOffset,
        [Nullable[double]]$ProgressTitleOffset
    )
    foreach ($bounds in @($IconBounds, $ProgressBounds)) {
        $bounds.Width | Should -BeGreaterThan 0
        $bounds.Height | Should -BeGreaterThan 0
    }
    [math]::Abs(($ProgressBounds.X + $ProgressBounds.Width / 2) - ($IconBounds.X + $IconBounds.Width / 2)) |
        Should -BeLessOrEqual $Scale -Because 'spinner replaces the original icon in the same leading slot, never a second column'
    [math]::Abs(($ProgressBounds.Y + $ProgressBounds.Height / 2) - ($IconBounds.Y + $IconBounds.Height / 2)) |
        Should -BeLessOrEqual $Scale -Because 'the replacement stays vertically centered in the original icon slot'
    $ProgressBounds.Width | Should -BeLessOrEqual ($IconBounds.Width + $Scale)
    $ProgressBounds.Height | Should -BeLessOrEqual ($IconBounds.Height + $Scale)
    if ($null -ne $TitleOffset) {
        $ProgressTitleOffset | Should -Not -BeNullOrEmpty
        [math]::Abs($ProgressTitleOffset - $TitleOffset) | Should -BeLessOrEqual $Scale -Because 'progress does not insert another leading column or shift the title'
    }
}
