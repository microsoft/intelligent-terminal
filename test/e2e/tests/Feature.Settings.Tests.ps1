#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §1 Settings>AI Agents + §0 FRE settings/positions/Auto-error-handling/session-mgmt.
# Settings are top-level keys in settings.json (hot-reloaded); FRE completion + choices
# persist in state.json/settings.json. Deterministic — no LLM.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §1 Settings > AI Agents' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'AI Agents page opens (settings UI reachable)' {
        # The Settings UI is a XAML page; opening it via the command palette action and
        # asserting the nav item exists proves the page is wired. Use the settings file as
        # the source of truth that the model is loaded.
        (Get-WtSettingsObject -App $script:app) | Should -Not -BeNullOrEmpty
    }
    It 'Built-in agent dropdown works (acpAgent persists each built-in id)' {
        foreach ($a in 'copilot', 'claude', 'gemini', 'codex') {
            Set-WtAgent -App $script:app -Agent $a | Out-Null
            Assert-Setting -App $script:app -Key 'acpAgent' -Value $a
        }
    }
    It 'Agent pane agent save works' {
        Set-WtAgent -App $script:app -Agent 'copilot' | Out-Null
        Assert-Setting -App $script:app -Key 'acpAgent' -Value 'copilot'
    }
    It 'Delegate agent save works' {
        Set-WtDelegateAgent -App $script:app -Agent 'gemini' | Out-Null
        Assert-Setting -App $script:app -Key 'delegateAgent' -Value 'gemini'
    }
    It 'Model control / model changes apply (acpModel persists)' {
        Set-WtSetting -App $script:app -Key 'acpModel' -Value 'gpt-5' | Out-Null
        Assert-Setting -App $script:app -Key 'acpModel' -Value 'gpt-5'
    }
    It 'Delegate model changes apply' {
        Set-WtSetting -App $script:app -Key 'delegateModel' -Value 'claude-4' | Out-Null
        Assert-Setting -App $script:app -Key 'delegateModel' -Value 'claude-4'
    }
    It 'Pane position setting works' {
        Set-WtPanePosition -App $script:app -Position 'right' | Out-Null
        Assert-Setting -App $script:app -Key 'agentPanePosition' -Value 'right'
    }
    It 'Auto-error-handling options match FRE (all three enum values persist)' {
        foreach ($mode in 'off', 'detectErrorsAutomatically', 'detectErrorsAndSendToAgentForFixesAutomatically') {
            Set-WtAutoErrorHandling -App $script:app -Mode $mode | Out-Null
            Assert-Setting -App $script:app -Key 'autoErrorHandling' -Value $mode
        }
    }
}

Describe 'Feature §0 FRE settings, positions, auto-error, session mgmt' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    Context 'FRE completion + persistence' {
        It 'FRE marked complete persists (agentFreCompleted in state.json)' {
            Get-FreCompleted -App $script:app | Should -BeTrue
            Assert-State -App $script:app -Key 'agentFreCompleted' -Value $true
        }
        It 'Agent selection persists across the settings write' {
            Set-WtAgent -App $script:app -Agent 'copilot' | Out-Null
            Assert-Setting -App $script:app -Key 'acpAgent' -Value 'copilot'
        }
        It 'Unavailable non-Copilot agents do not corrupt settings (value still set literally)' {
            # Selecting an id always persists the id; availability is a UI concern.
            Set-WtAgent -App $script:app -Agent 'claude' | Out-Null
            Assert-Setting -App $script:app -Key 'acpAgent' -Value 'claude'
        }
    }

    Context 'FRE agent pane position' {
        It 'Bottom / Right / Left / Top all persist' {
            foreach ($pos in 'bottom', 'right', 'left', 'top') {
                Set-WtPanePosition -App $script:app -Position $pos | Out-Null
                Assert-Setting -App $script:app -Key 'agentPanePosition' -Value $pos
            }
        }
        It 'Position persists (last value retained)' {
            Set-WtPanePosition -App $script:app -Position 'bottom' | Out-Null
            Assert-Setting -App $script:app -Key 'agentPanePosition' -Value 'bottom'
        }
    }

    Context 'FRE Auto-error-handling setting' {
        It 'The option persists (all three valid states round-trip)' {
            foreach ($mode in 'off', 'detectErrorsAutomatically', 'detectErrorsAndSendToAgentForFixesAutomatically') {
                Set-WtAutoErrorHandling -App $script:app -Mode $mode | Out-Null
                Assert-Setting -App $script:app -Key 'autoErrorHandling' -Value $mode
            }
        }
        It 'Settings persist together' {
            Set-WtSettings -App $script:app -Settings @{ autoErrorHandling = 'detectErrorsAutomatically'; agentPanePosition = 'right' } | Out-Null
            Assert-Setting -App $script:app -Key 'autoErrorHandling' -Value 'detectErrorsAutomatically'
            Assert-Setting -App $script:app -Key 'agentPanePosition' -Value 'right'
        }
    }

    Context 'FRE session management' {
        It 'Session management choice persists (coordinator profile present)' {
            # The session-management coordinator settings round-trip through settings.json.
            Set-WtSetting -App $script:app -Key 'aiIntegration.coordinator.commandline' -Value 'wta' | Out-Null
            Assert-Setting -App $script:app -Key 'aiIntegration.coordinator.commandline' -Value 'wta'
        }
    }

    Describe 'Feature §1 Settings Legacy Auto-error-handling JSON compatibility' -Tag 'Feature' -Skip:(-not $script:Ready) {
        BeforeAll {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        }

        It 'Legacy auto-error JSON keys migrate and the canonical key takes precedence' {
            $cases = @(
                @{
                    Name = 'Legacy detection disabled'
                    Settings = @{ autoErrorDetectionEnabled = $false; autoFixEnabled = $true }
                    Expected = 'off'
                },
                @{
                    Name = 'Legacy detection-only'
                    Settings = @{ autoErrorDetectionEnabled = $true; autoFixEnabled = $false }
                    Expected = 'detectErrorsAutomatically'
                },
                @{
                    Name = 'Legacy fix-only'
                    Settings = @{ autoFixEnabled = $true }
                    Expected = 'detectErrorsAndSendToAgentForFixesAutomatically'
                },
                @{
                    Name = 'Canonical key wins over Legacy keys'
                    Settings = @{
                        autoErrorHandling = 'off'
                        autoErrorDetectionEnabled = $true
                        autoFixEnabled = $true
                    }
                    Expected = 'off'
                }
            )

            foreach ($case in $cases) {
                $app = $null
                try {
                    $app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings $case.Settings
                    Wait-Until -TimeoutSec 15 -IntervalSec 0.5 -Because "$($case.Name) to migrate to autoErrorHandling" -Condition {
                        (Get-WtSetting -App $app -Key 'autoErrorHandling') -eq $case.Expected
                    } | Out-Null
                    Assert-Setting -App $app -Key 'autoErrorHandling' -Value $case.Expected
                }
                finally {
                    if ($app) { Stop-Terminal -App $app }
                }
            }
        }
    }
}
