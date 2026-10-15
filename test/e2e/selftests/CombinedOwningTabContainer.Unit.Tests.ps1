#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:suite = Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'
    $script:suiteAst = [Management.Automation.Language.Parser]::ParseFile(
        $script:suite, [ref]$null, [ref]$null)
    $script:originalHistoryMode = $env:ITE2E_HISTORY_INDICATORS_ONLY
    function Get-CombinedModeAssignment([string]$Name) {
        $node = $script:suiteAst.Find({ param($node)
            $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                $node.Left -is [Management.Automation.Language.VariableExpressionAst] -and
                $node.Left.VariablePath.UserPath -eq $Name
        }, $true)
        $node | Should -Not -BeNullOrEmpty
        $node.Extent.Text
    }
}

Describe 'Combined owning-tab container selection' -Tag 'Unit' {
    AfterAll { $env:ITE2E_HISTORY_INDICATORS_ONLY = $script:originalHistoryMode }

    It 'settles exact search values without replay and fails closed for <Fault>' -ForEach @(
        @{ Fault = 'missing then ready' }, @{ Fault = 'old then ready' }, @{ Fault = 'empty active' },
        @{ Fault = 'missing' }, @{ Fault = 'duplicate' }, @{ Fault = 'wrong PID' },
        @{ Fault = 'offscreen' }, @{ Fault = 'outside' }, @{ Fault = 'toggle off' },
        @{ Fault = 'case mismatch' }, @{ Fault = 'read failure' }, @{ Fault = 'unsupported' }
    ) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        Add-Type -AssemblyName WindowsBase
        $node = $script:suiteAst.Find({ param($n)
            $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Get-CombinedQuery'
        }, $true)
        $node.Extent.Text | Should -Match 'Wait-Until -TimeoutSec 5'
        $node.Extent.Text | Should -Not -Match 'Open-CombinedSearch|Set-UiValue|Get-UiValue'
        $source = $node.Extent.Text.Replace(
            '[Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))', '$testWindow')
        . ([scriptblock]::Create($source))
        $originalApp = $script:app
        try {
            $script:app = @{ Pid = 123; Hwnd = 1 }
            $testWindow = @{ Current = @{ ProcessId = 123; BoundingRectangle = [Windows.Rect]::new(0, 0, 100, 100) } }
            $value = if ($Fault -eq 'empty active') { '' } else { 'ExactQuery' }
            $search = [pscustomobject]@{
                Current = @{ AutomationId = 'SearchTextBox'; ProcessId = 123; IsOffscreen = $false;
                    BoundingRectangle = [Windows.Rect]::new(1, 1, 50, 10) }
                Pattern = @{ Current = @{ Value = $value } }
            }
            $search | Add-Member ScriptMethod GetCurrentPattern {
                param($pattern)
                if ($Fault -eq 'unsupported') { throw 'ValuePattern unsupported' }
                $this.Pattern
            }
            $button = [pscustomobject]@{
                Current = @{ AutomationId = 'SearchTabsButton'; ProcessId = 123; IsOffscreen = $false;
                    BoundingRectangle = [Windows.Rect]::new(1, 20, 10, 10) }
                Pattern = @{ Current = @{ ToggleState = [Windows.Automation.ToggleState]::On } }
            }
            $button | Add-Member ScriptMethod GetCurrentPattern { param($pattern) $this.Pattern }
            switch ($Fault) {
                'wrong PID' { $search.Current.ProcessId = 456 }
                'offscreen' { $search.Current.IsOffscreen = $true }
                'outside' { $search.Current.BoundingRectangle = [Windows.Rect]::new(200, 200, 10, 10) }
                'toggle off' { $button.Pattern.Current.ToggleState = [Windows.Automation.ToggleState]::Off }
                'case mismatch' { $search.Pattern.Current.Value = 'exactquery' }
            }
            $attempt = @{ Count = 0 }
            function Get-CombinedRawChildren {
                param($Window)
                $attempt.Count++
                if ($Fault -eq 'read failure') { throw 'nonempty UIA read failure' }
                if ($Fault -eq 'missing' -or ($Fault -eq 'missing then ready' -and $attempt.Count -eq 1)) { return }
                if ($Fault -eq 'old then ready') { $search.Pattern.Current.Value = if ($attempt.Count -eq 1) { 'old' } else { $value } }
                $search; $button
                if ($Fault -eq 'duplicate') { $search }
            }
            function Wait-Until {
                param($TimeoutSec, $Because, $Condition)
                $TimeoutSec | Should -Be 5
                foreach ($i in 1..3) {
                    try { if (& $Condition) { return $true } } catch {}
                }
                throw 'bounded timeout'
            }
            if ($Fault -in @('missing then ready', 'old then ready', 'empty active')) {
                Get-CombinedQuery $value | Should -BeExactly $value
                $attempt.Count | Should -Be $(if ($Fault -eq 'empty active') { 1 } else { 2 })
            } elseif ($Fault -in @('read failure', 'unsupported')) {
                { Get-CombinedQuery $value } | Should -Throw $(if ($Fault -eq 'unsupported') { '*ValuePattern unsupported*' } else { '*nonempty UIA read failure*' })
                $attempt.Count | Should -Be 1
            } else {
                { Get-CombinedQuery $value } | Should -Throw '*bounded timeout*'
            }
            $setter = $script:suiteAst.Find({ param($n)
                $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -eq 'Set-CombinedQuery'
            }, $true).Extent.Text
            $setter | Should -Match '(?s)Open-CombinedSearch\s+Set-UiValue.*?Out-Null\s+Get-CombinedQuery \$Value \| Out-Null'
            ([regex]::Matches($setter, 'Set-UiValue')).Count | Should -Be 1
        }
        finally { $script:app = $originalApp }
    }

    It 'extracts owning header titles and rejects a same-count ordinary replacement' {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $originalApp = $script:app
    try {
        $script:app = @{ Pid = 123 }
        $fixtureOwnerTitles = @{ initial = 'startup'; neighbor = 'fixture-neighbor'; ordinary = 'fixture-ordinary'; owner = 'fixture' }
        $rows = @('startup', 'fixture-neighbor', 'fixture') | ForEach-Object {
            [pscustomobject]@{ Current = @{ ProcessId = 123 }; Parts = @(
                [pscustomobject]@{ Current = @{ ProcessId = 123; ControlType = [Windows.Automation.ControlType]::Text; Name = $_; AutomationId = '' } }
            ) }
        }
        function Get-CombinedRows { param($Kind) $rows }
        function Get-CombinedRawChildren { param($Row) $Row.Parts }
        $assignment = Get-CombinedModeAssignment actualOwnerTitles
        . ([scriptblock]::Create($assignment))
        $expectedOwnerTitles = @('startup', 'fixture-neighbor', 'fixture')
        $actualOwnerTitles | Should -Be $expectedOwnerTitles
        $rows[0].Parts[0].Current.Name = 'fixture-ordinary'
        . ([scriptblock]::Create($assignment))
        $actualOwnerTitles | Should -HaveCount 3
        { $actualOwnerTitles | Should -Be $expectedOwnerTitles } | Should -Throw
    }
    finally { $script:app = $originalApp }
}

    It 'installs corresponding original numeric action arguments without changing action defaults' {
        $assignment = Get-CombinedModeAssignment startupSettings
        $command = ''; . ([scriptblock]::Create($assignment))
        @($startupSettings.actions) | Should -HaveCount 0
        @($startupSettings.keybindings) | Should -HaveCount 9
        foreach ($number in 1..9) {
            $binding = $startupSettings.keybindings[$number - 1]
            $binding.keys | Should -Be "ctrl+alt+$number"
            $binding.command.action | Should -Be 'switchToTab'
            $binding.command.index | Should -Be $(if ($number -eq 9) { [uint32]4294967295 } else { $number - 1 })
        }
    }

    It 'uses a bounded positive physical matrix with all focus origins and global ordinary search' {
        $assignment = Get-CombinedModeAssignment numberCases
        . ([scriptblock]::Create($assignment))
        $numberCases | Should -HaveCount 9
        @($numberCases.Origin | Sort-Object -Unique) | Should -Be @('Helper', 'ItemsList', 'Shell')
        @($numberCases.Search | Sort-Object -Unique) | Should -Be @('Closed', 'Empty', 'Filtered', 'NoMatch', 'Ordinary')
        @($numberCases.Number | Sort-Object -Unique) | Should -Be @(1, 2, 9)
        @($numberCases | Where-Object { $_.AgentsOnly -and $_.Search -eq 'Ordinary' }) | Should -HaveCount 1
        @($numberCases | Where-Object { $_.AgentsOnly -and $_.Search -eq 'NoMatch' }).Number | Should -Be @(1, 9)
        $body = $script:suiteAst.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Assert-MoveNumberKeys'
        }, $true).Body.Extent.Text
        $body | Should -Match 'Should -Not -Contain \$expected'
        $body | Should -Match 'if \(\$number -eq 9\) \{ \$navigationTargets\[-1\] \}'
        $body | Should -Match '\$agentsOnly -and \[string\]::IsNullOrEmpty\(\$query\)'
        $body | Should -Match '@\(Get-CombinedRows Live\)\.Count \| Should -Be \$displayOwners.Count'
        (Get-CombinedModeAssignment navigationTargets) | Should -Not -Match '\$query|Contains'
        $body | Should -Match 'navigation_basis ='
        $body | Should -Match 'display_basis ='
        $body | Should -Not -Match 'foreach \(\$(origin|number|searchMode|agentsOnly) in'
        $body | Should -Not -Match 'Set-WtPaneFocus[^\r\n]*\$helper.PaneSessionId'
        $body | Should -Match '''focus-pane'', ''-t'', \$helper.PaneSessionId'
        $body | Should -Not -Match '\$helperText|Get-AgentPaneText'
        $body | Should -Match 'Automation\]::Compare\(\$focused, \$helperPeers\[0\]\)'
        $body | Should -Match 'Get-Process -Id \$helper.HelperProcessId'
        $body | Should -Match 'Send-WtWindowKey.*-Ctrl -Alt -RequireForeground'
        $body | Should -Not -Match 'Send-WtKeys|send-keys'
        $calls = @($script:suiteAst.FindAll({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Assert-MoveNumberKeys'
        }, $true))
        $calls | Should -HaveCount 2
        ($numberCases.Count * $calls.Count) | Should -Be 18
        ($numberCases.Count * $calls.Count) | Should -BeLessOrEqual 20
    }

    It 'extracts helper-origin focus and identity gates without comparing different text APIs' {
        $function = $script:suiteAst.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Assert-MoveNumberKeys'
        }, $true)
        $focusBranch = $function.Find({ param($node)
            $node -is [Management.Automation.Language.IfStatementAst] -and
                $node.Clauses[0].Item1.Extent.Text -eq '$origin -eq ''Helper'''
        }, $true).Clauses[0].Item2
        $focusBranch.Extent.Text | Should -Match 'Get-AgentPaneSession.*\$helper.PaneSessionId'
        $focusBranch.Extent.Text | Should -Match '\$helperBefore.HelperProcessId \| Should -Be \$helper.HelperProcessId'
        $focusBranch.Extent.Text | Should -Match '\$helperBefore.AcpSessionId \| Should -Be \$helper.AcpSessionId'
        $focusCommand = $focusBranch.Find({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Invoke-WtCli'
        }, $true).Extent.Text
        $focusCommand | Should -Match '''focus-pane'', ''-t'', \$helper.PaneSessionId'
        $body = $function.Body.Extent.Text
        $body | Should -Match 'Automation\]::Compare\(\$focused, \$helperPeers\[0\]\)'
        $body | Should -Match '\$focused.GetRuntimeId\(\)\).*Should -Be @\(\$helperPeers\[0\].GetRuntimeId\(\)\)'
        $body | Should -Match '\$helperPeers\[0\].Current.HasKeyboardFocus \| Should -BeTrue'
        $body | Should -Match '\$root.Current.ProcessId \| Should -Be \$script:app.Pid'
        $body | Should -Match '\$helperPeers \| Should -HaveCount 1'
        $body | Should -Match '\$currentHelperProcess.StartTime \| Should -Be \$originalHelperProcess.StartTime'
        $body | Should -Match '\$nativeHelper.ParentProcessId \| Should -Be \$script:app.Pid'
        $body | Should -Match '\$nativeHelper.CommandLine \| Should -BeExactly \$originalHelperNative.CommandLine'
        $body | Should -Match '''get-pane-context'', ''--target'', \$owner.session_id'
        $body | Should -Not -Match '''get-pane-context'', ''--target'', \$helper.PaneSessionId'
        $body | Should -Not -Match 'stable_tab_id'
        $body | Should -Match '\$sourceContext.pane.session_id \| Should -Be \(\[string\]\$owner.session_id\)'
        $body | Should -Match '\$sourceContext.pane.tab_id \| Should -Be \(\[string\]\(Get-MoveOwner\).tab_id\)'
        $body | Should -Match '\$sourceContext.pane.window_id \| Should -Be \(\[string\]\$script:app.WindowId\)'
        $body | Should -Match '\[regex\]::Escape\(\$originalHelperStableTabId\)'
        $body | Should -Match '\$focusedSource = Get-ActivePane -App \$script:app'
        $body | Should -Match '\$focusedSource.session_id \| Should -Be \(\[string\]\$owner.session_id\)'
        foreach ($binding in @('helperAfter', 'helperAfterKey')) {
            $body | Should -Match ([regex]::Escape("`$$binding.HelperProcessId | Should -Be `$helperBefore.HelperProcessId"))
            $body | Should -Match ([regex]::Escape("`$$binding.AcpSessionId | Should -Be `$helperBefore.AcpSessionId"))
        }
        $body | Should -Match '\$document \| Should -Not -BeNullOrEmpty'
        $body | Should -Not -Match '\$helperText|DocumentRange.*(Substring|Trim)|Get-AgentPaneText'
    }

    It 'executes source-context ownership gates and rejects <Fault>' -ForEach @(
        @{ Fault = 'none' }, @{ Fault = 'RPC failure' }, @{ Fault = 'source session' },
        @{ Fault = 'tab index' }, @{ Fault = 'window' }, @{ Fault = 'focused helper owner' },
        @{ Fault = 'stable helper tab' }
    ) {
        $function = $script:suiteAst.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Assert-MoveNumberKeys'
        }, $true)
        $body = $function.Body.Extent.Text
        $start = $body.IndexOf('$sourceContext = Invoke-WtCli')
        $end = $body.IndexOf('$nativeHelper.CommandLine | Should -Match (''--owner-window-id', $start)
        $start | Should -BeGreaterThan -1
        $end | Should -BeGreaterThan $start
        $gates = [scriptblock]::Create($body.Substring($start, $end - $start))
        $originalApp = $script:app
        try {
            $script:app = @{ WindowId = 1 }
            $owner = @{ session_id = 'source-shell' }
            $originalHelperStableTabId = '46a39eed-32a3-415a-b55c-c6d0e98592fc'
            $nativeHelper = @{ CommandLine = "--owner-tab-id `"{${originalHelperStableTabId}}`"" }
            $context = @{ pane = @{ session_id = 'source-shell'; tab_id = 3; window_id = 1 } }
            $active = @{ session_id = 'source-shell'; tab_id = 3; window_id = 1 }
            switch ($Fault) {
                'source session' { $context.pane.session_id = 'other-shell' }
                'tab index' { $context.pane.tab_id = 2 }
                'window' { $context.pane.window_id = 2 }
                'focused helper owner' { $active.session_id = 'other-shell' }
                'stable helper tab' { $nativeHelper.CommandLine = '--owner-tab-id "{11111111-1111-1111-1111-111111111111}"' }
            }
            function Invoke-WtCli {
                param($App, $Arguments)
                $Arguments | Should -Be @('get-pane-context', '--target', 'source-shell')
                if ($Fault -eq 'RPC failure') { throw 'GetPaneContext failed: 0x80070490' }
                $context
            }
            function Get-MoveOwner { @{ tab_id = 3 } }
            function Get-ActivePane { param($App) $active }
            if ($Fault -eq 'none') { . $gates }
            else { { . $gates } | Should -Throw }
        }
        finally { $script:app = $originalApp }
    }

    It 'retains captured Unicode readiness and real helper focus despite differing text surfaces' {
        $path = Join-Path $PSScriptRoot '..\..\..\Generated Files\loop-gordon-release\agent-tab-reordering\hp-1041\operands.json'
        if (-not (Test-Path -LiteralPath $path)) { Set-ItResult -Skipped -Because 'Private diagnostic sample is not distributed with the repository'; return }
        $sample = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
        $sample.rawDocument | Should -Not -BeNullOrEmpty
        [int]$sample.rawDocument[1] | Should -Be 0x25CF
        ($sample.rawDocument -replace '\s', '').Contains(($sample.rawCapture -replace '\s', '')) | Should -BeFalse
        ($sample.rawDocument -replace '\s', '').Contains(($sample.observations.rawCaptureMetadata.content -replace '\s', '')) | Should -BeFalse
        $sample.observations.uniqueVisibleHelperPeers | Should -HaveCount 1
        $sample.observations.focusMatchesUniqueHelper | Should -BeTrue
        $sample.focus.runtime | Should -Be $sample.observations.uniqueVisibleHelperPeers[0].runtime
        $sample.observations.helperBindingAfter.HelperProcessId | Should -Be $sample.helperBinding.HelperProcessId
        $sample.observations.helperBindingAfter.AcpSessionId | Should -Be $sample.helperBinding.AcpSessionId
        $sample.observations.helperProcess.ParentProcessId | Should -Be $sample.focus.pid
    }

    It 'models stable navigation separately from global search display' -ForEach @(
        @{ Agents = $true; Query = ''; ExpectedDisplay = @('initial', 'neighbor', 'owner'); ExpectedNavigation = @('initial', 'neighbor', 'owner') },
        @{ Agents = $false; Query = ''; ExpectedDisplay = @('initial', 'neighbor', 'ordinary', 'owner'); ExpectedNavigation = @('initial', 'neighbor', 'ordinary', 'owner') },
        @{ Agents = $true; Query = 'fixture'; ExpectedDisplay = @('neighbor', 'ordinary', 'owner'); ExpectedNavigation = @('initial', 'neighbor', 'owner') },
        @{ Agents = $true; Query = 'ordinary'; ExpectedDisplay = @('ordinary'); ExpectedNavigation = @('initial', 'neighbor', 'owner') },
        @{ Agents = $true; Query = 'no-match'; ExpectedDisplay = @(); ExpectedNavigation = @('initial', 'neighbor', 'owner') }
    ) {
        $canonical = @('initial', 'neighbor', 'ordinary', 'owner')
        $fixtureOwnerTitles = @{ initial = 'startup'; neighbor = 'fixture-neighbor'; ordinary = 'fixture-ordinary'; owner = 'fixture-owner' }
        $qualifiedOwnerIds = @('initial', 'neighbor', 'owner')
        $agentsOnly = $Agents; $query = $Query
        . ([scriptblock]::Create((Get-CombinedModeAssignment navigationTargets)))
        . ([scriptblock]::Create((Get-CombinedModeAssignment displayOwners)))
        $navigationTargets | Should -Be $ExpectedNavigation
        $displayOwners | Should -Be $ExpectedDisplay
        $protocol = Get-Content (Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\TerminalPage.Protocol.cpp') -Raw
        $protocol | Should -Match '(?s)_getProtocolSourcePane.*?pane->IsAgentPane\(\).*?candidate->IsSourceOfAgentPane\(\)'
        $protocol | Should -Match 'const auto effectivePane = _getProtocolSourcePane\(tabImpl\)'
    }

    It 'makes every authored key a real owner transition before and after visible-neighbor moves' {
        . ([scriptblock]::Create((Get-CombinedModeAssignment numberCases)))
        foreach ($canonical in @(
            @('initial', 'neighbor', 'ordinary', 'owner'),
            @('initial', 'neighbor', 'owner', 'ordinary')
        )) {
            foreach ($case in $numberCases) {
                $agentsOnly = $case.AgentsOnly
                $qualifiedOwnerIds = @('initial', 'neighbor', 'owner')
                . ([scriptblock]::Create((Get-CombinedModeAssignment navigationTargets)))
                $expected = if ($case.Number -eq 9) { $navigationTargets[-1] } else { $navigationTargets[$case.Number - 1] }
                $start = if ($expected -eq 'owner') { 'neighbor' } else { 'owner' }
                $start | Should -Not -Be $expected
                if ($case.Origin -ne 'Shell') { $start | Should -Be 'owner' }
                if ($case.AgentsOnly -and $case.Search -eq 'Closed' -and $case.Number -eq 9) {
                    $expected | Should -Be 'owner'
                }
                if ($case.Search -eq 'Ordinary') {
                    $expected | Should -Be 'neighbor'
                }
                if ($case.Search -eq 'NoMatch') {
                    $expected | Should -Be $(if ($case.Number -eq 9) { 'owner' } else { 'initial' })
                }
            }
        }
    }

    It 'extracts the exact fixture oracle including startup and every explicitly opened seed owner' -ForEach @(
        @{ SeedCount = 0 }, @{ SeedCount = 8 }
    ) {
        $originalOwners = $script:openedFixtureOwners
        try {
            $script:openedFixtureOwners = @([pscustomobject]@{
                session_id = 'initial'; title = 'startup'; FixtureSession = @{ AcpSessionId = 'chat-fixture-startup' }
            })
            foreach ($i in 1..$SeedCount | Where-Object { $SeedCount -gt 0 }) {
                $script:openedFixtureOwners += [pscustomobject]@{
                    session_id = "seed-$i"; title = "seed-$i"; FixtureSession = @{ AcpSessionId = "chat-fixture-seed-$i" }
                }
            }
            $neighbor = @{ session_id = 'neighbor' }
            $ordinary = @{ session_id = 'ordinary' }
            $owner = @{ session_id = 'owner' }
            $title = 'fixture'
            $canonical = @($script:openedFixtureOwners.session_id) + @('neighbor', 'ordinary', 'owner')
            $fixtureOwnerTitles = @{}
            foreach ($opened in $script:openedFixtureOwners) { $fixtureOwnerTitles[$opened.session_id] = $opened.title }
            $fixtureOwnerTitles['neighbor'] = 'fixture-neighbor'
            $fixtureOwnerTitles['ordinary'] = 'fixture-ordinary'
            $fixtureOwnerTitles['owner'] = 'fixture'
            . ([scriptblock]::Create((Get-CombinedModeAssignment qualifiedOwnerIds)))
            $qualifiedOwnerIds | Should -Be (@($script:openedFixtureOwners.session_id) + @('neighbor', 'owner'))
            $assignment = Get-CombinedModeAssignment displayOwners
            foreach ($scenario in @(
                @{ Agents = $true; Query = ''; Expected = @($script:openedFixtureOwners.session_id) + @('neighbor', 'owner') },
                @{ Agents = $false; Query = ''; Expected = $canonical },
                @{ Agents = $true; Query = 'fixture'; Expected = @('neighbor', 'ordinary', 'owner') },
                @{ Agents = $true; Query = 'fixture-ordinary'; Expected = @('ordinary') }
            )) {
                $agentsOnly = $scenario.Agents
                $query = $scenario.Query
                . ([scriptblock]::Create($assignment))
                $displayOwners | Should -Be $scenario.Expected
                . ([scriptblock]::Create((Get-CombinedModeAssignment navigationTargets)))
                $navigationTargets | Should -Be $(if ($agentsOnly) { @($script:openedFixtureOwners.session_id) + @('neighbor', 'owner') } else { $canonical })
                $canonical[0] | Should -Be 'initial'
                $canonical | Should -Be (@($script:openedFixtureOwners.session_id) + @('neighbor', 'ordinary', 'owner'))
            }
            $body = $script:suiteAst.Find({ param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Assert-MoveNumberKeys'
            }, $true).Body.Extent.Text
            $body | Should -Not -Match 'native_agent_provider_id|is_agent_pane'
            $body | Should -Match '\$opened.FixtureSession.AcpSessionId'
            $body | Should -Match '\$actualOwnerTitles \| Should -Be \$expectedOwnerTitles'
            $body | Should -Match '\$headers \| Should -HaveCount 1'
            (Get-CombinedModeAssignment actualOwnerTitles) | Should -Not -Match 'IsOffscreen'
            $setup = Get-Content $script:suite -Raw
            $setup | Should -Match '(?s)Wait-AgentReady -App \$script:app -TimeoutSec 60.*?\$script:openedFixtureOwners =.*?FixtureSession = Get-AgentPaneSession'
            $setup | Should -Match '(?s)\$tab \| Add-Member -NotePropertyName FixtureSession.*?\$script:openedFixtureOwners \+=.*?FixtureSession = \$pane'
            $tabSource = Get-Content (Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\Tab.cpp') -Raw
            $tabSource | Should -Match '(?s)bool Tab::IsAgentTab\(\).*?AgentPaneContent.*?AgentSessionId.*?current->IsHidden\(\)'
        }
        finally { $script:openedFixtureOwners = $originalOwners }
    }

    It 'keeps separate terminal and XAML SwitchToTab chord forwarding branches' {
        $root = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp'
        $terminal = Get-Content (Join-Path $root 'AppKeyBindings.cpp') -Raw
        $terminal | Should -Match '(?s)bool AppKeyBindings::TryKeyChord\(.*?ShortcutAction::SwitchToTab\)\s*\{\s*return _dispatch.DoAction\(kc, cmd.ActionAndArgs\(\)\);'
        $xaml = Get-Content (Join-Path $root 'TerminalPage.cpp') -Raw
        $xaml | Should -Match '(?s)void TerminalPage::_KeyDownHandler\(.*?action.Action\(\) == ShortcutAction::SwitchToTab\) \?\s*_actionDispatch->DoAction\(chord, action\)'
    }

    It 'discovers <Count> cases for <Mode> without running live setup' -ForEach @(
        @{ Mode = 'default'; Focused = $false; History = ''; Count = 34 },
        @{ Mode = 'owning-tab only'; Focused = $true; History = ''; Count = 1 },
        @{ Mode = 'history compatibility'; Focused = $false; History = '1'; Count = 34 },
        @{ Mode = 'focused history compatibility'; Focused = $true; History = '1'; Count = 1 }
    ) {
        $env:ITE2E_HISTORY_INDICATORS_ONLY = $History
        $configuration = New-PesterConfiguration
        $configuration.Run.Container = if ($Mode -eq 'default') {
            New-PesterContainer -Path $script:suite
        } else {
            New-PesterContainer -Path $script:suite -Data @{ OwningTabMoveOnly = $Focused }
        }
        $configuration.Run.SkipRun = $true
        $configuration.Run.PassThru = $true
        $configuration.Output.Verbosity = 'None'
        $result = Invoke-Pester -Configuration $configuration
        $result.TotalCount | Should -Be $Count
        $result.FailedCount | Should -Be 0
        @($result.Tests | Where-Object Executed) | Should -HaveCount 0
        if ($Focused) {
            @($result.Tests).Name | Should -Be 'Agents view moves whole owning tabs without changing sessions'
        }
    }

    It 'preserves setup and scrolling branches for <Mode>' -ForEach @(
        @{ Mode = 'default'; Focused = $false; History = ''; Seeds = 8; Survival = 30; Scrolls = 1 },
        @{ Mode = 'owning-tab only'; Focused = $true; History = ''; Seeds = 0; Survival = 0; Scrolls = 1 },
        @{ Mode = 'history compatibility'; Focused = $false; History = '1'; Seeds = 0; Survival = 0; Scrolls = 0 },
        @{ Mode = 'focused history compatibility'; Focused = $true; History = '1'; Seeds = 0; Survival = 0; Scrolls = 0 }
    ) {
        $OwningTabMoveOnly = $Focused
        $env:ITE2E_HISTORY_INDICATORS_ONLY = $History
        . ([scriptblock]::Create((Get-CombinedModeAssignment survivalSeconds)))
        . ([scriptblock]::Create((Get-CombinedModeAssignment seedIndices)))
        . ([scriptblock]::Create((Get-CombinedModeAssignment scrollLists)))
        $survivalSeconds | Should -Be $Survival
        @($seedIndices) | Should -HaveCount $Seeds
        @($scrollLists) | Should -HaveCount $Scrolls
    }
}
