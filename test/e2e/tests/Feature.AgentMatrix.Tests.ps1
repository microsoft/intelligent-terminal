#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2: non-Copilot built-in agents (Claude / Codex / Gemini) connect through
# the ACP adapter.
#
# Copilot is the PRIMARY agent: its full behaviour (chat, autofix, insert/run, permission,
# rendering, slash, sessions, …) is covered in depth by the copilot-only feature suites. All
# built-in agents share the SAME path — agent pane -> helper -> master -> agent CLI (ACP) — and
# the only per-agent difference is the command `wta-master` spawns (agent_registry). So we do
# NOT re-test every behaviour once per agent. This is ONE consolidated case that proves the
# other built-in agents resolve to the right command, their ACP adapter connects, and a basic
# chat round-trip works. Each available+authenticated agent is exercised inside the single test;
# the case skips (honestly) only when none is installed+authenticated.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §2 non-Copilot built-in agents connect through the ACP adapter' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll { Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force }

    It 'Each installed+authenticated non-Copilot agent (Claude/Codex/Gemini) connects and answers' {
        # Installed AND authenticated? Spawn the CLI's print mode and look for the sentinel.
        function Test-CliAuthed([string]$Cli, [scriptblock]$Probe) {
            if (-not (Get-Command $Cli -ErrorAction SilentlyContinue)) { return $false }
            $job = Start-Job -ScriptBlock $Probe
            $out = ''
            if (Wait-Job $job -Timeout 50) { $out = ((Receive-Job $job 2>&1) -join "`n") } else { Stop-Job $job -ErrorAction SilentlyContinue }
            Remove-Job $job -Force -ErrorAction SilentlyContinue
            return [bool]($out -match 'AUTHOK')
        }

        $available = @()
        if (Test-CliAuthed 'claude' { claude -p "Reply with only the token AUTHOK" 2>&1 }) { $available += 'claude' }
        if (Test-CliAuthed 'codex'  { $null | codex exec "Reply with only the token AUTHOK" 2>&1 }) { $available += 'codex' }
        if (Test-CliAuthed 'gemini' { gemini -p "Reply with only the token AUTHOK" 2>&1 }) { $available += 'gemini' }

        if (-not $available.Count) {
            Set-ItResult -Skipped -Because 'no non-Copilot built-in agent (claude/codex/gemini) is installed and authenticated in this environment'
            return
        }

        # Each available agent: its own fresh terminal, connect, and a single chat round-trip.
        foreach ($id in $available) {
            $app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = $id }
            try {
                Open-AgentPane -App $app | Out-Null
                # Non-copilot agents connect via an `npx -y` ACP adapter whose first run can
                # cold-fetch the adapter, so allow a generous readiness window.
                (Wait-AgentReady -App $app -TimeoutSec 120) | Should -BeTrue -Because "$id should reach a connected ACP session"
                Send-AgentPrompt -App $app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
                Assert-AgentPaneText -App $app -Pattern '\b7\b' -TimeoutSec 90
            }
            finally { Stop-Terminal -App $app }
        }
    }
}
