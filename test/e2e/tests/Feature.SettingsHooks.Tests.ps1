#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §1 Settings > AI Agents — session-hook install / remove.
#
# The Settings editor's "Install hooks" / per-CLI "Remove" buttons run the SAME engine as
# `wta hooks install|uninstall|status` (agent_hooks_installer) — they detect supported CLIs and
# add/remove the wt-agent-hooks bridge in each CLI's plugin store (~/.copilot/config.json etc.).
# The Settings editor window CAN be opened by the harness (see Open-WtSettings / Feature.SettingsUi),
# but — exactly like the rest of §1, which tests the settings MODEL via settings.json rather than the
# editor widgets — we drive the underlying engine directly here and assert the on-disk effect, which
# is deterministic and independent of the button's foreground/click timing.
#
# SAFETY: these mutate the real ~/.copilot plugin config, so the whole Describe backs it up in
# BeforeAll and restores it in AfterAll; each test leaves copilot's hook state as it found it.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

Describe 'Feature §1 Settings session hooks (install/remove engine)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        $script:cfgBak = Backup-CopilotConfig    # restore the user's real hook state afterwards
        $script:HooksStatusJson = {
            $r = Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 30 -Raw
            if ($r.ExitCode -ne 0) { throw "wta hooks status failed (exit $($r.ExitCode)): $($r.StdErr)" }
            $r.StdOut | ConvertFrom-Json
        }
    }
    AfterAll {
        if ($script:cfgBak) { $script:cfgBak | Restore-CopilotConfig }
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'Session hooks install works (detects supported CLIs and installs copilot cleanly)' {
        # Start from a known-removed state so this proves the INSTALL transition, not a pre-existing one.
        Invoke-Wta -App $script:app -Arguments @('hooks', 'uninstall', '--cli', 'copilot', '--json') -TimeoutSec 30 -Raw | Out-Null
        Start-Sleep -Milliseconds 500
        Get-CopilotHooksInstalled | Should -BeFalse -Because 'precondition: copilot hooks removed before the install test'

        # The status report must ENUMERATE the supported CLIs (copilot/claude/gemini/codex) with a
        # per-CLI install state — this is exactly what the Settings button surfaces ("detects
        # supported CLIs and reports success/failure clearly").
        $status = & $script:HooksStatusJson
        @($status.clis).Count | Should -BeGreaterThan 0 -Because 'hooks status must enumerate supported CLIs'
        ($status.clis | Where-Object { $_.name -eq 'copilot' }) | Should -Not -BeNullOrEmpty -Because 'copilot must be a detected supported CLI'

        # Install for copilot and confirm it landed, both on disk and in the status report.
        $r = Invoke-Wta -App $script:app -Arguments @('hooks', 'install', '--cli', 'copilot') -TimeoutSec 60 -Raw
        $r.ExitCode | Should -Be 0 -Because 'hooks install must succeed for an installed+supported CLI'
        Start-Sleep -Milliseconds 500
        Get-CopilotHooksInstalled | Should -BeTrue -Because 'after install, copilot''s wt-agent-hooks plugin must be present + enabled'
        $after = & $script:HooksStatusJson
        ($after.clis | Where-Object { $_.name -eq 'copilot' }).plugin_installed | Should -BeTrue -Because 'status must report copilot as installed'
    }

    It 'Session hooks remove works (uninstall removes state without breaking the terminal)' {
        # Ensure installed first so this proves the REMOVE transition.
        Invoke-Wta -App $script:app -Arguments @('hooks', 'install', '--cli', 'copilot') -TimeoutSec 60 -Raw | Out-Null
        Start-Sleep -Milliseconds 500
        Get-CopilotHooksInstalled | Should -BeTrue -Because 'precondition: copilot hooks installed before the remove test'

        $r = Invoke-Wta -App $script:app -Arguments @('hooks', 'uninstall', '--cli', 'copilot', '--json') -TimeoutSec 30 -Raw
        $r.ExitCode | Should -Be 0 -Because 'hooks uninstall must succeed'
        $report = $r.StdOut | ConvertFrom-Json
        ($report.clis | Where-Object { $_.name -eq 'copilot' }).plugin_uninstalled | Should -BeTrue -Because 'uninstall must report copilot removed'
        Start-Sleep -Milliseconds 500
        Get-CopilotHooksInstalled | Should -BeFalse -Because 'after uninstall, copilot''s wt-agent-hooks plugin must be gone'

        # "without breaking the Settings page" — the terminal/protocol surface stays fully responsive
        # after the hook mutation (the closest harness-observable proxy for the Settings page staying
        # usable, since the editor UI itself isn't harness-openable).
        { Get-ActivePane -App $script:app } | Should -Not -Throw -Because 'the terminal must stay responsive after a hook remove'

        # Leave copilot's hooks re-installed so the machine ends as it started (AfterAll also restores).
        Invoke-Wta -App $script:app -Arguments @('hooks', 'install', '--cli', 'copilot') -TimeoutSec 60 -Raw | Out-Null
    }
}
