#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PRs #601, #606, #610, #611, #612, #616, #634, and #683: exercise ACP
# notifications, client requests, Session MCP actions, session configuration,
# and replacement lifecycle through the deployed Terminal, helper, master, and
# a real stdio agent.

BeforeDiscovery {
    $fixturePath = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue) -and
        ($fixturePath -notmatch '\s') -and
        ($env:TEMP -notmatch '\s')
    )
}

Describe 'Feature: ACP agent-pane protocol experience' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
    }
    BeforeEach {
        $script:requestLog = Join-Path $env:TEMP "ite2e-agent-protocol-$([guid]::NewGuid().ToString('N')).log"
        $command = "pwsh -NoProfile -File $script:fixture -LogPath $script:requestLog"
        $script:delegateLaunchMarker = "CONFIGURED_DELEGATE_$([guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant())"
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            acpAgent = 'custom:interaction-fixture'
            acpCustomCommand = $command
            acpModel = ''
            delegateAgent = 'custom:interaction-delegate'
            delegateCustomCommand = "cmd /k echo $script:delegateLaunchMarker"
        }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 30 |
            Should -BeTrue -Because 'the deterministic ACP interaction fixture must connect'
        $script:agentPane = (Get-AgentPaneSession -App $script:app).PaneSessionId
        $script:WindowId = [string]$script:app.WindowId
        $script:GetTabIds = {
            @((Get-WtTabs -App $script:app -WindowId $script:WindowId) | ForEach-Object { [string]$_.tab_id })
        }
        $script:WaitForNewWorkspace = {
            param([string[]]$BeforeTabIds)

            $tab = Wait-Until -TimeoutSec 20 -IntervalSec 0.4 -Because 'the confirmed action to create a new tab' -Condition {
                @(Get-WtTabs -App $script:app -WindowId $script:WindowId) |
                    Where-Object { [string]$_.tab_id -notin $BeforeTabIds } |
                    Select-Object -First 1
            }
            $panes = Wait-Until -TimeoutSec 10 -IntervalSec 0.4 -Because 'the new tab to expose its terminal pane' -Condition {
                $value = @(Get-WtPanes -App $script:app -WindowId $script:WindowId -TabId ([string]$tab.tab_id))
                if ($value.Count) { $value }
            }
            [pscustomobject]@{ Tab = $tab; Panes = @($panes) }
        }
    }
    AfterEach {
        if ($script:app) {
            Stop-Terminal -App $script:app
            $script:app = $null
        }
        if ($script:requestLog -and (Test-Path -LiteralPath $script:requestLog)) {
            Remove-Item -LiteralPath $script:requestLog -Force
        }
    }

    It 'ACP tool details and transcript order survive the real process boundary' {
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text 'TOOL_FLOW' | Out-Null

        $rendered = Wait-Until -TimeoutSec 15 -IntervalSec 0.2 -Because 'the ordered ACP tool transcript to render' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 100
            if ($text -match 'TOOL_DETAIL_MARKER' -and
                $text -match 'TOOL_OUTPUT_MARKER' -and
                $text -match 'PLAN_MARKER' -and
                $text -match 'AFTER_TOOL_MARKER') {
                $text
            }
        }

        $tool = $rendered.IndexOf('TOOL_DETAIL_MARKER', [System.StringComparison]::Ordinal)
        $plan = $rendered.IndexOf('PLAN_MARKER', [System.StringComparison]::Ordinal)
        $after = $rendered.IndexOf('AFTER_TOOL_MARKER', [System.StringComparison]::Ordinal)
        $tool | Should -BeLessThan $plan
        $plan | Should -BeLessThan $after
    }

    It 'Clarification modal returns the selected answer to the requesting ACP session' {
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text 'ASK_INPUT' | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane `
            -Pattern 'Choose the deterministic answer' -TimeoutSec 15
        $modal = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 60
        $modal | Should -Match 'Alpha'
        $modal | Should -Match 'Beta'

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Down | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null

        $answered = Wait-Until -TimeoutSec 15 -Because 'the selected answer to round-trip to the ACP agent' -Condition {
            Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match 'user-input-result\|.*"outcome":"answered".*"answer":"Beta".*"selected_index":1' } |
                Select-Object -First 1
        }
        $answered | Should -Not -BeNullOrEmpty
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane `
            -Pattern 'INPUT_RESULT:.*answered.*Beta' -TimeoutSec 15
    }

    It 'ACP session config picker preserves order and hot-applies a selection' {
        Clear-AgentInput -App $script:app -PaneSessionId $script:agentPane | Out-Null
        Invoke-AgentMenuItem -App $script:app -PaneSessionId $script:agentPane -Name '/config'

        $picker = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 50
        $mode = $picker.IndexOf('Mode', [System.StringComparison]::Ordinal)
        $reasoning = $picker.IndexOf('Reasoning', [System.StringComparison]::Ordinal)
        $mode | Should -BeGreaterOrEqual 0
        $mode | Should -BeLessThan $reasoning

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null
        Assert-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -Pattern '\bAsk\b' -TimeoutSec 10
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Down | Out-Null
        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null

        $applied = Wait-Until -TimeoutSec 15 -Because 'the config selection to reach the ACP agent' -Condition {
            Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match 'session/set_config_option\|mode\|code' } |
                Select-Object -First 1
        }
        $applied | Should -Not -BeNullOrEmpty

        Clear-AgentInput -App $script:app -PaneSessionId $script:agentPane | Out-Null
        Invoke-AgentMenuItem -App $script:app -PaneSessionId $script:agentPane -Name '/config'
        (Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 50) |
            Should -Match '(?m)Mode.*Code' -Because 'the live config_option_update must replace the old value'
    }

    It 'Agent pane title reflects the confirmed active model' {
        $title = Wait-Until -TimeoutSec 15 -Because 'the XAML pane title to publish the confirmed ACP model' -Condition {
            $value = Get-UiValue -App $script:app -Selector 'AgentLabelText'
            if ($value -match 'Fixture Model') { $value }
        }
        $title | Should -Match 'Fixture Model'
    }

    It 'New-tab command workspaces accept a split-direction hint across Session MCP' {
        $marker = [guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant()
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "TAB_DIRECTION_$marker" | Out-Null

        $accepted = Wait-Until -TimeoutSec 15 -Because 'the new_tab action with split_direction=auto to pass MCP validation' -Condition {
            Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match 'tab-direction-result\|.*"status":"accepted"' } |
                Select-Object -First 1
        }
        $accepted | Should -Not -BeNullOrEmpty
        $card = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 60
        $card | Should -Match "echo $marker"
        $card | Should -Match '(?i)Open in New Tab'
    }

    It 'Empty workspaces open without sending a command across Session MCP' {
        $marker = [guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant()
        $sentinel = "EMPTY_COMMAND_SENTINEL_$marker"
        $beforeTabs = & $script:GetTabIds
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "EMPTY_WORKSPACE_$marker" | Out-Null

        $accepted = Wait-Until -TimeoutSec 15 -Because 'the command-less create_workspace request to pass Session MCP validation' -Condition {
            Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match 'empty-workspace-result\|.*"status":"accepted"' } |
                Select-Object -First 1
        }
        $accepted | Should -Not -BeNullOrEmpty

        $openTabRegex = Get-WtaLocalizedTextRegex -Key 'recommendations.button_open_tab'
        if (-not $openTabRegex) { $openTabRegex = '(?i)Open Tab' }
        $card = Wait-Until -TimeoutSec 15 -Because 'the Helper confirmation card for an empty tab' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 60
            if ($text -match [regex]::Escape($sentinel) -and $text -match $openTabRegex) { $text }
        }
        $card | Should -Not -BeNullOrEmpty

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null
        $workspace = & $script:WaitForNewWorkspace $beforeTabs
        $workspace.Tab | Should -Not -BeNullOrEmpty
        $workspace.Panes.Count | Should -BeGreaterOrEqual 1
        (Get-WtPaneStatus -App $script:app -SessionId ([string]$workspace.Panes[0].session_id)).state |
            Should -Match 'run'

        Start-Sleep -Seconds 2
        (Get-WtCapture -App $script:app -SessionId ([string]$workspace.Panes[0].session_id) -MaxLines 30) |
            Should -Not -Match ([regex]::Escape($sentinel)) -Because 'omitting command must not send the summary sentinel into the new shell'
    }

    It 'Delegated tasks reach the configured agent in a new workspace across Session MCP' {
        $marker = [guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant()
        $task = "DELEGATED_TASK_$marker"
        $beforeTabs = & $script:GetTabIds
        Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "DELEGATE_WORKSPACE_$marker" | Out-Null

        $accepted = Wait-Until -TimeoutSec 15 -Because 'the delegate_task_in_new_workspace request to pass Session MCP validation' -Condition {
            Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match 'delegate-workspace-result\|.*"status":"accepted"' } |
                Select-Object -First 1
        }
        $accepted | Should -Not -BeNullOrEmpty

        $openTabRegex = Get-WtaLocalizedTextRegex -Key 'recommendations.button_open_in_new_tab'
        if (-not $openTabRegex) { $openTabRegex = '(?i)Open in New Tab' }
        $card = Wait-Until -TimeoutSec 15 -Because 'the Helper confirmation card for configured delegation' -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 60
            if ($text -match [regex]::Escape($task) -and $text -match $openTabRegex) { $text }
        }
        $card | Should -Not -BeNullOrEmpty

        Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null
        $workspace = & $script:WaitForNewWorkspace $beforeTabs
        $workspace.Tab | Should -Not -BeNullOrEmpty
        $workspace.Panes.Count | Should -BeGreaterOrEqual 1

        $delegatedPane = [string]$workspace.Panes[0].session_id
        Assert-Pane -App $script:app -SessionId $delegatedPane `
            -Match ([regex]::Escape($script:delegateLaunchMarker)) -TimeoutSec 15
        Assert-Pane -App $script:app -SessionId $delegatedPane `
            -Match ([regex]::Escape($task)) -TimeoutSec 15
    }

    It '/new physically closes the replaced ACP session before creating another' {
        $before = Get-AgentPaneSession -App $script:app -PaneSessionId $script:agentPane
        $before.AcpSessionId | Should -Not -BeNullOrEmpty

        Clear-AgentInput -App $script:app -PaneSessionId $script:agentPane | Out-Null
        Invoke-AgentMenuItem -App $script:app -PaneSessionId $script:agentPane -Name '/new'

        $after = Wait-Until -TimeoutSec 20 -Because '/new to attach a different ACP session' -Condition {
            $session = Get-AgentPaneSession -App $script:app -PaneSessionId $script:agentPane
            if ($session -and $session.AcpSessionId -ne $before.AcpSessionId) { $session }
        }
        $after.AcpSessionId | Should -Not -Be $before.AcpSessionId

        $events = Get-Content -LiteralPath $script:requestLog -ErrorAction SilentlyContinue
        $closeIndex = [array]::FindIndex([string[]]$events, [Predicate[string]]{ param($line) $line -match "session/close\|$([regex]::Escape($before.AcpSessionId))" })
        $newIndex = [array]::FindLastIndex([string[]]$events, [Predicate[string]]{ param($line) $line -match 'session/new\|' })
        $closeIndex | Should -BeGreaterOrEqual 0 -Because 'the replaced session must be physically released'
        $closeIndex | Should -BeLessThan $newIndex -Because 'session/close must finish before replacement session/new'
        $events | Should -Not -Match 'session/load' -Because '/new must not replay the replaced session'
    }
}
