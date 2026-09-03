#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist section 8 (C287, C288, C289) - how one agent hook is routed
# from `wtcli` directly into the master session registry.
#
# Why these live at E2E rather than in a unit test: the WTA unit tests exercise one
# helper's routing function and the master's handler in isolation, inside a single
# process. The defect only exists at the real process boundary: one
# `wtcli agent-hook` invocation becomes one COM broadcast that reaches master AND
# every helper. Master must route its own copy exactly once while helpers update
# only their local pane bindings; if a helper forwards, the old N-times
# amplification returns.
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

    }

    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'One hook broadcast applies once per helper fan-out' {
        # Regression: one `wtcli agent-hook` reached master once per connected
        # helper, so a single SessionStarted was applied 3x with 3 helpers and
        # master re-broadcast sessions/changed for each copy.
        #
        # The fixed architecture has no replay protocol at all: master subscribes
        # to the COM broadcast directly, while every helper updates only its local
        # pane binding. `agent.tool.starting` for an unseen session deliberately
        # expands into TWO master transitions - a synthetic start plus the tool
        # event - and each must appear exactly once.
        $sid = "fanout-$([guid]::NewGuid())"
        Initialize-LogOffsets -App $script:app | Out-Null

        script:Invoke-HookInjection -Event 'agent.tool.starting' -Payload @{
            session_id = $sid
            cwd        = 'C:\hook-routing-fanout'
            tool_name  = 'edit'
        }

        $processed = Wait-Until -TimeoutSec 30 -Because 'master to process its COM copy of the hook' -Condition {
            $master = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
            @($master -split "`r?`n" |
                    Where-Object { $_ -match 'processed COM agent hook' -and $_ -match [regex]::Escape($sid) }) |
                Select-Object -First 1
        }

        @($processed).Count |
            Should -Be 1 -Because 'one raw COM hook must be processed exactly once no matter how many helpers are alive'
        $processed | Should -Match 'transition_count=2' -Because 'the unseen-session start and the reported tool event must both land'
        $processed | Should -Match 'changed=true' -Because 'the post-reducer breadcrumb must prove state actually changed'
        $processed | Should -Match 'final_status=Some\(Working\)' -Because 'the final authoritative state must reflect the tool event, not stop after the synthetic start'

        # A helper-forwarded synthetic start is a lifecycle event and therefore
        # logged at info even in Release builds. Its absence is a stable,
        # non-debug oracle that helpers kept the broadcast local.
        $master = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $master | Should -Not -Match "received helper session hook.*$([regex]::Escape($sid))" -Because 'helpers maintain local bindings only; forwarding would recreate the N-times amplification'
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
            @($text -split "`r?`n" |
                    Where-Object { $_ -match 'processed COM agent hook' -and $_ -match [regex]::Escape($sid) }) |
                Select-Object -First 1
        }
        $applied | Should -Match 'transition_count=1' -Because 'only the real SessionStopped may be planned; a synthetic start would make this two'
        $applied | Should -Match 'changed=false' -Because 'stopping an unseen session must be a reducer no-op'
        $applied | Should -Match 'final_status=None' -Because 'no registry row may survive'

        # Oracle note: `wta sessions list` would be the more direct state oracle, but
        # it is identity-gated outside the package (see Feature.SessionList) and
        # cannot reach master's pipe from the harness. The master log is the next
        # deterministic product-owned signal, and it is where the fabricated row was
        # first observable.
        $text = Get-ItLogText -App $script:app -Name 'wta-main_master.log' -SinceStart
        $text | Should -Not -Match "received helper session hook.*$([regex]::Escape($sid))" -Because 'the terminal hook must come from master COM routing, not a helper forward'
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
            @($text -split "`r?`n" |
                    Where-Object { $_ -match 'processed COM agent hook' -and $_ -match [regex]::Escape($sid) }) |
                Select-Object -First 1
        }
        $created | Should -Match 'transition_count=2' -Because 'the synthetic start and ConnectionFailed must both land'
        $created | Should -Match 'changed=true' -Because 'the post-reducer breadcrumb must prove the failure changed state'
        $created | Should -Match 'final_status=Some\(Error\)' -Because 'the authoritative row must finish in Error, not merely be created'
    }
}
