#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE session management (and §1/§8 install parts) — the FRE hook-install
# behaviour, driven end-to-end: the FRE "Session management" toggle on Save shells the in-package
# wta to install agent hooks, and the result is observable on disk in ~/.copilot/config.json
# (installedPlugins[] wt-agent-hooks). We snapshot/restore that config so the developer's real
# hook state is preserved, and use Remove-CopilotHooksEntry to get a deterministic "not installed"
# baseline before each case.
#
# Not covered here: "Hook install failure" — inducing a missing-CLI / disabled-plugin / partial
# state is destructive and non-deterministic, so it stays manual (the failure handling itself is
# UT-adjacent: FreOverlay surfaces the Hooks problem and still completes, agent_hooks_installer
# error paths are unit-tested).

BeforeDiscovery {
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command winapp -ErrorAction SilentlyContinue) -and (Get-Command copilot -ErrorAction SilentlyContinue))
}

Describe 'Feature §0 FRE session-management hook install' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # Snapshot the real copilot config so install/uninstall during these tests is reverted.
        $script:cfgBackup = Backup-CopilotConfig
    }
    AfterAll {
        if ($script:cfgBackup) { Restore-CopilotConfig -State $script:cfgBackup }
    }

    It 'Session management on installs agent hooks (FRE Save)' {
        # Start from a deterministic not-installed baseline.
        Remove-CopilotHooksEntry
        Get-CopilotHooksInstalled | Should -BeFalse -Because 'the baseline must be not-installed so a later true proves the FRE installed it'

        $app = Start-TerminalFre -Package (Get-ItTestPackage)
        try {
            Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
            Start-Sleep -Seconds 1
            # Ensure session management is ON (default), then Save — Save shells `wta hooks install`.
            if ((Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState -ne 'on') {
                Invoke-UiElement -App $app -Selector 'SessionManagementToggle' | Out-Null; Start-Sleep -Milliseconds 800
            }
            Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
            # Hook install runs synchronously on Save (up to a 60s timeout) then the overlay dismisses.
            Test-Until -TimeoutSec 90 -IntervalSec 2 -Condition { -not (Test-FreShowing -App $app) } | Out-Null
            $installed = Test-Until -TimeoutSec 20 -IntervalSec 2 -Condition { Get-CopilotHooksInstalled }
            $installed | Should -BeTrue -Because 'enabling session management and saving the FRE must install the wt-agent-hooks plugin'

            # Hook logs are available: the install writes its decisions to the WTA hook log.
            $logDir = Get-ItLogDir -App $app
            if ($logDir) {
                $hookLog = Join-Path $logDir 'wta-install-hooks.log'
                Test-Path $hookLog | Should -BeTrue -Because 'hook install decisions must be recorded in wta-install-hooks.log'
                (Get-Item $hookLog).Length | Should -BeGreaterThan 0
            }
        }
        finally { Stop-Terminal -App $app }
    }

    It 'Session management off does not install hooks and leaves a usable terminal' {
        # Not-installed baseline; with the toggle OFF, Save must NOT install hooks.
        Remove-CopilotHooksEntry
        Get-CopilotHooksInstalled | Should -BeFalse

        $app = Start-TerminalFre -Package (Get-ItTestPackage)
        try {
            Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
            Start-Sleep -Seconds 1
            if ((Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState -eq 'on') {
                Invoke-UiElement -App $app -Selector 'SessionManagementToggle' | Out-Null; Start-Sleep -Milliseconds 800
            }
            (Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState | Should -Be 'off'
            Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
            Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition { -not (Test-FreShowing -App $app) } | Out-Null

            # No install happened…
            Get-CopilotHooksInstalled | Should -BeFalse -Because 'session management OFF must not install hooks'
            # …and the terminal is usable (session UI remains stable).
            (Get-WtSettingsObject -App $app) | Should -Not -BeNullOrEmpty
            $paneOk = Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { try { [bool](Get-ActivePane -App $app) } catch { $false } }
            $paneOk | Should -BeTrue -Because 'the terminal must remain usable after completing the FRE with session management off'
        }
        finally { Stop-Terminal -App $app }
    }
}
