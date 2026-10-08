#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    . (Join-Path $PSScriptRoot '..\tests\helpers\TabHeaderContext.ps1')
    . (Join-Path $PSScriptRoot '..\tests\helpers\PackageProfileActivation.ps1')
    . (Join-Path $PSScriptRoot '..\tests\helpers\KeptTabReattachment.ps1')
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $script:headerSource = (Get-Command Invoke-TestTabHeaderContextMenu).Definition
    function Read-FixtureAst([string]$Name) {
        $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot "..\tests\$Name.Tests.ps1"), [ref]$null, [ref]$errors)
        if ($errors.Count) { throw ($errors -join "`n") }
        $ast
    }
    $script:progressAst = Read-FixtureAst Feature.PaneProgress
}

Describe 'Packaged explicit profile activation' -Tag 'Unit' {
    It 'requires actual S_OK while recording the activation PID without equating it to the resident receiver' {
        { Assert-TestPackageProfileActivationResult 0 42 @{ Pid = 42 } } | Should -Not -Throw
        { Assert-TestPackageProfileActivationResult -2147467259 42 @{ Pid = 42 } } | Should -Throw '*HRESULT*'
        { Assert-TestPackageProfileActivationResult 0 99 @{ Pid = 42 } } | Should -Not -Throw
        { Assert-TestPackageProfileActivationResult 1 42 @{ Pid = 42 } } | Should -Throw '*HRESULT*'
    }

    It 'rejects a foreign original owner before any activation or silent fallback' {
        Mock Get-Process { @{ StartTime = [datetime]'2026-10-06T10:00:00Z'; Path = 'C:\foreign\WindowsTerminal.exe' } }
        Mock Initialize-TestPackageProfileActivation {}
        { Assert-TestPackagedProfileOwner @{ Pid = 42; Launched = $false; InstallLocation = 'C:\fixture'
            OwnedProcess = @{ Id = 42; HasExited = $false; StartTime = [datetime]'2026-10-06T10:00:00Z' } } } |
            Should -Throw '*original owned application lease*'
        Should -Invoke Initialize-TestPackageProfileActivation -Times 0
    }

    Describe 'Exact kept group reattachment oracle' -Tag 'Unit' {
        BeforeEach {
            $windows = @(@{window_id=1;tab_count=2})
            $context = @{pane=@{session_id='primary';window_id=1;tab_id=1;title='controlled'}}
            $panes = @(@{session_id='primary';window_id=1;tab_id=1},@{session_id='split';window_id=1;tab_id=1},
                @{session_id='helper';window_id=1;tab_id=1})
        }
        It 'requires complete original group membership and exact tab/window/count/title' {
            Test-TestKeptTabIdentity $windows $context $panes '1' primary controlled 2 @('primary','split','helper') |
                Should -BeTrue
            $panes = @($panes|Where-Object session_id -ne split)
            Test-TestKeptTabIdentity $windows $context $panes '1' primary controlled 2 @('primary','split','helper') |
                Should -BeFalse
        }
        It 'rejects a wrong <Field> rather than trusting focus-response success' -ForEach @(
            @{Field='session_id';Wrong='other'},@{Field='window_id';Wrong=2},@{Field='title';Wrong='other'}
        ) {
            $context.pane[$Field]=$Wrong
            Test-TestKeptTabIdentity $windows $context $panes '1' primary controlled 2 @('primary','split','helper') |
                Should -BeFalse
        }
        It 'rejects wrong counts and cross-tab pane association' {
            $windows[0].tab_count=1
            Test-TestKeptTabIdentity $windows $context $panes '1' primary controlled 2 @('primary','split','helper') |
                Should -BeFalse
            $windows[0].tab_count=2;$panes[1].tab_id=2
            Test-TestKeptTabIdentity $windows $context $panes '1' primary controlled 2 @('primary','split','helper') |
                Should -BeFalse
        }
        It 'keeps the same10s wait and independently verifies native canonical header, not child/terminal name' {
            $source=(Get-Command Wait-TestKeptTabReattachment).Definition
            $source|Should -Match 'Wait-Until -TimeoutSec 10'
            $source|Should -Match "'get-pane-context','--target'"
            $source|Should -Match "AutomationIdProperty,'ItemsList'"
            $source|Should -Match "AutomationId -eq 'PaneActivateButton'"
            $source|Should -Match '\$headers.Count -eq 1'
            $source|Should -Match 'GetWindowProcessId\(\$root\) -ne \$App.Pid'
            $source|Should -Match 'Get-WtPaneStatus'
            $source|Should -Match 'AcpSessionId \| Should -Be'
            $source|Should -Not -Match 'Wait-UiElement|Invoke-Ui|SendInput|SetFocus'
            (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text|Should -Not -Match 'KeepReattachDiagnostic|Save-TestKeepReattachFailure'
        }
    }
    It 'uses AO_NONE public activation with quoted validated GUID and bounded owned worker' {
        Initialize-TestPackageProfileActivation
        $source = (Get-Command Invoke-TestPackagedProfileActivation).Definition
        $source | Should -Match '\[guid\]::Parse\(\$Profile\)'
        $source | Should -Match 'ToString\(''B''\)'
        $source | Should -Match 'PackageProfileActivation\]::Activate'
        $source | Should -Match 'WaitForExit\(20000\)'
        $source | Should -Match '\$streams.Wait\(2000\)'
        $source | Should -Match 'Assert-TestPackagedProfileOwner \$App'
        $source | Should -Not -Match 'new-window|focus-pane|WindowingBehavior|Invoke-Native|ProfileLaunchDiagnostic|GetAwaiter|GetResult'
        (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text | Should -Match '\$launch.HResult \| Should -Be 0'
        (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text | Should -Match 'GetWindowProcessId\(\$profileHwnd\) \| Should -Be \$script:app.Pid'
        (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text | Should -Match 'IsWindowVisible\(\$profileHwnd\) \| Should -BeTrue'
        (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text | Should -Match '\$windows.Count -eq 1 -and \$windows\[0\].tab_count -eq 1'
    }
    It 'does not adopt or terminate an unknown returned PID' {
        $source = (Get-Command Invoke-TestPackagedProfileActivation).Definition
        $source | Should -Not -Match '\$App.Pid\s*=|Kill\(\$true\)|CloseMainWindow|existingPids|candidate|unexpected'
        $source | Should -Match '\$worker.Kill\(\)'
        $source | Should -Match 'ReturnedPid = if \(\[int\]\$result.HResult -eq 0\)'
        Mock Get-Process { throw 'Returned PID must never be inspected as owned.' }
        { Assert-TestPackageProfileActivationResult 0 99 @{ Pid = 42 } } | Should -Not -Throw
        { Assert-TestPackageProfileActivationResult -2147467259 99 @{ Pid = 42 } } |
            Should -Throw '*HRESULT*'
        Should -Invoke Get-Process -Times 0
    }
    It 'excludes every retained pane including a wrongly focused split in both launch phases' {
        $ast = Read-FixtureAst Feature.KeepRunningFocus
        $flatten = $ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                $node.Left.Extent.Text -eq '$retainedPaneIds'
        }, $true)[0]
        $retained = @(
            @{ Shell = 'primary'; Pids = @{ primary = 1; split = 2; helper = 3 } },
            @{ Shell = 'other-primary'; Pids = @{ 'other-primary' = 4; 'other-helper' = 5 } }
        )
        . ([scriptblock]::Create($flatten.Extent.Text))
        $retainedPaneIds | Should -Contain 'split'
        $retainedPaneIds | Should -Contain 'helper'
        { 'split' | Should -Not -BeIn $retainedPaneIds } | Should -Throw
        { 'ordinary-new' | Should -Not -BeIn $retainedPaneIds } | Should -Not -Throw
        ([regex]::Matches($ast.Extent.Text, 'session_id \| Should -Not -BeIn \$retainedPaneIds')).Count | Should -Be 2
    }
    It 'checks detached kept titles before closing the profile window and during the later bare phase' {
        $source = (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text
        $profile = $source.IndexOf('$script:app.WindowId = [string]$profileWindow.window_id')
        $profileClose = $source.IndexOf('Send-WtWindowKey', $profile)
        $check = $source.IndexOf('Test-UiElementExists -App $script:app -Selector $tab.Title', $profile)
        $check | Should -BeLessThan $profileClose
        $source.Substring($check, $profileClose - $check) | Should -Match 'Should -BeFalse'
        ([regex]::Matches($source, 'Test-UiElementExists -App \$script:app -Selector \$tab.Title')).Count | Should -Be 2
        { $true | Should -BeFalse } | Should -Throw
    }
}

Describe 'Canonical tab context selector contract' -Tag 'Unit' {
    It 'rejects an unrelated pane before reading UI or sending input' {
        Mock Invoke-WtCli { @{ pane = @{ session_id = 'other'; window_id = '1'; tab_id = '2' } } }
        Mock Get-WtTabs {}
        Mock Set-WtWindowForeground {}
        Mock Invoke-UiMouseDrag {}
        { Invoke-TestTabHeaderContextMenu -App @{ Pid = 42; WindowId = '1' } -PaneSessionId owned -Title exact } |
            Should -Throw '*requested pane*'
        Should -Invoke Get-WtTabs -Times 0
        Should -Invoke Set-WtWindowForeground -Times 0
        Should -Invoke Invoke-UiMouseDrag -Times 0
    }
    It 'rejects a matching pane belonging to another window before physical input' {
        Mock Invoke-WtCli { @{ pane = @{ session_id = 'owned'; window_id = '9'; tab_id = '2' } } }
        Mock Invoke-UiMouseDrag {}
        { Invoke-TestTabHeaderContextMenu -App @{ Pid = 42; WindowId = '1' } -PaneSessionId owned -Title exact } |
            Should -Throw '*owned window*'
        Should -Invoke Invoke-UiMouseDrag -Times 0
    }
    It 'rejects duplicate canonical titles instead of choosing the first peer' {
        Mock Invoke-WtCli { @{ pane = @{ session_id = 'owned'; window_id = '1'; tab_id = '2' } } }
        Mock Get-WtTabs { @(@{ tab_id = '2'; title = 'exact' }, @{ tab_id = '3'; title = 'exact' }) }
        Mock Invoke-UiMouseDrag {}
        { Invoke-TestTabHeaderContextMenu -App @{ Pid = 42; WindowId = '1' } -PaneSessionId owned -Title exact } |
            Should -Throw '*one canonical tab*'
        Should -Invoke Invoke-UiMouseDrag -Times 0
    }
    It 'uses structural ownership, actual scroll realization and the title point rather than a ListItem center' {
        $script:headerSource | Should -Match 'RawViewWalker'
        $script:headerSource | Should -Match 'ControlType\]::ListItem'
        $script:headerSource | Should -Match 'ControlType\]::TabItem'
        $script:headerSource | Should -Match "AutomationId -eq 'PaneActivateButton'"
        $script:headerSource | Should -Match 'ScrollIntoView\(\)'
        $script:headerSource | Should -Match 'ScrollIntoView\(\)[\s\S]*\$header = & \$resolve'
        $script:headerSource | Should -Match '\$bounds = \$header.Text.Current.BoundingRectangle'
        $script:headerSource | Should -Match '\.Contains\(\$point\)'
        $script:headerSource | Should -Match 'AutomationElement\]::FromPoint\(\$point\)'
        $script:headerSource | Should -Match '\$mouse.dwFlags = 0x0008'
        $script:headerSource | Should -Match '\$mouse.dwFlags = 0x0010'
        $script:headerSource | Should -Match '::SendInput\(2,'
        $script:headerSource | Should -Not -Match 'Invoke-UiMouseDrag|Invoke-UiClick|\.Invoke\(|\.SetFocus\('
    }
    It 'keeps both fixture callers on the shared canonical header resolver' {
        foreach ($name in @('Feature.KeepRunningFocus', 'Feature.PaneProgress')) {
            $ast = Read-FixtureAst $name
            $ast.Extent.Text | Should -Match 'helpers\\TabHeaderContext.ps1'
            $ast.Extent.Text | Should -Match 'Invoke-TestTabHeaderContextMenu'
            $ast.Extent.Text | Should -Not -Match 'Invoke-UiClick[^\r\n]+-Selector \$title -Right'
        }
    }
    It 'uses one physical-coordinate context for header bounds, hit testing and input, restoring it on failure' {
        $script:headerSource.IndexOf('SetThreadDpiAwarenessContext([IntPtr]::new(-4))') |
            Should -BeLessThan $script:headerSource.IndexOf('$context = Invoke-WtCli')
        $script:headerSource | Should -Match 'finally \{ \[void\].*SetThreadDpiAwarenessContext\(\$dpi\) \}'
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $before = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($before)
        Mock Invoke-WtCli { @{ pane = @{ session_id = 'other'; window_id = '1'; tab_id = '2' } } }
        { Invoke-TestTabHeaderContextMenu -App @{ Pid = 42; WindowId = '1' } -PaneSessionId owned -Title exact } |
            Should -Throw '*requested pane*'
        $after = [ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext([IntPtr]::new(-4))
        try { $after | Should -Be $before }
        finally { [void][ItE2E.ItWtWin32Input]::SetThreadDpiAwarenessContext($after) }
    }
    It 'keeps the UIA ancestry rejection and original process lease while recording exact hit evidence' {
        $script:headerSource | Should -Match "\`$hitChain \+= @\{"
        $script:headerSource | Should -Match 'header_runtime_id'
        $script:headerSource | Should -Match 'row_runtime_id'
        $script:headerSource | Should -Match 'previous_dpi_context'
        $script:headerSource | Should -Match 'native_hit_root'
        $script:headerSource | Should -Match 'name = if \(\$hit.Current.ProcessId -eq \$App.Pid\)'
        $script:headerSource | Should -Match 'id = if \(\$hit.Current.ProcessId -eq \$App.Pid\)'
        $script:headerSource | Should -Match 'Assert-TestTabHeaderPointSafety \$guard'
        $script:headerSource | Should -Match '\$current.StartTime -eq \$App.OwnedProcess.StartTime'
        $script:headerSource | Should -Match 'GetAncestor\(\$nativeHit, 2\) -ne \$root'
        $script:headerSource.IndexOf('Assert-TestTabHeaderPointSafety $guard') |
            Should -BeLessThan $script:headerSource.IndexOf('::SendInput')
        $script:headerSource | Should -Match '\$env:ITE2E_RUN_TOKEN -cne \$runToken'
        $script:headerSource | Should -Match '\$_.run_token -ceq \$runToken'
        $script:headerSource | Should -Match '\$cursor.X -ne \$nativePoint.X'
        $script:headerSource | Should -Match '\$cursor.Y -ne \$nativePoint.Y'
        $script:headerSource | Should -Match '@\(1, 2, 4, 5, 6, 16, 17, 18, 91, 92\)'
        $script:headerSource | Should -Match '\$final.Fingerprint -cne \$second.Fingerprint'
        $script:headerSource | Should -Match '-not \$final.FullyVisible'
        $script:headerSource | Should -Match '-not \$final.InHeaderBand'
        $script:headerSource | Should -Match '-not \$final.OutsideActions'
        $script:headerSource.LastIndexOf('WindowFromPoint(') |
            Should -BeLessThan $script:headerSource.IndexOf('::SendInput')
    }
    It 'captures an open context flyout without reacquiring the root foreground' {
        $openMenu = $script:progressAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Open-TabMenu'
        }, $true)[0]
        $openMenu.Extent.Text | Should -Match 'Save-CompositorFrame[^\r\n]+-PreserveFlyout'
        $digest = $script:progressAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-VisualDigest'
        }, $true)[0]
        $branch = $digest.FindAll({
            param($node)
            $node -is [Management.Automation.Language.IfStatementAst] -and $node.Clauses[0].Item1.Extent.Text -eq '$PreserveFlyout'
        }, $true)[0]
        $branch.Clauses[0].Item2.Extent.Text | Should -Match 'IsOwnedRootOrPopup'
        $branch.Clauses[0].Item2.Extent.Text | Should -Not -Match 'Set-WtWindowForeground'
        $branch.ElseClause.Extent.Text | Should -Match 'Set-WtWindowForeground'
    }
    It 'constructs the established paired physical right input without injecting it' {
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $start = $script:headerSource.IndexOf('$down =')
        $end = $script:headerSource.IndexOf('foreach ($key', $start)
        . ([scriptblock]::Create($script:headerSource.Substring($start, $end - $start)))
        $inputs.Count | Should -Be 2
        $inputs[0].type | Should -Be 0
        $inputs[0].data.mouse.dwFlags | Should -Be 8
        $inputs[1].data.mouse.dwFlags | Should -Be 16
        $inputSize | Should -Be ([Runtime.InteropServices.Marshal]::SizeOf($down))
    }
}

Describe 'Opaque host independent header guards' -Tag 'Unit' {
    BeforeAll {
        $helperAst = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\helpers\TabHeaderContext.ps1'), [ref]$null, [ref]$null)
        $geometryAssignment = $helperAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -eq '$geometry'
        }, $true)[0]
        . ([scriptblock]::Create($geometryAssignment.Extent.Text))
        function New-HeaderGeometryFixture {
            $parts = @(foreach ($item in @(
                @{ Id = 'TabGroupToggleButton'; Rect = @(96, 220, 28, 28) },
                @{ Id = 'TabCloseButton'; Rect = @(264, 220, 28, 28) },
                @{ Id = 'PaneActivateButton'; Rect = @(106, 251, 158, 30) },
                @{ Id = 'PaneCloseButton'; Rect = @(266, 251, 26, 26) }
            )) {
                [pscustomobject]@{ Current = @{
                    ProcessId = 42; IsOffscreen = $false; AutomationId = $item.Id
                    ControlType = [Windows.Automation.ControlType]::Button
                    BoundingRectangle = [Windows.Rect]::new($item.Rect[0], $item.Rect[1], $item.Rect[2], $item.Rect[3])
                } }
            })
            $row = [pscustomobject]@{
                Current = @{ IsOffscreen = $false; BoundingRectangle = [Windows.Rect]::new(84, 215, 208, 104) }
                Parts = $parts; RuntimeId = @(42, 7, 4, 30)
            }
            $row | Add-Member ScriptMethod FindAll { param($scope, $condition) @($this.Parts) }
            $row | Add-Member ScriptMethod GetRuntimeId { $this.RuntimeId }
            $text = [pscustomobject]@{ Current = @{
                IsOffscreen = $false; BoundingRectangle = [Windows.Rect]::new(128, 226, 136, 19)
            } }
            $text | Add-Member ScriptMethod GetRuntimeId { @(42, 7, 4, 34) }
            @{ Text = $text; Row = $row; Vertical = $true; Container = @{
                Current = @{ BoundingRectangle = [Windows.Rect]::new(78, 163, 220, 542) }
            } }
        }
    }
    BeforeEach {
        $App = @{ Pid = 42 }
        $bounds = [Windows.Rect]::new(128, 226, 136, 19)
        $point = [Windows.Point]::new(196, 236)
        $guard = @{
            ProtocolOwned = $true; FullyVisible = $true; InHeaderBand = $true; OutsideActions = $true
            Stable = $true; LeaseOwned = $true; RunLease = $true; NativeOwned = $true
            ForegroundOwned = $true; NoKnownOverlay = $true; DeepHit = $false; OpaqueRoot = $true; ForbiddenHit = $false
        }
    }
    It 'does not accept an owned hosting root alone' {
        { Assert-TestTabHeaderPointSafety @{ OpaqueRoot = $true } } | Should -Throw '*ProtocolOwned*'
    }
    It 'allows opaque-host input only with every independent guard present' {
        { Assert-TestTabHeaderPointSafety $guard } | Should -Not -Throw
    }
    It 'preserves the ordinary deep-header path' {
        $guard.DeepHit = $true
        $guard.OpaqueRoot = $false
        { Assert-TestTabHeaderPointSafety $guard } | Should -Not -Throw
    }
    It 'rejects a missing <GuardName> even for the same owned opaque root' -ForEach @(
        @{ GuardName = 'ProtocolOwned' }, @{ GuardName = 'FullyVisible' }, @{ GuardName = 'InHeaderBand' },
        @{ GuardName = 'OutsideActions' }, @{ GuardName = 'Stable' }, @{ GuardName = 'LeaseOwned' },
        @{ GuardName = 'RunLease' }, @{ GuardName = 'NativeOwned' }, @{ GuardName = 'ForegroundOwned' },
        @{ GuardName = 'NoKnownOverlay' }
    ) {
        $guard[$GuardName] = $false
        { Assert-TestTabHeaderPointSafety $guard } | Should -Throw "*$GuardName*"
    }
    It 'never lets an explicit pane/button or foreign hit use the opaque branch' {
        $guard.ForbiddenHit = $true
        { Assert-TestTabHeaderPointSafety $guard } | Should -Throw '*belongs to a pane*'
        $guard.DeepHit = $true
        { Assert-TestTabHeaderPointSafety $guard } | Should -Throw '*belongs to a pane*'
    }
    It 'rejects an unrelated same-process peer rather than treating it as the opaque host' {
        $guard.OpaqueRoot = $false
        { Assert-TestTabHeaderPointSafety $guard } | Should -Throw '*covered*'
    }
    It 'recognizes existing <Kind> overlay peers rather than relying on host ownership' -ForEach @(
        @{ Kind = 'menu'; Id = ''; Class = ''; Type = 'Menu' },
        @{ Kind = 'popup'; Id = ''; Class = 'Popup'; Type = 'Pane' },
        @{ Kind = 'dialog'; Id = ''; Class = 'ContentDialog'; Type = 'Window' },
        @{ Kind = 'FRE'; Id = 'WelcomePage'; Class = ''; Type = 'Pane' },
        @{ Kind = 'settings'; Id = 'SettingsNav'; Class = ''; Type = 'Pane' },
        @{ Kind = 'search'; Id = 'SearchTextBox'; Class = ''; Type = 'Edit' }
    ) {
        $peer = @{ Current = @{
            IsOffscreen = $false; BoundingRectangle = [Windows.Rect]::new(0, 0, 300, 300)
            ControlType = [Windows.Automation.ControlType]::$Type; AutomationId = $Id; ClassName = $Class; Name = ''
        } }
        Test-KnownTabHeaderOverlayPeer $peer '' | Should -BeTrue
        $peer.Current.IsOffscreen = $true
        Test-KnownTabHeaderOverlayPeer $peer '' | Should -BeFalse
    }
    It 'recognizes the framework localized palette name without a made-up AutomationId' {
        $peer = @{ Current = @{
            IsOffscreen = $false; BoundingRectangle = [Windows.Rect]::new(0, 0, 300, 300)
            ControlType = [Windows.Automation.ControlType]::Pane; AutomationId = ''; ClassName = ''; Name = 'Command palette'
        } }
        Test-KnownTabHeaderOverlayPeer $peer '^Command palette$' | Should -BeTrue
    }
    It 'rejects a point in the pane-child region even inside the expanded group row' {
        $header = New-HeaderGeometryFixture
        $point = [Windows.Point]::new(196, 267)
        $actual = & $geometry $header
        $actual.InHeaderBand | Should -BeFalse
        $actual.OutsideActions | Should -BeFalse
        $guard.InHeaderBand = $actual.InHeaderBand
        $guard.OutsideActions = $actual.OutsideActions
        { Assert-TestTabHeaderPointSafety $guard } | Should -Throw '*InHeaderBand*'
    }
    It 'accepts the real top title geometry but rejects a close-action point' {
        $header = New-HeaderGeometryFixture
        $actual = & $geometry $header
        $actual.FullyVisible | Should -BeTrue
        $actual.InHeaderBand | Should -BeTrue
        $actual.OutsideActions | Should -BeTrue
        $point = [Windows.Point]::new(277, 233)
        $action = & $geometry $header
        $action.OutsideActions | Should -BeFalse
        $action.FullyVisible | Should -BeFalse
    }
    It 'rejects partially clipped titles even with an owned root and nonzero title rectangle' {
        $header = New-HeaderGeometryFixture
        $header.Container.Current.BoundingRectangle = [Windows.Rect]::new(78, 163, 180, 542)
        (& $geometry $header).FullyVisible | Should -BeFalse
    }
    It 'detects fresh peer recycling and changed geometry instead of reusing a header snapshot' {
        $header = New-HeaderGeometryFixture
        $original = (& $geometry $header).Fingerprint
        $header.Row.RuntimeId = @(42, 7, 4, 31)
        (& $geometry $header).Fingerprint | Should -Not -Be $original
        $header.Row.RuntimeId = @(42, 7, 4, 30)
        $header.Text.Current.BoundingRectangle = [Windows.Rect]::new(128, 227, 136, 19)
        (& $geometry $header).Fingerprint | Should -Not -Be $original
    }
}

