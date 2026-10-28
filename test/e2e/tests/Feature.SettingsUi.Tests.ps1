#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §1/§6 — the SETTINGS EDITOR UI itself, driven for real.
#
# Earlier these were written off as "the Settings editor can't be opened by the harness". That was
# wrong: WT opens Settings on the Ctrl+, accelerator, and while wtcli send-keys only reaches a
# pane's conpty, an OS-level keystroke to the WT WINDOW (Send-WtWindowKey / Open-WtSettings) does
# hit WT's accelerator handler. Once open, the Settings editor is a normal XAML surface — winapp
# can inspect / search / invoke / set-value its controls. This suite proves the editor is
# drivable end-to-end (page opens, nav works, the model control is visible for a custom agent).
#
# Foreground-focus dependent: Open-WtSettings needs the WT window to take foreground, so if that's
# denied (locked session / competing foreground app) the suite SKIPS rather than failing.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §1/§6 Settings editor UI (opened via Ctrl+, accelerator)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # A custom agent so the model control is expected to render on the AI Agents page.
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT }
        else { Join-Path $PSScriptRoot '..\artifacts' }
        $evidence = Join-Path $artifactRoot "settings-ui\$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $evidence -Force | Out-Null
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$((Join-Path $evidence 'fixture.log').Replace("'", "''"))'"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:settings-ui-fixture'
            acpCustomCommand = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
            autoErrorDetectionEnabled = $false
            agentSessionManagementEnabled = $false
            defaultProfile = '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'
            disabledProfileSources = @(
                'Windows.Terminal.PowershellCore', 'Windows.Terminal.Wsl',
                'Windows.Terminal.Azure', 'Windows.Terminal.VisualStudio',
                'Windows.Terminal.SSH', 'Microsoft.WSL'
            )
            profiles = @{ list = @(
                @{ guid = '{0caa0dad-35be-5f56-a8ff-afceeeaa6101}'; commandline = 'cmd.exe' }
                @{ guid = '{61c54bbd-c2c6-5271-96e7-009a87ff44bf}'; commandline = 'cmd.exe'; hidden = $true }
            ) }
        }

        # Try to open Settings once; if the window can't take foreground, mark the suite skippable.
        $script:settingsOpen = $false
        try { Open-WtSettings -App $script:app -TimeoutSec 20 | Out-Null; $script:settingsOpen = $true }
        catch { Write-Host "Open-WtSettings failed (foreground denied?): $($_.Exception.Message)" }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'AI Agents settings page opens and shows the agent picker' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'LaunchNavItem'
        [xml]$manifest = Get-Content -LiteralPath (Join-Path $script:app.InstallLocation 'AppxManifest.xml') -Raw
        $startupTasks = @($manifest.SelectNodes("//*[local-name()='StartupTask' and @TaskId='StartTerminalOnLoginTask']"))
        $startupLabel = Get-WtReswTextRegex -Key 'Globals_StartOnUserLogin.Header'
        $startupLabel | Should -Not -BeNullOrEmpty
        $defaultProfileLabel = Get-WtReswTextRegex -Key 'Globals_DefaultProfile.Header'
        $defaultProfileLabel | Should -Not -BeNullOrEmpty
        (Get-UiTree -App $script:app -Depth 20) | Should -Match $defaultProfileLabel -Because 'Startup must render before checking optional settings'
        $shown = Test-Until -TimeoutSec 3 -IntervalSec 0.5 -Condition {
            (Get-UiTree -App $script:app -Depth 20) -match $startupLabel
        }
        $shown | Should -Be ($startupTasks.Count -gt 0) -Because 'a packaged app may intentionally omit the optional login task'
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # The AI Agents page renders the agent picker group (stable AutomationId AcpAgent).
        Test-UiElementExists -App $script:app -Selector 'AcpAgent' -TimeoutSec 8 | Should -BeTrue -Because 'the AI Agents page must show the agent picker'
    }

    It 'Settings retains the localized Tab Mode control' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AppearanceNavItem' | Out-Null
        $tabModeLabel = Get-WtReswTextRegex -Key 'Globals_TabLayout.Header'
        $tabModeLabel | Should -Not -BeNullOrEmpty
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition {
                (Get-UiTree -App $script:app -Depth 18) -match $tabModeLabel
            }) | Should -BeTrue -Because 'removing the FRE choice must not remove Tab Mode from Appearance'
    }

    It 'Model selection visible: the model control shows on the AI Agents page for a custom agent' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'
        # With a custom agent selected, the model control must be visible. The model container's
        # x:Name (AcpModel) is NOT surfaced as a winapp selector (winapp gives it a generated
        # grp-model-<hash> slug), so we assert its localized header instead — matched against an
        # ALL-LOCALES regex of AIAgents_AcpModel.Header (en-US "Model") so it holds on non-en-US
        # builds rather than hard-coding the English label.
        $rx = Get-WtReswTextRegex -Key 'AIAgents_AcpModel.Header'
        if (-not $rx) { $rx = '(?i)\bModel\b' }
        (Test-Until -TimeoutSec 8 -IntervalSec 0.5 -Condition { (Get-UiTree -App $script:app -Depth 16) -match $rx }) |
            Should -BeTrue -Because 'the model control (localized "Model" header) must be visible when a custom agent is selected'
    }

    It 'Token usage toggle defaults on and persists when disabled' {
        if (-not $script:settingsOpen) { Set-ItResult -Skipped -Because 'the WT window could not take foreground to open Settings (env precondition)'; return }
        Invoke-SettingsNav -App $script:app -NavItem 'AIAgentsNavItem'

        Test-UiElementExists -App $script:app -Selector 'ShowTokenUsageAndCostToggle' -TimeoutSec 8 |
            Should -BeTrue -Because 'Settings > Agents must expose the token usage preference'
        (Get-UiElement -App $script:app -Selector 'ShowTokenUsageAndCostToggle').toggleState |
            Should -Be 'on' -Because 'token usage and cost must be shown by default'

        Invoke-UiElement -App $script:app -Selector 'ShowTokenUsageAndCostToggle' | Out-Null
        Invoke-UiElement -App $script:app -Selector 'SaveButton' | Out-Null
        Wait-Until -TimeoutSec 8 -Because 'the Settings toggle to persist showTokenUsageAndCost=false' -Condition {
            (Get-WtSettingsObject -App $script:app).showTokenUsageAndCost -eq $false
        } | Out-Null
        Assert-Setting -App $script:app -Key 'showTokenUsageAndCost' -Value $false
    }
}

