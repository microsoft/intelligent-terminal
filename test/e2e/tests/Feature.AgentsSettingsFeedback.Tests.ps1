#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $script:Ready = [bool](Get-Command winapp -ErrorAction SilentlyContinue)
}

Describe 'Feature Agents settings feedback' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            language = 'en-US'
            agentSessionManagementEnabled = $false
            acpAgent = 'custom:settings-fixture'
            acpCustomCommand = 'settings-fixture --acp'
            delegateAgent = 'custom:settings-fixture'
            delegateCustomCommand = 'settings-fixture'
        }
        $script:root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$script:app.Hwnd)

        function Find-Control([string]$Id) {
            $condition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            $script:root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        }

        function Require-Control([string]$Id) {
            Wait-Until -TimeoutSec 8 -Because "settings control '$Id' to appear" -Condition {
                Find-Control $Id
            }
        }

        function Invoke-Control([string]$Id) {
            (Require-Control $Id).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        }

        function Set-ControlText([string]$Id, [string]$Text) {
            (Require-Control $Id).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
        }

        function Show-AgentsPage {
            # Use UIA rather than a foreground-dependent accelerator to open Settings.
            if (-not (Find-Control 'SettingsNav')) {
                (Require-Control 'NewTabButton').GetCurrentPattern(
                    [System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
                Invoke-UiElement -App $script:app -Selector 'Settings' | Out-Null
            }
            Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem' | Out-Null
        }

        function Scroll-Settings([bool]$Bottom) {
            $scroll = (Require-Control 'SettingsMainPage_ScrollViewer').GetCurrentPattern(
                [System.Windows.Automation.ScrollPattern]::Pattern)
            if ($scroll.Current.VerticallyScrollable) {
                $scroll.SetScrollPercent(-1, $(if ($Bottom) { 100 } else { 0 }))
            }
        }

        function Select-AgentEntry([string]$ComboId, [string]$Label) {
            $combo = Require-Control $ComboId
            $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
            $condition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                [System.Windows.Automation.ControlType]::ListItem)
            $labelCondition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::NameProperty, $Label)
            $selectedItem = Wait-Until -TimeoutSec 8 -Because "the expanded '$Label' entry to render" -Condition {
                foreach ($item in $combo.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
                    if ($item.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $labelCondition)) {
                        return $item
                    }
                }
            }
            $selectedItem.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
            Wait-Until -TimeoutSec 8 -Because "'$Label' to become selected" -Condition {
                (Get-SelectedAgentText $ComboId) -eq $Label
            } | Out-Null
        }

        function Get-SelectedAgentLabel([string]$ComboId) {
            $selection = (Require-Control $ComboId).GetCurrentPattern(
                [System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            $selection.Count | Should -Be 1
            $condition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                [System.Windows.Automation.ControlType]::Text)
            $selection[0].FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        }

        function Get-SelectedAgentText([string]$ComboId) {
            (Get-SelectedAgentLabel $ComboId).Current.Name
        }

        function Get-TextRectangle([System.Windows.Automation.AutomationElement]$Control, [string]$Text) {
            $document = $Control.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange
            $index = $document.GetText(-1).IndexOf($Text, [StringComparison]::Ordinal)
            if ($index -lt 0) { throw "Expected rendered text '$Text' was not found." }
            # RichTextBlock's UIA provider does not implement FindText.
            $range = $document.Clone()
            $start = [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start
            $end = [System.Windows.Automation.Text.TextPatternRangeEndpoint]::End
            $range.MoveEndpointByRange($end, $range, $start)
            $range.MoveEndpointByUnit($start, [System.Windows.Automation.Text.TextUnit]::Character, $index) | Out-Null
            $range.MoveEndpointByRange($end, $range, $start)
            $range.MoveEndpointByUnit($end, [System.Windows.Automation.Text.TextUnit]::Character, $Text.Length) | Out-Null
            $rectangles = $range.GetBoundingRectangles()
            if ($rectangles.Count -eq 0) { throw "Expected rendered text '$Text' is not visible." }
            $rectangles[0]
        }

        function Click-SettingsPoint([double]$X, [double]$Y) {
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
            try {
                [ItE2E.ItWtWin32Input]::SetCursorPos([int]$X, [int]$Y) | Should -BeTrue
                $inputs = foreach ($flag in @(2, 4)) {
                    $mouse = [ItE2E.ItWtWin32Input+MOUSEINPUT]::new()
                    $mouse.dwFlags = $flag
                    $union = [ItE2E.ItWtWin32Input+INPUTUNION]::new()
                    $union.mouse = $mouse
                    $input = [ItE2E.ItWtWin32Input+INPUT]::new()
                    $input.data = $union
                    $input
                }
                [ItE2E.ItWtWin32Input]::SendInput(2, $inputs,
                    [Runtime.InteropServices.Marshal]::SizeOf($inputs[0])) | Should -Be 2
            }
            finally {
                [ItE2E.ItWtWin32Input]::SetCursorPos($cursor[0], $cursor[1]) | Out-Null
            }
        }

        Show-AgentsPage
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    BeforeEach {
        Show-AgentsPage
    }

    It 'Agent CLI settings explain prompt delegation' {
        Scroll-Settings $true
        $tree = Get-UiTree -App $script:app -Selector 'SettingsMainPage_ScrollViewer' -Depth 5
        $tree | Should -Match 'Agent CLI'
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty, 'Agent CLI')
        $group = @($script:root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) |
            Where-Object { $_.Current.ControlType -eq [System.Windows.Automation.ControlType]::Group } |
            Select-Object -First 1
        $group | Should -Not -BeNullOrEmpty
        $help = @($group.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition)) |
            Where-Object { $_.Current.AutomationId -eq 'HelpTextBlock' } |
            Select-Object -First 1
        $help.Current.Name | Should -Be 'Create a new tab with your preferred agent CLI, optionally including a prompt and context from the active pane. Press Alt+Shift+/ to open the command palette in prompt mode, or Alt+Shift+B to launch without an initial prompt.'
    }

    It 'Custom agent Save closes the editor' {
        foreach ($kind in @('Acp', 'Delegate')) {
            Scroll-Settings ($kind -eq 'Delegate')
            $comboId = "${kind}AgentComboBox"
            $boxId = "Custom${kind}CommandBox"
            $saveId = "Custom${kind}AgentSaveButton"
            $cancelId = "Custom${kind}AgentCancelButton"
            $editId = "Custom${kind}AgentEditButton"
            $previewId = "Custom${kind}CommandPreviewText"
            $name = "settings-feedback-$($kind.ToLowerInvariant())"
            Select-AgentEntry $comboId '+ Add New...'
            Set-ControlText $boxId "$name --first"
            Invoke-Control $saveId
            Wait-Until -TimeoutSec 8 -Because "$kind Save to collapse its form" -Condition {
                -not (Find-Control $boxId) -and -not (Find-Control $saveId) -and -not (Find-Control $cancelId)
            } | Out-Null
            Get-SelectedAgentText $comboId | Should -Be $name
            (Require-Control $editId).SetFocus()
            $previewOffset = (Require-Control $previewId).Current.BoundingRectangle.Left -
                (Get-SelectedAgentLabel $comboId).Current.BoundingRectangle.Left
            [math]::Abs($previewOffset) | Should -BeLessOrEqual 1 -Because 'preview and picker command text must share the same left inset'

            Invoke-Control $editId
            $box = Require-Control $boxId
            $box.SetFocus()
            $box.GetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern).Current.Value | Should -Be "$name --first"
            $text = Get-TextRectangle $box $name
            [math]::Abs($text.X - (Get-SelectedAgentLabel $comboId).Current.BoundingRectangle.Left) |
                Should -BeLessOrEqual 1 -Because 'editable and preview command text must align'
            Set-ControlText $boxId "$name --edited"
            Invoke-Control $saveId
            Wait-Until -TimeoutSec 8 -Because "$kind edited Save to collapse its form" -Condition {
                -not (Find-Control $boxId) -and -not (Find-Control $saveId) -and -not (Find-Control $cancelId)
            } | Out-Null
            Get-SelectedAgentText $comboId | Should -Be $name

            Select-AgentEntry $comboId 'settings-fixture'
            Select-AgentEntry $comboId $name
            Wait-Until -TimeoutSec 8 -Because "$kind re-selection to retain read-only preview" -Condition {
                (Find-Control $previewId) -and (Find-Control $editId) -and
                    -not (Find-Control $boxId) -and -not (Find-Control $saveId) -and -not (Find-Control $cancelId)
            } | Out-Null
            (Require-Control $previewId).Current.Name | Should -Be "$name --edited"

            Select-AgentEntry $comboId '+ Add New...'
            Set-ControlText $boxId 'unsaved-agent --cancelled'
            Invoke-Control $cancelId
            Get-SelectedAgentText $comboId | Should -Be $name
            Invoke-Control $editId
            (Require-Control $boxId).GetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern).Current.Value | Should -Be "$name --edited"
            Invoke-Control $cancelId
        }
    }

    It 'Custom model inputs stay equal width' {
        Scroll-Settings $false
        $expander = Require-Control 'CustomModelProvidersExpander'
        $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        Invoke-Control 'CustomProviderAddButton'
        try {
            foreach ($filled in @($false, $true)) {
                if ($filled) {
                    Set-ControlText 'CustomProviderBaseUrlBox' ('http://127.0.0.1:7771/' + ('long-path/' * 8) + 'v1')
                    Set-ControlText 'CustomProviderModelIdBox' ('long-model-name-' * 8)
                    Set-ControlText 'CustomProviderApiKeyBox' ('test-only-key-' * 10)
                }
                $widths = @('CustomProviderBaseUrlBox', 'CustomProviderModelIdBox', 'CustomProviderApiKeyBox') |
                    ForEach-Object {
                        $control = Require-Control $_
                        $control.SetFocus()
                        Wait-Until -TimeoutSec 5 -Because "$_ to be visible for measurement" -Condition {
                            -not $control.Current.IsOffscreen -and $control.Current.BoundingRectangle.Width -gt 0
                        } | Out-Null
                        $control.Current.BoundingRectangle.Width
                    }
                $widths[0] | Should -BeGreaterThan 0
                $widths[1] | Should -Be $widths[0]
                $widths[2] | Should -Be $widths[0]
            }
        }
        finally {
            Invoke-Control 'CustomProviderCancelButton'
        }
    }

    It 'Agents help links only target their text' {
        Scroll-Settings $false
        $expander = Require-Control 'CustomModelProvidersExpander'
        $pattern = $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
        $pattern.Collapse()
        Set-WtWindowForeground -App $script:app | Should -BeTrue -Because 'physical click tests require the test window in front'
        foreach ($id in @('PageSubtitlePrivacyLinkButton', 'CustomModelsLearnMoreButton')) {
            $link = Require-Control $id
            $bounds = $link.Current.BoundingRectangle
            $bounds.Width | Should -BeGreaterThan 0
            $bounds.Width | Should -BeLessThan ($expander.Current.BoundingRectangle.Width / 2)
            $y = $bounds.Top + $bounds.Height / 2
            $link.Current.ControlType | Should -Be ([System.Windows.Automation.ControlType]::Hyperlink)
            $link.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern) | Should -Not -BeNullOrEmpty
            # The legacy UIA point lookup stops at the XAML Islands host. Real
            # clicks prove adjacent text/whitespace does not launch the browser.
            foreach ($x in @(($bounds.Left - 12), ($bounds.Right + 12), ($expander.Current.BoundingRectangle.Right - 30))) {
                Click-SettingsPoint $x $y
                Test-Until -TimeoutSec 2 -IntervalSec 0.1 -Condition {
                    [ItE2E.ItWtWin32Input]::GetForegroundWindow().ToInt64() -ne $script:app.Hwnd
                } | Should -BeFalse -Because 'clicking outside link text must not navigate away from Terminal'
            }
        }
        $pattern.Expand()
        $pattern.Current.ExpandCollapseState | Should -Be ([System.Windows.Automation.ExpandCollapseState]::Expanded)
        $pattern.Collapse()
        $pattern.Current.ExpandCollapseState | Should -Be ([System.Windows.Automation.ExpandCollapseState]::Collapsed)
    }

    It 'Agents help links share the description baseline' {
        Scroll-Settings $false
        foreach ($case in @(
            @{ Control = 'PageSubtitleText'; Prefix = 'experience.'; Link = 'PageSubtitlePrivacyLink' },
            @{ Control = 'CustomModelsDescriptionText'; Prefix = 'endpoint.'; Link = 'CustomModelsCaptionLink' }
        )) {
            $control = Require-Control $case.Control
            $prefix = Get-TextRectangle $control $case.Prefix
            $link = (Require-Control $case.Link).Current.BoundingRectangle
            [math]::Abs($prefix.Y - $link.Y) | Should -BeLessOrEqual 1
            [math]::Abs($prefix.Height - $link.Height) | Should -BeLessOrEqual 1
        }
    }
}
