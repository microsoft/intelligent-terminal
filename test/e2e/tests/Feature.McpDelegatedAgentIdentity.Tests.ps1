#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #1070 / d1778bad: the configured custom provider must survive both the
# initial helper CLI bootstrap and an identity-only agent_config_changed event.
# The existing ACP fixture calls the real Session MCP endpoint; Enter confirms
# its recommendation through the public executor, ShellManager, WTCLI and COM.
# The narrow native fixture never emits hooks, so pane identity
# and Agents projection cannot be repaired by agent.session.start.
# One coherent case includes the ordinary commandless create_workspace control.
# Existing protection: AgentProtocolExperience, ProposalMcpRouting, CustomDelegate,
# and AgentsModeActions. No paid CLI, model prompt, resolver shim or WSL required.

Describe 'Feature: MCP delegated agent identity' -Tag @('Feature', 'McpDelegatedAgentIdentity') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:app = $null
        $script:ownsConfig = $false
        if ($env:ITE2E_PACKAGE -ne 'Dev') { throw 'MCP delegated identity requires explicit Dev selection.' }
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        $head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $head -or -not $env:ITE2E_SOURCE_COMMIT -or
            -not $env:ITE2E_SOURCE_COMMIT.StartsWith($head, [StringComparison]::Ordinal)) {
            throw 'Supply source provenance from the exact-source build receipt.'
        }
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply deployed application and WTA hashes from the exact-source build receipt.'
        }
        $appHash = (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash
        $wtaHash = (Get-FileHash -LiteralPath $script:target.WtaPath).Hash
        $appHash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        $wtaHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Existing backup requires recovery before testing: $path"
            }
            $script:originalHashes[$path] = if (Test-Path -LiteralPath $path) {
                (Get-FileHash -LiteralPath $path).Hash
            } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:runId = [guid]::NewGuid().ToString('N')
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) "mcp-delegated-identity-$script:runId"
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $script:requestLog = Join-Path $script:evidence 'acp-requests.log'
        $script:launchLog = Join-Path $script:evidence 'native-launches.jsonl'
        @{
            source_commit = $env:ITE2E_SOURCE_COMMIT; app_sha256 = $appHash; wta_sha256 = $wtaHash
            package = $script:target.Package; install_location = $script:target.InstallLocation
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json')
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $acpFixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $nativeFixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-McpIdentityDelegate.ps1')).Path
        $invocation = "& '$($acpFixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))'"
        # ACP spawning splits on whitespace; reuse the established bare-pwsh
        # encoded invocation. The native delegate below still tests a full path.
        $acpCommand = 'pwsh -NoProfile -EncodedCommand ' +
            [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        # Both IDs deliberately differ from pwsh.exe and the script basename.
        $script:firstProvider = "custom:mcp-one-$script:runId"
        $script:secondProvider = "custom:mcp-two-$script:runId"
        $script:delegateCommand = "`"$pwsh`" -NoLogo -NoProfile -File `"$nativeFixture`" -LogPath `"$script:launchLog`" -RunId $script:runId"
        $script:ownsConfig = $true
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            acpAgent = 'custom:mcp-identity-acp'; acpCustomCommand = $acpCommand; acpModel = ''
            delegateAgent = $script:firstProvider; delegateCustomCommand = $script:delegateCommand; delegateModel = ''
            autoFixEnabled = $false; 'warning.confirmOnClose' = 'never'
            'aiIntegration.confirmation.createOperations' = 'prompt'
            actions = @(); keybindings = @()
            defaultProfile = '{1b916f6b-31a6-4463-8d9b-164487fae1c9}'
            profiles = @{
                list = @(@{
                    guid = '{1b916f6b-31a6-4463-8d9b-164487fae1c9}'
                    name = 'ite2e-mcp-identity'; commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                    startingDirectory = $script:evidence; commandPaletteAgent = ''
                    reloadEnvironmentVariables = $false
                })
            }
        }
        $script:app.Launched | Should -BeTrue
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true
        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue
        $script:windowId = [string]$script:app.WindowId
        $script:sourceShell = [string](Get-ActivePane -App $script:app).session_id
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Should -BeTrue
        $script:sourceSession = Get-AgentPaneSession -App $script:app
        $script:agentPane = [string]$script:sourceSession.PaneSessionId
        $script:sourceSession.AcpSessionId | Should -Not -BeNullOrEmpty

        function Get-IdentityElement {
            param([string]$Id)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id))
        }
        function Set-IdentityView {
            param([bool]$Agents)
            $expected = if ($Agents) { 'Agents' } else { 'Tabs' }
            if ((Get-IdentityElement VerticalTabsHeader).Current.Name -ne $expected) {
                Invoke-UiClick -App $script:app -Selector VerticalTabsHeaderButton | Out-Null
            }
            Wait-Until -TimeoutSec 10 -Because 'requested sidebar projection is visible' -Condition {
                (Get-IdentityElement VerticalTabsHeader).Current.Name -eq $expected
            } | Out-Null
        }
        function Get-IdentityRows {
            param([string]$Title)
            $list = Get-IdentityElement ItemsList
            if (-not $list -or $list.Current.IsOffscreen) { throw 'The owned sidebar tab list is not visible.' }
            @($list.FindAll([Windows.Automation.TreeScope]::Children,
                [Windows.Automation.Condition]::TrueCondition) | Where-Object {
                $row = $_
                -not $row.Current.IsOffscreen -and $row.Current.BoundingRectangle.Width -gt 0 -and
                    $row.Current.BoundingRectangle.Height -gt 0 -and
                    ($row.Current.Name -eq $Title -or
                        $row.FindFirst([Windows.Automation.TreeScope]::Descendants,
                            [Windows.Automation.PropertyCondition]::new(
                                [Windows.Automation.AutomationElement]::NameProperty, $Title)))
            })
        }
        function Get-IdentityLaunches {
            if (Test-Path -LiteralPath $script:launchLog) {
                @(Get-Content -LiteralPath $script:launchLog | Where-Object { $_.Trim() } |
                    ForEach-Object { $_ | ConvertFrom-Json })
            }
        }
        function Get-IdentitySessions {
            $pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
            (Invoke-Wta -App $script:app -TimeoutSec 10 -Arguments @(
                'sessions', 'list', '--master', $pipe, '--json', '--include-status')).sessions
        }
        function Invoke-IdentityWorkspace {
            param([string]$Marker, [switch]$Ordinary)
            Set-WtPaneFocus -App $script:app -SessionId $script:sourceShell | Out-Null
            $beforeTabs = @((Get-WtTabs -App $script:app -WindowId $script:windowId).tab_id)
            $beforeLaunches = @(Get-IdentityLaunches).Count
            $logOffset = @(Get-Content -LiteralPath $script:requestLog).Count
            $prefix = if ($Ordinary) { 'EMPTY_WORKSPACE' } else { 'DELEGATE_WORKSPACE' }
            $responseKind = if ($Ordinary) { 'empty-workspace-result' } else { 'delegate-workspace-result' }
            Clear-AgentInput -App $script:app -PaneSessionId $script:agentPane | Out-Null
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agentPane -Text "${prefix}_$Marker" | Out-Null
            $accepted = Wait-Until -TimeoutSec 20 -Because 'this ACP turn calls its session MCP tool successfully' -Condition {
                Get-Content -LiteralPath $script:requestLog | Select-Object -Skip $logOffset |
                    Where-Object { $_ -match "$responseKind\|.*`"status`":`"accepted`"" } |
                    Select-Object -Last 1
            }
            $accepted | Should -Not -BeNullOrEmpty
            $button = if ($Ordinary) { 'recommendations.button_open_tab' } else { 'recommendations.button_open_in_new_tab' }
            $buttonRegex = Get-WtaLocalizedTextRegex -Key $button
            if (-not $buttonRegex) { $buttonRegex = if ($Ordinary) { '(?i)Open Tab' } else { '(?i)Open in New Tab' } }
            $card = Wait-Until -TimeoutSec 15 -Because 'the owning helper displays the confirmation card' -Condition {
                $text = Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 80
                if ($text -match [regex]::Escape($Marker) -and $text -match $buttonRegex) { $text }
            }
            $card | Set-Content -LiteralPath (Join-Path $script:evidence "$Marker.card.txt")
            @(Get-WtTabs -App $script:app -WindowId $script:windowId | Where-Object tab_id -NotIn $beforeTabs).Count |
                Should -Be 0 -Because 'MCP acceptance alone must not bypass user confirmation'
            @(Get-IdentityLaunches).Count | Should -Be $beforeLaunches
            Send-AgentKey -App $script:app -PaneSessionId $script:agentPane -Key Enter | Out-Null
            $tab = Wait-Until -TimeoutSec 25 -Because 'the confirmed recommendation creates exactly one target tab' -Condition {
                $tabs = @(Get-WtTabs -App $script:app -WindowId $script:windowId | Where-Object tab_id -NotIn $beforeTabs)
                if ($tabs.Count -eq 1) { $tabs[0] }
            }
            $panes = @(Get-WtPanes -App $script:app -WindowId $script:windowId -TabId ([string]$tab.tab_id))
            $panes.Count | Should -Be 1
            $pane = $panes[0]
            $pane.is_agent_pane | Should -BeFalse -Because 'the delegate is native Terminal content, not the assistant helper'
            $pane | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $script:evidence "$Marker.pane.json")
            [pscustomobject]@{ Tab = $tab; Pane = $pane }
        }
        function Assert-IdentityNativeTarget {
            param($Workspace, [string]$Provider, [string]$Marker)
            $pane = $Workspace.Pane
            $pane.native_agent_provider_id | Should -Be $Provider -Because 'canonical provider intent must be seeded at pane creation, not inferred from pwsh or repaired by a hook'
            $receipt = Wait-Until -TimeoutSec 20 -Because 'the configured native fixture runs in this exact new pane' -Condition {
                Get-IdentityLaunches | Where-Object pane_session_id -EQ $pane.session_id | Select-Object -Last 1
            }
            $receipt.run_id | Should -Be $script:runId
            $receipt.source | Should -Be 'host'
            $receipt.mode | Should -Be 'fresh'
            $receipt.native_pid | Should -BeNullOrEmpty
            $receipt.cwd.TrimEnd('\') | Should -Be $script:evidence.TrimEnd('\')
            $receipt.task | Should -BeExactly "DELEGATED_TASK_$Marker"
            $receipt.command_line | Should -Match ([regex]::Escape('Mock-McpIdentityDelegate.ps1'))
            $receipt.command_line | Should -Not -Match '(?i)--session-id|--resume|--acp'
            (Get-WtPaneStatus -App $script:app -SessionId $pane.session_id).pid | Should -Be $receipt.pid
            $sessions = @(Get-IdentitySessions)
            $matchingSessionCount = @($sessions | Where-Object pane_session_id -EQ $pane.session_id).Count
            $matchingSessionCount |
                Should -Be 0 -Because 'a no-pin custom launch must not fabricate a conversation SID'
            Set-IdentityView $true
            Wait-Until -TimeoutSec 10 -Because 'the native target is represented in Agents before any session hook' -Condition {
                @(Get-IdentityRows -Title ([string]$pane.title)).Count -eq 1
            } | Out-Null
            @{
                provider_id = $Provider; pane = $pane; launch = $receipt
                session_absence = @{
                    queried_pane_session_id = $pane.session_id; matching_record_count = $matchingSessionCount
                }
                agents_row_count = @(Get-IdentityRows -Title ([string]$pane.title)).Count
                expected_icon = 'existing generic custom-provider Message glyph; rendered glyph review remains manual'
                hook_emission = 'none: native fixture has no session identifier or hook path'
            } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $script:evidence "$Marker.identity.json")
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "$Marker.agents.png") | Out-Null
            Get-UiTree -App $script:app -Selector ItemsList -Depth 8 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Marker.agents.tree.txt")
        }
    }

    AfterAll {
        if ($script:app) {
            $owned = $script:app.OwnedProcess
            if (-not $script:app.Launched -or -not $owned -or $owned.Id -ne $script:app.Pid) {
                throw 'Cleanup requires the captured owned host; configuration backups retained.'
            }
            # list-panes omits AgentPaneContent, and the ACP index omits unready
            # helpers. Native window close shuts down the entire owned pane tree
            # before any generic master kill, including hidden/uninitialized leases.
            if (-not $owned.HasExited) {
                if (-not $owned.CloseMainWindow()) {
                    throw 'Owned window rejected graceful close; refusing master-first teardown.'
                }
                if (-not (Test-Until -TimeoutSec 8 -IntervalSec 0.2 -Condition { $owned.HasExited })) {
                    throw 'Owned host remains active after graceful close; configuration backups retained.'
                }
            }
            Stop-Terminal -App $script:app
        }
        elseif ($script:ownsConfig -and $script:target -and
            -not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            Restore-WtConfig -App $script:target
        }
        if ($script:ownsConfig -and -not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            foreach ($path in $script:originalHashes.Keys) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                $actual | Should -Be $script:originalHashes[$path] -Because 'restore exact pre-run settings and state bytes'
            }
        }
    }

    It 'MCP delegated native identity survives bootstrap and hot updates' {
        $firstMarker = [guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant()
        $first = Invoke-IdentityWorkspace -Marker $firstMarker
        Assert-IdentityNativeTarget -Workspace $first -Provider $script:firstProvider -Marker $firstMarker

        Initialize-LogOffsets -App $script:app | Out-Null
        Set-WtDelegateAgent -App $script:app -Agent $script:secondProvider | Out-Null
        Wait-Until -TimeoutSec 20 -Because 'the existing source helper receives the identity-only settings update' -Condition {
            (Get-ItLogText -App $script:app -Name "wta-main_helper-$($script:sourceSession.HelperProcessId).log" -SinceStart) -match
                'delegate config hot-updated from settings change'
        } | Out-Null
        $settings = Invoke-WtCli -App $script:app -Arguments @('get-settings')
        $settings.delegateAgent | Should -Be $script:secondProvider
        (Get-WtSetting -App $script:app -Key delegateCustomCommand) | Should -BeExactly $script:delegateCommand
        (Get-WtSetting -App $script:app -Key delegateModel) | Should -BeNullOrEmpty -Because 'settings serialization may omit the unset default model'
        $sameHelper = Get-AgentPaneSession -App $script:app -PaneSessionId $script:agentPane
        $sameHelper.HelperProcessId | Should -Be $script:sourceSession.HelperProcessId
        $sameHelper.AcpSessionId | Should -Be $script:sourceSession.AcpSessionId
        $secondMarker = [guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant()
        $second = Invoke-IdentityWorkspace -Marker $secondMarker
        Assert-IdentityNativeTarget -Workspace $second -Provider $script:secondProvider -Marker $secondMarker
        $second.Pane.session_id | Should -Not -Be $first.Pane.session_id
        $retained = @(Get-WtPanes -App $script:app -WindowId $script:windowId -TabId ([string]$first.Tab.tab_id))[0]
        $retained.native_agent_provider_id | Should -Be $script:firstProvider -Because 'hot updates affect future launches, not previously created native content'

        $negativeMarker = [guid]::NewGuid().ToString('N').Substring(0, 12).ToUpperInvariant()
        $launchCount = @(Get-IdentityLaunches).Count
        $ordinary = Invoke-IdentityWorkspace -Marker $negativeMarker -Ordinary
        $ordinary.Pane.native_agent_provider_id | Should -BeNullOrEmpty
        (Get-WtPaneStatus -App $script:app -SessionId $ordinary.Pane.session_id).state | Should -Match 'run'
        @(Get-IdentityLaunches).Count | Should -Be $launchCount -Because 'create_workspace without an agent must open an ordinary shell'
        Set-IdentityView $false
        Wait-Until -TimeoutSec 10 -Because 'the completed ordinary workspace is visible in Tabs' -Condition {
            @(Get-IdentityRows -Title ([string]$ordinary.Pane.title)).Count -eq 1
        } | Out-Null
        Set-IdentityView $true
        for ($sample = 0; $sample -lt 4; $sample++) {
            @(Get-IdentityRows -Title ([string]$ordinary.Pane.title)).Count |
                Should -Be 0 -Because 'the completed ordinary shell must remain excluded from the Agents projection'
            Start-Sleep -Milliseconds 250
        }
        @(Get-IdentityRows -Title ([string]$first.Pane.title)).Count | Should -Be 1
        @(Get-IdentityRows -Title ([string]$second.Pane.title)).Count | Should -Be 1
        $sessions = @(Get-IdentitySessions)
        $queriedPaneIds = @($first.Pane.session_id, $second.Pane.session_id, $ordinary.Pane.session_id)
        @($sessions | Where-Object { $_.pane_session_id -in $queriedPaneIds }).Count | Should -Be 0
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'ordinary-excluded.agents.png') | Out-Null
        @{
            first = $first; second = $second; ordinary = $ordinary; source_helper = $sameHelper
            delegate_command_unchanged = $script:delegateCommand; delegate_model_unchanged = ''
            native_launches = @(Get-IdentityLaunches)
            session_absence = @($queriedPaneIds | ForEach-Object {
                $paneId = $_
                @{
                    queried_pane_session_id = $paneId
                    matching_record_count = @($sessions | Where-Object pane_session_id -EQ $paneId).Count
                }
            })
            scope = 'host new-tab; no claim for WSL, custom splitting, or rendered glyph acceptance'
        } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $script:evidence 'acceptance.json')
    }
}
