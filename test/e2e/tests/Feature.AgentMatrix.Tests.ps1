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
        # Classify each CLI precisely so a skip is actionable: not-installed vs
        # installed-but-unauthenticated vs authed (only 'authed' agents are exercised).
        function Get-CliStatus([string]$Cli, [scriptblock]$Probe) {
            if (-not (Get-Command $Cli -ErrorAction SilentlyContinue)) { return 'not-installed' }
            $job = Start-Job -ScriptBlock $Probe
            $out = ''
            $finished = Wait-Job $job -Timeout 50
            if ($finished) { $out = ((Receive-Job $job 2>&1) -join "`n") } else { Stop-Job $job -ErrorAction SilentlyContinue }
            Remove-Job $job -Force -ErrorAction SilentlyContinue
            # A probe that never returns is NOT the same as unauthenticated — surface it
            # distinctly so a hung/changed CLI doesn't masquerade as a clean auth gap in CI.
            if (-not $finished) { return 'probe-timeout' }
            if ($out -match 'AUTHOK') { return 'authed' } else { return 'installed-unauthenticated' }
        }

        $status = [ordered]@{}
        $status['claude'] = Get-CliStatus 'claude' { claude -p "Reply with only the token AUTHOK" 2>&1 }
        $status['codex']  = Get-CliStatus 'codex'  { $null | codex exec "Reply with only the token AUTHOK" 2>&1 }
        $status['gemini'] = Get-CliStatus 'gemini' { gemini -p "Reply with only the token AUTHOK" 2>&1 }

        $detail = (($status.Keys | ForEach-Object { "${_}=$($status[$_])" }) -join ', ')
        Write-ItLog -Level INFO -Message "AgentMatrix CLI status: $detail"
        $available = @($status.Keys | Where-Object { $status[$_] -eq 'authed' })

        if (-not $available.Count) {
            # Surface WHY each CLI was skipped (not-installed vs installed-but-unauthenticated)
            # so CI results stay actionable and an auth regression isn't silently collapsed.
            Set-ItResult -Skipped -Because "no non-Copilot built-in agent is installed+authenticated ($detail)"
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
                # Non-Copilot agents answer via the npx ACP adapter (extra hop + remote model
                # latency), so the first reply is markedly slower than Copilot's local-ish path.
                # 90s was demonstrably too tight (observed turns approaching it); 150s keeps the
                # consolidated external-CLI case from flaking on adapter/model latency.
                Assert-AgentPaneText -App $app -Pattern '\b7\b' -TimeoutSec 150
            }
            finally { Stop-Terminal -App $app }
        }
    }
}
