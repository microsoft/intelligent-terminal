#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE UI flow (the overlay click-through). Driven via winapp ui
# (the FRE is a XAML overlay with AutomationId buttons NextButton / SaveButton).
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §0 FRE overlay flow' -Tag 'Feature' -Skip:(-not $script:Ready) {

    Context 'FRE opens' {
        It 'FRE opens correctly (Welcome page shows on a fresh profile)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                Test-UiElementExists -App $app -Selector 'WelcomePage' -TimeoutSec 10 | Should -BeTrue
                Test-UiElementExists -App $app -Selector 'NextButton' -TimeoutSec 5 | Should -BeTrue
            }
            finally { Stop-Terminal -App $app }
        }
    }

    Context 'FRE privacy/help link' {
        It 'FRE privacy / help link is present on the welcome page' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                $tree = Get-UiTree -App $app -Depth 8
                $tree | Should -Match 'Learn more|privacy|Privacy'
            }
            finally { Stop-Terminal -App $app }
        }
    }

    Context 'FRE completion' {
        It 'FRE can be completed (Next -> Save dismisses the overlay and marks complete)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                Test-FreShowing -App $app | Should -BeTrue
                Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
                Wait-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                # The overlay is dismissed…
                $dismissed = Test-Until -TimeoutSec 20 -Condition { -not (Test-FreShowing -App $app) }
                $dismissed | Should -BeTrue
                # …and the completion flag is persisted (written async after save/setup work).
                $flagged = Test-Until -TimeoutSec 20 -Condition { Get-FreCompleted -App $app }
                $flagged | Should -BeTrue
            }
            finally { Stop-Terminal -App $app }
        }

        It 'FRE save progress / completion leaves a usable terminal (settings valid)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            try {
                Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
                Wait-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
                Test-Until -TimeoutSec 20 -Condition { -not (Test-FreShowing -App $app) } | Out-Null
                # The terminal is usable: settings.json parses and a pane responds
                # (allow a moment for the page to settle after FRE save).
                (Get-WtSettingsObject -App $app) | Should -Not -BeNullOrEmpty
                $paneOk = Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { try { [bool](Get-ActivePane -App $app) } catch { $false } }
                $paneOk | Should -BeTrue
            }
            finally { Stop-Terminal -App $app }
        }
    }

    Context 'FRE close safety' {
        It 'FRE can be closed safely (closing the window mid-FRE leaves settings valid)' {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $app = Start-TerminalFre -Package (Get-ItTestPackage)
            Test-FreShowing -App $app | Should -BeTrue
            # Closing the window during FRE must not corrupt settings/state.
            Stop-Terminal -App $app
            # Relaunch (FRE was never completed, so settings.json must still parse).
            $app2 = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
            try {
                (Get-WtSettingsObject -App $app2) | Should -Not -BeNullOrEmpty
            }
            finally { Stop-Terminal -App $app2 }
        }
    }
}