Describe 'Evidence-specific point-aware overlay predicate' -Tag 'Unit' {
    BeforeAll {
        $body = (Get-Command Test-KnownTabHeaderOverlayPeer).Definition
        $body = $body.Replace('[ItE2E.ItWtWin32Input]::WindowFromPoint($np)', '(& $nativeHit $np)')
        $body = $body.Replace('[ItE2E.ItWtWin32Input]::GetWindowProcessId($hostHandle)', '(& $nativePid $hostHandle)')
        $body = $body.Replace('[ItE2E.ItWtWin32Input]::GetWindowProcessId($hit)', '(& $nativePid $hit)')
        $body = $body.Replace('[ItE2E.ItWtWin32Input]::GetAncestor($hit, 2)', '(& $nativeRoot $hit)')
        $body = $body.Replace('[Windows.Automation.TreeWalker]::RawViewWalker.GetParent($ancestor)', '(& $parent $ancestor)')
        $script:pointPredicate = [scriptblock]::Create($body)
        function New-OverlayPeer([string]$Class, [string]$Type, [int]$Handle, [int[]]$Id, [Windows.Rect]$Bounds, [bool]$Offscreen) {
            $peer = [pscustomobject]@{
                Current = @{ ProcessId = 5100; ClassName = $Class; ControlType = [Windows.Automation.ControlType]::$Type
                    NativeWindowHandle = $Handle; BoundingRectangle = $Bounds; IsOffscreen = $Offscreen; AutomationId = ''; Name = '' }
                Id = $Id; Children = @(); Parent = $null
            }
            $peer | Add-Member ScriptMethod GetRuntimeId { $this.Id }
            $peer | Add-Member ScriptMethod FindAll { param($scope, $condition) @($this.Children) }
            $peer
        }
    }
    BeforeEach {
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $app = @{ Pid = 5100; Hwnd = 6227538 }
        $point = [Windows.Point]::new(432, 509)
        $popupHost = New-OverlayPeer Xaml_WindowedPopupClass Pane 1771824 @(42,1771824) ([Windows.Rect]::new(196,197,1173,611)) $false
        $child = New-OverlayPeer Popup Window 0 @(42,1771824,4,114) ([Windows.Rect]::Empty) $true
        $tip = New-OverlayPeer ToolTip ToolTip 0 @(42,19793138,4,113) ([Windows.Rect]::new(182,373,502,96)) $false
        $popupHost.Children = @($child); $child.Parent = $popupHost; $tip.Parent = $child
        $peers = @($popupHost,$child,$tip)
        $nativeHit = { [IntPtr]6227538 }
        $nativePid = { param($handle) 5100 }
        $nativeRoot = { param($handle) [IntPtr]6227538 }
        $parent = { param($peer) $peer.Parent }
    }
    It 'accepts the exact complete abe522 retained-host shape and noncovering tooltip at the actual point' {
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeFalse
        & $script:pointPredicate $tip '' $point $app $peers | Should -BeFalse
    }
    It 'still rejects a covering <Role> semantic overlay' -ForEach @(
        @{ Role = 'ToolTip' }, @{ Role = 'Menu' }, @{ Role = 'MenuItem' }
    ) {
        $overlay = New-OverlayPeer $Role $Role 0 @(42,7) ([Windows.Rect]::new(400,480,100,100)) $false
        & $script:pointPredicate $overlay '' $point $app @($overlay) | Should -BeTrue
    }
    It 'keeps settings authoritative even when its own region does not contain the header' {
        $tip.Current.AutomationId = 'SettingsNav'
        & $script:pointPredicate $tip '' $point $app $peers | Should -BeTrue
    }
    It 'rejects native popup hits, foreign ownership and missing tooltip associations' {
        $nativeHit = { [IntPtr]1771824 }
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $nativeHit = { [IntPtr]6227538 }; $nativePid = { param($handle) 99 }
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $nativePid = { param($handle) 5100 }
        & $script:pointPredicate $popupHost '' $point $app @($popupHost,$child) | Should -BeTrue
    }
    It 'rejects visible, unknown, incomplete and ambiguous child content' {
        $child.Current.IsOffscreen = $false
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $child.Current.IsOffscreen = $true; $child.Current.ClassName = 'Unknown'
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $child.Current.ClassName = 'Popup'; $popupHost.Children = @()
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $popupHost.Children = @($child,$child)
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
    }
    It 'rejects a linked tooltip that covers the header instead of granting a host class exemption' {
        $tip.Current.BoundingRectangle = [Windows.Rect]::new(400,480,100,100)
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
    }
    It 'rejects stale fresh child identity and over-depth associations' {
        $popupHost | Add-Member -NotePropertyName Reads -NotePropertyValue 0
        $popupHost | Add-Member ScriptMethod FindAll { param($scope,$condition)
            $this.Reads++
            if ($this.Reads -gt 1) { $this.Id = @(42,1771825) }
            @($this.Children)
        } -Force
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $popupHost.Id = @(42,1771824)
        $tip.Parent = $null
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
    }
    It 'rejects an association beyond eight ancestors and a foreign native root' {
        $node = $child
        foreach ($i in 1..8) {
            $link = New-OverlayPeer Border Pane 0 @(42,900,$i) ([Windows.Rect]::Empty) $true
            $link.Parent = $node
            $node = $link
        }
        $tip.Parent = $node
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
        $tip.Parent = $child
        $nativeRoot = { param($handle) [IntPtr]999 }
        & $script:pointPredicate $popupHost '' $point $app $peers | Should -BeTrue
    }
}

