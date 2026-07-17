#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #296: `/agent` selects a runtime-only agent override for one tab while the
# shared master keeps sibling tabs and their conversations alive.
# Release checklist: C225-C226 cover the slash-command UX; C227-C228 cover
# per-tab isolation and global-default/override behavior.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature per-tab agent selection through /agent' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue -Because 'the default Copilot pane must connect before /agent is exercised'
        $active = Get-ActivePane -App $script:app
        $script:defaultPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $active.session_id -TimeoutSec 30).PaneSessionId
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It '/agent appears in the slash menu and opens a keyboard-operable picker' {
        $titleRe = Get-WtaLocalizedTextRegex -Key 'agent_picker.title'
        if (-not $titleRe) { $titleRe = '(?i)Select agent' }
        try {
            # /agent is below the initially visible rows on short panes, so navigate the real
            # slash menu instead of assuming every registered command is visible at once.
            Invoke-AgentMenuItem -App $script:app -PaneSessionId $script:defaultPane -Name '/agent' | Out-Null
            Assert-AgentPaneText -App $script:app -PaneSessionId $script:defaultPane -Pattern $titleRe -TimeoutSec 15
            (Get-AgentPaneText -App $script:app -PaneSessionId $script:defaultPane -MaxLines 50) |
                Should -Match '(?i)copilot' -Because 'the installed current agent must be present in the picker'

            # Enter commits the initially selected current agent. That selection is a no-op:
            # it must close the picker without rebuilding the pane.
            Send-AgentKey -App $script:app -PaneSessionId $script:defaultPane -Key Enter | Out-Null
            (Get-AgentPaneSession -App $script:app -PaneSessionId $script:defaultPane) |
                Should -Not -BeNullOrEmpty -Because 'selecting the current agent must not rebuild its pane'
        }
        finally {
            Clear-AgentInput -App $script:app -PaneSessionId $script:defaultPane | Out-Null
        }
    }

    It '/agent rejects an unavailable id without changing the pane or global setting' {
        Clear-AgentInput -App $script:app -PaneSessionId $script:defaultPane | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:defaultPane -Text '/agent no-such-agent-e2e' | Out-Null
        # The prose is localized and contains a substituted %{agent} placeholder. Match the
        # locked command/id tokens instead of treating the untranslated template as literal text.
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:defaultPane -Pattern '(?s)no-such-agent-e2e.*\/agent' -TimeoutSec 15
        (Get-AgentPaneSession -App $script:app -PaneSessionId $script:defaultPane) |
            Should -Not -BeNullOrEmpty -Because 'an invalid agent id must leave the current pane running'
        Assert-Setting -App $script:app -Key 'acpAgent' -Value 'copilot'
    }
}

