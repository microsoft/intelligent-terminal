#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2 "Master death is a consistent degraded state" (#329):
#   If wta-master exits, the agent pane shows a single consistent degraded state and requires
#   /restart to recover — no silent "split-brain" where one pane silently gets a fresh master
#   while orphaned panes stay dead.
#
# This suite exercises the REAL failure by killing wta-master out from under a live helper and
# asserting the whole contract end-to-end (not just the Rust unit-tested latch):
#   1. the pane leaves Connected (the degraded state is entered),
#   2. the `/` popup is filtered to ONLY /restart (the locale-independent degraded signal),
#   3. NO master is silently respawned while degraded (the anti-split-brain guarantee),
#   4. /restart brings up exactly one fresh master and the pane reconnects.
#
# Architecture facts this relies on (verified live + tools/wta/AGENTS.md):
#   * master  = `wta.exe --master <pipe>`         (spawned once by C++ SharedWta)
#   * helper  = `wta.exe --connect-master <pipe>` (one per agent pane)
#   * both are children of the WindowsTerminal.exe process (this app's Pid), so we kill ONLY
#     this app's master and never another instance's.
#   * the helper detects master death proactively (client.rs: "pipe closed (master gone)" ->
#     AgentFailure::TransportLost -> connection.lost), so no prompt is needed to trigger it.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentMasterDeath.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §2 master death is a consistent degraded state (#329)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue -Because 'the agent pane must be connected before we can test losing the connection'

        # This app's wta-master(s): a wta.exe child of THIS WindowsTerminal.exe launched with
        # `--master` (not `--connect-master`). Scoping to the app's Pid guarantees we never touch
        # another instance's master.
        $script:GetMasters = {
            @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" -ErrorAction SilentlyContinue |
                Where-Object {
                    $_.ParentProcessId -eq $script:app.Pid -and
                    $_.CommandLine -match '--master(\s|$|")' -and
                    $_.CommandLine -notmatch '--connect-master'
                })
        }
        # The degraded hint reuses the localized `connection.lost` string; match it across every
        # bundled locale so the assertion is locale-robust (the running build renders one locale).
        $script:LostRe = Get-WtaLocalizedTextRegex -Key 'connection.lost'
        if (-not $script:LostRe) { $script:LostRe = '(?i)connection.*lost|/restart' }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'killing wta-master enters a degraded state, blocks all slash commands but /restart, does not silently respawn, and recovers via /restart' {
        # --- there is exactly one live master while connected ---
        $masters = & $script:GetMasters
        # A connected agent pane runs EXACTLY ONE wta-master per WindowsTerminal process — the
        # C++ SharedWta singleton owns a single master and fans every pane onto it. Asserting
        # `-Be 1` (not just `> 0`) makes this gate itself catch a split-brain regression where a
        # second master already exists while connected.
        $masters.Count | Should -Be 1 -Because 'a connected agent pane implies exactly one shared wta-master (SharedWta is a singleton)'
        $killedPids = @($masters.ProcessId)

        # --- kill the master out from under the live helper ---
        foreach ($mp in $killedPids) { Stop-Process -Id $mp -Force -ErrorAction SilentlyContinue }
        Wait-Until -TimeoutSec 15 -Because 'the killed wta-master process(es) to be gone' -Condition {
            @(& $script:GetMasters | Where-Object { $killedPids -contains $_.ProcessId }).Count -eq 0
        } | Out-Null

        # --- (1) the pane leaves Connected: the connected input placeholder disappears ---
        # Wait-AgentReady returns false once the pane is no longer user-visibly connected. A short
        # timeout is enough because the helper detects the dead pipe proactively.
        $stillReady = Wait-AgentReady -App $script:app -TimeoutSec 20
        $stillReady | Should -BeFalse -Because 'after master dies the pane must drop out of the Connected state, not appear half-alive'

        # --- degraded hint is surfaced somewhere in the pane (best-effort, locale-robust) ---
        Test-AgentPopupShown -App $script:app -Pattern $script:LostRe -TimeoutSec 20 |
            Should -BeTrue -Because 'the degraded pane must tell the user the connection was lost and to /restart'

        # --- (2) the `/` popup is filtered to ONLY /restart ---
        # Type '/' and read the rendered TUI menu. In transport_lost, command_popup_state() is
        # filtered to /restart only (slash_command_tests.rs), so no other command may appear.
        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text '/' -NoSubmit | Out-Null
        Wait-Until -TimeoutSec 12 -Because 'the degraded /restart popup to render' -Condition {
            (Get-AgentPaneText -App $script:app -MaxLines 40) -match '/restart'
        } | Out-Null
        $menu = Get-AgentPaneText -App $script:app -MaxLines 40
        $menu | Should -Match '/restart' -Because 'the degraded popup must still offer /restart (the one recovery command)'
        # No other slash command may be offered while degraded — each would hit the dead pipe.
        foreach ($blocked in '/help', '/clear', '/new', '/sessions', '/agent', '/model', '/fix', '/stop') {
            $menu | Should -Not -Match ([regex]::Escape($blocked)) -Because "while degraded the popup must hide $blocked (only /restart runs)"
        }

        # --- (3) NO silent respawn: while degraded, master count must stay 0 ---
        # PR #329's core anti-split-brain guarantee: opening/using a degraded pane must NOT lazily
        # respawn a fresh master. Poll for a few seconds to catch a late respawn.
        for ($i = 0; $i -lt 6; $i++) {
            @(& $script:GetMasters).Count | Should -Be 0 -Because 'no wta-master may be silently respawned while the stack is degraded (that would be split-brain)'
            Start-Sleep -Milliseconds 500
        }

        # --- (4) /restart recovers: one fresh master, pane reconnects ---
        Clear-AgentInput -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -Text '/restart' | Out-Null   # type it and submit
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue -Because '/restart must respawn the stack and reconnect the pane'

        $recovered = & $script:GetMasters
        $recovered.Count | Should -Be 1 -Because '/restart must bring up exactly one master (not zero, not a split-brain duplicate)'
        ($killedPids -contains $recovered[0].ProcessId) | Should -BeFalse -Because 'the recovered master must be a genuinely fresh process, not the killed one'

        # The reconnected pane must actually work again.
        Send-AgentPrompt -App $script:app -Text 'What is 7 plus 7? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern '\b14\b' -TimeoutSec 50
    }
}