Describe 'Modern Agents surface fixture contract' -Tag 'Unit' {
    It 'uses native header and History identities without restoring the removed toolbar or enabling search' {
        $refresh = Read-FixtureAst Feature.SessionRefresh
        $refresh.Extent.Text | Should -Not -Match 'TabHistoryButton|HistorySearchTextBox'
        $refresh.Extent.Text | Should -Match 'VerticalTabsHeaderButton'
        $refresh.Extent.Text | Should -Match 'Test-NativeVisible -Id HistoryList'
        $refresh.Extent.Text | Should -Match "Should -Be 'Agents'"
        $refresh.Extent.Text | Should -Match 'Test-NativeVisible -Id SearchTextBox \| Should -BeFalse'
    }

    It 'keeps the physical Ctrl Shift slash accelerator and native Agents oracle' {
        $hotkeys = Read-FixtureAst Feature.AgentHotkeys
        $hotkeys.Extent.Text | Should -Match 'Send-WtWindowKey -App \$App -Vk 0xBF -Ctrl -Shift -RequireForeground'
        $hotkeys.Extent.Text | Should -Match "AutomationIdProperty, 'VerticalTabsHeader'"
        $hotkeys.Extent.Text | Should -Match "header.Current.Name -eq 'Agents'"
    }
}