Describe 'Feature two tabs run different agents through one master' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $installed = @('claude', 'codex', 'gemini') |
            Where-Object { Get-Command $_ -ErrorAction SilentlyContinue }
        $status = [ordered]@{}
        foreach ($agent in $installed) {
            $status[$agent] = Get-AgentCliStatus -Agent $agent
        }
        $script:targetAgent = @($installed | Where-Object { $status[$_] -eq 'authed' } | Select-Object -First 1)
        if (-not $script:targetAgent) { $script:targetAgent = @($installed | Select-Object -First 1) }
        $script:targetAgent = $script:targetAgent | Select-Object -First 1
        $script:targetAuthenticated = $script:targetAgent -and $status[$script:targetAgent] -eq 'authed'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        $script:GetMaster = {
            @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" -ErrorAction SilentlyContinue |
                Where-Object {
                    $_.ParentProcessId -eq $script:app.Pid -and
                    $_.CommandLine -match '--master(\s|$|")' -and
                    $_.CommandLine -notmatch '--connect-master'
                }) | Select-Object -First 1
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Switching tab B rebuilds only tab B and leaves tab A conversation alive' {
        if (-not $script:targetAgent) {
            Set-ItResult -Skipped -Because 'no non-Copilot built-in agent CLI is installed'
            return
        }

        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue
        $tabA = Get-ActivePane -App $script:app
        $script:tabAPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $tabA.session_id -TimeoutSec 30).PaneSessionId
        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabAPane -Text 'What is 20 plus 22? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -Pattern '\b42\b' -TimeoutSec 60

        $tabB = New-WtTab -App $script:app -Title 'per-tab-agent-b'
        $script:tabBShellPane = $tabB.session_id
        Set-WtPaneFocus -App $script:app -SessionId $tabB.session_id
        Open-AgentPane -App $script:app | Out-Null
        $tabBSession = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $script:tabBShellPane -TimeoutSec 30
        $script:oldTabBPane = $tabBSession.PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:oldTabBPane -TimeoutSec 90 | Should -BeTrue

        # `/agent` only offers installed + GPO-allowed agents. Treat a CLI that exists on
        # this shell's PATH but is absent from the picker as an environment prerequisite gap.
        Send-AgentPrompt -App $script:app -PaneSessionId $script:oldTabBPane -Text '/agent' | Out-Null
        $picker = Get-AgentPaneText -App $script:app -PaneSessionId $script:oldTabBPane -MaxLines 60
        if ($picker -notmatch ('(?i)\(' + [regex]::Escape($script:targetAgent) + '\)')) {
            Set-ItResult -Skipped -Because "$script:targetAgent is installed for the test shell but unavailable to the packaged helper or blocked by policy"
            return
        }
        Clear-AgentInput -App $script:app -PaneSessionId $script:oldTabBPane | Out-Null

        $masterBefore = & $script:GetMaster
        $masterBefore | Should -Not -BeNullOrEmpty
        $script:masterPid = $masterBefore.ProcessId
        Initialize-LogOffsets -App $script:app | Out-Null
        Send-AgentPrompt -App $script:app -PaneSessionId $script:oldTabBPane -Text "/agent $script:targetAgent" | Out-Null

        (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                -not (Get-AgentPaneSession -App $script:app -PaneSessionId $script:oldTabBPane)
            }) | Should -BeTrue -Because 'switching agents must replace tab B helper/pane'
        $newB = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $script:tabBShellPane -ExcludePaneSessionId $script:oldTabBPane -TimeoutSec 45
        $script:tabBPane = $newB.PaneSessionId
        $script:tabBPane | Should -Match '[0-9A-Fa-f-]{36}'

        (& $script:GetMaster).ProcessId | Should -Be $masterBefore.ProcessId -Because 'a per-tab switch must reuse the shared master'
        (Get-AgentPaneSession -App $script:app -PaneSessionId $script:tabAPane) |
            Should -Not -BeNullOrEmpty -Because 'tab A pane must survive tab B switching agents'
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -Pattern '\b42\b' -TimeoutSec 10
        Assert-Setting -App $script:app -Key 'acpAgent' -Value 'copilot'
        Assert-Log -App $script:app -Name 'wta-main_master.log' -Pattern ('agent CLI spawned.*agent_cmd=.*' + [regex]::Escape($script:targetAgent)) -TimeoutSec 60
        $expectedSource = (Get-Culture).TextInfo.ToTitleCase($script:targetAgent)
        Assert-Log -App $script:app -Name 'wta-main_master.log' -Pattern (
            'cli_source resolved.*resolved_agent_id=' + [regex]::Escape($script:targetAgent) +
            '.*cli_source=Some\(' + [regex]::Escape($expectedSource) + '\)'
        ) -TimeoutSec 30
    }

    It 'Both agent tabs remain independently usable after the switch' {
        if (-not $script:tabBPane) {
            Set-ItResult -Skipped -Because 'the preceding environment-gated switch did not create tab B'
            return
        }
        if (-not $script:targetAuthenticated) {
            Set-ItResult -Skipped -Because "$script:targetAgent is installed but its non-interactive authentication probe did not succeed"
            return
        }
        if (-not (Wait-AgentReady -App $script:app -PaneSessionId $script:tabBPane -TimeoutSec 150)) {
            Set-ItResult -Skipped -Because "$script:targetAgent is installed but not authenticated or its ACP adapter did not connect"
            return
        }

        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabBPane -Text 'What is 3 plus 4? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:tabBPane -Pattern '\b7\b' -TimeoutSec 150
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:tabBPane -MaxLines 80) |
            Should -Not -Match '\b42\b' -Because 'tab B must not inherit tab A conversation'

        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabAPane -Text 'What is 4 plus 5? Reply with only the number.' | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:tabAPane -Pattern '\b9\b' -TimeoutSec 60
    }

    It 'A newly opened tab still follows the global default agent' {
        if (-not $script:tabBPane) {
            Set-ItResult -Skipped -Because 'the preceding environment-gated switch did not create tab B'
            return
        }

        $tabC = New-WtTab -App $script:app -Title 'per-tab-agent-default'
        Set-WtPaneFocus -App $script:app -SessionId $tabC.session_id
        Open-AgentPane -App $script:app | Out-Null
        $tabCSession = Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $tabC.session_id -TimeoutSec 30
        $script:tabCPane = $tabCSession.PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:tabCPane -TimeoutSec 90 | Should -BeTrue

        # If tab C follows the global Copilot default, selecting Copilot is a no-op and the
        # same helper remains alive. If it incorrectly inherited tab B's override, this would
        # rebuild tab C and invalidate the pinned session id.
        Send-AgentPrompt -App $script:app -PaneSessionId $script:tabCPane -Text '/agent copilot' | Out-Null
        $pinnedPaneClosed = Test-Until -TimeoutSec 5 -IntervalSec 0.5 -Condition {
            -not (Get-AgentPaneSession -App $script:app -PaneSessionId $script:tabCPane)
        }
        $pinnedPaneClosed | Should -BeFalse -Because 'selecting the inherited global default must not rebuild tab C'
        (Get-AgentPaneSession -App $script:app -PaneSessionId $script:tabCPane) |
            Should -Not -BeNullOrEmpty -Because 'new tabs must follow the global default rather than inherit a sibling override'
    }

    It 'Changing the global default rebuilds follower tabs but preserves overridden tab B' {
        if (-not $script:tabBPane -or -not $script:tabCPane) {
            Set-ItResult -Skipped -Because 'the preceding environment-gated cases did not create both tabs'
            return
        }

        Set-WtAgent -App $script:app -Agent $script:targetAgent | Out-Null
        (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                -not (Get-AgentPaneSession -App $script:app -PaneSessionId $script:tabCPane)
            }) | Should -BeTrue -Because 'tab C follows the global default and must rebuild when that default changes'
        (Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
                -not (Get-AgentPaneSession -App $script:app -PaneSessionId $script:tabAPane)
            }) | Should -BeTrue -Because 'tab A also follows the global default and must rebuild when that default changes'
        (Get-AgentPaneSession -App $script:app -PaneSessionId $script:tabBPane) |
            Should -Not -BeNullOrEmpty -Because 'the explicit per-tab override must survive a global default change'
        (& $script:GetMaster).ProcessId | Should -Be $script:masterPid -Because 'a global default change must not restart the multi-agent master'
        Assert-Setting -App $script:app -Key 'acpAgent' -Value $script:targetAgent
    }
}
