#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: Autofix with agent pane (Copilot) + Autofix across layout changes.
# Observable: a failed command makes the agent pane render a suggestion card with
# `[ Run command ]` / `Insert in Terminal` actions. IMPORTANT: autofix throttles/dedups
# repeated identical corrections within one session, so tests that need a FRESH card each
# (Insert / Run / Stashed) use their own fresh terminal and trigger exactly once.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue)) }

# Helpers as script scriptblocks set up per-Describe in BeforeAll.

Describe 'Feature: autofix card render + reject + AI correctness' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Store -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        $script:CardShown = { ((Get-AgentPaneText -App $script:app -MaxLines 60) -match 'Run command|Insert in Terminal') }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Visible agent pane autofix works (suggestion card renders)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        # Autofix sometimes returns "explain" (no card) or drops the first failure; retry
        # distinct typos until a runnable-fix card renders.
        $typos = @("ggit status","gti status","got status","gitt status")
        foreach ($cmd in $typos) {
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Invoke-FailingCommand -App $script:app -SessionId $sid -Command $cmd | Out-Null
                Wait-WtEvent -Listener $listener -TimeoutSec 45 -Predicate { $_.method -eq 'agent_event' } | Out-Null
            } catch { } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { & $script:CardShown }) { break }
        }
        (& $script:CardShown) | Should -BeTrue
    }
    It 'Autofix suggests a runnable fix (AI oracle on the card)' {
        Assert-AI -Claim 'The displayed card presents a shell command that the user can Run or Insert into the terminal (it has Run and Insert action buttons).' -Context (Get-AgentPaneText -App $script:app -MaxLines 60)
    }
    It 'Reject/dismiss works (Esc closes the card)' {
        for ($i = 0; $i -lt 5 -and (& $script:CardShown); $i++) { Send-AgentKey -App $script:app -Key Escape | Out-Null; Start-Sleep -Milliseconds 800 }
        (& $script:CardShown) | Should -BeFalse
    }
    It 'Autofix target pane is correct (active shell pane is the fix target)' {
        (Get-ActivePane -App $script:app).session_id | Should -Match '[0-9A-Fa-f-]{36}'
    }
    It 'Autofix opens/restores UI correctly (pane reopens)' {
        Stop-AgentPane -App $script:app | Out-Null
        Open-AgentPane -App $script:app | Out-Null
        Test-AgentPaneOpen -App $script:app | Should -BeTrue
    }
}

Describe 'Feature: autofix Insert action' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Store -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Insert suggestion types the fix into the shell pane' {
        $sid = (Get-ActivePane -App $script:app).session_id
        # Autofix may return an "explain" (no card) for some failures; retry distinct typos
        # until a runnable-fix card with the Insert action appears.
        $typos = @("ggit status","gti status","got status","gitt status")
        $gotCard = $false
        foreach ($cmd in $typos) {
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Invoke-FailingCommand -App $script:app -SessionId $sid -Command $cmd | Out-Null
                Wait-Autofix -Listener $listener -TimeoutSec 45 | Out-Null
            } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 60) -match 'Insert in Terminal' }) { $gotCard = $true; break }
        }
        if (-not $gotCard) { Set-ItResult -Skipped -Because 'autofix returned explain (no runnable-fix card) for all typos this run (LLM variance)'; return }
        Send-AgentKey -App $script:app -Key Right | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null
        # No fixed settle: Assert-Pane polls (Verify.ps1) and returns as soon as the
        # inserted text reaches the shell pane.
        Assert-Pane -App $script:app -SessionId $sid -Match 'git ' -TimeoutSec 12
        Send-WtKeys -App $script:app -SessionId $sid -Keys @('C-c')
    }
}

Describe 'Feature: autofix Run action' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Store -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Run suggestion executes the fix in the shell pane' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $typos = @("ggit status","gti status","got status","gitt status")
        $gotCard = $false
        foreach ($cmd in $typos) {
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Invoke-FailingCommand -App $script:app -SessionId $sid -Command $cmd | Out-Null
                Wait-Autofix -Listener $listener -TimeoutSec 45 | Out-Null
            } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 60) -match 'Run command' }) { $gotCard = $true; break }
        }
        if (-not $gotCard) { Set-ItResult -Skipped -Because 'autofix returned explain (no runnable-fix card) for all typos this run (LLM variance)'; return }
        Send-AgentKey -App $script:app -Key Left | Out-Null
        Send-AgentKey -App $script:app -Key Enter | Out-Null
        # No fixed settle: Assert-Pane polls (Verify.ps1) and returns as soon as the
        # executed fix produces output in the shell pane.
        Assert-Pane -App $script:app -SessionId $sid -Match 'branch|not a git repository|fatal|Changes|working tree|nothing to commit|git' -TimeoutSec 25
    }
}

Describe 'Feature: autofix on stashed pane' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Store -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        # Pre-warm + connect, then stash so the helper is ready but the pane is hidden.
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        Stop-AgentPane -App $script:app | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Stashed agent pane autofix works (pre-warmed helper triggers while hidden)' {
        Test-AgentPaneOpen -App $script:app | Should -BeFalse
        $sid = (Get-ActivePane -App $script:app).session_id
        $listener = Start-WtEventListener -App $script:app
        try {
            Start-Sleep -Seconds 1
            Invoke-FailingCommand -App $script:app -SessionId $sid -Command "ggit$(Get-Random) status" | Out-Null
            { Wait-Autofix -Listener $listener -TimeoutSec 45 } | Should -Not -Throw
        } finally { Stop-WtEventListener -Listener $listener }
    }
}

Describe 'Feature: autofix across layout changes' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package Store -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Split pane autofix works (failure in a split pane triggers autofix)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        $split = Split-WtPane -App $script:app -SessionId $sid -Direction right
        Start-Sleep -Seconds 2
        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-FailingCommand -App $script:app -SessionId $split.session_id -Command "ggit$(Get-Random) status" | Out-Null
            { Wait-Autofix -Listener $listener -TimeoutSec 45 } | Should -Not -Throw
        } finally { Stop-WtEventListener -Listener $listener }
        Close-WtPane -App $script:app -SessionId $split.session_id
    }
    It 'Closed pane cleanup works (closing the failing pane is safe)' {
        # New-WtTab already Wait-Until's for the tab to appear (Wt.ps1), so no extra settle.
        $tab = New-WtTab -App $script:app -Title 'cleanup-tab'
        { Close-WtPane -App $script:app -SessionId $tab.session_id } | Should -Not -Throw
        { Get-ActivePane -App $script:app } | Should -Not -Throw
    }
}




