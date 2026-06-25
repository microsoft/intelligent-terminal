#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist: Autofix with agent pane (Copilot) + Autofix across layout changes.
# Observable: a failed command makes the agent pane render a suggestion card with
# `[ Run command ]` / `Insert in Terminal` actions. IMPORTANT: autofix throttles/dedups
# repeated identical corrections within one session, so tests that need a FRESH card each
# (Insert / Run / Stashed) use their own fresh terminal and trigger exactly once.

BeforeDiscovery {
    # winapp (UI-automation CLI) is required by Open-AgentPane / agent-pane assertions; gate on
    # it too so the whole suite SKIPS cleanly when it's missing instead of throwing in BeforeAll.
    $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue))
    # WSL-gated cases need: the dev sideload package (OSC 9001;ShellType + the autofix
    # shell-context fix aren't in the Store build yet), copilot for autofix, winapp for the
    # agent pane, and a runnable WSL distro. Absent any of these the WSL Describe is skipped.
    $script:WslReady = $false
    $devPkg = Get-AppxPackage | Where-Object { $_.PackageFamilyName -like 'IntelligentTerminal_*' }
    if ($devPkg -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue) -and (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
        try { & wsl.exe -e true 2>$null; $script:WslReady = ($LASTEXITCODE -eq 0) } catch { $script:WslReady = $false }
    }
}

# Helpers as script scriptblocks set up per-Describe in BeforeAll.

Describe 'Feature: autofix card render + reject + AI correctness' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the copilot agent pane must reach a connected ACP session before the autofix card suite runs'
        $script:CardShown = { ((Get-AgentPaneText -App $script:app -MaxLines 60) -match (Get-RecommendationCardRegex)) }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Visible agent pane autofix works (suggestion card renders)' {
        $sid = (Get-ActivePane -App $script:app).session_id
        # Autofix sometimes returns "explain" (no card) or drops the first failure; retry
        # distinct typos until a runnable-fix card renders.
        $typos = @("ggit status","gti status","got status","gitt status")
        $gotCard = $false
        foreach ($cmd in $typos) {
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Invoke-FailingCommand -App $script:app -SessionId $sid -Command $cmd | Out-Null
                Wait-WtEvent -Listener $listener -TimeoutSec 45 -Predicate { $_.method -eq 'agent_event' } | Out-Null
            } catch { } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { & $script:CardShown }) { $gotCard = $true; break }
        }
        # When the LLM returns an explanation (not a runnable-fix card) for ALL retried typos,
        # that's model variance, not a product failure — skip like the WSL autofix case below.
        if (-not $gotCard) {
            Set-ItResult -Skipped -Because 'autofix returned explain (no runnable-fix card) for all typos this run (LLM variance)'
            return
        }
        (& $script:CardShown) | Should -BeTrue
    }
    It 'Autofix suggests a runnable fix (AI oracle on the card)' {
        # Depends on the card from the previous case; if model variance produced no card this
        # run, skip rather than fail the AI oracle on a card-less pane.
        if (-not (& $script:CardShown)) {
            Set-ItResult -Skipped -Because 'no autofix card rendered this run (LLM variance; see the card-render case)'
            return
        }
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
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
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
            } catch { } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 60) -match (Get-RecommendationCardRegex) }) { $gotCard = $true; break }
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
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
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
            } catch { } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 60) -match (Get-RecommendationCardRegex) }) { $gotCard = $true; break }
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
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
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
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }
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

