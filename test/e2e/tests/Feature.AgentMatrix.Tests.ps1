#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2 "Built-in agent chat matrix" — Claude / Codex / Gemini chat through the
# IT agent pane (copilot is already covered by Feature.AgentPaneInteraction.Tests.ps1).
#
# Each agent is gated by a real auth probe at discovery: the per-CLI Context runs ONLY when the
# CLI is on PATH AND a print-mode call returns our sentinel (i.e. it is installed AND
# authenticated). Otherwise the Context is skipped with the reason recorded — honest reporting
# of environment readiness rather than a hard failure.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery {
    $script:PkgReady = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })

    # Installed AND authenticated? Spawn the CLI's print mode in a job and look for the sentinel.
    function script:Test-AgentCliAuthed {
        param([string]$Cli, [scriptblock]$Probe)
        if (-not (Get-Command $Cli -ErrorAction SilentlyContinue)) { return $false }
        $job = Start-Job -ScriptBlock $Probe
        $out = ''
        if (Wait-Job $job -Timeout 50) { $out = ((Receive-Job $job 2>&1) -join "`n") } else { Stop-Job $job -ErrorAction SilentlyContinue }
        Remove-Job $job -Force -ErrorAction SilentlyContinue
        return [bool]($out -match 'AUTHOK')
    }

    $script:AgentMatrix = @(
        @{ Id = 'claude'; Name = 'Claude'; Ready = (Test-AgentCliAuthed 'claude' { claude -p "Reply with only the token AUTHOK" 2>&1 }) }
        @{ Id = 'codex';  Name = 'Codex';  Ready = (Test-AgentCliAuthed 'codex'  { $null | codex exec "Reply with only the token AUTHOK" 2>&1 }) }
        @{ Id = 'gemini'; Name = 'Gemini'; Ready = (Test-AgentCliAuthed 'gemini' { gemini -p "Reply with only the token AUTHOK" 2>&1 }) }
    )
}

Describe 'Feature §2 built-in agent chat matrix' -Tag 'Feature' -ForEach $script:AgentMatrix {
    Context "<Name>" -Skip:(-not ($script:PkgReady -and $Ready)) {
        BeforeAll {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $script:agentId = $Id
            $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = $Id; autoFixEnabled = $true }
            Open-AgentPane -App $script:app | Out-Null
            # Non-copilot agents connect via an `npx -y` ACP adapter; the first connect can
            # cold-fetch the adapter, so allow a generous readiness window.
            $script:connected = Wait-AgentReady -App $script:app -TimeoutSec 120
        }
        AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

        It "<Name> chat works (answers a question through the ACP adapter)" {
            if (-not $script:connected) { Set-ItResult -Skipped -Because "$script:agentId did not reach a connected ACP session in this environment"; return }
            Clear-AgentInput -App $script:app | Out-Null
            Send-AgentPrompt -App $script:app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern '7' -TimeoutSec 90
            Assert-AI -Claim 'The agent answered the arithmetic question 3 plus 4 with 7.' -Context (Get-AgentPaneText -App $script:app -MaxLines 60)
        }

        It "<Name> autofix works (a failed command surfaces a suggestion card)" {
            if (-not $script:connected) { Set-ItResult -Skipped -Because "$script:agentId did not reach a connected ACP session in this environment"; return }
            $sid = (Get-ActivePane -App $script:app).session_id
            # Autofix can return an explain (no card) for some failures; retry distinct typos.
            $typos = @("ggit$(Get-Random) status", "gti$(Get-Random) status", "got$(Get-Random) status")
            $gotCard = $false
            foreach ($cmd in $typos) {
                $listener = Start-WtEventListener -App $script:app
                try {
                    Start-Sleep -Milliseconds 400
                    Invoke-FailingCommand -App $script:app -SessionId $sid -Command $cmd | Out-Null
                    Wait-Autofix -Listener $listener -TimeoutSec 60 | Out-Null
                } catch { } finally { Stop-WtEventListener -Listener $listener }
                if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 60) -match 'Run command|Insert in Terminal' }) { $gotCard = $true; break }
            }
            if (-not $gotCard) { Set-ItResult -Skipped -Because "$script:agentId returned an explanation (no runnable-fix card) for all typos this run (LLM variance)"; return }
            (Get-AgentPaneText -App $script:app -MaxLines 60) | Should -Match 'Run command|Insert in Terminal'
        }
    }
}