Describe 'Canonical header actual run receipt handoff' -Tag 'Unit' {
    BeforeAll {
        $helperAst = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\helpers\TabHeaderContext.ps1'), [ref]$null, [ref]$null)
        $leaseIf = @($helperAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.IfStatementAst] -and
                $node.Clauses[0].Item1.Extent.Text -like '$runToken -and*$env:ITE2E_OWNED_PROCESS_RECEIPT*'
        }, $true))
        $leaseIf.Count | Should -Be 1
        $script:runReceiptSource = [scriptblock]::Create($leaseIf[0].Extent.Text)
        $script:savedRunToken = $env:ITE2E_RUN_TOKEN
        $script:savedReceiptPath = $env:ITE2E_OWNED_PROCESS_RECEIPT
    }

    Describe 'Canonical header rejection diagnostics' -Tag 'Unit' {
        It 'records only the requested public fields for a fixture-owned matched peer' {
            $peer = @{ Current = @{
                ProcessId = 42; ControlType = [Windows.Automation.ControlType]::Menu
                AutomationId = 'fixture-menu'; ClassName = 'MenuFlyoutPresenter'; IsOffscreen = $false
                BoundingRectangle = [Windows.Rect]::new(10, 20, 30, 40); Name = 'PRIVATE-NAME-CANARY'
            } }
            $receipt = Get-TestTabHeaderOverlayReceipt $peer 42
            @($receipt.Keys | Sort-Object) | Should -Be @('AutomationId', 'Bounds', 'Class', 'ControlType', 'IsOffscreen')
            $receipt.Bounds | Should -Be '10,20,30,40'
            ($receipt | ConvertTo-Json) | Should -Not -Match 'PRIVATE-NAME-CANARY|ProcessId|History|Tree'
        }

        Describe 'Keep context flyout normalization' -Tag 'Unit' {
            It 'dismisses the invoked Keep action before reopening in both existing cases' {
                $source = (Read-FixtureAst Feature.KeepRunningFocus).Extent.Text
                $pattern = 'Invoke-UiElement[^\r\n]+KeepTabRunningMenuItem[^\r\n]*\r?\n\s+Close-TestOwnedTabFlyout -App \$script:app[^\r\n]*\r?\n\s+Invoke-TestTabHeaderContextMenu'
                ([regex]::Matches($source, $pattern)).Count | Should -Be 2
            }
            It 'uses owned public Escape and waits for absence without changing header safety guards' {
                $source = (Get-Command Close-TestOwnedTabFlyout).Definition
                $source | Should -Match '\$current.StartTime -ne \$App.OwnedProcess.StartTime'
                $source | Should -Match '\$current.Path -ne'
                $source | Should -Match '\$_.run_token -ceq \$runToken'
                $source | Should -Match 'UtcDateTime.Ticks'
                $source | Should -Match 'IsOwnedRootOrPopup'
                $source | Should -Match 'GetAncestor\(\$hit, 2\) -ne \$root'
                $source | Should -Match '\$checkEscapeState ='
                $source | Should -Match 'foreach \(\$escape in 1\.\.2\)'
                $source | Should -Match '::keybd_event\(\[byte\]0x1B, 0, 0,'
                $source | Should -Match '::keybd_event\(\[byte\]0x1B, 0, 0x2,'
                $source | Should -Match 'GetForegroundWindow\(\) -ne \$root'
                $source | Should -Match '\$cursor.X -ne \$point.X'
                $source | Should -Match '\$cursor.Y -ne \$point.Y'
                $source | Should -Match 'Wait-UiElement -App \$App -Selector KeepTabRunningMenuItem -Gone -TimeoutSec 5'
                $source | Should -Match 'Test-KnownTabHeaderOverlayPeer'
                $source.IndexOf('SetCursorPos(') | Should -BeLessThan $source.IndexOf('$checkEscapeState =')
                $source.IndexOf('& $checkEscapeState', $source.IndexOf('foreach ($escape')) |
                    Should -BeLessThan $source.IndexOf('& $deliverEscape', $source.IndexOf('foreach ($escape'))
                $source | Should -Not -Match 'Send-WtWindowKey|Set-WtWindowForeground|ForceForeground|ClickOwnedCaption|SetFocus|Invoke-UiElement|\.Hide\('
                { Assert-TestTabHeaderPointSafety @{ OpaqueRoot = $true; NoKnownOverlay = $false } } | Should -Throw
            }
            It 'refuses a non-owned or stale process before sending dismissal input' {
                $start = [datetime]'2026-10-06T10:28:22Z'
                $app = @{ Pid = 42; Launched = $false; InstallLocation = 'C:\fixture'
                    OwnedProcess = @{ Id = 42; HasExited = $false; StartTime = $start } }
                Mock Get-Process { @{ StartTime = $start; Path = 'C:\fixture\WindowsTerminal.exe' } }
                Mock Send-WtWindowKey {}
                { Close-TestOwnedTabFlyout -App $app } | Should -Throw '*original owned process lease*'
                $app.Launched = $true
                $app.OwnedProcess.StartTime = $start.AddTicks(1)
                { Close-TestOwnedTabFlyout -App $app } | Should -Throw '*original owned process lease*'
                Should -Invoke Send-WtWindowKey -Times 0
            }
            It 'stops before <AllowedSends> sends when <Change> occurs after cursor placement or first Escape' -ForEach @(
                @{ Change = 'held input after cursor placement'; StopCheck = 1; AllowedSends = 0 },
                @{ Change = 'held input after first Escape'; StopCheck = 2; AllowedSends = 1 },
                @{ Change = 'foreign foreground after cursor placement'; StopCheck = 1; AllowedSends = 0 },
                @{ Change = 'foreign foreground after first Escape'; StopCheck = 2; AllowedSends = 1 }
            ) {
                $ast = [Management.Automation.Language.Parser]::ParseInput(
                    (Get-Command Close-TestOwnedTabFlyout).Definition, [ref]$null, [ref]$null)
                $loop = $ast.FindAll({
                    param($node)
                    $node -is [Management.Automation.Language.ForEachStatementAst] -and $node.Variable.VariablePath.UserPath -eq 'escape'
                }, $true)[0]
                $trace = @{ Checks = 0; Sends = 0 }
                $checkEscapeState = {
                    $trace.Checks++
                    if ($trace.Checks -eq $StopCheck) { throw $Change }
                }
                $deliverEscape = { $trace.Sends++ }
                { . ([scriptblock]::Create($loop.Extent.Text)) } | Should -Throw "*$Change*"
                $trace.Sends | Should -Be $AllowedSends
                $trace.Checks | Should -Be $StopCheck
            }
            It 'records successful menu-Gone and the actual last remaining predicate only after timeout' {
                $source = (Get-Command Close-TestOwnedTabFlyout).Definition
                $source.IndexOf('$menuGone = $false') | Should -BeLessThan $source.IndexOf('Wait-UiElement')
                $source.IndexOf('Wait-UiElement') | Should -BeLessThan $source.IndexOf('$menuGone = $true')
                $source | Should -Match 'MenuGone = \$menuGone'
                $source | Should -Match '\$settle.Clear = \$matched.Count -eq 0'
                $source | Should -Match 'RemainingPredicate = ''KnownOverlayAbsent''; PredicateClear = \$settle.Clear'
                $source | Should -Match 'Get-TestTabHeaderOverlayReceipt \$peer \$App.Pid'
                $source | Should -Not -Match '\$peer.Current.Name|HistoryList|Get-UiTree'
            }
            It 'preserves the original absence timeout without delivering extra input or exposing names' {
                $ast = [Management.Automation.Language.Parser]::ParseInput(
                    (Get-Command Close-TestOwnedTabFlyout).Definition, [ref]$null, [ref]$null)
                $catch = $ast.FindAll({
                    param($node)
                    $node -is [Management.Automation.Language.CatchClauseAst] -and $node.Body.Extent.Text -like '*throw $timeoutError*'
                }, $true)[0].Body.Extent.Text
                $catch | Should -Match '\$timeoutError = \$_'
                $catch | Should -Match 'throw \$timeoutError'
                $catch | Should -Match 'ExtraInputDelivered = \$false'
                $catch | Should -Match 'MatchedOverlays = @\(\$settle.Matched.ToArray\(\)\)'
                $catch | Should -Not -Match 'keybd_event|SendInput|SetCursorPos|SetForeground|Invoke-Ui|return \$true|Current.Name'
            }
        }
        It 'does not serialize metadata for a foreign matched peer' {
            $peer = @{ Current = @{
                ProcessId = 99; Name = 'FOREIGN-PRIVATE-CANARY'; AutomationId = 'foreign-private'
            } }
            Get-TestTabHeaderOverlayReceipt $peer 42 | Should -BeNullOrEmpty
        }
        It 'names actual false clauses but not short-circuited checks' {
            $facts = @{
                FullyVisible = $false; InHeaderBand = $true; OutsideActions = $true
                StableGeometry = $false; NoKnownOverlay = $null; SameHitPeer = $null
            }
            @(Get-TestTabHeaderFinalReasons $facts) | Should -Be @('FullyVisible', 'StableGeometry')
        }
        It 'distinguishes overlay and hit-peer failures without fabricating a pass' {
            @(Get-TestTabHeaderFinalReasons @{ NoKnownOverlay = $false; SameHitPeer = $null }) |
                Should -Be @('NoKnownOverlay')
            @(Get-TestTabHeaderFinalReasons @{ NoKnownOverlay = $true; SameHitPeer = $false }) |
                Should -Be @('SameHitPeer')
            @(Get-TestTabHeaderFinalReasons @{ NoKnownOverlay = $true; SameHitPeer = $true }) |
                Should -BeNullOrEmpty
            { Assert-TestTabHeaderPointSafety @{ OpaqueRoot = $true; NoKnownOverlay = $false } } |
                Should -Throw
        }
        It 'keeps the original final rejection and cannot deliver input from its diagnostic branch' {
            $ast = [Management.Automation.Language.Parser]::ParseFile(
                (Join-Path $PSScriptRoot '..\tests\helpers\TabHeaderContext.ps1'), [ref]$null, [ref]$null)
            $finalIf = $ast.FindAll({
                param($node)
                $node -is [Management.Automation.Language.IfStatementAst] -and
                    $node.Clauses[0].Item1.Extent.Text -like '*$final.FullyVisible*'
            }, $true)[0]
            $body = $finalIf.Clauses[0].Item2.Extent.Text
            $body | Should -Match "throw 'Canonical tab header geometry, hit peer or known overlay changed before physical input.'"
            $body | Should -Match 'false_clauses'
            $body | Should -Match 'right_input_delivered = \$false'
            $body | Should -Not -Match 'SendInput|SetCursorPos|Invoke-Ui|return \$true'
            $body.IndexOf('original guard rejection is unchanged.') |
                Should -BeLessThan $body.LastIndexOf("throw 'Canonical")
            $script:headerSource | Should -Match '\$matched.Count -eq 0'
            $script:headerSource | Should -Match '\$finalOverlayClear = & \$overlayClear'
            $script:headerSource | Should -Match '\$finalHitSame = \[Windows.Automation.Automation\]::Compare'
        }
    }
    BeforeEach {
        $env:ITE2E_RUN_TOKEN = '0a576dce948c4a23ad7867695bbe8d4d'
        $env:ITE2E_OWNED_PROCESS_RECEIPT = Join-Path $TestDrive 'owned-processes.jsonl'
        $runToken = $env:ITE2E_RUN_TOKEN
        $runLease = $false
        $record = @{
            pid = 33588; path = 'C:\fixture\WindowsTerminal.exe'
            start_utc = '2026-10-06T10:28:22.3687155Z'; run_token = $runToken
        }
        $App = [pscustomobject]@{
            Pid = $record.pid
            OwnedProcess = [pscustomobject]@{
                Id = $record.pid; Path = $record.path
                StartTime = [datetimeoffset]::Parse($record.start_utc).LocalDateTime
            }
        }
        $record | ConvertTo-Json -Compress | Set-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT
    }
    AfterAll {
        $env:ITE2E_RUN_TOKEN = $script:savedRunToken
        $env:ITE2E_OWNED_PROCESS_RECEIPT = $script:savedReceiptPath
    }
    It 'binds actual default JSON date values to exact native process UTC ticks' {
        . $script:runReceiptSource
        if ($PSVersionTable.PSVersion -ge [version]'7.5') { $records[0].start_utc | Should -BeOfType ([datetime]) }
        ([datetimeoffset]$records[0].start_utc).UtcDateTime.Ticks |
            Should -Be $App.OwnedProcess.StartTime.ToUniversalTime().Ticks
        $runLease | Should -BeTrue
    }
    It 'also handles older parser string dates without requiring the newer DateKind switch' {
        Mock ConvertFrom-Json { [pscustomobject]$record }
        . $script:runReceiptSource
        $records[0].start_utc | Should -BeOfType ([string])
        $runLease | Should -BeTrue
    }
    It 'matches the same original process start when its Process projection uses UTC kind' {
        $App.OwnedProcess.StartTime = [datetimeoffset]::Parse($record.start_utc).UtcDateTime
        . $script:runReceiptSource
        $runLease | Should -BeTrue
    }
    It 'refuses an absent captured run token' {
        $runToken = ''
        . $script:runReceiptSource
        $runLease | Should -BeFalse
    }
    It 'refuses a missing owned receipt file' {
        $env:ITE2E_OWNED_PROCESS_RECEIPT = Join-Path $TestDrive 'not-created.jsonl'
        . $script:runReceiptSource
        $runLease | Should -BeFalse
    }
    It 'refuses an absent original process projection' {
        $App.OwnedProcess = $null
        . $script:runReceiptSource
        $runLease | Should -BeFalse
    }
    It 'refuses a mismatched <Field> without accepting the other matching fields' -ForEach @(
        @{ Field = 'pid'; Wrong = 19704 },
        @{ Field = 'path'; Wrong = 'C:\neighbor\WindowsTerminal.exe' },
        @{ Field = 'run_token'; Wrong = 'different-run' }
    ) {
        $record[$Field] = $Wrong
        $record | ConvertTo-Json -Compress | Set-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT
        . $script:runReceiptSource
        $runLease | Should -BeFalse
    }
    It 'refuses stale process starts down to one 100ns tick' {
        $record.start_utc = [datetimeoffset]::Parse($record.start_utc).AddTicks(1).ToString("yyyy-MM-dd'T'HH:mm:ss.fffffff'Z'")
        $record | ConvertTo-Json -Compress | Set-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT
        . $script:runReceiptSource
        $runLease | Should -BeFalse
    }
    It 'refuses duplicate matching lease records rather than selecting the first' {
        @($record, $record) | ForEach-Object { $_ | ConvertTo-Json -Compress } |
            Set-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT
        . $script:runReceiptSource
        $runLease | Should -BeFalse
    }
    It 'fails closed on a missing or invalid receipt date' -ForEach @(
        @{ BadDate = $null }, @{ BadDate = 'not-a-timestamp' }
    ) {
        $record.start_utc = $BadDate
        $record | ConvertTo-Json -Compress | Set-Content -LiteralPath $env:ITE2E_OWNED_PROCESS_RECEIPT
        { . $script:runReceiptSource } | Should -Throw
        $runLease | Should -BeFalse
    }
}