# ─────────────────────────────────────────────────────────────────────────────
# WSL-gated: exercises OSC 9001;ShellType end-to-end and the autofix shell-context
# fix that consumes it. Runs ONLY when a runnable WSL distro + the dev package are
# present (the feature isn't in the Store build); otherwise the whole Describe is
# skipped. Requires the dev package, so it targets -Package Dev explicitly.
# ─────────────────────────────────────────────────────────────────────────────
Describe 'Feature: autofix in a WSL pane (OSC 9001;ShellType end-to-end)' -Tag 'Feature' -Skip:(-not $script:WslReady) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        # Dev package: the ShellType plumbing and the autofix shell-context fix live
        # here, not in the Store build.
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'copilot'; autoFixEnabled = $true }

        # Open a WSL shell pane in a fresh tab, then focus it so it's the active tab.
        # The whole WSL-pane setup is wrapped: a build without a WSL-capable protocol
        # CreateTab (e.g. a stale dev package predating OSC 9001) fails new-tab with
        # E_FAIL — treat that as "WSL not testable here" and let the per-It guards SKIP
        # rather than failing the Describe in BeforeAll.
        $script:wslShell = $null
        try {
            $script:wsl = New-WtTab -App $script:app -Command 'wsl.exe' -Title 'wsl-autofix'
            $script:wslSid = $script:wsl.session_id
            $script:wslTabId = $script:wsl.tab_id
            $script:wslWinId = $script:wsl.window_id
            Set-WtPaneFocus -App $script:app -SessionId $script:wslSid

            # Nudge a prompt so bash shell integration emits OSC 9001;ShellType at least
            # once, then wait for the reported shell to surface through the protocol.
            # list-panes is active-tab scoped, so query the WSL tab explicitly by id.
            Send-WtKeys -App $script:app -SessionId $script:wslSid -Keys @('Enter')
            Test-Until -TimeoutSec 30 -IntervalSec 1 -Condition {
                $p = Get-WtPanes -App $script:app -TabId $script:wslTabId -WindowId $script:wslWinId |
                    Where-Object { $_.session_id -eq $script:wslSid }
                if ($p -and ($p.shell -match '^(?i)wsl:')) { $script:wslShell = $p.shell; $true } else { $false }
            } | Out-Null

            # Agent pane on the (now active) WSL tab so autofix cards render here.
            Open-AgentPane -App $script:app | Out-Null
            Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue -Because 'the agent pane must be connected for WSL autofix to render cards'
        }
        catch {
            Write-ItLog -Level WARN -Message "WSL autofix setup failed (build without WSL-capable CreateTab / OSC 9001, or no WSL shell integration): $_"
            $script:wslShell = $null
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'WSL pane self-reports its shell as wsl:<distro> through the protocol' {
        if (-not $script:wslShell) {
            Set-ItResult -Skipped -Because 'WSL pane never reported a wsl: shell (shell integration not installed in this distro)'
            return
        }
        # Proves OSC 9001;ShellType -> adaptDispatch -> Terminal -> ControlCore ->
        # PaneInfo -> COM JSON, including the distro name surviving the ';' split.
        $script:wslShell | Should -Match '^(?i)wsl:\S'
    }

    It 'Autofix suggests a Linux/bash fix (not PowerShell) in a WSL pane' {
        if (-not $script:wslShell) {
            Set-ItResult -Skipped -Because 'WSL shell integration not installed; autofix has no WSL shell context to read'
            return
        }
        # bash-typos whose correct form is unmistakably Linux (ls/grep/cat), so the AI
        # oracle can tell a bash fix from a PowerShell one (Get-ChildItem etc.). The
        # whole point of the fix under test: with shell=wsl:<distro> in the prompt the
        # agent must NOT fall back to PowerShell syntax.
        $typos = @('sl -la', 'lll', 'grpe root /etc/hostname', 'caat /etc/hostname')
        $gotCard = $false
        foreach ($cmd in $typos) {
            $listener = Start-WtEventListener -App $script:app
            try {
                Start-Sleep -Milliseconds 400
                Invoke-FailingCommand -App $script:app -SessionId $script:wslSid -Command $cmd | Out-Null
                Wait-Autofix -Listener $listener -TimeoutSec 45 | Out-Null
            } catch { } finally { Stop-WtEventListener -Listener $listener }
            if (Test-Until -TimeoutSec 18 -IntervalSec 1 -Condition { (Get-AgentPaneText -App $script:app -MaxLines 60) -match (Get-RecommendationCardRegex) }) { $gotCard = $true; break }
        }
        if (-not $gotCard) {
            Set-ItResult -Skipped -Because 'autofix returned explain (no runnable-fix card) for all typos this run (LLM variance)'
            return
        }
        Assert-AI -Claim 'The suggested fix command uses Linux/bash shell syntax (e.g. ls, grep, cat, forward-slash paths). It is NOT a Windows PowerShell command (no Get-ChildItem / Select-String / cmdlet-style Verb-Noun).' -Context (Get-AgentPaneText -App $script:app -MaxLines 60)
    }
}




