#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Sidebar rail toggle background contract' -Tag Unit {
    BeforeAll {
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        [xml]$xaml = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\TabRowControl.xaml') -Raw
        $ns = [Xml.XmlNamespaceManager]::new($xaml.NameTable)
        $ns.AddNamespace('p', 'http://schemas.microsoft.com/winfx/2006/xaml/presentation')
        $ns.AddNamespace('x', 'http://schemas.microsoft.com/winfx/2006/xaml')
        $template = $xaml.SelectSingleNode('//p:ControlTemplate[@x:Key="VerticalRailToggleTemplate"]', $ns)
        $template | Should -Not -BeNullOrEmpty
    }

    It 'starts with transparent backgrounds before pointer interaction' {
        $backgrounds = @($template.SelectNodes('.//*[@Background]', $ns) | Where-Object {
            $_.GetAttribute('Visibility') -ne 'Collapsed'
        })
        $backgrounds.Count | Should -BeGreaterThan 0
        @($backgrounds | Where-Object {
            $_.GetAttribute('Background') -notin @('Transparent', '{TemplateBinding Background}')
        }) | Should -HaveCount 0
    }

    It 'does not enable a persistent background in <State>' -TestCases @(
        @{ State = 'RailExpanded' },
        @{ State = 'RailCollapsed' }
    ) {
        param($State)

        $stateNode = $template.SelectSingleNode(
            './/p:VisualStateGroup[@x:Name="RailStates"]/p:VisualState[@x:Name="' + $State + '"]', $ns)
        $stateNode | Should -Not -BeNullOrEmpty
        $persistentBackgrounds = @(
            foreach ($setter in $stateNode.SelectNodes('.//p:Setter', $ns)) {
                if ($setter.Target.EndsWith('.Background')) {
                    if ($setter.Value -notin @('Transparent', '{TemplateBinding Background}')) {
                        $setter.Target
                    }
                }
                elseif ($setter.Target.EndsWith('.Visibility') -and $setter.Value -eq 'Visible') {
                    $targetName = $setter.Target.Substring(0, $setter.Target.LastIndexOf('.'))
                    $target = $template.SelectSingleNode('.//*[@x:Name="' + $targetName + '"]', $ns)
                    $target | Should -Not -BeNullOrEmpty
                    $background = $target.GetAttribute('Background')
                    if ($background -and $background -notin @('Transparent', '{TemplateBinding Background}')) {
                        $setter.Target
                    }
                }
            }
        )
        $persistentBackgrounds | Should -HaveCount 0 -Because 'rail layout must not look hovered without pointer interaction'
    }

    It 'retains a themed interaction background in <State>' -TestCases @(
        @{ State = 'PointerOver' },
        @{ State = 'Pressed' }
    ) {
        param($State)

        $stateNode = $template.SelectSingleNode(
            './/p:VisualStateGroup[@x:Name="CommonStates"]/p:VisualState[@x:Name="' + $State + '"]', $ns)
        $stateNode | Should -Not -BeNullOrEmpty
        $backgrounds = @($stateNode.SelectNodes(
            './/p:Setter[@Target="RailTogglePresenter.Background"]', $ns))
        $backgrounds | Should -HaveCount 1
        $backgrounds[0].Value | Should -Match '^\{ThemeResource .+\}$'
    }
}
