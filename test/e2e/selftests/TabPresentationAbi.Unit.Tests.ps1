#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:appRoot = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp'

    function Get-IdlDeclarations([string]$Source, [string]$Class) {
        $body = [regex]::Match($Source, "(?ms)runtimeclass $Class\b[^{]*\{(.*?)^\s*\};").Groups[1].Value
        if (-not $body) { throw "Missing runtimeclass $Class" }
        $body = $body -replace '//[^\r\n]*', ''
        @([regex]::Matches($body, '(?m)^\s*[\w.<>]+\s+\w+\s*(?:\{[^}]*\}|\([^)]*\))?\s*;') |
            ForEach-Object { $_.Value -replace '\s+', '' })
    }

    function Test-DeclarationPrefix([string[]]$Actual, [string[]]$Expected) {
        if ($Actual.Count -lt $Expected.Count) { return $false }
        for ($i = 0; $i -lt $Expected.Count; $i++) {
            if ($Actual[$i] -cne ($Expected[$i] -replace '\s+', '')) { return $false }
        }
        return $true
    }

    # Frozen legacy declarations from main0d5e0f3dd. New members may only follow
    # this prefix (doc/specs/agent-history-sidebar-keyboard.md, WinRT contract).
    $script:headerPrefix = @(
        'String Title;', 'String SearchText;', 'Double RenamerMaxWidth;',
        'TerminalTabStatus TabStatus;', 'String MetadataText;',
        'String MetadataAutomationName;', 'String AutomationName;',
        'TextHighlightColor MetadataHighlightColor;', 'IVector<HighlightedRun> MetadataHighlightRanges;'
    )
    $script:displayPrefix = @(
        'Microsoft.UI.Xaml.Controls.TabViewItem Tab { get; };',
        'Windows.Foundation.Collections.IObservableVector<TabStripPaneItem> PaneItems { get; };',
        'Object Header;', 'String Title;', 'String SearchText;',
        'Windows.UI.Xaml.Controls.IconElement Icon;',
        'Windows.UI.Xaml.Controls.Primitives.FlyoutBase ContextFlyout;',
        'Boolean IsExpanded;', 'Boolean IsGroup;',
        'Windows.UI.Xaml.Visibility HeaderVisibility;',
        'Windows.UI.Xaml.Visibility CloseVisibility;',
        'Windows.UI.Xaml.Visibility SelectionVisibility;',
        'Windows.UI.Xaml.Visibility GroupVisibility;',
        'Windows.UI.Xaml.Visibility ChildrenVisibility;',
        'String ChevronGlyph;', 'Windows.UI.Xaml.Visibility IconVisibility;',
        'Double HeaderMinHeight;', 'Windows.UI.Xaml.Thickness LeadingContentMargin;',
        'String ToolTipText;', 'String AcceleratorKey;', 'Boolean IsPinned;',
        'Windows.UI.Xaml.Visibility PinnedIconVisibility;',
        'TabHeaderPresentation Presentation { get; };',
        'Boolean IsMetadataVisible;', 'Boolean ShowProgressRing;', 'Object IconSource;',
        'Windows.UI.Xaml.Controls.IconElement CreateIcon(Object source);'
    )
    $script:headerSource = Get-Content (Join-Path $script:appRoot 'TabHeaderControl.idl') -Raw
    $script:displaySource = Get-Content (Join-Path $script:appRoot 'TabStrip.idl') -Raw
}

Describe 'Tab presentation append-only WinRT declarations' -Tag 'Unit' {
    It 'preserves the header legacy signatures and appends IconSource after MetadataHighlightRanges' {
        $actual = Get-IdlDeclarations $script:headerSource TabHeaderPresentation
        Test-DeclarationPrefix $actual ($script:headerPrefix + 'Object IconSource;') | Should -BeTrue
    }

    It 'preserves the display-item legacy signatures and appends the span after CreateIcon' {
        $actual = Get-IdlDeclarations $script:displaySource TabStripDisplayItem
        Test-DeclarationPrefix $actual ($script:displayPrefix + 'Int32 LeadingContentColumnSpan;') | Should -BeTrue
    }

    It 'rejects the original header insertion before metadata slots' {
        $regressed = ($script:headerSource -replace '(?m)^\s*Object IconSource;\r?\n', '') -replace
            'String MetadataText;', 'Object IconSource; String MetadataText;'
        # Preserve declaration line structure so the negative control exercises ordering.
        $regressed = $regressed.Replace('Object IconSource; String MetadataText;', "Object IconSource;`n        String MetadataText;")
        Test-DeclarationPrefix (Get-IdlDeclarations $regressed TabHeaderPresentation) $script:headerPrefix | Should -BeFalse
    }

    It 'rejects the original span insertion before existing icon members' {
        $regressed = ($script:displaySource -replace '(?m)^\s*Int32 LeadingContentColumnSpan;\r?\n', '') -replace
            'Object IconSource;', "Int32 LeadingContentColumnSpan;`n        Object IconSource;"
        Test-DeclarationPrefix (Get-IdlDeclarations $regressed TabStripDisplayItem) $script:displayPrefix | Should -BeFalse
    }

    It 'rejects inserting IconSource before upstream metadata highlight members' {
        $regressed = ($script:headerSource -replace '(?m)^\s*Object IconSource;\r?\n', '') -replace
            'TextHighlightColor MetadataHighlightColor;', "Object IconSource;`n        TextHighlightColor MetadataHighlightColor;"
        Test-DeclarationPrefix (Get-IdlDeclarations $regressed TabHeaderPresentation) $script:headerPrefix | Should -BeFalse
    }
}
