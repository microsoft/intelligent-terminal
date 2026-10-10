#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
param([switch]$KeepRunningOnly)

# Real UI actions -> App TraceLogging -> bounded ETW capture -> typed TDH payloads.
# No policy writes, model quota, or synthetic telemetry producers.
Describe 'Feature: sidebar telemetry' -Tag 'Feature', 'Telemetry', 'SidebarTelemetry' -Skip:($env:ITE2E_TELEMETRY -ne '1') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\SidebarExpansionEvents.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TelemetryTrace.ps1')
        . (Join-Path $PSScriptRoot 'helpers\TelemetryFunnels.Scenarios.ps1')
        $script:app = $null
        $script:target = $null
        $script:originalHashes = $null
        $script:ownedPids = [Collections.Generic.HashSet[int]]::new()
        $script:fixturePanes = @()
        $script:phases = [ordered]@{}
        $script:phaseErrors = @{}
        $script:rowFieldCases = @(
            @{ Phase = 'fields-directory'; Selector = 'RichTabAgentStatusVisibleItem'; Fields = 'workingDirectory' }
            @{ Phase = 'fields-empty'; Selector = 'RichTabWorkingDirectoryVisibleItem'; Fields = '' }
            @{ Phase = 'fields-branch'; Selector = 'RichTabBranchVisibleItem'; Fields = 'branch' }
            @{ Phase = 'fields-repository-branch'; Selector = 'RichTabRepositoryVisibleItem'; Fields = 'repository,branch' }
            @{ Phase = 'fields-remove-repository'; Selector = 'RichTabRepositoryVisibleItem'; Fields = 'branch' }
            @{ Phase = 'fields-branch-changes'; Selector = 'RichTabChangesVisibleItem'; Fields = 'branch,changes' }
            @{ Phase = 'fields-changes'; Selector = 'RichTabBranchVisibleItem'; Fields = 'changes' }
            @{ Phase = 'fields-agent-changes'; Selector = 'RichTabAgentStatusVisibleItem'; Fields = 'agentStatus,changes' }
            @{ Phase = 'fields-agent'; Selector = 'RichTabChangesVisibleItem'; Fields = 'agentStatus' }
            @{ Phase = 'fields-defaults'; Selector = 'RichTabWorkingDirectoryVisibleItem'; Fields = 'agentStatus,workingDirectory' }
        )
        $script:rowFieldSelectors = [ordered]@{
            agentStatus = 'RichTabAgentStatusVisibleItem'
            workingDirectory = 'RichTabWorkingDirectoryVisibleItem'
            repository = 'RichTabRepositoryVisibleItem'
            branch = 'RichTabBranchVisibleItem'
            changes = 'RichTabChangesVisibleItem'
        }
        $script:appProvider = '24a1622f-7da7-5c77-3303-d850bd1ab2ed'
        $script:wtaProvider = '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b'
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'Sidebar telemetry validation requires explicitly selected Dev.' }
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply TerminalApp.dll and WTA SHA256 values from the exact-source build receipt.'
        }
        if ($env:ITE2E_TELEMETRY_POLICY_APPROVED -eq '1') {
            throw 'Run this policy-free suite without ITE2E_TELEMETRY_POLICY_APPROVED.'
        }
        $script:target = Resolve-ItApp -Package Dev
        @(Get-WtProcessesForApp -App $script:target) | Should -HaveCount 0 -Because 'user Dev processes must not be adopted or stopped'
        (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash -LiteralPath $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) { throw "Existing backup requires recovery: $path" }
            $script:originalHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:root = [IO.Path]::GetFullPath((Join-Path $root ('sidebar-telemetry-' + [guid]::NewGuid().ToString('N'))))
        New-Item -ItemType Directory -Path $script:root -Force | Out-Null
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $log = Join-Path $script:root 'fixture.log'
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($log.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))

        function Sync-SidebarWindow {
            $window = Wait-Until -TimeoutSec 15 -Because 'one visible window belongs to the owned Dev process' -Condition {
                $windows = @(Get-WtWindowHwnds -App $script:app | Where-Object {
                    $_.pid -eq $script:app.Pid -and $_.title -notin @('PopupHost', 'Popup')
                })
                if ($windows.Count -eq 1) { $windows[0] }
            }
            $script:app.Hwnd = $window.hwnd
            Wait-Until -TimeoutSec 45 -Because 'the Dev window owns foreground after the elevated collector opens' -Condition {
                Set-WtWindowForeground -App $script:app -Attempts 2 -DelayMs 200
            } | Out-Null
        }
        function Set-SidebarHistory {
            param([bool]$Open)
            try {
                Set-TestSidebarScope -App $script:app -AgentsOnly $Open -Recent $Open
                Wait-Until -TimeoutSec 10 -Condition {
                    (Get-UiElement -App $script:app -Selector VerticalTabsHeader).name -eq 'Tabs'
                } | Out-Null
            }
            catch {
                Get-UiTree -App $script:app -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'filter-error-ui.txt')
                throw
            }
        }
        function Get-AgentViewRowCount {
            $path = Join-Path $script:root ('agent-view-' + [guid]::NewGuid().ToString('N') + '.jsonl')
            # The external test runner does not inherit package identity.
            $pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
            $result = Invoke-Wta -App $script:app -Arguments @('sessions', 'list', '--master', $pipe, '--origin', 'shell', '--json') -Raw
            $result.StdOut | Set-Content -LiteralPath $path
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            $rows = @($result.StdOut -split '\r?\n' | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json })
            foreach ($row in $rows) {
                $row.session_id | Should -Not -BeNullOrEmpty
                $row.provider_id | Should -Not -BeNullOrEmpty
                ($row.location -eq 'Host' -or $row.location.Wsl.distro) | Should -BeTrue
            }
            $script:agentViewSessionIds = @($rows.session_id)
            $rows.Count
        }
        function Add-AgentViewFixtures {
            $expected = @(
                foreach ($pane in @($script:tabA.session_id, $script:tabB.session_id)) {
                    $id = 'sidebar-telemetry-' + [guid]::NewGuid().ToString('N')
                    $path = Join-Path $script:root "$id.json"
                    @{ session_id = $id; cwd = $script:root; tool_name = 'edit' } |
                        ConvertTo-Json -Compress | Set-Content -LiteralPath $path
                    $command = "Get-Content -Raw -LiteralPath '$($path.Replace("'", "''"))' | & '$($script:app.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event agent.tool.starting"
                    Invoke-RunCommand -App $script:app -SessionId $pane -Command $command -SettleSec 3 | Out-Null
                    $script:fixturePanes += $pane
                    $id
                }
            )
            Wait-Until -TimeoutSec 20 -Because 'both shell-origin hook fixtures appear in the real master registry' -Condition {
                Get-AgentViewRowCount | Out-Null
                @($expected | Where-Object { $_ -notin $script:agentViewSessionIds }).Count -eq 0
            } | Out-Null
        }
        function Wait-AgentViewLoaded {
            param([int]$Count)
            Wait-UiElement -App $script:app -Selector HistoryHeaderButton | Out-Null
            Wait-UiElement -App $script:app -Selector HistoryLoadingIndicator -Gone | Out-Null
            if ($Count) {
                Wait-UiElement -App $script:app -Selector ItemsList | Out-Null
            }
            else {
                Wait-UiElement -App $script:app -Selector 'No agent sessions found.' | Out-Null
            }
        }
        function Set-SidebarQuery {
            param([string]$Text)
            $search = Get-UiElement -App $script:app -Selector SearchTextBox
            if (-not $search -or $search.isOffscreen -or $search.height -le 0) {
                Invoke-UiClick -App $script:app -Selector SearchTabsButton | Out-Null
            }
            Set-UiValue -App $script:app -Selector SearchTextBox -Value $Text | Out-Null
            (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be $Text
        }
        function Get-SidebarHeader {
            param([string]$Title)
            $tree = Get-UiTree -App $script:app -Selector ItemsList -Depth 8
            $pattern = '(?m)^(?<Indent>[ \t]*)(?<Selector>lbl-textview-\S+|TextView) Text "' + [regex]::Escape($Title) + '"(?<State>[^\r\n]*)'
            $matches = @([regex]::Matches($tree, $pattern))
            if ($matches.Count -gt 1) {
                # Expanded split panes can repeat the tab title beneath its parent header.
                $depth = ($matches | ForEach-Object { $_.Groups['Indent'].Length } | Measure-Object -Minimum).Minimum
                $matches = @($matches | Where-Object { $_.Groups['Indent'].Length -eq $depth })
            }
            if ($matches.Count -gt 1) { throw "Ambiguous sidebar parent header: $Title" }
            if ($matches.Count -eq 1 -and $matches[0].Groups['State'].Value -notmatch '\[offscreen\]') {
                $matches[0].Groups['Selector'].Value
            }
        }
        function Wait-SidebarHeader {
            param([string]$Title, [switch]$Hidden)
            Wait-Until -TimeoutSec 15 -Because "sidebar parent visibility for $Title (hidden=$Hidden)" -Condition {
                [bool](Get-SidebarHeader -Title $Title) -ne [bool]$Hidden
            } | Out-Null
        }
        function Open-SidebarContextMenu {
            param([string]$Title)
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            Wait-SidebarHeader -Title $Title
            Invoke-UiClick -App $script:app -Selector (Get-SidebarHeader -Title $Title) -Right | Out-Null
            Wait-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
        }
        function Invoke-SidebarKeepRunning {
            param([string]$Title, [bool]$Enable)
            Open-SidebarContextMenu -Title $Title
            (Get-UiElement -App $script:app -Selector KeepTabRunningMenuItem).name |
                Should -Be $(if ($Enable) { 'Turn on headless mode' } else { 'Turn off headless mode' })
            Invoke-UiElement -App $script:app -Selector KeepTabRunningMenuItem | Out-Null
            Open-SidebarContextMenu -Title $Title
            (Get-UiElement -App $script:app -Selector KeepTabRunningMenuItem).name |
                Should -Be $(if ($Enable) { 'Turn off headless mode' } else { 'Turn on headless mode' })
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
        }
        function Assert-SidebarRowFields {
            param([AllowEmptyString()][string]$Fields)
            foreach ($field in $script:rowFieldSelectors.Keys) {
                (Get-UiElement -App $script:app -Selector $script:rowFieldSelectors[$field]).toggleState |
                    Should -Be $(if ($field -in ($Fields -split ',')) { 'on' } else { 'off' })
            }
        }
        function Assert-SidebarSchema {
            param($Event, [string]$Field, [string]$Type = 'UInt32')
            $Event.Provider | Should -Be $script:appProvider
            $Event.ProcessId | Should -Be $script:app.Pid
            $Event.Types.PartA_PrivTags | Should -Match 'UInt64$'
            $startup = @($script:records | Where-Object { $_.Provider -eq $script:appProvider -and $_.Name -eq 'AppCreated' })[0]
            $Event.Fields.PartA_PrivTags | Should -Be $startup.Fields.PartA_PrivTags
            $expected = @('PartA_PrivTags')
            if ($Field) {
                $expected += $Field
                $Event.Types[$Field] | Should -Match ($Type + '$')
            }
            if ($Event.Name -eq 'SidebarRowFieldsChanged') {
                $expected += 'Source'
                $Event.Types.Source | Should -Match 'AnsiString$'
            }
            @($Event.Fields.Keys | Sort-Object) | Should -Be @($expected | Sort-Object)
        }

        $trace = Start-TestTelemetryTrace -Directory (Join-Path $script:root 'capture')
        try {
            $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = 'en-US'; tabLayout = 'horizontal'; confirmOnClose = 'never'
                firstWindowPreference = 'defaultProfile'; startupActions = ''; windowingBehavior = 'useNew'
                acpAgent = 'custom:sidebar-fixture'; acpCustomCommand = "pwsh -NoProfile -EncodedCommand $encoded"
                acpModel = ''; autoErrorDetectionEnabled = $false; autoFixEnabled = $false
            }
            $script:app.Launched | Should -BeTrue
            Save-TelemetryOwnedProcesses -App $script:app
            Sync-SidebarWindow
            @{
                package = $script:target.Package; installLocation = $script:target.InstallLocation
                appPid = $script:app.Pid; appSha256 = $env:ITE2E_EXPECTED_APP_SHA256
                wtaSha256 = $env:ITE2E_EXPECTED_WTA_SHA256
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'package.json')
            $script:shell = Get-ActivePane -App $script:app
            $firstHelper = Wait-TelemetryOnlyOwnedAgent -App $script:app
            Wait-AgentReady -App $script:app -PaneSessionId $firstHelper.PaneSessionId -TimeoutSec 40 | Should -BeTrue
            $script:privateAgentSessionId = (Get-AgentPaneSession -App $script:app -PaneSessionId $firstHelper.PaneSessionId).AcpSessionId
            $script:privateAgentSessionId | Should -Not -BeNullOrEmpty
            Invoke-TelemetryPhase -Name session-id-privacy -Action {
                $marker = 'TELEMETRY_CHAT_' + [guid]::NewGuid().ToString('N')
                Send-AgentPrompt -App $script:app -PaneSessionId $firstHelper.PaneSessionId -Text $marker | Out-Null
                Assert-AgentPaneText -App $script:app -PaneSessionId $firstHelper.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 20
                Wait-Until -TimeoutSec 20 -Because 'the real ACP prompt completes' -Condition {
                    (Get-Content -LiteralPath $log -Raw).Contains("telemetry-chat-complete|$($script:privateAgentSessionId)|$marker")
                } | Out-Null
                Start-Sleep -Seconds 1
            }
            Set-WtSetting -App $script:app -Key tabLayout -Value vertical | Out-Null
            Sync-SidebarWindow
            Wait-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            Test-UiElementEnabled -App $script:app -Selector SearchTabsButton | Should -BeTrue
            Get-UiTree -App $script:app -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'initial-sidebar.txt')
            Invoke-TelemetryPhase -Name second-window -Action { Invoke-TelemetrySecondWindow }
            if ($script:phaseErrors.ContainsKey('second-window')) { throw $script:phaseErrors['second-window'] }
            $script:titleA = 'IT-sidebar-A-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
            $script:titleB = 'IT-sidebar-B-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
            $existingHelpers = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
            $script:tabA = New-WtTab -App $script:app -Command 'pwsh -NoProfile' -Title $script:titleA
            $helperA = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $existingHelpers -TimeoutSec 40
            @(Get-AgentPaneSessions -App $script:app | Where-Object PaneSessionId -notin $existingHelpers) | Should -HaveCount 1
            Wait-AgentReady -App $script:app -PaneSessionId $helperA.PaneSessionId -TimeoutSec 40 | Should -BeTrue
            $existingHelpers = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
            $script:tabB = New-WtTab -App $script:app -Command 'pwsh -NoProfile' -Title $script:titleB
            $helperB = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $existingHelpers -TimeoutSec 40
            @(Get-AgentPaneSessions -App $script:app | Where-Object PaneSessionId -notin $existingHelpers) | Should -HaveCount 1
            Wait-AgentReady -App $script:app -PaneSessionId $helperB.PaneSessionId -TimeoutSec 40 | Should -BeTrue
            $script:helperB = $helperB
            Split-WtPane -App $script:app -SessionId $script:tabA.session_id -Direction right -Size 0.4 -Command 'pwsh -NoProfile' | Out-Null
            Set-WtPaneFocus -App $script:app -SessionId $script:tabA.session_id
            Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
            $script:tabCount = @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId)).Count
            $script:tabCount | Should -Be 3

            Invoke-TelemetryPhase -Name pre-reattach-prompt -Action {
                $script:preReattachAgentSessionId = (Get-AgentPaneSession -App $script:app -PaneSessionId $script:helperB.PaneSessionId).AcpSessionId
                $script:preReattachAgentSessionId | Should -Not -BeNullOrEmpty
                $marker = 'TELEMETRY_CHAT_' + [guid]::NewGuid().ToString('N')
                Send-AgentPrompt -App $script:app -PaneSessionId $script:helperB.PaneSessionId -Text $marker | Out-Null
                Assert-AgentPaneText -App $script:app -PaneSessionId $script:helperB.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 20
                Wait-Until -TimeoutSec 20 -Because 'the retained ACP session completes its first user prompt' -Condition {
                    (Get-Content -LiteralPath $log -Raw).Contains("telemetry-chat-complete|$($script:preReattachAgentSessionId)|$marker")
                } | Out-Null
                Start-Sleep -Seconds 1
            }
            function Invoke-OtherSidebarScenarios {
                Invoke-TelemetryPhase -Name fields-menu-only -Action {
                    Invoke-UiElement -App $script:app -Selector FilterTabsButton | Out-Null
                    Assert-SidebarRowFields -Fields 'agentStatus,workingDirectory'
                    Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
                }
                Invoke-TelemetryPhase -Name fields-third-disabled -Action {
                    Invoke-UiElement -App $script:app -Selector FilterTabsButton | Out-Null
                    Test-UiElementEnabled -App $script:app -Selector RichTabRepositoryVisibleItem | Should -BeFalse
                    Invoke-UiClick -App $script:app -Selector RichTabRepositoryVisibleItem | Out-Null
                    Assert-SidebarRowFields -Fields 'agentStatus,workingDirectory'
                    Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
                }
                foreach ($case in $script:rowFieldCases) {
                    Invoke-TelemetryPhase -Name $case.Phase -Action {
                        Invoke-UiElement -App $script:app -Selector FilterTabsButton | Out-Null
                        Test-UiElementEnabled -App $script:app -Selector $case.Selector | Should -BeTrue
                        Invoke-UiElement -App $script:app -Selector $case.Selector | Out-Null
                        Invoke-UiElement -App $script:app -Selector FilterTabsButton | Out-Null
                        Assert-SidebarRowFields -Fields $case.Fields
                        Get-UiTree -App $script:app -Depth 8 |
                            Set-Content -LiteralPath (Join-Path $script:root "$($case.Phase).txt")
                        Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
                    }
                    if ($case.Phase -eq 'fields-repository-branch') {
                        Invoke-TelemetryPhase -Name fields-session-start -Action {
                            $existing = @(Get-AgentPaneSessions -App $script:app).PaneSessionId
                            $tab = New-WtTab -App $script:app -Command 'pwsh -NoProfile' -Title 'IT-sidebar-start-snapshot'
                            $helper = $null
                            try {
                                $helper = Wait-NewAgentPaneSession -App $script:app -ExcludePaneSessionId $existing -TimeoutSec 40
                                Wait-AgentReady -App $script:app -PaneSessionId $helper.PaneSessionId -TimeoutSec 40 | Should -BeTrue
                                Save-TelemetryOwnedProcesses -App $script:app
                            }
                            finally {
                                if ($helper) {
                                    Close-WtPane -App $script:app -SessionId $helper.PaneSessionId
                                    Wait-Until -TimeoutSec 20 -Because 'the temporary session helper retires' -Condition {
                                        -not (Get-Process -Id $helper.HelperProcessId -ErrorAction SilentlyContinue)
                                    } | Out-Null
                                }
                                Close-WtPane -App $script:app -SessionId $tab.session_id
                                Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
                            }
                        }
                    }
                }
                Invoke-TelemetryPhase -Name search-open -Action {
                    Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
                    Wait-UiElement -App $script:app -Selector SearchTextBox | Out-Null
                }
                Invoke-TelemetryPhase -Name search-edit -Action {
                    Set-SidebarQuery -Text $script:titleB
                    Wait-SidebarHeader -Title $script:titleB
                    Wait-SidebarHeader -Title $script:titleA -Hidden
                    Set-SidebarQuery -Text 'IT-sidebar-'
                    Wait-SidebarHeader -Title $script:titleA
                }
                Invoke-TelemetryPhase -Name search-close -Action {
                    Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
                    Wait-UiElement -App $script:app -Selector SearchTextBox -Gone | Out-Null
                }
                Invoke-TelemetryPhase -Name search-reopen -Action {
                    Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
                    Wait-UiElement -App $script:app -Selector SearchTextBox | Out-Null
                }
                Invoke-TelemetryPhase -Name filter-all -Action {
                    $count = Get-AgentViewRowCount
                    Set-SidebarHistory -Open $true
                    Wait-AgentViewLoaded -Count $count
                    @{ Count = $count }
                }
                Invoke-TelemetryPhase -Name history-refresh -Action {
                    Wait-AgentViewLoaded -Count $script:phases['filter-all'].Data.Count
                    Start-Sleep -Seconds 7
                }
                Invoke-TelemetryPhase -Name filter-search-edit -Action {
                    $query = 'no-session-' + [guid]::NewGuid().ToString('N')
                    Set-SidebarQuery -Text $query
                    (Get-UiValue -App $script:app -Selector SearchTextBox) | Should -Be $query
                    try {
                        Wait-Until -TimeoutSec 20 -Because 'the filtered agent view renders its empty-result message after refresh' -Condition {
                            (Get-UiTree -App $script:app -Depth 12) -match
                                '(?m)^\s*(?:HistoryMessage|lbl-historymessage-\S+) Text "No (?:matching agent sessions|agent sessions found)\."'
                        } | Out-Null
                    }
                    finally {
                        Get-UiTree -App $script:app -Depth 12 |
                            Set-Content -LiteralPath (Join-Path $script:root 'filter-search-edit-ui.txt')
                    }
                }
                Invoke-TelemetryPhase -Name filter-live-search -Action {
                    Set-SidebarHistory -Open $false
                    Add-AgentViewFixtures
                    Set-SidebarQuery -Text $script:titleA
                    Wait-SidebarHeader -Title $script:titleA
                    $count = Get-AgentViewRowCount
                    $count | Should -BeGreaterOrEqual 2
                    Set-SidebarHistory -Open $true
                    Wait-AgentViewLoaded -Count $count
                    Get-UiTree -App $script:app -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'agent-view.txt')
                    @{ Count = $count }
                }
                Invoke-TelemetryPhase -Name filter-live-no-match -Action {
                    Set-SidebarHistory -Open $false
                    Set-SidebarQuery -Text 'IT-sidebar-no-match'
                    Wait-SidebarHeader -Title $script:titleA -Hidden
                    Wait-SidebarHeader -Title $script:titleB -Hidden
                    $count = Get-AgentViewRowCount
                    Set-SidebarHistory -Open $true
                    Wait-AgentViewLoaded -Count $count
                    @{ Count = $count }
                }
                Set-SidebarHistory -Open $false
                Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            }
            if (-not $KeepRunningOnly) {
                Invoke-OtherSidebarScenarios
                foreach ($phase in @('tab-order-pin', 'tab-order-unpin')) {
                    Invoke-TelemetryPhase -Name $phase -Action {
                        Open-SidebarContextMenu -Title $script:titleA
                        Invoke-UiElement -App $script:app -Selector PinTabMenuItem | Out-Null
                        Wait-SidebarHeader -Title $script:titleA
                        Start-Sleep -Milliseconds 300
                    }
                }
            }
            Invoke-TelemetryPhase -Name pin-first -Action {
                Invoke-SidebarKeepRunning -Title $script:titleA -Enable $true
            }
            Invoke-TelemetryPhase -Name pin-second-hidden-first -Action {
                Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
                Set-SidebarQuery -Text $script:titleB
                Wait-SidebarHeader -Title $script:titleA -Hidden
                Invoke-SidebarKeepRunning -Title $script:titleB -Enable $true
            }
            Invoke-TelemetryPhase -Name unpin -Action {
                Invoke-SidebarKeepRunning -Title $script:titleB -Enable $false
            }
            Invoke-TelemetryPhase -Name repin -Action {
                Invoke-SidebarKeepRunning -Title $script:titleB -Enable $true
            }
            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            Invoke-TelemetryPhase -Name retain-restore -Action {
                $before = Get-WtPaneStatus -App $script:app -SessionId $script:tabB.session_id
                $script:restoredAgentSessionId = (Get-AgentPaneSession -App $script:app -PaneSessionId $script:helperB.PaneSessionId).AcpSessionId
                $script:restoredAgentSessionId | Should -Be $script:preReattachAgentSessionId
                Open-SidebarContextMenu -Title $script:titleB
                Invoke-UiElement -App $script:app -Selector 'Close tab' | Out-Null
                Wait-Until -TimeoutSec 15 -Because 'the kept tab detaches' -Condition {
                    $window = @(Get-WtWindows -App $script:app | Where-Object { [string]$_.window_id -eq [string]$script:app.WindowId })
                    $window.Count -eq 1 -and $window[0].tab_count -eq ($script:tabCount - 1)
                } | Out-Null
                Wait-SidebarHeader -Title $script:titleB -Hidden
                Set-WtPaneFocus -App $script:app -SessionId $script:tabB.session_id
                Wait-SidebarHeader -Title $script:titleB
                (Get-WtPaneStatus -App $script:app -SessionId $script:tabB.session_id).pid | Should -Be $before.pid
                (Get-AgentPaneSession -App $script:app -PaneSessionId $script:helperB.PaneSessionId).AcpSessionId |
                    Should -Be $script:restoredAgentSessionId
                @(Get-WtWindows -App $script:app | Where-Object { [string]$_.window_id -eq [string]$script:app.WindowId })[0].tab_count |
                    Should -Be $script:tabCount
                Invoke-SidebarKeepRunning -Title $script:titleB -Enable $false
                Invoke-SidebarKeepRunning -Title $script:titleA -Enable $false
            }
            Invoke-TelemetryPhase -Name reattach-prompt -Action {
                $marker = 'TELEMETRY_CHAT_' + [guid]::NewGuid().ToString('N')
                Send-AgentPrompt -App $script:app -PaneSessionId $script:helperB.PaneSessionId -Text $marker | Out-Null
                Assert-AgentPaneText -App $script:app -PaneSessionId $script:helperB.PaneSessionId -Pattern "ACK:$marker" -TimeoutSec 20
                Wait-Until -TimeoutSec 20 -Because 'the retained ACP session completes a prompt after reattachment' -Condition {
                    (Get-Content -LiteralPath $log -Raw).Contains("telemetry-chat-complete|$($script:restoredAgentSessionId)|$marker")
                } | Out-Null
                Start-Sleep -Seconds 1
            }
            Invoke-TelemetryPhase -Name layout-refresh -Action {
                Set-WtSetting -App $script:app -Key tabLayout -Value horizontal | Out-Null
                Sync-SidebarWindow
                $transition = Wait-Until -TimeoutSec 30 -Because 'layout reload completes or explicitly requests restart' -Condition {
                    $restart = Get-UiElement -App $script:app -Selector TabLayoutRestartInfoBar
                    if ($restart -and -not $restart.isOffscreen) { 'restartRequired' }
                    elseif (-not (Get-UiElement -App $script:app -Selector SearchTabsButton)) { 'horizontal' }
                }
                Set-WtSetting -App $script:app -Key tabLayout -Value vertical | Out-Null
                Sync-SidebarWindow
                Wait-Until -TimeoutSec 30 -Because 'the vertical sidebar is restored after the layout setting reload' -Condition {
                    $button = Get-UiElement -App $script:app -Selector SearchTabsButton
                    $button -and -not $button.isOffscreen
                } | Out-Null
                Start-Sleep -Seconds 1
                @{ Transition = $transition }
            }
            Save-TelemetryOwnedProcesses -App $script:app
        }
        finally {
            try {
                if ($script:app) {
                    Get-UiTree -App $script:app -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'final-ui.txt')
                }
            }
            finally { Stop-TestTelemetryTrace -Trace $trace }
        }
        $script:records = @(Read-TestTelemetryTrace -Directory $trace.Directory -ProcessIds @($script:ownedPids) `
            -IncludeEventName @('AppCreated', 'AgentSessionStarted', 'AcpNewSessionComplete', 'AgentPromptSent', 'AgentResponseFirstToken', 'AgentResponseComplete', 'SidebarStateOnLaunch', 'SidebarSearchOpened', 'SidebarAgentFilterApplied', 'SidebarTabPinned', 'SidebarRowFieldsChanged', 'TabPinChanged', 'KeepRunningMarked', 'KeepRunningDetached', 'KeepRunningReattachStarted', 'KeepRunningReattached'))
        Initialize-TelemetryPhaseClock -CaptureDirectory $trace.Directory
        ConvertTo-Json -InputObject $script:records -Depth 12 | Set-Content -LiteralPath (Join-Path $script:root 'scoped-events.json')
        [xml]$raw = Get-Content -LiteralPath (Join-Path $trace.Directory 'events.xml') -Raw
        $script:metadata = @(
            foreach ($event in $raw.SelectNodes("//*[local-name()='Event']")) {
                $system = $event.SelectSingleNode("*[local-name()='System']")
                if (-not $system) { continue }
                $provider = $system.SelectSingleNode("*[local-name()='Provider']")
                $execution = $system.SelectSingleNode("*[local-name()='Execution']")
                if ($provider.GetAttribute('Guid').Trim('{}') -ne $script:appProvider -or
                    [int]$execution.GetAttribute('ProcessID') -ne $script:app.Pid) { continue }
                $name = $event.SelectSingleNode("*[local-name()='RenderingInfo']/*[local-name()='Task']").InnerText
                if ($name -notin @('AppCreated', 'SidebarSearchOpened', 'SidebarAgentFilterApplied', 'SidebarTabPinned', 'SidebarRowFieldsChanged', 'TabPinChanged', 'KeepRunningMarked', 'KeepRunningDetached', 'KeepRunningReattachStarted', 'KeepRunningReattached')) { continue }
                [pscustomobject]@{
                    Name = $name
                    Level = $system.SelectSingleNode("*[local-name()='Level']").InnerText
                    Keywords = $system.SelectSingleNode("*[local-name()='Keywords']").InnerText
                }
            }
        )
        $script:metadata | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'event-metadata.json')
    }

    AfterAll {
        if ($script:app -and $script:app.Launched) {
            # Retire fixture sessions while their COM host is still available.
            # Otherwise late session-close notifications can activate a headless host.
            $helpers = @(Get-AgentPaneSessions -App $script:app)
            foreach ($helper in $helpers) {
                [int]$helper.HelperProcessId | Should -BeIn @($script:ownedPids)
                Close-WtPane -App $script:app -SessionId $helper.PaneSessionId
            }
            Wait-Until -TimeoutSec 20 -Because 'owned fixture helpers exit before their host' -Condition {
                @($helpers | Where-Object { Get-Process -Id $_.HelperProcessId -ErrorAction SilentlyContinue }).Count -eq 0
            } | Out-Null
            # Retire shell-hook bindings before the final window closes, too.
            foreach ($pane in $script:fixturePanes) {
                $pane | Should -Not -Be $script:shell.session_id
                Close-WtPane -App $script:app -SessionId $pane
            }
            Start-Sleep -Seconds 2
            Stop-Terminal -App $script:app -RestoreSettings $false
        }
        if ($script:target -and $script:originalHashes) {
            if (@(Get-WtProcessesForApp -App $script:target).Count) {
                throw 'Dev remains active; retain configuration backups rather than overwrite live settings.'
            }
            Restore-WtConfig -App $script:target
            foreach ($path in $script:originalHashes.Keys) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                $actual | Should -Be $script:originalHashes[$path]
            }
        }
    }

    It 'Agent telemetry excludes provider session identifiers' {
        foreach ($name in @('AgentSessionStarted', 'AcpNewSessionComplete', 'AgentPromptSent', 'AgentResponseFirstToken', 'AgentResponseComplete')) {
            $events = @($script:records | Where-Object Name -eq $name)
            $events.Count | Should -BeGreaterThan 0 -Because "$name must be captured from the real App/WTA path"
            foreach ($event in $events) {
                if ($name -in @('AgentPromptSent', 'AgentResponseComplete')) {
                    [guid]::Parse($event.Fields.SessionId) | Should -Not -Be ([guid]::Empty)
                    $event.Types.SessionId | Should -Match 'AnsiString$'
                }
                else {
                    $event.Fields.Keys | Should -Not -Contain 'SessionId'
                    $event.Types.Keys | Should -Not -Contain 'SessionId'
                }
                $event.Fields.Keys | Should -Not -Contain 'session_id'
                ($event.Fields | ConvertTo-Json -Compress) | Should -Not -Match ([regex]::Escape($script:privateAgentSessionId))
            }
        }
        foreach ($name in @('AgentPromptSent', 'AgentResponseFirstToken', 'AgentResponseComplete')) {
            @(Get-TelemetryPhaseEvents -Phase session-id-privacy -Name $name) | Should -HaveCount 1
        }
    }

    It 'Sidebar startup snapshots preserve the consolidated launch contract' {
        if ($script:phaseErrors.Count) { throw ($script:phaseErrors.Values | Out-String) }
        $created = @($script:records | Where-Object { $_.Provider -eq $script:appProvider -and $_.Name -eq 'AppCreated' })
        $created | Should -HaveCount 2 -Because 'settings reload and activation must not emit window-created snapshots'
        $created[0].Fields.SidebarEnabled | Should -BeIn @('false', '0')
        $created[1].Fields.SidebarEnabled | Should -BeIn @('true', '1')
        foreach ($event in $created) {
            $event.Types.SidebarEnabled | Should -Match 'Boolean$'
            $event.Types.PartA_PrivTags | Should -Match 'UInt64$'
            @($event.Fields.Keys) | Should -HaveCount 14
        }
        $script:metadata | Should -HaveCount (19 + $script:rowFieldCases.Count + $created.Count)
        $startupMetadata = @($script:metadata | Where-Object Name -eq AppCreated)[0]
        foreach ($metadata in $script:metadata) {
            [int]$metadata.Level | Should -Be 5
            $metadata.Keywords | Should -Be $startupMetadata.Keywords
        }
        @($script:records | Where-Object Name -eq SidebarStateOnLaunch) | Should -HaveCount 0
    }

    It 'Sidebar search telemetry counts opening rather than editing' {
        foreach ($phase in @('search-open', 'search-reopen', 'pin-second-hidden-first')) {
            $events = @(Get-TelemetryPhaseEvents -Phase $phase -Name SidebarSearchOpened -Provider $script:appProvider)
            $events | Should -HaveCount 1
            Assert-SidebarSchema -Event $events[0]
        }
        foreach ($phase in @('search-edit', 'search-close', 'filter-search-edit', 'layout-refresh')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name SidebarSearchOpened) | Should -HaveCount 0
        }
        @($script:records | Where-Object Name -eq SidebarSearchOpened) | Should -HaveCount 3
    }

    It 'Sidebar filter telemetry counts loaded agent session rows' {
        foreach ($phase in @('filter-all', 'filter-live-search', 'filter-live-no-match')) {
            $events = @(Get-TelemetryPhaseEvents -Phase $phase -Name SidebarAgentFilterApplied -Provider $script:appProvider)
            $events | Should -HaveCount 1
            Assert-SidebarSchema -Event $events[0] -Field row_count
            [uint32]$events[0].Fields.row_count | Should -Be $script:phases[$phase].Data.Count
        }
        foreach ($phase in @('history-refresh', 'filter-search-edit', 'layout-refresh')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name SidebarAgentFilterApplied) | Should -HaveCount 0
        }
        @($script:records | Where-Object Name -eq SidebarAgentFilterApplied) | Should -HaveCount 3
    }

    It 'Sidebar pin telemetry counts explicit keep-running opt-ins' {
        foreach ($case in @(@{ Phase = 'pin-first'; Count = 1 }, @{ Phase = 'pin-second-hidden-first'; Count = 2 }, @{ Phase = 'repin'; Count = 2 })) {
            $events = @(Get-TelemetryPhaseEvents -Phase $case.Phase -Name SidebarTabPinned -Provider $script:appProvider)
            $events | Should -HaveCount 1
            Assert-SidebarSchema -Event $events[0] -Field pinned_count
            [uint32]$events[0].Fields.pinned_count | Should -Be $case.Count
        }
        foreach ($phase in @('unpin', 'retain-restore', 'layout-refresh')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name SidebarTabPinned) | Should -HaveCount 0
        }
        @($script:records | Where-Object Name -eq SidebarTabPinned) | Should -HaveCount 3
    }

    It 'Keep-running telemetry correlates opt-in, retention, and live reattachment' {
        $ids = @()
        foreach ($case in @(@{ Phase = 'pin-first'; Count = 1 }, @{ Phase = 'pin-second-hidden-first'; Count = 2 }, @{ Phase = 'repin'; Count = 2 })) {
            $events = @(Get-TelemetryPhaseEvents -Phase $case.Phase -Name KeepRunningMarked -Provider $script:appProvider)
            $events | Should -HaveCount 1
            $event = $events[0]
            @($event.Fields.Keys | Sort-Object) | Should -Be @('HasAgentPane', 'KeepId', 'KeepRunningTabCount', 'PartA_PrivTags', 'TotalTabCount')
            $event.Types.KeepId | Should -Match 'UnicodeString$'
            $event.Types.HasAgentPane | Should -Match 'Boolean$'
            $event.Types.TotalTabCount | Should -Match 'UInt32$'
            $event.Types.KeepRunningTabCount | Should -Match 'UInt32$'
            [uint32]$event.Fields.TotalTabCount | Should -Be $script:tabCount
            [uint32]$event.Fields.KeepRunningTabCount | Should -Be $case.Count
            $event.Fields.KeepId | Should -Match '^\{[0-9a-fA-F-]{36}\}$'
            $event.Fields.HasAgentPane | Should -BeIn @('true', '1')
            $ids += $event.Fields.KeepId
        }
        @($ids | Select-Object -Unique) | Should -HaveCount 3
        foreach ($phase in @('unpin', 'retain-restore', 'layout-refresh')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name KeepRunningMarked) | Should -HaveCount 0
        }
        $detached = @(Get-TelemetryPhaseEvents -Phase retain-restore -Name KeepRunningDetached -Provider $script:appProvider)
        $reattached = @(Get-TelemetryPhaseEvents -Phase retain-restore -Name KeepRunningReattached -Provider $script:appProvider)
        $started = @(Get-TelemetryPhaseEvents -Phase retain-restore -Name KeepRunningReattachStarted -Provider $script:appProvider)
        $started | Should -HaveCount 1
        $detached | Should -HaveCount 1
        $reattached | Should -HaveCount 1
        @($detached[0].Fields.Keys | Sort-Object) | Should -Be @('HasAgentPane', 'KeepId', 'PartA_PrivTags')
        @($reattached[0].Fields.Keys | Sort-Object) | Should -Be @('AttemptId', 'HasAgentPane', 'HasAgentSession', 'KeepId', 'Outcome', 'PartA_PrivTags')
        $reattached[0].Fields.AttemptId | Should -Be $started[0].Fields.AttemptId
        [guid]::Parse($started[0].Fields.AttemptId) | Should -Not -Be ([guid]::Empty)
        $started[0].Types.AttemptId | Should -Match 'UnicodeString$'
        $reattached[0].Types.HasAgentSession | Should -Match 'Boolean$'
        $reattached[0].Fields.HasAgentSession | Should -BeIn @('true', '1')
        $detached[0].Types.KeepId | Should -Match 'UnicodeString$'
        $reattached[0].Types.Outcome | Should -Match 'AnsiString$'
        $detached[0].Fields.KeepId | Should -Be $ids[2]
        $reattached[0].Fields.KeepId | Should -Be $ids[2]
        $reattached[0].Fields.Outcome | Should -BeExactly 'live'
        @($script:records | Where-Object Name -eq KeepRunningMarked) | Should -HaveCount 3
        @($script:records | Where-Object Name -eq KeepRunningDetached) | Should -HaveCount 1
        @($script:records | Where-Object Name -eq KeepRunningReattached) | Should -HaveCount 1
    }

    It 'Restored agent prompt telemetry identifies the surviving ACP session' {
        $before = @(Get-TelemetryPhaseEvents -Phase session-id-privacy -Name AgentPromptSent -Provider $script:wtaProvider)
        $beforeRetained = @(Get-TelemetryPhaseEvents -Phase pre-reattach-prompt -Name AgentPromptSent -Provider $script:wtaProvider)
        $after = @(Get-TelemetryPhaseEvents -Phase reattach-prompt -Name AgentPromptSent -Provider $script:wtaProvider)
        $before | Should -HaveCount 1
        $beforeRetained | Should -HaveCount 1
        $after | Should -HaveCount 1
        $before[0].Fields.Reattached | Should -BeIn @('false', '0')
        $beforeRetained[0].Fields.Reattached | Should -BeIn @('false', '0')
        $after[0].Fields.Reattached | Should -BeIn @('true', '1')
        $before[0].Fields.UserPromptOrdinal | Should -BeExactly 'First'
        $beforeRetained[0].Fields.UserPromptOrdinal | Should -BeExactly 'First'
        $after[0].Fields.UserPromptOrdinal | Should -BeExactly 'Second'
        $beforeRetained[0].ProcessId | Should -Be $after[0].ProcessId
        $after[0].Fields.IsAutofix | Should -BeIn @('false', '0')
        $after[0].Types.Reattached | Should -Match 'Boolean$'
        $after[0].Types.UserPromptOrdinal | Should -Match 'AnsiString$'
        @($script:records | Where-Object Name -eq AgentPromptSent) | Should -HaveCount 3
        $after[0].Fields.SessionId | Should -Be $beforeRetained[0].Fields.SessionId
        $after[0].Fields.SessionId | Should -Not -Be $before[0].Fields.SessionId
        $after[0].Fields.TurnId | Should -Not -Be $beforeRetained[0].Fields.TurnId
        $restore = @(Get-TelemetryPhaseEvents -Phase retain-restore -Name KeepRunningReattached)[0]
        $after[0].Fields.KeepId | Should -Be $restore.Fields.KeepId
        $after[0].Fields.AttemptId | Should -Be $restore.Fields.AttemptId
        $beforeRetained[0].Fields.KeepId | Should -BeNullOrEmpty
        foreach ($phase in @('session-id-privacy', 'pre-reattach-prompt', 'reattach-prompt')) {
            $sent = @(Get-TelemetryPhaseEvents -Phase $phase -Name AgentPromptSent)[0]
            $completed = @(Get-TelemetryPhaseEvents -Phase $phase -Name AgentResponseComplete | Where-Object { $_.Fields.TurnId -eq $sent.Fields.TurnId })
            $completed | Should -HaveCount 1
            $completed[0].Fields.SessionId | Should -Be $sent.Fields.SessionId
            $completed[0].Fields.IsAutofix | Should -BeIn @('false', '0')
        }
        foreach ($phase in @('pin-first', 'pin-second-hidden-first', 'unpin', 'repin', 'retain-restore')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name AgentPromptSent) | Should -HaveCount 0
        }
    }

    It 'Sidebar row-field telemetry reports only selected field identifiers' {
        foreach ($case in $script:rowFieldCases) {
            $events = @(Get-TelemetryPhaseEvents -Phase $case.Phase -Name SidebarRowFieldsChanged -Provider $script:appProvider)
            $events | Should -HaveCount 1
            Assert-SidebarSchema -Event $events[0] -Field fields -Type AnsiString
            $events[0].Fields.fields | Should -BeExactly $case.Fields
            $events[0].Fields.Source | Should -BeExactly 'UserChange'
        }
        foreach ($phase in @('fields-menu-only', 'fields-third-disabled', 'history-refresh', 'filter-live-search', 'layout-refresh', 'retain-restore')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name SidebarRowFieldsChanged) | Should -HaveCount 0
        }
        $launches = @($script:records | Where-Object Name -eq AppCreated)
        @($script:records | Where-Object Name -eq SidebarRowFieldsChanged) | Should -HaveCount ($script:rowFieldCases.Count + $launches.Count)
    }

    It 'Sidebar row-field telemetry separates window launch from user changes' {
        $launches = @($script:records | Where-Object Name -eq AppCreated)
        $snapshots = @($script:records | Where-Object { $_.Name -eq 'SidebarRowFieldsChanged' -and $_.Fields.Source -eq 'Launch' })
        $snapshots | Should -HaveCount $launches.Count
        foreach ($snapshot in $snapshots) {
            Assert-SidebarSchema -Event $snapshot -Field fields -Type AnsiString
            $snapshot.Fields.fields | Should -BeExactly 'agentStatus,workingDirectory'
        }
        @(Get-TelemetryPhaseEvents -Phase fields-session-start -Name AgentSessionStarted) | Should -HaveCount 1
        @(Get-TelemetryPhaseEvents -Phase fields-session-start -Name SidebarRowFieldsChanged) | Should -HaveCount 0
    }

    It 'Tab-order pin telemetry remains distinct from keep-running opt-ins' {
        foreach ($case in @(@{ Phase = 'tab-order-pin'; Pinned = $true; Count = 1 }, @{ Phase = 'tab-order-unpin'; Pinned = $false; Count = 0 })) {
            $events = @(Get-TelemetryPhaseEvents -Phase $case.Phase -Name TabPinChanged)
            $events | Should -HaveCount 1
            $events[0].Types.Pinned | Should -Match 'Boolean$'
            $events[0].Types.PinnedCount | Should -Match 'UInt32$'
            $events[0].Fields.Pinned | Should -BeIn $(if ($case.Pinned) { @('true', '1') } else { @('false', '0') })
            [uint32]$events[0].Fields.PinnedCount | Should -Be $case.Count
            @(Get-TelemetryPhaseEvents -Phase $case.Phase -Name KeepRunningMarked) | Should -HaveCount 0
        }
        foreach ($phase in @('pin-first', 'pin-second-hidden-first', 'unpin', 'repin', 'retain-restore')) {
            @(Get-TelemetryPhaseEvents -Phase $phase -Name TabPinChanged) | Should -HaveCount 0
        }
    }
}
