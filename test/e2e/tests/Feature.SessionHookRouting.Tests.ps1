#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist section 8 (C279, C280, C281) - how a single agent hook is routed
# from `wtcli` through every connected helper into the master session registry.
#
# Why these live at E2E rather than in a unit test: the WTA unit tests exercise one
# helper's routing function and the master's handler in isolation, inside a single
# process. The defects these cases guard only exist in the fan-out itself - one
# `wtcli agent-hook` invocation becomes one COM broadcast that reaches EVERY
# subscribed helper process, each of which independently forwards it to master over
# its own named pipe. No unit test can span that boundary, and it is exactly where
# master used to apply one real hook N times.
#
#   Invoke-Pester test/e2e/tests/Feature.SessionHookRouting.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: session hook fan-out routing' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true

        # Extra tabs, NOT agent panes. Every eligible tab pre-warms a stashed helper,
        # and a stashed helper is already connected to master and already subscribed
        # to the COM broadcast - which is all these cases need. Going through the UIA
        # agent-pane toggle would add a `winapp` dependency and an ACP handshake that
        # have nothing to do with hook routing.
        $script:shellPane = (Get-ActivePane -App $script:app).session_id
        foreach ($i in 1..2) { New-WtTab -App $script:app -Title "hook-routing-tab-$i" | Out-Null }
        Set-WtPaneFocus -App $script:app -SessionId $script:shellPane

        # A fan-out is the precondition for every case here, so wait for master to
        # actually have more than one helper rather than discovering it via a
        # confusing assertion failure later.
        Wait-Until -TimeoutSec 60 -Because 'master to accept more than one helper connection' -Condition {
            $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log'
            $ids = @([regex]::Matches($text, 'helper_id=HelperId\((?<n>\d+)\)') |
                    ForEach-Object { $_.Groups['n'].Value } | Sort-Object -Unique)
            if ($ids.Count -ge 2) { $ids }
        } | Should -Not -BeNullOrEmpty -Because 'a single-helper run would pass every case below even with the dedupe removed'

        function script:Invoke-HookInjection {
            <#
            .SYNOPSIS
                Fire one `wtcli agent-hook` from a real Terminal shell pane.
            .DESCRIPTION
                Payload travels by file so no assertion depends on JSON quoting
                surviving Send-WtInput. Returns nothing; callers assert on the
                master log or the session registry.
            #>
            param(
                [Parameter(Mandatory)][string]$Event,
                [Parameter(Mandatory)][hashtable]$Payload,
                [int]$SettleSec = 6
            )
            $file = Join-Path $TestDrive "hook-$([guid]::NewGuid().ToString('N')).json"
            [System.IO.File]::WriteAllText($file, ($Payload | ConvertTo-Json -Compress))
            $cmd = "Get-Content -Raw -LiteralPath '$file' | wtcli.exe agent-hook --cli-source copilot --event $Event"
            Invoke-RunCommand -App $script:app -SessionId $script:shellPane -Command $cmd -SettleSec $SettleSec | Out-Null
        }

        function script:Get-HookApplyTally {
            <#
            .SYNOPSIS
                Count master's replay rejections per fully-qualified broadcast id.
            .DESCRIPTION
                The master log is the earliest deterministic signal that separates
                "applied once" from "applied N times": the reducer is idempotent, so
                the resulting registry row looks identical either way, and the
                sessions/changed fan-out is not externally observable. Returns a map
                of '<guid>#<slot>' -> dropped count.
            #>
            param([Parameter(Mandatory)][string]$Text)
            $tally = @{}
            foreach ($line in ($Text -split "`r?`n")) {
                if ($line -match 'dropped replay of a hook already applied.*broadcast_id=(?<id>[0-9A-Fa-f-]+#\w+)') {
                    $id = $Matches['id']
                    if (-not $tally.ContainsKey($id)) { $tally[$id] = 0 }
                    $tally[$id]++
                }
            }
            $tally
        }
    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'One hook broadcast applies once per helper fan-out' {
        # Regression: one `wtcli agent-hook` reached master once per connected
        # helper, so a single SessionStarted was applied 3x with 3 helpers and
        # master re-broadcast sessions/changed for each copy.
        #
        # `agent.tool.starting` for a session master has never seen deliberately
        # expands into TWO published events - a synthetic start plus the tool event.
        # They must dedupe INDEPENDENTLY: an earlier attempt keyed the dedupe on the
        # event's position in that expansion, which is per-helper state (a helper
        # that already knows the session emits no synthetic start), so one helper's
        # placeholder aliased onto another helper's real event and suppressed it.
        $sid = "fanout-$([guid]::NewGuid())"
        Initialize-LogOffsets -App $script:app | Out-Null

        script:Invoke-HookInjection -Event 'agent.tool.starting' -Payload @{
            session_id = $sid
            cwd        = 'C:\hook-routing-fanout'
            tool_name  = 'edit'
        }

        $tally = Wait-Until -TimeoutSec 30 -Because 'master to record its replay rejections for the broadcast' -Condition {
            $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
            $t = script:Get-HookApplyTally -Text $text
            if ($t.Keys.Count -ge 1) { $t }
        }

        # A fan-out actually happened. Without this the case would still pass on a
        # single-helper machine even with the dedupe deleted.
        ($tally.Values | Measure-Object -Sum).Sum |
            Should -BeGreaterThan 0 -Because 'more than one helper must have forwarded the same broadcast, or this case proves nothing'

        # Both expansion slots survived, each deduped on its own key. If the suffix
        # were positional these would collide and one slot would be missing.
        $slots = @($tally.Keys | ForEach-Object { ($_ -split '#')[1] } | Sort-Object -Unique)
        $slots | Should -Contain 'start' -Because 'the synthetic start for an unseen session is published on its own slot'
        $slots | Should -Contain 'primary' -Because 'the reported tool event keeps its own slot and must not be swallowed as a replay of the start'

        # The load-bearing assertion: exactly one apply, whatever the helper count.
        $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $applied = @([regex]::Matches($text, 'received helper session hook event=(?<kind>\w+) \{ key: "(?<key>[^"]+)"') |
                Where-Object { $_.Groups['key'].Value -eq $sid } |
                ForEach-Object { $_.Groups['kind'].Value })
        @($applied | Where-Object { $_ -eq 'SessionStarted' }).Count |
            Should -Be 1 -Because 'the synthetic start must be applied once no matter how many helpers forwarded it'
        @($applied | Where-Object { $_ -eq 'ToolStarting' }).Count |
            Should -Be 1 -Because 'the reported tool event must be applied once, and must not be lost to the start slot'
    }

    It 'Terminal hook for an unknown session creates no row' {
        # Regression: a terminal event for a session WTA never saw start was given a
        # fabricated SessionStarted titled after its cwd basename, then immediately
        # stopped - leaving a permanent Ended row that reconcile cannot prune,
        # because it only drops ids the listing agent itself once returned.
        $sid = "ghost-$([guid]::NewGuid())"
        Initialize-LogOffsets -App $script:app | Out-Null

        script:Invoke-HookInjection -Event 'agent.session.end' -Payload @{
            session_id = $sid
            cwd        = 'C:\hook-routing-ghost'
            reason     = 'user_exit'
        }

        # Prove the injection completed before asserting an absence, so a silently
        # dropped command cannot masquerade as the fix working.
        $applied = Wait-Until -TimeoutSec 30 -Because 'master to process the terminal hook' -Condition {
            $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
            if ($text -match "SessionStopped \{ key: `"$([regex]::Escape($sid))`"") { $true }
        }
        $applied | Should -BeTrue -Because 'the hook must actually reach master for the absence below to mean anything'

        # Oracle note: `wta sessions list` would be the more direct state oracle, but
        # it is identity-gated outside the package (see Feature.SessionList) and
        # cannot reach master's pipe from the harness. The master log is the next
        # deterministic product-owned signal, and it is where the fabricated row was
        # first observable.
        $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $text | Should -Not -Match "SessionStarted \{ key: `"$([regex]::Escape($sid))`"" -Because 'a terminal event must never fabricate a start for a session WTA has not seen'
        $text | Should -Not -Match 'title: "hook-routing-ghost"' -Because 'the cwd basename must not become a session title'
    }

    It 'Agent error for an unknown session still records the failure' {
        # False-positive control for the case above. `agent.error` also arrives for a
        # session master may not know, but it reports a LIVE session that is failing,
        # and its ConnectionFailed reducer resolves the row through the pane binding
        # rather than the session key. Excluding it alongside the terminal events
        # would silently drop a first-observed connection failure.
        $sid = "err-$([guid]::NewGuid())"
        Initialize-LogOffsets -App $script:app | Out-Null

        script:Invoke-HookInjection -Event 'agent.error' -Payload @{
            session_id = $sid
            cwd        = 'C:\hook-routing-error'
            error      = 'agent CLI exited 1'
        }

        # The synthetic start IS the row creation: without it the pane-keyed
        # ConnectionFailed reducer has nothing to resolve and the failure is lost.
        $created = Wait-Until -TimeoutSec 30 -Because 'master to create the row the failure reducer needs' -Condition {
            $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
            if ($text -match "SessionStarted \{ key: `"$([regex]::Escape($sid))`"") { $true }
        }
        $created | Should -BeTrue -Because 'a connection failure must stay visible even when master never saw the session start'

        $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $text | Should -Match 'ConnectionFailed' -Because 'the failure itself must also reach master, not just the row it lands on'
    }
}
