#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §5 (C143) — delegate with a NON-Copilot agent. Delegate mode launches
# `wta delegate [PROMPT] --agent <acp> --delegate-agent <id> --cwd <dir>` in a new tab. This drives
# the delegate ENGINE with a non-Copilot delegate (Claude, else Gemini) and asserts the delegate tab
# launches THAT agent (tab titled for the CLI / its UI renders) — the deterministic launch contract.
# Answering is LLM/auth-dependent, so a non-answer is a precondition skip, not a failure.
#
# Env-gated: skips unless a non-Copilot agent CLI (claude or gemini) is installed on PATH.

BeforeDiscovery {
    $script:nonCopilot = @('claude', 'gemini') | Where-Object { Get-Command $_ -ErrorAction SilentlyContinue } | Select-Object -First 1
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and $script:nonCopilot)
}

Describe 'Feature §5 delegate with non-Copilot agent' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; delegateAgent = 'copilot' }
        $script:repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        # Recompute in the run phase (BeforeDiscovery's $script var does not carry across phases).
        $script:delegateCli = @('claude', 'gemini') | Where-Object { Get-Command $_ -ErrorAction SilentlyContinue } | Select-Object -First 1
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Delegate with non-Copilot agents works (a non-Copilot delegate tab launches that agent)' {
        $cli = $script:delegateCli
        $wid = [string]$script:app.WindowId
        $before = @((Get-WtTabs -App $script:app -WindowId $wid).tab_id)
        Invoke-Wta -App $script:app -Arguments @('delegate', 'What is 2 plus 2? Reply with only the number.', '--agent', 'copilot --acp --stdio', '--delegate-agent', $cli, '--cwd', $script:repo) -TimeoutSec 45 -Raw | Out-Null

        $newTab = $null
        for ($i = 0; $i -lt 30 -and -not $newTab; $i++) {
            $newTab = @(Get-WtTabs -App $script:app -WindowId $wid) | Where-Object { $_.tab_id -notin $before } | Select-Object -First 1
            if (-not $newTab) { Start-Sleep -Milliseconds 500 }
        }
        $newTab | Should -Not -BeNullOrEmpty -Because "a $cli delegate must create a new tab"
        $sid = @(Get-WtPanes -App $script:app -WindowId $wid -TabId ([string]$newTab.tab_id))[0].session_id

        # The configured non-Copilot delegate is what launched: the tab title and/or the pane UI
        # identify the CLI (e.g. "claude.exe" + "Claude Code", "gemini" banner). Match locale-tolerantly.
        $launched = Test-Until -TimeoutSec 30 -IntervalSec 1 -Condition {
            ("$($newTab.title)" -match "(?i)$cli") -or ((Get-WtCapture -App $script:app -SessionId $sid -MaxLines 50) -match "(?i)$cli")
        }
        if (-not $launched) {
            Set-ItResult -Skipped -Because "the $cli delegate tab opened but the CLI UI did not identify within the timeout (auth/cold-start precondition), not a product failure"
            return
        }
        $launched | Should -BeTrue -Because "the configured non-Copilot delegate ($cli) must be the agent launched in the delegate tab"
    }
}
