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
        $status = [ordered]@{}
        $status['claude'] = Get-AgentCliStatus -Agent 'claude'
        $status['codex']  = Get-AgentCliStatus -Agent 'codex'
        $status['gemini'] = Get-AgentCliStatus -Agent 'gemini'

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
                $shellPane = Get-ActivePane -App $app
                $agentPane = (Wait-NewAgentPaneSession -App $app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
                # Non-copilot agents connect via an `npx -y` ACP adapter whose first run can
                # cold-fetch the adapter, so allow a generous readiness window.
                (Wait-AgentReady -App $app -PaneSessionId $agentPane -TimeoutSec 120) | Should -BeTrue -Because "$id should reach a connected ACP session"
                Send-AgentPrompt -App $app -PaneSessionId $agentPane -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
                # Non-Copilot agents answer via the npx ACP adapter (extra hop + remote model
                # latency), so the first reply is markedly slower than Copilot's local-ish path.
                # 90s was demonstrably too tight (observed turns approaching it); 150s keeps the
                # consolidated external-CLI case from flaking on adapter/model latency.
                Assert-AgentPaneText -App $app -PaneSessionId $agentPane -Pattern '\b7\b' -TimeoutSec 150
            }
            finally { Stop-Terminal -App $app }
        }
    }
}