Describe 'Feature profile global agent selection' -Tag 'Feature', 'ProfileGlobalAgent' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $script:profileGuid = '{42e4499d-63ee-48ce-9fe9-d8d72e0dfb98}'
        $script:profileName = 'ITE2E Global Agent'
        $script:fixedProfileGuid = '{42e4499d-63ee-48ce-9fe9-d8d72e0dfb99}'
        $script:fixedProfileName = 'ITE2E Fixed Agent'
        $script:longLabelProfileGuid = '{42e4499d-63ee-48ce-9fe9-d8d72e0dfb9a}'
        $script:longLabelProfileName = 'ITE2E Long Agent Labels'
        # Spaces deliberately exercise the raw unknown-backend fallback; WslDistroName rejects them.
        $script:spacedBackendLabel = 'ITE2E Long Backend Ubuntu Development Distribution For Header Wrapping Regression'
        $script:unbrokenBackendLabel = 'ITE2E' + ('UbuntuDevelopmentDistribution' * 2) + 'HeaderWrappingRegression'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            language = 'en-US'
            acpAgent = 'copilot'
            delegateAgent = 'claude'
            autoErrorDetectionEnabled = $false
            agentSessionManagementEnabled = $false
            defaultProfile = $script:fixedProfileGuid
            firstWindowPreference = 'defaultProfile'
            profiles = @{
                defaults = @{ agentPaneBackend = 'host:codex'; commandPaletteAgent = 'host:codex' }
                list = @(
                    @{
                        guid = $script:profileGuid
                        name = $script:profileName
                        commandline = 'cmd.exe'
                        agentPaneBackend = 'host:copilot'
                        commandPaletteAgent = 'host:claude'
                    }
                    @{
                        guid = $script:fixedProfileGuid
                        name = $script:fixedProfileName
                        commandline = 'cmd.exe'
                        agentPaneBackend = 'host:copilot'
                        commandPaletteAgent = 'host:claude'
                    }
                    @{
                        guid = $script:longLabelProfileGuid
                        name = $script:longLabelProfileName
                        commandline = 'cmd.exe'
                        hidden = $true
                        agentPaneBackend = "wsl:$($script:spacedBackendLabel):copilot"
                        commandPaletteAgent = "wsl:$($script:unbrokenBackendLabel):claude"
                    }
                )
            }
        }

        function Get-ProfileAgentExpander {
            param([string]$Header)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::IsExpandCollapsePatternAvailableProperty, $true)
            $expander = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                Where-Object { $_.Current.Name.StartsWith($Header) } | Select-Object -First 1
            $scroller = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SettingsMainPage_ScrollViewer'))
            $viewport = if ($scroller) { $scroller.Current.BoundingRectangle } else { $null }
            if (-not $expander -or $expander.Current.IsOffscreen -or
                ($viewport -and ($expander.Current.BoundingRectangle.Top -lt $viewport.Top -or
                    $expander.Current.BoundingRectangle.Bottom -gt $viewport.Bottom))) {
                $scroller | Should -Not -BeNullOrEmpty
                $scroll = $scroller.GetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern)
                $scroll.Current.VerticallyScrollable | Should -BeTrue
                $scroll.SetScrollPercent([Windows.Automation.ScrollPattern]::NoScroll, 0)
                Wait-Until -TimeoutSec 8 -Because "$Header must be reachable in the Settings scroll viewport" -Condition {
                    $script:profileAgentVisibleExpander = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                        Where-Object { $_.Current.Name.StartsWith($Header) } | Select-Object -First 1
                    if ($script:profileAgentVisibleExpander -and -not $script:profileAgentVisibleExpander.Current.IsOffscreen) {
                        $bounds = $script:profileAgentVisibleExpander.Current.BoundingRectangle
                        if ($bounds.Top -ge $viewport.Top -and $bounds.Bottom -le $viewport.Bottom) { return $true }
                    }
                    if ($scroll.Current.VerticalScrollPercent -lt 100) {
                        $scroll.Scroll([Windows.Automation.ScrollAmount]::NoAmount, [Windows.Automation.ScrollAmount]::SmallIncrement)
                    }
                    $false
                } | Out-Null
                $expander = $script:profileAgentVisibleExpander
            }
            $expander | Should -Not -BeNullOrEmpty
            $expander
        }

        function Get-ProfileAgentCombo {
            param([string]$Header)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $expander = Get-ProfileAgentExpander -Header $Header
            $expander.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ComboBox)
            Wait-Until -TimeoutSec 8 -Because "$Header must expose its agent choices" -Condition {
                $script:profileAgentCombo = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                    Where-Object { $_.Current.Name.StartsWith($Header) } | Select-Object -First 1
                $null -ne $script:profileAgentCombo
            } | Out-Null
            $script:profileAgentCombo
        }

        function Assert-ProfileAgentPreview {
            param([string]$Header, [string]$ExpectedLabel)
            $expander = Get-ProfileAgentExpander -Header $Header
            $expander.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)
            # The separately bound accessible name can be correct while the visible preview is broken.
            $shown = Test-Until -TimeoutSec 8 -Condition {
                $script:profileAgentPreviewNames = @($expander.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                    Where-Object { -not $_.Current.IsOffscreen -and -not $_.Current.BoundingRectangle.IsEmpty } |
                    ForEach-Object { $_.Current.Name }
                $script:profileAgentPreviewNames -contains $ExpectedLabel -and
                    -not ($script:profileAgentPreviewNames -match 'Microsoft\.Terminal\.Settings\.Editor\.AgentEntry')
            }
            $shown | Should -BeTrue -Because "$Header must visibly preview '$ExpectedLabel', not [$($script:profileAgentPreviewNames -join '; ')]"
        }

        function Get-ProfileAgentSelection {
            param($Combo)
            $selected = $Combo.GetCurrentPattern([Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()[0]
            if ($selected.Current.Name) { return $selected.Current.Name }
            # Agents-page ListItems are unnamed; their provider label is a child Text peer.
            $textCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)
            $labels = @($selected.FindAll([Windows.Automation.TreeScope]::Descendants, $textCondition))
            $labels.Count | Should -Be 1 -Because 'the selected agent must have one unambiguous provider label'
            $labels[0].Current.Name
        }

        function Get-ProfileAgentPreviewPeer {
            param([string]$Header, [string]$ExpectedLabel)
            Assert-ProfileAgentPreview -Header $Header -ExpectedLabel $ExpectedLabel
            $expander = Get-ProfileAgentExpander -Header $Header
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text),
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::NameProperty, $ExpectedLabel))
            Wait-Until -TimeoutSec 8 -Because 'measure one settled collapsed-header Text, not its accessible expander name' -Condition {
                $peers = @($expander.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) |
                    Where-Object { -not $_.Current.IsOffscreen -and -not $_.Current.BoundingRectangle.IsEmpty }
                $script:profileAgentPreviewPeer = $peers | Select-Object -First 1
                @($peers).Count -eq 1
            } | Out-Null
            $script:profileAgentPreviewPeer
        }

        function Select-ProfileAgent {
            param($Combo, [string]$Name)
            $expand = $Combo.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
            $expand.Expand()
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem))
            $itemCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            $labelCondition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text))
            Wait-Until -TimeoutSec 8 -Because "$Name must be a distinct selectable option" -Condition {
                $choices = @($Combo.FindAll([Windows.Automation.TreeScope]::Descendants, $condition))
                if (-not $choices.Count) {
                    $choices = @($Combo.FindAll([Windows.Automation.TreeScope]::Descendants, $itemCondition)) |
                        Where-Object {
                            -not $_.Current.Name -and
                                @($_.FindAll([Windows.Automation.TreeScope]::Descendants, $labelCondition)).Count -eq 1
                        }
                    $choices = @($choices)
                }
                $choices.Count | Should -BeLessOrEqual 1 -Because 'the exact provider label must identify a unique ListItem'
                $script:profileAgentChoice = $choices | Select-Object -First 1
                $null -ne $script:profileAgentChoice
            } | Out-Null
            $script:profileAgentChoice.GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
            if ($expand.Current.ExpandCollapseState -eq [Windows.Automation.ExpandCollapseState]::Expanded) { $expand.Collapse() }
        }

        function Save-ProfileAgents {
            param([AllowEmptyString()][string]$Pane, [AllowEmptyString()][string]$Palette,
                [string]$ProfileGuid = $script:profileGuid)
            Invoke-UiElement -App $script:app -Selector SaveButton | Out-Null
            try {
                # Empty WinRT strings serialize as explicit JSON null, not an absent override.
                Wait-Until -TimeoutSec 8 -Because 'both profile selections must persist exactly' -Condition {
                    $profile = @((Get-WtSettingsObject -App $script:app).profiles.list) |
                        Where-Object guid -eq $ProfileGuid | Select-Object -First 1
                    $null -ne $profile -and
                        $profile.PSObject.Properties.Name -contains 'agentPaneBackend' -and
                        $profile.PSObject.Properties.Name -contains 'commandPaletteAgent' -and
                        [string]$profile.agentPaneBackend -ceq $Pane -and [string]$profile.commandPaletteAgent -ceq $Palette
                } | Out-Null
            }
            catch {
                @((Get-WtSettingsObject -App $script:app).profiles.list) |
                    Where-Object { $_.guid -in @($script:profileGuid, $script:fixedProfileGuid) } |
                    Select-Object guid, name, agentPaneBackend, commandPaletteAgent | ConvertTo-Json | Write-Host
                throw
            }
        }

        function Open-ProfileAgentSettings {
            param([string]$Name = $script:profileName, [switch]$CurrentSession)
            if ($CurrentSession) {
                Test-UiElementExists -App $script:app -Selector SettingsNav -TimeoutSec 1 | Should -BeTrue
            }
            else {
                Open-WtSettings -App $script:app | Out-Null
            }
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $nav = $root.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'SettingsNav'))
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Name),
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::IsSelectionItemPatternAvailableProperty, $true))
            $profile = $nav.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $profile | Should -Not -BeNullOrEmpty
            $profile.SetFocus()
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'profile navigation must update the visible page' -Condition {
                (Get-UiTree -App $script:app -Selector NavigationBreadcrumbBar -Depth 5) -match [regex]::Escape($Name)
            } | Out-Null
        }

        function Close-ProfileAgentSettings {
            Send-WtWindowKey -App $script:app -Vk 0x57 -Ctrl -Shift | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'the old Settings clone must close' -Condition {
                -not (Test-UiElementExists -App $script:app -Selector SettingsNav -TimeoutSec 1)
            } | Out-Null
        }

        function Change-GlobalAgents {
            param([string]$Pane, [string]$Palette)
            Close-ProfileAgentSettings
            Set-WtSetting -App $script:app -Key acpAgent -Value $Pane | Out-Null
            Set-WtSetting -App $script:app -Key delegateAgent -Value $Palette | Out-Null
        }

        function Test-ProfileHostAgents {
            # Use the same launch-ready host probe as AgentAvailability, not fallback UI labels.
            $probe = Invoke-Wta -App $script:app -Arguments @('probe-host-agents')
            foreach ($id in @('copilot', 'claude')) {
                $status = @($probe.availability | Where-Object id -eq $id)
                $status.Count | Should -Be 1 -Because 'an invalid probe response is a product failure'
                $status[0].native_cli_found | Should -BeOfType ([bool])
                $status[0].requires_npx | Should -BeOfType ([bool])
                $probe.npx_found | Should -BeOfType ([bool])
                if (-not $status[0].native_cli_found -or ($status[0].requires_npx -and -not $probe.npx_found)) {
                    Set-ItResult -Skipped -Because "external prerequisite missing: installed Windows $id CLI and its required npx"
                    return $false
                }
                $status[0].launch_ready | Should -BeTrue
                @($probe.agents.id) | Should -Contain $id
            }
            return $true
        }

        function Get-GlobalAgentCombo {
            param([string]$AutomationId)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $AutomationId)
            Wait-Until -TimeoutSec 8 -Because "$AutomationId must render on the actual Agents page" -Condition {
                $script:globalAgentCombo = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
                $null -ne $script:globalAgentCombo
            } | Out-Null
            $script:globalAgentCombo
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Profile agent pickers distinguish global and fixed selections' {
        if (-not (Test-ProfileHostAgents)) { return }
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        Select-ProfileAgent $pane 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent $palette 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette '' -ProfileGuid $script:fixedProfileGuid
        Close-ProfileAgentSettings
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Be 'Use global setting (GitHub Copilot)'
        (Get-ProfileAgentSelection $palette) | Should -Be 'Use global setting (Claude)'
        Select-ProfileAgent $pane 'GitHub Copilot · Windows'
        Select-ProfileAgent $palette 'Claude · Windows'
        Save-ProfileAgents -Pane 'host:copilot' -Palette 'host:claude' -ProfileGuid $script:fixedProfileGuid
        Close-ProfileAgentSettings
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')) | Should -Be 'GitHub Copilot · Windows'
        (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')) | Should -Be 'Claude · Windows'
        $fixed = @((Get-WtSettingsObject -App $script:app).profiles.list) |
            Where-Object guid -eq $script:fixedProfileGuid | Select-Object -First 1
        $fixed.agentPaneBackend | Should -Be 'host:copilot'
        $fixed.commandPaletteAgent | Should -Be 'host:claude'

        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        Select-ProfileAgent $pane 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent $palette 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''

        Change-GlobalAgents -Pane claude -Palette codex
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Be 'GitHub Copilot · Windows'
        (Get-ProfileAgentSelection $palette) | Should -Be 'Claude · Windows'
        Save-ProfileAgents -Pane 'host:copilot' -Palette 'host:claude' -ProfileGuid $script:fixedProfileGuid

        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Be 'Use global setting (Claude)'
        (Get-ProfileAgentSelection $palette) | Should -Be 'Use global setting (Codex)'

        Change-GlobalAgents -Pane copilot -Palette claude
        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        (Get-ProfileAgentSelection $pane) | Should -Be 'Use global setting (GitHub Copilot)'
        (Get-ProfileAgentSelection $palette) | Should -Be 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''
    }

    It 'Profile agent picker previews show selected labels' -Tag 'ProfileAgentPreview' {
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        $fixedPane = Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')
        $fixedPalette = Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')
        Assert-ProfileAgentPreview -Header 'Agent pane agent' -ExpectedLabel $fixedPane
        Assert-ProfileAgentPreview -Header 'Command palette agent' -ExpectedLabel $fixedPalette

        Open-ProfileAgentSettings
        $pane = Get-ProfileAgentCombo -Header 'Agent pane agent'
        $palette = Get-ProfileAgentCombo -Header 'Command palette agent'
        Select-ProfileAgent $pane 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent $palette 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''
        Close-ProfileAgentSettings
        Open-ProfileAgentSettings
        Assert-ProfileAgentPreview -Header 'Agent pane agent' -ExpectedLabel 'Use global setting (GitHub Copilot)'
        Assert-ProfileAgentPreview -Header 'Command palette agent' -ExpectedLabel 'Use global setting (Claude)'
    }

    It 'Profile agent headers wrap long selected labels' -Tag 'ProfileAgentWrapping' {
        if (-not (Test-ProfileHostAgents)) { return }
        Open-ProfileAgentSettings -Name $script:longLabelProfileName
        try {
            foreach ($case in @(
                @{ Header = 'Agent pane agent'; Short = 'GitHub Copilot · Windows'; Seed = $script:spacedBackendLabel }
                @{ Header = 'Command palette agent'; Short = 'Claude · Windows'; Seed = $script:unbrokenBackendLabel }
            )) {
                $combo = Get-ProfileAgentCombo -Header $case.Header
                $longLabel = Get-ProfileAgentSelection $combo
                $longLabel | Should -Match ([regex]::Escape($case.Seed))
                # Keep the same list: navigation/rebuild would drop its now-unselected fallback row.
                Select-ProfileAgent $combo $case.Short
                $shortPeer = Get-ProfileAgentPreviewPeer -Header $case.Header -ExpectedLabel $case.Short
                $shortHeight = $shortPeer.Current.BoundingRectangle.Height
                $shortHeight | Should -BeGreaterThan 0
                $combo = Get-ProfileAgentCombo -Header $case.Header
                Select-ProfileAgent $combo $longLabel
                (Get-ProfileAgentSelection $combo) | Should -BeExactly $longLabel
                $wrapped = Test-Until -TimeoutSec 8 -Condition {
                    $peer = Get-ProfileAgentPreviewPeer -Header $case.Header -ExpectedLabel $longLabel
                    $peer.Current.BoundingRectangle.Height -gt (1.5 * $shortHeight)
                }
                $longPeer = Get-ProfileAgentPreviewPeer -Header $case.Header -ExpectedLabel $longLabel
                Write-Host "$($case.Header): short Text height=$shortHeight; long Text height=$($longPeer.Current.BoundingRectangle.Height)"
                $wrapped | Should -BeTrue -Because "$($case.Header) must render multiline text, not just expose the full UIA name"
            }
        }
        finally {
            # No Save, profile launch, WSL probe or agent invocation; discard only this Settings clone.
            Close-ProfileAgentSettings
        }
    }

    It 'Cached profile agent choices refresh after unsaved global edits' -Tag 'ProfileAgentRefresh' {
        if (-not (Test-ProfileHostAgents)) { return }
        Open-ProfileAgentSettings
        Change-GlobalAgents -Pane copilot -Palette claude
        Open-ProfileAgentSettings
        Select-ProfileAgent (Get-ProfileAgentCombo -Header 'Agent pane agent') 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent (Get-ProfileAgentCombo -Header 'Command palette agent') 'Use global setting (Claude)'
        Save-ProfileAgents -Pane '' -Palette ''

        Open-ProfileAgentSettings -Name 'Defaults'
        Select-ProfileAgent (Get-ProfileAgentCombo -Header 'Agent pane agent') 'Use global setting (GitHub Copilot)'
        Select-ProfileAgent (Get-ProfileAgentCombo -Header 'Command palette agent') 'Use global setting (Claude)'
        Invoke-UiElement -App $script:app -Selector SaveButton | Out-Null
        Wait-Until -TimeoutSec 8 -Because 'Defaults must explicitly follow globals before the unsaved edit' -Condition {
            $defaults = (Get-WtSettingsObject -App $script:app).profiles.defaults
            $defaults.PSObject.Properties.Name -contains 'agentPaneBackend' -and
                $defaults.PSObject.Properties.Name -contains 'commandPaletteAgent' -and
                [string]$defaults.agentPaneBackend -ceq '' -and [string]$defaults.commandPaletteAgent -ceq ''
        } | Out-Null
        Open-ProfileAgentSettings -Name $script:fixedProfileName
        Select-ProfileAgent (Get-ProfileAgentCombo -Header 'Agent pane agent') 'GitHub Copilot · Windows'
        Select-ProfileAgent (Get-ProfileAgentCombo -Header 'Command palette agent') 'Claude · Windows'
        Save-ProfileAgents -Pane 'host:copilot' -Palette 'host:claude' -ProfileGuid $script:fixedProfileGuid

        Close-ProfileAgentSettings
        Open-ProfileAgentSettings
        # Cache both global-following view models after the last Save and its settings reload.
        foreach ($name in @($script:profileName, 'Defaults')) {
            Open-ProfileAgentSettings -Name $name -CurrentSession
            (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')) | Should -Be 'Use global setting (GitHub Copilot)'
            (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')) | Should -Be 'Use global setting (Claude)'
        }
        Open-ProfileAgentSettings -Name $script:fixedProfileName -CurrentSession
        (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')) | Should -Be 'GitHub Copilot · Windows'
        (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')) | Should -Be 'Claude · Windows'

        # From here, never save, close/reopen Settings or write disk.
        Open-ProfileAgentSettings -Name 'Agents' -CurrentSession
        $pane = Get-GlobalAgentCombo -AutomationId AcpAgentComboBox
        $palette = Get-GlobalAgentCombo -AutomationId DelegateAgentComboBox
        Select-ProfileAgent $pane 'Claude'
        Select-ProfileAgent $palette 'GitHub Copilot'
        (Get-ProfileAgentSelection $pane) | Should -Be 'Claude'
        (Get-ProfileAgentSelection $palette) | Should -Be 'GitHub Copilot'

        foreach ($name in @($script:profileName, 'Defaults')) {
            Open-ProfileAgentSettings -Name $name -CurrentSession
            (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')) | Should -Be 'Use global setting (Claude)'
            (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')) | Should -Be 'Use global setting (GitHub Copilot)'
            Assert-ProfileAgentPreview -Header 'Agent pane agent' -ExpectedLabel 'Use global setting (Claude)'
            Assert-ProfileAgentPreview -Header 'Command palette agent' -ExpectedLabel 'Use global setting (GitHub Copilot)'
        }
        Open-ProfileAgentSettings -Name $script:fixedProfileName -CurrentSession
        (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Agent pane agent')) | Should -Be 'GitHub Copilot · Windows'
        (Get-ProfileAgentSelection (Get-ProfileAgentCombo -Header 'Command palette agent')) | Should -Be 'Claude · Windows'
        Assert-ProfileAgentPreview -Header 'Agent pane agent' -ExpectedLabel 'GitHub Copilot · Windows'
        Assert-ProfileAgentPreview -Header 'Command palette agent' -ExpectedLabel 'Claude · Windows'
        $persisted = Get-WtSettingsObject -App $script:app
        $persisted.acpAgent | Should -Be 'copilot' -Because 'global UI edits must remain unsaved throughout the regression'
        $persisted.delegateAgent | Should -Be 'claude'
    }
}

Describe 'Feature Settings editor language direction' -Tag 'Feature', 'SettingsRtl' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:languageResults = @{}
    }

    It 'Settings follows UI direction: <Language>' -ForEach @(
        @{ Language = 'ar-SA'; Rtl = $true }
        @{ Language = 'he-IL'; Rtl = $true }
        @{ Language = 'qps-plocm'; Rtl = $true }
        @{ Language = 'en-US'; Rtl = $false }
    ) {
        $app = $null
        $foregroundUnavailable = $false
        $script:languageResults[$Language] = 'Failed'
        try {
            $app = Start-Terminal -Package (Get-ItTestPackage) -TimeoutSec 60 -Settings @{
                language = $Language
                tabLayout = 'horizontal'
                initialCols = 120
                initialRows = 35
                firstWindowPreference = 'defaultProfile'
                minimizeToNotificationArea = $false
                alwaysShowNotificationIcon = $false
                showTokenUsageAndCost = $true
                'aiIntegration.coordinator.enabled' = $false
            } -State @{ featureRolloutId = '{00000000-0000-0000-0000-000000000001}'; sidebarRolloutVersion = 1; sidebarRolloutAssignment = 'Sidebar'; sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true }
            if (-not (Test-WtWindowKeyFocusable -App $app)) {
                $foregroundUnavailable = $true
                Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+, accelerator'
                return
            }
            Open-WtSettings -App $app -TimeoutSec 25 | Out-Null
            $settingsRoot = Get-UiElement -App $app -Selector SettingsNav
            $settingsRoot | Should -Not -BeNullOrEmpty
            $center = $settingsRoot.x + $settingsRoot.width / 2
            foreach ($selector in 'SettingsSearchBox', 'SaveButton', 'ResetButton') {
                $element = Get-UiElement -App $app -Selector $selector
                $element | Should -Not -BeNullOrEmpty -Because "$selector must be visible"
                $element.isOffscreen | Should -BeFalse
                $onRight = $element.x + $element.width / 2 -gt $center
                $expectedOnRight = if ($selector -eq 'SettingsSearchBox') { $Rtl } else { -not $Rtl }
                $onRight | Should -Be $expectedOnRight -Because "$selector must follow $Language layout"
            }
            Invoke-SettingsNav -App $app -NavItem AIAgentsNavItem | Out-Null
            $tokenUsageToggle = Get-UiElement -App $app -Selector ShowTokenUsageAndCostToggle
            $tokenUsageToggle.toggleState | Should -Be 'on'
            $tokenUsageToggle.isOffscreen | Should -BeFalse
            ($tokenUsageToggle.x + $tokenUsageToggle.width / 2 -gt $center) |
                Should -Be (-not $Rtl) -Because "the setting control must follow $Language layout"
            Invoke-UiElement -App $app -Selector ShowTokenUsageAndCostToggle | Out-Null
            Invoke-UiElement -App $app -Selector SaveButton -TimeoutSec 15 | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'Save must persist the changed preference' -Condition {
                (Get-WtSettingsObject -App $app).showTokenUsageAndCost -eq $false
            } | Out-Null
            Assert-Setting -App $app -Key language -Value $Language
            Invoke-UiElement -App $app -Selector ShowTokenUsageAndCostToggle | Out-Null
            (Get-UiElement -App $app -Selector ShowTokenUsageAndCostToggle).toggleState | Should -Be 'on'
            Invoke-UiElement -App $app -Selector ResetButton -TimeoutSec 15 | Out-Null
            Wait-Until -TimeoutSec 8 -Because 'Discard must restore the saved preference' -Condition {
                (Get-UiElement -App $app -Selector ShowTokenUsageAndCostToggle).toggleState -eq 'off'
            } | Out-Null
            Assert-Setting -App $app -Key showTokenUsageAndCost -Value $false
            Invoke-SettingsNav -App $app -NavItem LaunchNavItem | Out-Null
        }
        finally {
            if ($app) { Stop-Terminal -App $app | Out-Null }
            if ($foregroundUnavailable) { $script:languageResults[$Language] = 'Skipped' }
        }
        $script:languageResults[$Language] = 'Passed'
    }

    It 'Settings follows UI direction: complete matrix' {
        $script:languageResults.Values | Should -Not -Contain 'Failed' -Because 'every language must pass'
        if ($script:languageResults.Count -ne 4 -or $script:languageResults.Values -contains 'Skipped') {
            Set-ItResult -Skipped -Because 'not all four language cases completed'
            return
        }
    }
}
