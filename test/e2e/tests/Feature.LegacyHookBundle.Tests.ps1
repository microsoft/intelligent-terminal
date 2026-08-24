#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §8 (C270, C271) — hooks installed by an OLDER Intelligent
# Terminal must keep working against a newer one.
#
# PR #571 replaced the PowerShell hook bridge with `wtcli agent-hook`, but that
# only changes the bundle Terminal SHIPS. The bundle a user already INSTALLED is
# a copy inside their CLI's plugin cache; upgrading Terminal never rewrites it,
# and the auto-upgrade that should can fail outright — `<cli> plugin install`
# replaces the whole plugin directory, which Windows refuses while a CLI process
# holds a handle on it (observed as "Access is denied (os error 5)"). So "old
# hooks, new Terminal" is a state real users sit in, sometimes for a long time.
#
# It keeps working only because `wtcli send-event` — the subcommand the legacy
# script calls — was left in place. Nothing else in the tree records that this
# is load-bearing, so deleting it as "superseded by agent-hook" would break
# every un-refreshed user with no other test going red. That is the silent
# dependency these two cases exist to make loud.
#
# The fixture under test/e2e/fixtures/legacy-hook-bundle is a verbatim copy of
# the pre-#571 bundle and must never be edited. See its README.
#
#   Invoke-Pester test/e2e/tests/Feature.LegacyHookBundle.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §8 legacy hook bundle compatibility' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true

        $script:Fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\legacy-hook-bundle')).Path
        $script:LegacyScript = Join-Path $script:Fixture 'send-event.ps1'
        $script:LegacyHooks = Join-Path $script:Fixture 'hooks.json'

        function script:Get-LegacyCommand {
            <#
            .SYNOPSIS
                The command line the legacy bundle really shipped for one WTA topic,
                with the plugin-root placeholder pointed at the fixture.
            .DESCRIPTION
                Read from the frozen hooks.json rather than retyped here: the exact
                spelling — `powershell` (not pwsh), -NonInteractive, -ExecutionPolicy
                Bypass, -CliSource, the positional event — is the thing under test, and
                a hand-copied approximation would quietly stop matching it.
            #>
            param([string]$Event)
            $json = Get-Content -Raw -LiteralPath $script:LegacyHooks | ConvertFrom-Json
            foreach ($topic in $json.hooks.PSObject.Properties) {
                foreach ($matcher in $topic.Value) {
                    foreach ($h in $matcher.hooks) {
                        if ($h.command -and $h.command -match ([regex]::Escape($Event) + '\s*$')) {
                            # The CLI expands ${CLAUDE_PLUGIN_ROOT} to the installed plugin
                            # directory; the fixture stands in for it.
                            return $h.command -replace [regex]::Escape('${CLAUDE_PLUGIN_ROOT}/hooks'), ($script:Fixture -replace '\\', '/')
                        }
                    }
                }
            }
            throw "no legacy hook command for '$Event'"
        }

        function script:Write-Payload {
            param([string]$Name, [string]$Json, [string]$Dir)
            $p = Join-Path $Dir "$Name.json"
            [System.IO.File]::WriteAllText($p, $Json)
            $p
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Legacy hook bundle still delivers after an upgrade (pre-571 hooks reach a post-571 Terminal)' {
        $paneId = (Get-ActivePane -App $script:app).session_id
        $agentSessionId = "legacy-$([guid]::NewGuid())"
        $secret = 'legacy-prompt-must-not-cross'
        $payload = script:Write-Payload -Name 'legacy' -Dir $TestDrive -Json (@{
                session_id = $agentSessionId
                cwd        = 'C:\legacy-hook-test'
                prompt     = $secret
            } | ConvertTo-Json -Compress)

        $legacy = script:Get-LegacyCommand -Event 'agent.prompt.submit'
        $legacy | Should -Match 'send-event\.ps1' -Because 'the fixture command must still point at the legacy script'

        $listener = Start-WtEventListener -App $script:app
        try {
            Invoke-RunCommand -App $script:app -SessionId $paneId -SettleSec 20 `
                -Command "Get-Content -Raw -LiteralPath '$payload' | $legacy" | Out-Null

            # Wait-WtEvent throws on timeout, which would surface as a bare "timed out"
            # with no hint that the legacy transport is what broke.
            $event = $null
            try {
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 60 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $agentSessionId
                }
            }
            catch { }
            $event | Should -Not -BeNullOrEmpty -Because 'a pre-571 hook must still reach Terminal — check that `wtcli send-event` still exists and still publishes the agent_event envelope'

            # The legacy path and the native bridge must converge on ONE envelope.
            # If they diverge, `wtcli send-event` can keep exiting 0 while every
            # un-refreshed user silently stops being tracked.
            $event.params.event | Should -Be 'agent.prompt.submit'
            $event.params.pane_id | Should -Be $paneId -Because 'WT_SESSION must still carry the pane identity'
            $event.params.cli_source | Should -Be 'copilot' -Because '-CliSource is how the legacy script tags its origin'
            $event.params.payload.cwd | Should -Be 'C:\legacy-hook-test'
            @($event.params.payload.PSObject.Properties.Name) |
                Should -Not -Contain 'prompt' -Because 'the legacy script redacts prompt content too'
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }

    It 'Legacy hook bundle without WT_SESSION publishes unattributed (a pane-less legacy hook never hijacks the focused pane)' {
        # The other half of the un-refreshed-user state. The frozen script only
        # passes `-p` when it can read WT_SESSION, and a CLI whose hook process
        # never inherited it (an agent-pane CLI is spawned outside the conpty
        # chain, so it has none) therefore calls `wtcli send-event` with no pane
        # at all. `send-event` used to fill that gap with GetActivePane(), which
        # is not "the pane this agent runs in" but "wherever the user is looking
        # right now" — so the event bound to a stranger's pane. For
        # agent.session.start specifically that is destructive rather than merely
        # wrong: WTA's orphan-handover branch demotes the previous owner of a
        # reused pane to Ended, so an un-refreshed hook firing while the user
        # looks at a tracked pane silently kills that pane's real session, and
        # Enter on the session list then focuses someone else's pane.
        #
        # An empty pane_id is the honest answer and one WTA already models
        # (`pane_known` false -> no handover, no active_by_pane write, route by
        # cli_source). This case pins that: with no WT_SESSION the event must
        # still arrive, and must NOT carry the focused pane's id.
        $paneId = (New-WtTab -App $script:app).session_id
        $agentSessionId = "legacy-nopane-$([guid]::NewGuid())"
        $payload = script:Write-Payload -Name 'legacy-nopane' -Dir $TestDrive -Json (@{
                session_id = $agentSessionId
                cwd        = 'C:\legacy-hook-test'
            } | ConvertTo-Json -Compress)

        $legacy = script:Get-LegacyCommand -Event 'agent.session.start'

        $listener = Start-WtEventListener -App $script:app
        try {
            # Clear only WT_SESSION. WT_COM_CLSID stays, so the script runs all
            # the way through and really publishes -- otherwise "no focused-pane
            # id" would be satisfied by the event never being sent at all, and
            # the case would pass for the wrong reason.
            Invoke-RunCommand -App $script:app -SessionId $paneId -SettleSec 20 `
                -Command "`$env:WT_SESSION=''; Get-Content -Raw -LiteralPath '$payload' | $legacy" | Out-Null

            $event = $null
            try {
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 60 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $agentSessionId
                }
            }
            catch { }
            $event | Should -Not -BeNullOrEmpty -Because 'a pane-less legacy hook must still publish -- dropping it would lose tracking for every un-refreshed user whose CLI has no WT_SESSION'

            $event.params.pane_id | Should -BeNullOrEmpty -Because '`wtcli send-event` without -p must publish an empty pane_id, not resolve one'
            $event.params.pane_id | Should -Not -Be $paneId -Because 'attributing a pane-less hook to the focused pane is what evicts that pane''s real session'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }

    It 'Legacy hook bundle degrades quietly without Terminal (an uninstalled Terminal leaves stale hooks harmless)' {        # Uninstalling Terminal removes the bridge but leaves every hook registration
        # behind. For the legacy bundle the first gate is WT_COM_CLSID: no Terminal,
        # no variable, so the script must exit 0 before it tries anything. Clearing
        # the variable reproduces that gate exactly, and unlike a PATH scrub it cannot
        # be defeated by the script's own fallback of locating wtcli.exe through the
        # package InstallLocation.
        $paneId = (New-WtTab -App $script:app).session_id
        $gatedId = "legacy-gated-$([guid]::NewGuid())"
        $controlId = "legacy-control-$([guid]::NewGuid())"

        $gated = script:Write-Payload -Name 'legacy-gated' -Dir $TestDrive -Json (@{ session_id = $gatedId } | ConvertTo-Json -Compress)
        $control = script:Write-Payload -Name 'legacy-control' -Dir $TestDrive -Json (@{ session_id = $controlId } | ConvertTo-Json -Compress)
        $legacy = script:Get-LegacyCommand -Event 'agent.session.start'

        # The script's own output goes to a FILE. Asserting on the pane capture would
        # match the command being echoed back — the marker, the payload path and the
        # word wtcli all appear in the line that sets the test up.
        $noise = Join-Path $TestDrive 'legacy-gated.out'

        $listener = Start-WtEventListener -App $script:app
        try {
            $gatedCmd = "`$saved=`$env:WT_COM_CLSID; `$env:WT_COM_CLSID=''; " +
            "Get-Content -Raw -LiteralPath '$gated' | $legacy *> '$noise'; " +
            '"GATED" + "=$LASTEXITCODE"'
            $out = Invoke-RunCommand -App $script:app -SessionId $paneId -Command $gatedCmd -SettleSec 20
            $out | Should -Match 'GATED=0' -Because 'a stale hook must never fail its CLI once Terminal is gone'

            (Get-Content -Raw -LiteralPath $noise -ErrorAction SilentlyContinue) |
                Should -BeNullOrEmpty -Because 'a hook for an uninstalled product must not print anything at all'

            # Positive control: same pane, gate restored. Waiting for THIS event proves
            # the listener was live and that enough time passed for a gated event to
            # have shown up, so the count assertion below means something.
            $controlCmd = "`$env:WT_COM_CLSID=`$saved; " +
            "Get-Content -Raw -LiteralPath '$control' | $legacy"
            Invoke-RunCommand -App $script:app -SessionId $paneId -Command $controlCmd -SettleSec 20 | Out-Null
            $controlEvent = $null
            try {
                $controlEvent = Wait-WtEvent -Listener $listener -TimeoutSec 60 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $controlId
                }
            }
            catch { }
            $controlEvent | Should -Not -BeNullOrEmpty -Because 'restoring WT_COM_CLSID must restore publishing, or this case proves nothing'

            @(Get-WtEvents -Listener $listener -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $gatedId
                }).Count | Should -Be 0 -Because 'the gated run had no Terminal to talk to, so it must publish nothing'
        }
        finally {
            Stop-WtEventListener -Listener $listener
            try { Close-WtPane -App $script:app -SessionId $paneId } catch { }
        }
    }
}
