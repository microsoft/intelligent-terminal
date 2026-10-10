#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Combined History metadata layout contract' -Tag Unit {
    BeforeAll {
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        [xml]$xaml = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\TabStrip.xaml') -Raw
        $ns = [Xml.XmlNamespaceManager]::new($xaml.NameTable)
        $ns.AddNamespace('p', 'http://schemas.microsoft.com/winfx/2006/xaml/presentation')
        $ns.AddNamespace('x', 'http://schemas.microsoft.com/winfx/2006/xaml')
        $row = $xaml.SelectSingleNode('//p:DataTemplate[@x:Key="HistoryRowTemplate"]/p:Grid', $ns)
        $source = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\TabStrip.cpp') -Raw
    }
    It 'keeps a leading centered 16px icon outside the metadata text grid' {
        @($row.SelectNodes('p:Grid.ColumnDefinitions/p:ColumnDefinition', $ns)).Width | Should -Be @('Auto', '*', 'Auto')
        @($row.SelectNodes('p:Grid.RowDefinitions/p:RowDefinition', $ns)).Height | Should -Be @('Auto', 'Auto', 'Auto')
        $icon = $row.SelectSingleNode('p:ContentControl[@x:Name="HistoryProviderIcon"]', $ns)
        $icon | Should -Not -BeNullOrEmpty
        $icon.GetAttribute('Grid.RowSpan') | Should -Be '3'
        $icon.Width | Should -Be '16'
        $icon.Height | Should -Be '16'
        $icon.Margin | Should -Be '0,0,12,0'
        $icon.VerticalAlignment | Should -Be 'Center'
        $icon.GetAttribute('AutomationProperties.Name') | Should -Be '{x:Bind ProviderDisplayName}'
        $icon.GetAttribute('AutomationProperties.AccessibilityView') | Should -Be 'Raw'
        $icon.IsTabStop | Should -Be 'False'
        $row.SelectSingleNode('p:Grid[@x:Name="HistoryMetadata"]/*[@x:Name="HistoryProviderIcon"]', $ns) | Should -BeNullOrEmpty
    }
    It 'aligns title cwd and mutually exclusive age or status with a separate ordinary-color suffix' {
        $title = $row.SelectSingleNode('*[@x:Name="HistoryTitleText"]', $ns)
        $metadata = $row.SelectSingleNode('p:Grid[@x:Name="HistoryMetadata"]', $ns)
        $title.GetAttribute('Grid.Column') | Should -Be '1'
        $metadata.GetAttribute('Grid.Column') | Should -Be '1'
        $metadata.GetAttribute('Grid.Row') | Should -Be '2'
        $cwd = $row.SelectSingleNode('*[@x:Name="HistoryCwdText"]', $ns)
        $cwd.GetAttribute('Grid.Column') | Should -Be '1'
        $cwd.GetAttribute('Grid.Row') | Should -Be '1'
        $cwd.Text | Should -Be '{x:Bind Cwd}'
        $cwd.SearchText | Should -Be '{x:Bind SearchQuery}'
        @($metadata.SelectNodes('*[@x:Name]', $ns) | ForEach-Object { $_.GetAttribute('Name', $ns.LookupNamespace('x')) }) |
            Should -Be @('HistorySubtitleText', 'HistoryStatusText', 'HistoryWindowHintText')
        $metadata.SelectSingleNode('*[@x:Name="HistorySubtitleText"]', $ns).Visibility | Should -Be '{x:Bind IsHistorical, Mode=OneWay}'
        $hint = $metadata.SelectSingleNode('*[@x:Name="HistoryWindowHintText"]', $ns)
        $hint.TextBlockStyle | Should -Be '{StaticResource HistorySubtitleTextStyle}'
        $hint.GetAttribute('Grid.Column') | Should -Be '1'
        $metadata.SelectSingleNode('*[@x:Name="HistoryStatusText"]', $ns).Text | Should -Be '{x:Bind StatusLabelText, Mode=OneWay}'
        $metadata.SelectSingleNode('*[@x:Name="HistoryStatusText"]', $ns).Visibility |
            Should -Be '{x:Bind mtu:Converters.InvertedBooleanToVisibility(IsHistorical), Mode=OneWay}'
    }
    It 'preserves selection palette order spanning all rows and columns and recolors text' {
        $selection = $row.SelectSingleNode('p:Border[@x:Name="HistorySelectionBackground"]', $ns)
        $selection.GetAttribute('Grid.RowSpan') | Should -Be '3'
        $selection.GetAttribute('Grid.ColumnSpan') | Should -Be '3'
        $selection.SelectSingleNode('p:ContentControl[@x:Name="HistorySelectionPalette"]', $ns) | Should -Not -BeNullOrEmpty
        $row.SelectSingleNode('*[not(contains(local-name(), "."))][1]', $ns) | Should -Be $selection
        $source | Should -Match 'for \(const auto name : \{ L"HistoryTitleText", L"HistoryCwdText", L"HistorySubtitleText", L"HistoryStatusText", L"HistoryWindowHintText" \}\)'
    }
    It 'bounds three-line text and keeps only the passive trailing Keep Running icon' {
        $metadata = $row.SelectSingleNode('p:Grid[@x:Name="HistoryMetadata"]', $ns)
        @($metadata.SelectNodes('p:Grid.ColumnDefinitions/p:ColumnDefinition', $ns)).Width |
            Should -Be @('Auto', '*')
        $row.SelectSingleNode('.//*[@x:Name="HistoryProviderNameText" or @x:Name="HistoryOwnershipButton"]', $ns) | Should -BeNullOrEmpty
        $keep = $row.SelectSingleNode('*[@x:Name="HistoryKeepRunningIcon"]', $ns)
        $keep.GetAttribute('Grid.Column') | Should -Be '2'
        $keep.GetAttribute('Grid.RowSpan') | Should -Be '3'
        $component = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\LocalTests_TerminalApp\TabTests.cpp') -Raw
        $component | Should -Match '1000\.0f, 180\.0f, 1000\.0f'
        $component | Should -Match 'detailPosition\.X \+ detailText\.ActualWidth\(\) <= width - row\.Padding\(\)\.Right'
    }
    It 'keeps the revised C387 title exactly mapped without changing its stable ID' {
        $checklist = Get-Content -LiteralPath (Join-Path $root 'doc\release-check-list.md') -Raw
        $suite = Get-Content -LiteralPath (Join-Path $root 'test\e2e\tests\Feature.CombinedAgentsSidebar.Tests.ps1') -Raw
        $title = 'History metadata shows time useful status and agent name with a leading icon'
        $checklist | Should -Match ('`C387`.*\*\*' + [regex]::Escape($title) + ':')
        $suite | Should -Match ("It '" + [regex]::Escape($title) + "'")
    }
}

