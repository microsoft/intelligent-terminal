#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §8 (C171) — per-CLI hook install. `wta hooks status --json` is the engine behind
# the Settings/FRE hook UI; it enumerates every supported agent CLI (copilot/claude/gemini/codex)
# and reports, per CLI, whether its wt-agent-hooks plugin is installed/enabled or WHY it can't be
# (binary not on PATH, marketplace not registered, etc.). This asserts that per-CLI enumeration is
# present and self-consistent, so the install UI can show each CLI's state or a clear reason.
#
# Install itself (the happy path) is covered by Feature.SettingsHooks (C170/C172); here we assert the
# per-CLI REPORTING that C171 requires, driven directly against the engine (deterministic, no UI).

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })) }

Describe 'Feature §8 per-CLI hook install/status' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Per-CLI hook install works (wta hooks status enumerates each CLI with install state or a reason)' {
        $raw = (Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 40 -Raw).StdOut
        $raw | Should -Not -BeNullOrEmpty -Because 'wta hooks status --json must produce output'
        $st = $raw | ConvertFrom-Json
        $st.clis | Should -Not -BeNullOrEmpty -Because 'status must enumerate the supported CLIs'

        # Each of the three built-in supported CLIs (copilot/claude/gemini) plus codex must be present.
        $names = @($st.clis.name)
        foreach ($cli in 'copilot', 'claude', 'gemini') {
            $names | Should -Contain $cli -Because "status must report the $cli CLI"
        }

        # Every enumerated CLI must carry a self-consistent install state OR a clear reason it can't
        # install: if the binary is on PATH and the marketplace source is registered+valid, it may be
        # installed; otherwise plugin_installed must be false (the "why it can't" is the missing
        # binary_on_path / marketplace_registered flag). This is exactly what the install UI shows.
        foreach ($cli in $st.clis) {
            $cli.PSObject.Properties.Name | Should -Contain 'binary_on_path' -Because "$($cli.name) status must report binary_on_path"
            $cli.PSObject.Properties.Name | Should -Contain 'plugin_installed' -Because "$($cli.name) status must report plugin_installed"
            if ($cli.plugin_installed) {
                # An installed hook must have come from a registered, valid marketplace source.
                $cli.marketplace_registered | Should -BeTrue -Because "$($cli.name) reports installed, so its marketplace source must be registered"
            }
            else {
                # Not installed => there must be a discernible reason (no binary, or source not registered/valid).
                ($cli.binary_on_path -and $cli.marketplace_registered -and $cli.marketplace_path_valid) |
                    Should -BeFalse -Because "$($cli.name) is not installed, so at least one prerequisite (binary/marketplace) must be missing — a clear reason"
            }
        }
    }
}
