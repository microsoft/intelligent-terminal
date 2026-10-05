#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Combined History metadata layout contract' -Tag Unit {
    BeforeAll {
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        [xml]$xaml = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\TabStrip.xaml') -Raw
        $ns = [Xml.XmlNamespaceManager]::new($xaml.NameTable)
        $ns.AddNamespace('p', 'http://schemas.microsoft.com/winfx/2006/xaml/presentation')
        $ns.AddNamespace('x', 'http://schemas.microsoft.com/winfx/2006/xaml')
        $row = $xaml.SelectSingleNode('//p:ListView[@x:Name="HistoryList"]/p:ListView.ItemTemplate/p:DataTemplate/p:Grid', $ns)
        $source = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\TabStrip.cpp') -Raw
    }
    It 'keeps a leading centered 16px icon outside the metadata text grid' {
        @($row.SelectNodes('p:Grid.ColumnDefinitions/p:ColumnDefinition', $ns)).Width | Should -Be @('Auto', '*')
        $icon = $row.SelectSingleNode('p:ContentControl[@x:Name="HistoryProviderIcon"]', $ns)
        $icon | Should -Not -BeNullOrEmpty
        $icon.GetAttribute('Grid.RowSpan') | Should -Be '2'
        $icon.Width | Should -Be '16'
        $icon.Height | Should -Be '16'
        $icon.Margin | Should -Be '0,0,12,0'
        $icon.VerticalAlignment | Should -Be 'Center'
        $icon.GetAttribute('AutomationProperties.Name') | Should -Be '{x:Bind ProviderDisplayName}'
        $row.SelectSingleNode('p:Grid[@x:Name="HistoryMetadata"]/*[@x:Name="HistoryProviderIcon"]', $ns) | Should -BeNullOrEmpty
    }
    It 'aligns both text rows and keeps time status provider then ownership' {
        $title = $row.SelectSingleNode('*[@x:Name="HistoryTitleText"]', $ns)
        $metadata = $row.SelectSingleNode('p:Grid[@x:Name="HistoryMetadata"]', $ns)
        $title.GetAttribute('Grid.Column') | Should -Be '1'
        $metadata.GetAttribute('Grid.Column') | Should -Be '1'
        $metadata.GetAttribute('Grid.Row') | Should -Be '1'
        @($metadata.SelectNodes('*[@x:Name]', $ns) | ForEach-Object { $_.GetAttribute('Name', $ns.LookupNamespace('x')) }) |
            Should -Be @('HistorySubtitleText', 'HistoryStatusText', 'HistoryProviderNameText', 'HistoryOwnershipButton')
        $provider = $metadata.SelectSingleNode('*[@x:Name="HistoryProviderNameText"]', $ns)
        $provider.Text | Should -Be '{x:Bind ProviderDisplayName}'
        $provider.SearchText | Should -Be '{x:Bind SearchQuery}'
        $provider.TextBlockStyle | Should -Be '{StaticResource HistorySubtitleTextStyle}'
        $provider.GetAttribute('Grid.Column') | Should -Be '2'
        $metadata.SelectSingleNode('*[@x:Name="HistoryStatusText"]', $ns).Visibility |
            Should -Be '{x:Bind mtu:Converters.InvertedBooleanToVisibility(IsHistorical), Mode=OneWay}'
    }
    It 'preserves selection palette order spanning both rows and columns and recolors provider text' {
        $selection = $row.SelectSingleNode('p:Border[@x:Name="HistorySelectionBackground"]', $ns)
        $selection.GetAttribute('Grid.RowSpan') | Should -Be '2'
        $selection.GetAttribute('Grid.ColumnSpan') | Should -Be '2'
        $selection.SelectSingleNode('p:ContentControl[@x:Name="HistorySelectionPalette"]', $ns) | Should -Not -BeNullOrEmpty
        $row.SelectSingleNode('*[not(contains(local-name(), "."))][1]', $ns) | Should -Be $selection
        $source | Should -Match 'for \(const auto name : \{ L"HistoryTitleText", L"HistorySubtitleText", L"HistoryStatusText", L"HistoryProviderNameText" \}\)'
    }
    It 'bounds time and provider text while reserving status and ownership at the minimum sidebar width' {
        $metadata = $row.SelectSingleNode('p:Grid[@x:Name="HistoryMetadata"]', $ns)
        @($metadata.SelectNodes('p:Grid.ColumnDefinitions/p:ColumnDefinition', $ns)).Width |
            Should -Be @('*', 'Auto', '*', 'Auto')
        $metadata.HorizontalAlignment | Should -Be 'Left' -Because 'short metadata must not fill the entire sidebar width'
        $component = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\LocalTests_TerminalApp\TabTests.cpp') -Raw
        $component | Should -Match '360\.0f, 180\.0f, 360\.0f'
        $component | Should -Match 'provider\.Text\(L"custom:combined-sidebar-fixture"\)'
        $component | Should -Match 'ownershipPosition\.X \+ ownership\.ActualWidth\(\) <= width - row\.Padding\(\)\.Right'
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
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
        foreach ($name in @('Assert-CombinedHistoryMetadata', 'Assert-CombinedOwnershipButton')) {
            $function = $ast.FindAll({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
            }, $true)[0]
            . ([scriptblock]::Create($function.Extent.Text))
        }
        function Save-CombinedActionEvidence {}
        function Get-CombinedRows {}
        function Get-CombinedRawChildren {}
        function Get-CombinedVisiblePart {}
        function Get-CombinedRowText {}
        function New-MetadataPart([string]$Name, [double]$X, [double]$Y, [double]$Width, [double]$Height, [string]$Id = '') {
            [pscustomobject]@{ Current = [pscustomobject]@{
                Name = $Name; AutomationId = $Id; IsOffscreen = $false; IsEnabled = $true
                ControlType = [Windows.Automation.ControlType]::Text
                BoundingRectangle = [Windows.Rect]::new($X, $Y, $Width, $Height)
            } }
        }
    }
    BeforeEach {
        $script:evidence = $PSScriptRoot
        $script:row = New-MetadataPart 'session' 0 0 320 56
        $script:icon = New-MetadataPart 'Copilot' 12 20 16 16 HistoryProviderIcon
        $script:icon.Current.ControlType = [Windows.Automation.ControlType]::Custom
        $script:parts = @(
            (New-MetadataPart 'session' 40 8 100 16),
            (New-MetadataPart 'just now' 40 28 48 16),
            (New-MetadataPart 'Idle' 92 28 24 16),
            (New-MetadataPart 'Copilot' 124 28 50 16)
        )
        Mock Save-CombinedActionEvidence {}
        Mock Get-CombinedRows { $script:row }
        Mock Get-CombinedRawChildren { if ($args[0] -eq $script:row) { $script:parts } }
        Mock Get-CombinedVisiblePart { $script:icon }
        Mock Get-CombinedRowText { 'session just now Idle Copilot' }
        Mock Add-Content {}
    }
    It 'accepts leading centered provider identity and ordered live metadata without a PathIcon peer' {
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Not -Throw
    }
    It 'accepts historical time and provider text without redundant status' {
        $script:parts = @($script:parts[0], $script:parts[1], (New-MetadataPart 'Copilot' 96 28 50 16))
        { Assert-CombinedHistoryMetadata -Title session -Provider Copilot } | Should -Not -Throw
    }
    It 'rejects a trailing provider icon' {
        $script:icon.Current.BoundingRectangle = [Windows.Rect]::new(180, 28, 16, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects an icon aligned only with the metadata rather than both rows' {
        $script:icon.Current.BoundingRectangle = [Windows.Rect]::new(12, 28, 16, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects missing provider display text' {
        $script:parts = $script:parts[0..2]
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects provider text extending outside the bounded row' {
        $script:parts[3].Current.BoundingRectangle = [Windows.Rect]::new(124, 28, 240, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects provider text before meaningful status' {
        $script:parts[3].Current.BoundingRectangle = [Windows.Rect]::new(88, 28, 50, 16)
        { Assert-CombinedHistoryMetadata -Title session -Status Idle -Provider Copilot } | Should -Throw
    }
    It 'rejects ownership placed after the leading icon but before provider text' {
        $button = New-MetadataPart 'Restore background tab' 30 28 24 24 HistoryOwnershipButton
        $button.Current.ControlType = [Windows.Automation.ControlType]::Button
        $script:parts += $button
        { Assert-CombinedOwnershipButton Background } | Should -Throw
    }
    It 'accepts an actionable ownership button after provider display text' {
        $button = New-MetadataPart 'Restore background tab' 180 28 24 24 HistoryOwnershipButton
        $button.Current.ControlType = [Windows.Automation.ControlType]::Button
        $script:parts += $button
        { Assert-CombinedOwnershipButton Background } | Should -Not -Throw
    }
}