Describe 'Combined History real-oracle non-live controls' -Tag Unit {
    BeforeAll {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        Add-Type -AssemblyName WindowsBase
        . (Join-Path $PSScriptRoot '..\fixtures\SidebarRelativeTimeOracle.ps1')
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
        foreach ($name in @('Assert-CombinedHistoryMetadata', 'Assert-CombinedOwnershipHint')) {
            $function = $ast.FindAll({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
            }, $true)[0]
            . ([scriptblock]::Create($function.Extent.Text))
        }
        function Save-CombinedActionEvidence {}
        function Get-CombinedRows {}
        function Get-CombinedRawChildren { param($Element, [switch]$ContentView) }
        function Get-CombinedVisiblePart {}
        function Get-CombinedRowText {}
        function Get-CombinedSnapshot {}
        function New-MetadataPart([string]$Name, [double]$X, [double]$Y, [double]$Width, [double]$Height, [string]$Id = '') {
            [pscustomobject]@{ Current = [pscustomobject]@{
                Name = $Name; AutomationId = $Id; IsOffscreen = $false; IsEnabled = $true; IsKeyboardFocusable = $true
                ControlType = [Windows.Automation.ControlType]::Text
                BoundingRectangle = [Windows.Rect]::new($X, $Y, $Width, $Height)
            } }
        }
    }
    BeforeEach {
        $script:evidence = $PSScriptRoot
        $script:row = New-MetadataPart "session $([char]0xB7) Copilot" 0 0 320 72
        $script:icon = New-MetadataPart 'Copilot' 12 28 16 16 HistoryProviderIcon
        $script:icon.Current.ControlType = [Windows.Automation.ControlType]::Custom
        $script:parts = @(
            (New-MetadataPart 'session' 40 8 100 16),
            (New-MetadataPart 'C:\fixture' 40 28 100 16),
            (New-MetadataPart 'Idle' 40 48 24 16)
        )
        Mock Save-CombinedActionEvidence {}
        Mock Get-CombinedRows { $script:row }
        $script:contentIcon = $false
        Mock Get-CombinedRawChildren {
            if ($Element -eq $script:row) {
                $script:parts
                if ($ContentView -and $script:contentIcon) { $script:icon }
            }
        }
        Mock Get-CombinedVisiblePart { $script:icon }
        Mock Get-CombinedRowText { 'session just now Idle Copilot' }
        $script:metadataSource = [DateTimeOffset]::UtcNow
        Mock Get-CombinedSnapshot {
            @{ sessions = @(@{ title = 'session'; provider_id = 'copilot'; cwd = 'C:\fixture'
                last_activity_at_ms = $script:metadataSource.ToUnixTimeMilliseconds() }) }
        }
        Mock Add-Content {}
        $PSDefaultParameterValues = @{ 'Assert-CombinedHistoryMetadata:Cwd' = 'C:\fixture' }
    }
    It 'accepts leading centered provider identity and ordered live metadata without a PathIcon peer' {
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Not -Throw
    }
    It 'accepts historical time and container provider identity without redundant status' {
        $script:parts[2].Current.Name = 'just now'
        { Assert-CombinedHistoryMetadata -Title session -Provider Copilot } | Should -Not -Throw
    }
    It 'accepts Windows ICU LONG ages independently derived from the source timestamp' {
        . (Join-Path $PSScriptRoot '..\fixtures\SidebarRelativeTimeOracle.ps1')
        $script:metadataSource = [DateTimeOffset]::UtcNow.AddSeconds(-130)
        $script:parts[2].Current.Name = [ItSidebarAgeOracle]::Format('en-US', 'minute', 2)
        { Assert-CombinedHistoryMetadata -Title session -Provider Copilot } | Should -Not -Throw
    }
    It 'rejects plausible age text that disagrees with the source timestamp' {
        $script:metadataSource = [DateTimeOffset]::UtcNow.AddSeconds(-130)
        $script:parts[2].Current.Name = '99 minutes ago'
        { Assert-CombinedHistoryMetadata -Title session -Provider Copilot } | Should -Throw
    }
    It 'rejects a duplicate visible semantic age leaf' {
        $script:parts += New-MetadataPart 'just now' 40 48 48 16
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'does not count a Raw-only decorative SymbolIcon text leaf as metadata' {
        $glyph = New-MetadataPart ([string][char]0xE8F2) 12 20 16 16
        Mock Get-CombinedRawChildren {
            if ($Element -eq $script:row) {
                $script:parts
                if (-not $ContentView) { $glyph }
            }
        }
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Not -Throw
    }
    It 'still rejects a fourth historical Content-view metadata text leaf' {
        $script:parts = @($script:parts[0], $script:parts[1], (New-MetadataPart 'just now' 40 48 50 16),
            (New-MetadataPart 'unexpected' 150 28 48 16))
        { Assert-CombinedHistoryMetadata -Title session -Provider Copilot } | Should -Throw
    }
    It 'rejects an offscreen zero-size historical Content-view status semantic' {
        $hiddenStatus = New-MetadataPart 'Historical' 0 0 0 0
        $hiddenStatus.Current.IsOffscreen = $true
        $script:parts = @($script:parts[0], $script:parts[1], (New-MetadataPart 'just now' 40 48 50 16), $hiddenStatus)
        { Assert-CombinedHistoryMetadata -Title session -Provider Copilot } | Should -Throw
    }
    It 'rejects a trailing provider icon' {
        $script:icon.Current.BoundingRectangle = [Windows.Rect]::new(180, 28, 16, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects duplicate provider semantics from an icon in Content view' {
        $script:contentIcon = $true
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects an icon aligned only with metadata rather than all three rows' {
        $script:icon.Current.BoundingRectangle = [Windows.Rect]::new(12, 48, 16, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects missing provider identity on the row container' {
        $script:row.Current.Name = 'session'
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects cwd text extending outside the bounded row' {
        $script:parts[1].Current.BoundingRectangle = [Windows.Rect]::new(40, 28, 300, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects status on the cwd line' {
        $script:parts[2].Current.BoundingRectangle = [Windows.Rect]::new(40, 28, 50, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects a dedicated ownership action before metadata' {
        $button = New-MetadataPart 'Restore background tab' 30 28 24 24 HistoryOwnershipButton
        $button.Current.ControlType = [Windows.Automation.ControlType]::Button
        $script:parts += $button
        { Assert-CombinedOwnershipHint Background } | Should -Throw
    }
    It 'rejects a dedicated ownership action after metadata' {
        $button = New-MetadataPart 'Restore background tab' 180 28 24 24 HistoryOwnershipButton
        $button.Current.ControlType = [Windows.Automation.ControlType]::Button
        $script:parts += $button
        { Assert-CombinedOwnershipHint Background } | Should -Throw
    }
    It 'accepts a textual ownership hint without an action button' {
        Mock Get-CombinedRowText { "session Idle $([char]0xB7) In another window" }
        { Assert-CombinedOwnershipHint OtherWindow } | Should -Not -Throw
    }
    It 'rejects a stale ownership hint on a headless background session' {
        Mock Get-CombinedRowText { "session Idle $([char]0xB7) In another window" }
        { Assert-CombinedOwnershipHint Background } | Should -Throw
    }
    It 'rejects missing cwd even when a provider text leaf preserves the leaf count' {
        $script:parts[1].Current.Name = 'Copilot'
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects age co-rendered with live status even without a provider text leaf' {
        $script:parts += New-MetadataPart 'just now' 68 48 48 16
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects provider identity in a nonfocusable row container' {
        $script:row.Current.IsKeyboardFocusable = $false
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'accepts a separate foreign-window suffix after the status' {
        $script:parts += New-MetadataPart " $([char]0xB7) In another window" 64 48 140 16
        Mock Get-CombinedRowText { "session Idle $([char]0xB7) In another window" }
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot -OtherWindow } | Should -Not -Throw
    }

    It 'rejects a foreign-window suffix merged into the colored status label' {
        $script:parts[2].Current.Name = "Idle $([char]0xB7) In another window"
        Mock Get-CombinedRowText { "session Idle $([char]0xB7) In another window" }
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot -OtherWindow } | Should -Throw
    }
    It 'accepts a named passive Keep Running icon without counting it as metadata' {
        $script:parts += New-MetadataPart 'Keep running' 280 28 16 16 HistoryKeepRunningIcon
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Not -Throw
    }
    It 'accepts the inner glyph of the exact passive Keep Running icon subtree' {
        $script:badge = New-MetadataPart 'Keep running' 280 28 16 16 HistoryKeepRunningIcon
        $script:badge.Current.ControlType = [Windows.Automation.ControlType]::Custom
        $script:badgeGlyph = New-MetadataPart ([string][char]0xE8EE) 280 28 16 16
        $script:parts += @($script:badge, $script:badgeGlyph)
        Mock Get-CombinedRawChildren {
            if ($Element -eq $script:row) { $script:parts }
            elseif ($Element -eq $script:badge) { $script:badgeGlyph }
        }
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Not -Throw
    }
}

Describe 'Sidebar timestamp oracle locale controls' -Tag Unit {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\fixtures\SidebarRelativeTimeOracle.ps1')
    }
    It 'formats LONG numeric units using independent source and capture timestamps' {
        $source = [DateTimeOffset]::Parse('2026-10-01T23:30:00Z')
        foreach ($case in @(
            @{ Unit = 'minute'; Capture = '2026-10-01T23:32:10Z'; Expected = '2 minutes ago' },
            @{ Unit = 'hour'; Capture = '2026-10-02T02:32:00Z'; Expected = '3 hours ago' },
            @{ Unit = 'day'; Capture = '2026-10-05T23:32:00Z'; Expected = '4 days ago' }
        )) {
            $count = [ItSidebarAgeOracle]::Count($case.Unit, $source, [DateTimeOffset]::Parse($case.Capture))
            [ItSidebarAgeOracle]::Format('en-US', $case.Unit, $count) | Should -Be $case.Expected
            [ItSidebarAgeOracle]::Format('ar-SA', $case.Unit, $count) | Should -Not -Be $case.Expected
        }
    }
    It 'formats UTC long calendar dates without weekdays instead of relative weeks months or years' {
        $source = [DateTimeOffset]::Parse('2026-10-02T01:30:00+02:00')
        [ItSidebarAgeOracle]::CalendarDate('en-US', $source) | Should -Be 'October 1, 2026'
        $arabic = [ItSidebarAgeOracle]::CalendarDate('ar-SA', $source)
        $arabic | Should -Not -Be 'October 1, 2026'
        $arabic | Should -Not -Match 'الخميس|الجمعة'
        $arabic | Should -Be ([ItSidebarAgeOracle]::CalendarDate('ar-SA', $source.ToUniversalTime()))
        [ItSidebarAgeOracle]::CalendarDate('en-US', $source) |
            Should -Not -Be ([ItSidebarAgeOracle]::Format('en-US', 'week', 2))
    }
}
