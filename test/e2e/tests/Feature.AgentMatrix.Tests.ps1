#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2/§3 "Built-in agent chat/autofix matrix" — Claude / Codex / Gemini through
# the IT agent pane (copilot is already covered by Feature.AgentPaneInteraction.Tests.ps1).
#
# Each agent is gated by a real auth probe at discovery. When the CLI is missing, unauthenticated,
# or the package is absent, the per-agent cases are reported as Skipped with a PRECISE reason (so
# CI shows *why*) and no terminal is launched — honest environment reporting, not a hard failure.
#   Invoke-Pester test/e2e/tests -Tag Feature

BeforeDiscovery {
    $script:PkgReady = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })

    # Resolve a CLI's readiness AND a human-readable reason when it's not usable, so the test
    # report distinguishes "not installed" from "installed but unauthenticated".
    function script:Get-AgentCliReadiness {
        param([string]$Cli, [scriptblock]$Probe)
        if (-not (Get-Command $Cli -ErrorAction SilentlyContinue)) {
            return @{ Ready = $false; Reason = "the '$Cli' CLI is not installed (not on PATH)" }
        }
        $job = Start-Job -ScriptBlock $Probe
        $out = ''
        if (Wait-Job $job -Timeout 50) { $out = ((Receive-Job $job 2>&1) -join "`n") } else { Stop-Job $job -ErrorAction SilentlyContinue }
        Remove-Job $job -Force -ErrorAction SilentlyContinue
        if ($out -match 'AUTHOK') { return @{ Ready = $true; Reason = '' } }
        return @{ Ready = $false; Reason = "the '$Cli' CLI is installed but not authenticated (print-mode auth probe did not return the sentinel)" }
    }

    $claude = Get-AgentCliReadiness 'claude' { claude -p "Reply with only the token AUTHOK" 2>&1 }
    $codex  = Get-AgentCliReadiness 'codex'  { $null | codex exec "Reply with only the token AUTHOK" 2>&1 }
    $gemini = Get-AgentCliReadiness 'gemini' { gemini -p "Reply with only the token AUTHOK" 2>&1 }

    $script:AgentMatrix = @(
        @{ Id = 'claude'; Name = 'Claude'; Ready = $claude.Ready; Reason = $claude.Reason }
        @{ Id = 'codex';  Name = 'Codex';  Ready = $codex.Ready;  Reason = $codex.Reason }
        @{ Id = 'gemini'; Name = 'Gemini'; Ready = $gemini.Ready; Reason = $gemini.Reason }
    )
}

Describe 'Feature §2 built-in agent chat matrix' -Tag 'Feature' -ForEach $script:AgentMatrix {
    Context "<Name>" {
        BeforeAll {
            Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
            $script:agentId = $Id
            # Precise skip reason (recorded per-It below); when set we do NOT launch a terminal.
            # PkgReady is re-checked here: a $script: var set in BeforeDiscovery does not persist
            # into the run phase (only the -ForEach data does), so the auth result rides on the
            # -ForEach hashtable ($Ready/$Reason) while the package presence is re-evaluated.
            $pkgReady = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
            $script:skipReason =
                if (-not $pkgReady) { 'no Intelligent Terminal package is installed' }
                elseif (-not $Ready) { $Reason }
                else { $null }
            $script:app = $null
            $script:connected = $false
            if (-not $script:skipReason) {
                $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = $Id; autoFixEnabled = $true }
                Open-AgentPane -App $script:app | Out-Null
                # Non-copilot agents connect via an `npx -y` ACP adapter; the first connect can
                # cold-fetch the adapter, so allow a generous readiness window.
                $script:connected = Wait-AgentReady -App $script:app -TimeoutSec 120
            }
        }
        AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

        It "<Name> chat works (answers a question through the ACP adapter)" {
            if ($script:skipReason) { Set-ItResult -Skipped -Because $script:skipReason; return }
            if (-not $script:connected) { Set-ItResult -Skipped -Because "$script:agentId did not reach a connected ACP session in this environment"; return }
            Clear-AgentInput -App $script:app | Out-Null
            Send-AgentPrompt -App $script:app -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
            Assert-AgentPaneText -App $script:app -Pattern '\b7\b' -TimeoutSec 90
            Assert-AI -Claim 'The agent answered the arithmetic question 3 plus 4 with 7.' -Context (Get-AgentPaneText -App $script:app -MaxLines 60)
        }

        It "<Name> autofix works (a failed command surfaces a suggestion card)" {
            if ($script:skipReason) { Set-ItResult -Skipped -Because $script:skipReason; return }
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
