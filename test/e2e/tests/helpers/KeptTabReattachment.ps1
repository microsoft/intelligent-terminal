function Test-TestKeptTabIdentity {
    param($Windows, $Context, $Panes, [string]$WindowId, [string]$RequestedPane,
        [string]$Title, [int]$ExpectedTabs, [string[]]$RetainedPaneIds)
    $owned = @($Windows | Where-Object { [string]$_.window_id -eq $WindowId })
    $null -ne $Context -and $owned.Count -eq 1 -and @($Windows).Count -eq 1 -and
        [int]$owned[0].tab_count -eq $ExpectedTabs -and
        [string]$Context.pane.session_id -eq $RequestedPane -and
        [string]$Context.pane.window_id -eq $WindowId -and $Context.pane.title -eq $Title -and
        @($Panes | Where-Object { $_.session_id -eq $RequestedPane }).Count -eq 1 -and
        @($RetainedPaneIds | Where-Object { $_ -notin @($Panes.session_id) }).Count -eq 0 -and
        @($Panes | Where-Object { [string]$_.tab_id -ne [string]$Context.pane.tab_id -or
            [string]$_.window_id -ne $WindowId }).Count -eq 0
}

function Wait-TestKeptTabReattachment {
    param($App, [string]$RequestedPane, [string]$Title, [int]$ExpectedTabs,
        [string[]]$RetainedPaneIds, $OriginalPids, $OriginalHelper)
    Assert-TestPackagedProfileOwner $App
    # GetProtocolPanes walks the visible root tree; the stashed helper is retained
    # separately. Its original PID/session are checked below, not fabricated into that tree.
    $shellPaneIds = @($RetainedPaneIds | Where-Object { $_ -ne $OriginalHelper.PaneSessionId })
    Wait-Until -TimeoutSec 10 -Because 'the exact original kept group is attached and its canonical header is rendered' -Condition {
        $context = Invoke-WtCli -App $App -Arguments @('get-pane-context','--target',$RequestedPane,'--max-lines','0','--max-chars','0')
        $windows = @(Get-WtWindows -App $App)
        $panes = @(Get-WtPanes -App $App -WindowId $App.WindowId -TabId $context.pane.tab_id)
        if (-not (Test-TestKeptTabIdentity $windows $context $panes ([string]$App.WindowId) $RequestedPane $Title $ExpectedTabs $shellPaneIds)) {
            return $false
        }
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $root = [IntPtr][long]$App.Hwnd
        if ([ItE2E.ItWtWin32Input]::GetWindowProcessId($root) -ne $App.Pid -or
            [ItE2E.ItWtWin32Input]::GetAncestor($root,2) -ne $root -or
            -not [ItE2E.ItWtWin32Input]::IsWindowVisible($root)) { throw 'Reattached header window changed ownership or visibility.' }
        $window = [Windows.Automation.AutomationElement]::FromHandle($root)
        if ($window.Current.ProcessId -ne $App.Pid) { throw 'Reattachment UIA root belongs to another process.' }
        $list = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty,'ItemsList'))
        if (-not $list -or $list.Current.IsOffscreen) { return $false }
        $walker = [Windows.Automation.TreeWalker]::RawViewWalker
        $texts = $list.FindAll([Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$Title),
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Text)))
        $headers = @(
            foreach ($text in $texts) {
                if ($text.Current.ProcessId -ne $App.Pid -or $text.Current.IsOffscreen -or
                    $text.Current.BoundingRectangle.Width -le 0 -or $text.Current.BoundingRectangle.Height -le 0 -or
                    -not $list.Current.BoundingRectangle.Contains($text.Current.BoundingRectangle)) { continue }
                $ancestor = $walker.GetParent($text)
                while ($ancestor -and -not [Windows.Automation.Automation]::Compare($ancestor,$list)) {
                    if ($ancestor.Current.AutomationId -eq 'PaneActivateButton') { break }
                    if ($ancestor.Current.ControlType -eq [Windows.Automation.ControlType]::ListItem) {
                        if ($ancestor.Current.ProcessId -eq $App.Pid -and -not $ancestor.Current.IsOffscreen) { $text }
                        break
                    }
                    $ancestor = $walker.GetParent($ancestor)
                }
            }
        )
        $headers.Count -eq 1
    } | Out-Null
    foreach ($id in $RetainedPaneIds) {
        $status = Get-WtPaneStatus -App $App -SessionId $id
        $status.pid | Should -Be $OriginalPids[$id]
        $status.state | Should -Be 'running'
    }
    $helper = Get-AgentPaneSession -App $App -PaneSessionId $OriginalHelper.PaneSessionId
    $helper.AcpSessionId | Should -Be $OriginalHelper.AcpSessionId
    $helper.HelperProcessId | Should -Be $OriginalHelper.HelperProcessId
}
