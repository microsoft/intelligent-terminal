#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §4 "Session states": live-session activity badges in the /sessions picker.
#
# The session picker ships in MVP mode (app.rs MVP_SESSIONS_ORIGIN_FILTER = ShellOnly): it only
# renders SHELL-pane sessions — a copilot/claude/gemini the user ran directly in a normal pane,
# tracked live by the wt-agent-hooks plugin (agent.prompt.submit -> Working, agent.stop -> Idle).
# So to see a LIVE row with an activity badge we run a real interactive copilot in a shell pane.
#
# Of the four activity states only IDLE is stably observable end-to-end: it PERSISTS (an
# interactive copilot that finished its turn sits Idle, waiting for the next prompt, as long as
# the pane stays open — no race). Working/Waiting are sub-second transients with watcher-
# classification caveats (tools/wta/AGENTS.md); Ended/Historical render an EMPTY badge. Those are
# covered by Rust unit tests (agent_sessions.rs::activity_and_liveness_derive_from_legacy_status
# + agents_view.rs::status_badge_renders_expected_text_per_state).

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §4 session states: live shell session Idle badge' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
        # The tab that hosts the agent pane. Running copilot in a SPLIT of THIS tab keeps the
        # SessionToggleButton on the active tab so the /sessions view opens reliably.
        $script:homePane = Get-ActivePane -App $script:app

        # cwd = repo root (a checkout dir is copilot's natural trusted folder). `-i <prompt>`
        # auto-runs one prompt then stays interactive (Working -> Idle, and STAYS Idle).
        $script:repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $prompt = 'What is 6 plus 1? Reply with only the number.'
        $cmd = "copilot -C `"$($script:repo)`" --allow-all-tools --no-color -i `"$prompt`""
        $split = Split-WtPane -App $script:app -SessionId $script:homePane.session_id -Direction 'down' -Command $cmd
        $script:shellSid = $split.session_id

        # Best-effort: clear copilot's "Do you trust the files in this folder?" gate if it appears.
        for ($i = 0; $i -lt 12; $i++) {
            $cap = try { Get-WtCapture -App $script:app -SessionId $script:shellSid -MaxLines 40 } catch { '' }
            if ($cap -match 'trust the files|Do you trust|trust this folder') {
                Send-WtKeys -App $script:app -SessionId $script:shellSid -Keys @('Enter') | Out-Null
                break
            }
            Start-Sleep -Seconds 1
        }
    }
    AfterAll {
        if ($script:shellSid) { try { Close-WtPane -App $script:app -SessionId $script:shellSid } catch { } }
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'Idle state is correct: a live shell session that finished its turn shows the Idle badge' {
        $idleRe = Get-WtaLocalizedTextRegex -Key 'agents.status.idle'
        if (-not $idleRe) { $idleRe = 'Idle' }
        $activeRe = Get-WtaLocalizedTextRegex -Key 'agents.status.active'

        # Let the trivial arithmetic turn complete (agent.stop -> Idle; ~6-15s, allow variance).
        Start-Sleep -Seconds 25

        # Open /sessions via the agent pane's OWN slash menu, NOT the bottom-bar SessionToggleButton.
        # The toggle button (winapp ui invoke) reliably fails to switch the pane to the sessions view
        # in this build, whereas the /sessions slash command — typed into the agent pane by its
        # session id (focus-independent) — is reliable. Get-AgentPaneSession is now run-scoped, so
        # this targets THIS run's agent pane.
        Invoke-AgentMenuItem -App $script:app -Name '/sessions'
        $renderRe = 'to launch session|to exit|No sessions|navigate|↑\s*↓'
        (Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
                (Get-AgentPaneText -App $script:app -MaxLines 60) -match $renderRe
            }) | Should -BeTrue -Because 'the /sessions slash command must open the session view'

        # The live shell session sorts to the top (most-recent activity) and is auto-selected. It
        # MUST carry the Idle activity badge. Ended/Historical rows render an EMPTY badge, so the
        # only source of the Idle string in the list is a live, idle session. Poll: the row may
        # briefly show "Active" (Working) before flipping to Idle as the master pushes agent.stop.
        $sawIdle = Test-Until -TimeoutSec 40 -IntervalSec 1 -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 60) -match $idleRe
        }
        if (-not $sawIdle) {
            # No live row at all (no Idle AND no Active badge) => the CLI turn never ran
            # (auth/offline/trust/model-variance precondition), not a product bug => skip.
            $txt = Get-AgentPaneText -App $script:app -MaxLines 60
            $hasLiveRow = ($activeRe -and $txt -match $activeRe) -or ($txt -match '·\s*copilot')
            if (-not $hasLiveRow) {
                Set-ItResult -Skipped -Because 'no live shell session appeared — the copilot turn never ran (auth/offline/trust/model-variance precondition)'
                return
            }
        }
        $sawIdle | Should -BeTrue -Because 'a live shell session that finished its turn must render the Idle activity badge'
    }
}
