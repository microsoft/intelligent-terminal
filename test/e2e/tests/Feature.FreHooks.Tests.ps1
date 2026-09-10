#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §0 FRE session management (and §1/§8 install parts) — the FRE hook-install
# behaviour, driven end-to-end: the FRE "Session management" toggle on Save shells the in-package
# wta to install agent hooks, and the result is observable on disk in ~/.copilot/config.json
# (installedPlugins[] wt-agent-hooks). We snapshot/restore that config so the developer's real
# hook state is preserved, and use Remove-CopilotHooksEntry to get a deterministic "not installed"
# baseline before each case.
#
# Hook install failure is induced without changing the user's CLI configuration: a Debug-only
# LocalState marker makes the packaged FRE hooks operation return false. FRE must surface the
# blocking Hooks problem, disable session management, and succeed on retry with Sessions skipped.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = [bool](
            (Get-Command winapp -ErrorAction SilentlyContinue) -and
            (Get-Command copilot -ErrorAction SilentlyContinue) -and
            (Test-WtExecutionPolicyControllable) -and
            (-not (Test-WtPwshBlocksShellIntegration)))
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature §0 FRE session-management hook install' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # Snapshot the real copilot config so install/uninstall during these tests is reverted.
        $script:cfgBackup = Backup-CopilotConfig
        $script:epBackup = Set-WtExecutionPolicy -Value RemoteSigned
        $script:failureMarker = Join-Path (Resolve-ItApp -Package Dev).LocalStateDir 'fre-e2e-hooks-failure'
        Remove-Item -LiteralPath $script:failureMarker -Force -ErrorAction SilentlyContinue
    }
    AfterAll {
        Remove-Item -LiteralPath $script:failureMarker -Force -ErrorAction SilentlyContinue
        if ($script:epBackup) { Restore-WtExecutionPolicy -State $script:epBackup }
        if ($script:cfgBackup) { Restore-CopilotConfig -State $script:cfgBackup }
    }

    It 'Session management on installs agent hooks (FRE Save)' {
        # Start from a deterministic not-installed baseline.
        Remove-CopilotHooksEntry
        Get-CopilotHooksInstalled | Should -BeFalse -Because 'the baseline must be not-installed so a later true proves the FRE installed it'

        $app = Start-TerminalFre -Package Dev
        try {
            Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
            Start-Sleep -Seconds 1
            # Ensure session management is ON (default), then Save — Save shells `wta hooks install`.
            if ((Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState -ne 'on') {
                Invoke-UiElement -App $app -Selector 'SessionManagementToggle' | Out-Null; Start-Sleep -Milliseconds 800
            }
            Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
            # Hook install runs synchronously on Save (up to a 60s timeout) then the overlay dismisses.
            Test-Until -TimeoutSec 90 -IntervalSec 2 -Condition { -not (Test-FreShowing -App $app) } |
                Should -BeTrue -Because 'successful hook setup must dismiss the FRE'
            $installed = Test-Until -TimeoutSec 20 -IntervalSec 2 -Condition { Get-CopilotHooksInstalled }
            $installed | Should -BeTrue -Because 'enabling session management and saving the FRE must install the wt-agent-hooks plugin'

            $freLog = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
            Test-FreProgressOrder -Log $freLog -Events @(
                'attempt=started'
                'setup=running'
                'setup=completed'
                'agent=running'
                'agent=completed'
                'error-detection=running'
                'sessions=running'
                'sessions=completed'
            ) | Should -BeTrue -Because 'the FRE checklist must enter Error Detection before Sessions and preserve completed rows'
            $freLog | Should -Match '\[FRE\] Progress: error-detection=(completed|warning)' -Because 'the hooks happy path requires non-blocking shell integration'

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

        $app = Start-TerminalFre -Package Dev
        try {
            Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
            Start-Sleep -Seconds 1
            if ((Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState -eq 'on') {
                Invoke-UiElement -App $app -Selector 'SessionManagementToggle' | Out-Null; Start-Sleep -Milliseconds 800
            }
            (Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState | Should -Be 'off'
            Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
            Test-Until -TimeoutSec 60 -IntervalSec 2 -Condition { -not (Test-FreShowing -App $app) } |
                Should -BeTrue -Because 'saving with Sessions disabled must complete and dismiss the FRE'
            Get-FreCompleted -App $app | Should -BeTrue

            # No install happened…
            Get-CopilotHooksInstalled | Should -BeFalse -Because 'session management OFF must not install hooks'
            (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) |
                Should -Not -Match '\[FRE\] Progress: sessions=running' -Because 'a disabled step must not appear in the progressive checklist'
            # …and the terminal is usable (session UI remains stable).
            (Get-WtSettingsObject -App $app) | Should -Not -BeNullOrEmpty
            $paneOk = Test-Until -TimeoutSec 15 -IntervalSec 1 -Condition { try { [bool](Get-ActivePane -App $app) } catch { $false } }
            $paneOk | Should -BeTrue -Because 'the terminal must remain usable after completing the FRE with session management off'
        }
        finally { Stop-Terminal -App $app }
    }

    It 'Hook install failure blocks once, disables Sessions, and succeeds on retry' {
        Remove-CopilotHooksEntry
        Get-CopilotHooksInstalled | Should -BeFalse
        $app = Start-TerminalFre -Package Dev
        try {
            Invoke-UiElement -App $app -Selector 'NextButton' -TimeoutSec 10 | Out-Null
            Start-Sleep -Seconds 1
            if ((Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState -ne 'on') {
                Invoke-UiElement -App $app -Selector 'SessionManagementToggle' | Out-Null
                Start-Sleep -Milliseconds 800
            }

            New-Item -ItemType File -Path $script:failureMarker -Force | Out-Null
            Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null

            $failed = Test-Until -TimeoutSec 30 -IntervalSec 1 -Condition {
                (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) -match '\[FRE\] Progress: sessions=failed'
            }
            $failed | Should -BeTrue
            (Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart) |
                Should -Match '\[FRE\] E2E: forcing hooks install failure' -Because 'the deterministic Debug fault seam must drive this failure'
            Test-UiElementExists -App $app -Selector 'ErrorPanel' -TimeoutSec 10 |
                Should -BeTrue -Because 'hooks failure must surface the existing blocking FRE error panel'
            Get-FreCompleted -App $app | Should -BeFalse
            (Get-UiElement -App $app -Selector 'SessionManagementToggle').toggleState |
                Should -Be 'off' -Because 'the existing hooks remediation disables Sessions before retry'

            $firstAttemptLog = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
            Test-FreProgressOrder -Log $firstAttemptLog -Events @(
                'error-detection=running'
                'sessions=running'
                'sessions=failed'
            ) | Should -BeTrue -Because 'hooks failure must not prevent the earlier Error Detection step from running'

            Remove-Item -LiteralPath $script:failureMarker -Force
            Invoke-UiElement -App $app -Selector 'SaveButton' -TimeoutSec 10 | Out-Null
            $completed = Test-Until -TimeoutSec 60 -IntervalSec 1 -Condition { Get-FreCompleted -App $app }
            $completed | Should -BeTrue -Because 'retry with Sessions disabled must complete the FRE'

            $allAttemptsLog = Get-ItLogText -App $app -Name 'terminal-agent-pane.log' -SinceStart
            ([regex]::Matches($allAttemptsLog, '\[FRE\] Progress: attempt=started')).Count |
                Should -Be 2 -Because 'retry must reset and start a new checklist attempt'
            ([regex]::Matches($allAttemptsLog, '\[FRE\] Progress: sessions=running')).Count |
                Should -Be 1 -Because 'the retry must omit the now-disabled Sessions step'
        }
        finally {
            Remove-Item -LiteralPath $script:failureMarker -Force -ErrorAction SilentlyContinue
            Stop-Terminal -App $app
        }
    }
}
