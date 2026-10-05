#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    . (Join-Path $PSScriptRoot '..\tests\helpers\TestTerminalCleanup.ps1')
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $fixtureAst = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
    $checkedCleanup = $fixtureAst.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Invoke-CombinedCheckedCleanup'
    }, $true)[0]
    . ([scriptblock]::Create($checkedCleanup.Extent.Text))

    function Get-FixtureCleanup([string]$File, [string]$DescribeName) {
        $tokens = $null
        $errors = $null
        $path = Join-Path $PSScriptRoot ("..\tests\" + $File)
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        if ($errors) { throw 'Fixture source could not be parsed.' }
        $describe = $ast.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst] -and
            $node.GetCommandName() -eq 'Describe' -and $node.CommandElements[1].Value -eq $DescribeName
        }, $true)[0]
        $cleanup = $describe.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'AfterAll'
        }, $true)[0]
        $body = $cleanup.CommandElements[1].ScriptBlock.Extent.Text
        [scriptblock]::Create($body.Substring(1, $body.Length - 2))
    }
}

Describe 'Combined cleanup failure preservation' -Tag 'Unit' {
    It 'retains original and cleanup exceptions independently' {
        $primary = [Management.Automation.ErrorRecord]::new([Exception]::new('original'),
            'original', [Management.Automation.ErrorCategory]::NotSpecified, $null)
        try {
            Invoke-CombinedCheckedCleanup -PrimaryFailure $primary -Action { throw 'cleanup' }
            throw 'Expected aggregate'
        }
        catch {
            $_.Exception | Should -BeOfType ([AggregateException])
            $_.Exception.InnerExceptions.Count | Should -Be 2
            $_.Exception.InnerExceptions[0].Message | Should -Be 'original'
            $_.Exception.InnerExceptions[1].Message | Should -Be 'cleanup'
        }
    }
    It 'never suppresses cleanup failure without a primary failure' {
        { Invoke-CombinedCheckedCleanup -Action { throw 'cleanup' } } | Should -Throw '*cleanup*'
    }
}

Describe 'Combined History target safety' -Tag @('Unit', 'CombinedHistoryTargetSafety') {
    BeforeAll {
        $historyFunction = $fixtureAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Invoke-CombinedHistoryRow'
        }, $true)[0]
        . ([scriptblock]::Create($historyFunction.Extent.Text))
        foreach ($name in @('isReady', 'getTarget')) {
            $assignment = $historyFunction.FindAll({
                param($node)
                $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                    $node.Left.Extent.Text -eq ('$' + $name)
            }, $true)[0]
            . ([scriptblock]::Create($assignment.Extent.Text))
        }
        function Get-CombinedSnapshot {}
        function Get-CombinedElement {}
        function Get-CombinedRows {}
        function Get-CombinedRawChildren {}
        $script:app = [pscustomobject]@{ Pid = 42 }
        $script:evidence = $TestDrive
    }
    It 'rejects clipped rows even with a visible nonzero rectangle' {
        $list = [pscustomobject]@{ Current = @{ IsOffscreen = $false; BoundingRectangle = @{
            Left = 10; Top = 10; Right = 110; Bottom = 110; Width = 100; Height = 100
        } } }
        $row = [pscustomobject]@{ Current = @{ IsOffscreen = $false; BoundingRectangle = @{
            Left = 10; Top = 100; Right = 110; Bottom = 130; Width = 100; Height = 30
        } } }
        (& $isReady $row $list) | Should -BeFalse
        $row.Current.BoundingRectangle.Top = 80
        $row.Current.BoundingRectangle.Bottom = 110
        (& $isReady $row $list) | Should -BeTrue
    }
    It 'rejects a sole row with the wrong controlled title' {
        $Title = 'intended'
        Mock Get-UiValue { 'intended' }
        Mock Get-CombinedRows { [pscustomobject]@{ Current = @{ ProcessId = 42 } } }
        Mock Get-CombinedRawChildren { [pscustomobject]@{ Current = @{ ControlType = [Windows.Automation.ControlType]::Text; Name = 'other' } } }
        { & $getTarget } | Should -Throw '*controlled title*'
    }
    It 'preserves the original failure when diagnostics see a <Kind> viewport' -ForEach @(
        @{ Kind = 'null' }, @{ Kind = 'stale' }
    ) {
        Mock Get-CombinedSnapshot { throw 'original-target-failure' }
        Mock Get-CombinedElement {
            if ($Kind -eq 'stale') { throw 'stale-viewport' }
            $null
        }
        Mock Set-Content {}
        { Invoke-CombinedHistoryRow -Title intended -SessionId exact } | Should -Throw '*original-target-failure*'
        Should -Invoke Set-Content -Times 1 -Exactly -ParameterFilter {
            $Path -like '*history-action-diagnostic-error.json'
        }
    }
}

Describe 'Owned caption foreground safety' -Tag 'Unit' {
    It 'fails closed on an unknown owned popup without changing foreground' {
        InModuleScope ItE2E {
            Initialize-WtWin32Input
            $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
            [ItE2E.ItWtWin32Input]::IsOwnedRootOrPopup([IntPtr]::Zero, [IntPtr]::Zero, 1) | Should -BeFalse
            [ItE2E.ItWtWin32Input]::GetForegroundWindow() | Should -Be $foreground
        }
    }
    It 'gates physical clicks but does not refocus UIA pattern operations' {
        InModuleScope ItE2E {
            $source = (Get-Command Invoke-WinAppUi).Definition
            $source | Should -Match "\`$UiArgs\[0\] -eq 'click' -and -not \(Set-WtWindowForeground"
            $source | Should -Match '\$current.StartTime -ne \$App.OwnedProcess.StartTime'
            $source | Should -Match 'GetAncestor\(\$hwnd, 2\) -ne \$hwnd'
        }
    }
    It 'rejects an invalid root without selecting a click point or changing foreground' {
        InModuleScope ItE2E {
            Initialize-WtWin32Input
            $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
            [ItE2E.ItWtWin32Input]::ClickOwnedCaption([IntPtr]::Zero, 1) | Should -BeFalse
            [ItE2E.ItWtWin32Input]::ClickOwnedPoint([IntPtr]::Zero, 1, 0, 0) | Should -BeFalse
            [ItE2E.ItWtWin32Input]::LastCaptionPoint | Should -BeNullOrEmpty
            [ItE2E.ItWtWin32Input]::GetForegroundWindow() | Should -Be $foreground
        }
    }
    It 'rejects a mismatched target PID before acquiring foreground' {
        $app = [pscustomobject]@{ Hwnd = 1; Pid = 1 }
        { Set-WtWindowForeground -App $app -Attempts 1 } | Should -Throw '*no longer belongs*'
    }
    It 'has no caption injection bypass and guards the final paired mouse input' {
        $source = Get-Content (Join-Path $PSScriptRoot '..\ItE2E\Public\Ui.ps1') -Raw
        $caption = [regex]::Match($source, '(?s)public static bool ClickOwnedCaption\b.*?(?=\r?\n    // Attach only)').Value
        $point = [regex]::Match($source, '(?s)public static bool ClickOwnedPoint\b.*?(?=\r?\n    // A real caption)').Value
        $caption | Should -Not -BeNullOrEmpty
        $point | Should -Not -BeNullOrEmpty
        $caption | Should -Not -Match '\bSendInput\s*\('
        $caption | Should -Match 'if \(!ClickOwnedPoint\(hWnd, pid, point\.X, point\.Y\)\)'
        $caption.LastIndexOf('SendMessageTimeout(') | Should -BeLessThan $caption.IndexOf('ClickOwnedPoint(')
        $point | Should -Match 'new int\[\] \{ 1, 2, 4, 5, 6, 16, 17, 18, 91, 92 \}'
        $point | Should -Match 'if \(IsKeyDown\(key\)\) return false;'
        $point | Should -Match 'GetWindowProcessId\(root\) != pid'
        $point | Should -Match 'cursor\.X != x \|\| cursor\.Y != y'
        $point | Should -Match 'GetAncestor\(pointWindow, 2\) != root \|\| GetWindowProcessId\(pointWindow\) != pid\) return false'
        $point | Should -Match 'inputs\[0\]\.data\.mouse\.dwFlags = 2;'
        $point | Should -Match 'inputs\[1\]\.data\.mouse\.dwFlags = 4;'
        $point.IndexOf('IsKeyDown(') | Should -BeLessThan $point.IndexOf('GetCursorPos(')
        $point.IndexOf('GetCursorPos(') | Should -BeLessThan $point.IndexOf('WindowFromPoint(')
        $point.IndexOf('GetWindowProcessId(pointWindow)') | Should -BeLessThan $point.IndexOf('SendInput(')
        ([regex]::Matches($point, '\bSendInput\s*\(')).Count | Should -Be 1
    }
}

Describe 'Background canonical header context source safety' -Tag 'Unit' {
    BeforeAll {
        $script:groupContextSource = $fixtureAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Invoke-CombinedOwnedGroupContext'
        }, $true)[0].Extent.Text
    }
    It 'targets the unique canonical header rather than the expanded group container' {
        $script:groupContextSource | Should -Match 'get-pane-context'
        $script:groupContextSource | Should -Match '\$tabs \| Where-Object title -eq \$Title'
        $script:groupContextSource | Should -Match 'BoundingRectangle.Height / 2\) -ge \$toggle.Top'
        $script:groupContextSource | Should -Match 'BoundingRectangle.Height / 2\) -le \$toggle.Bottom'
        $script:groupContextSource | Should -Match '\$bounds.Contains\(\$point\)'
        $script:groupContextSource | Should -Match 'Canonical header geometry changed'
        $script:groupContextSource | Should -Not -Match '\.SetFocus\(|-Vk 0x79|Invoke-UiMouseDrag'
    }
    It 'checks the original lease and immediate Win32 point ownership before paired right input' {
        $source = $script:groupContextSource
        $source | Should -Match '\$current.StartTime -ne \$script:app.OwnedProcess.StartTime'
        $source | Should -Match 'GetForegroundWindow\(\) -ne \$root'
        $source | Should -Match 'GetAncestor\(\$nativeHit, 2\) -ne \$root'
        $source | Should -Match 'GetWindowProcessId\(\$nativeHit\) -ne \$script:app.Pid'
        $source | Should -Match '@\(1, 2, 4, 5, 6, 16, 17, 18, 91, 92\)'
        $source | Should -Match 'cursor.X -ne \$nativePoint.X'
        $source | Should -Match 'cursor.Y -ne \$nativePoint.Y'
        $source.IndexOf('original owned process lease') | Should -BeLessThan $source.IndexOf('::SetCursorPos(')
        $source.IndexOf('refuses held input.') | Should -BeLessThan $source.IndexOf('::SetCursorPos(')
        $source | Should -Match '\$cursorMoved -and -not \$clickDelivered'
        $source | Should -Match 'Cursor restoration refused while a mouse button is held'
        $source.LastIndexOf('WindowFromPoint(') | Should -BeLessThan $source.IndexOf('::SendInput(')
        $source | Should -Match '\$mouse.dwFlags = 0x0008'
        $source | Should -Match '\$mouse.dwFlags = 0x0010'
        $source | Should -Match 'finally \{\s+\[void\].*SetThreadDpiAwarenessContext\(\$dpi\)'
        ([regex]::Matches($source, '::SendInput\(')).Count | Should -Be 1
    }
    It 'constructs right-down and right-up structs without injecting input' {
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $source = $script:groupContextSource
        $start = $source.IndexOf('$down =')
        $end = $source.IndexOf('$dpi =', $start)
        . ([scriptblock]::Create($source.Substring($start, $end - $start)))
        $inputs.Count | Should -Be 2
        $inputs[0].type | Should -Be 0
        $inputs[0].data.mouse.dwFlags | Should -Be 8
        $inputs[1].data.mouse.dwFlags | Should -Be 16
    }
}

Describe 'Representative mouse helper regressions' -Tag 'Unit' {
    It 'finds exact Unicode text and trims a surplus glyph in provider units' -Tag 'UiTextBounds' {
        InModuleScope ItE2E {
            $text = 'A' + [char]::ConvertFromUtf32(0x1F600)
            $range = [pscustomobject]@{ Text = $text + [char]::ConvertFromUtf32(0x1F600); Moves = 0 }
            $range | Add-Member ScriptMethod GetText { param($limit) $this.Text }
            $range | Add-Member ScriptMethod MoveEndpointByUnit {
                param($endpoint, $unit, $count)
                if ("$endpoint" -ne 'End' -or "$unit" -ne 'Character' -or $count -ne -1) {
                    throw 'Expected one provider-character step.'
                }
                $this.Text = $this.Text.Substring(0, $this.Text.Length - 2)
                $this.Moves++
                -1
            }
            $document = [pscustomobject]@{ Range = $range; ExpectedText = $text }
            $document | Add-Member ScriptMethod FindText {
                param($value, $backward, $ignoreCase)
                if ($value -cne $this.ExpectedText -or $backward -or $ignoreCase) {
                    throw 'Expected a forward literal text search.'
                }
                $this.Range
            }

            $result = Find-ItExactTextRange -DocumentRange $document -Text $text
            [string]::Equals($result.GetText(-1), $text, [StringComparison]::Ordinal) | Should -BeTrue
            $result.Moves | Should -Be 1
        }
    }

    It 'sends complete Escape events without leaving a prefix for the next character' -Tag 'MouseInput' {
        Mock -ModuleName ItE2E Invoke-WtCli {}
        Mock -ModuleName ItE2E Send-AgentWin32Key {}
        Mock -ModuleName ItE2E Start-Sleep {}
        Send-AgentKey -App ([pscustomobject]@{ Pid = 1 }) -PaneSessionId 'owned-pane' -Key Escape -Count 2 | Out-Null
        Should -Invoke -ModuleName ItE2E Send-AgentWin32Key -Times 2 -Exactly -ParameterFilter {
            $PaneSessionId -eq 'owned-pane' -and $Vk -eq 0x1B -and $Sc -eq 1 -and $Uc -eq 27
        }
        Should -Invoke -ModuleName ItE2E Invoke-WtCli -Times 0 -Exactly
    }

    It 'confirms subscription before emitting the owner-tab probe' -Tag 'OwnerProbeReady' {
        $script:ownerProbeReady = $false
        Mock -ModuleName ItE2E Start-WtEventListener {
            param($App, $WaitForReady)
            $script:ownerProbeReady = [bool]$WaitForReady
            [pscustomobject]@{ fixture = $true }
        }
        Mock -ModuleName ItE2E Start-Sleep {}
        Mock -ModuleName ItE2E Invoke-RunCommand {
            if (-not $script:ownerProbeReady) { throw 'Probe emitted before subscription readiness.' }
        }
        Mock -ModuleName ItE2E Wait-WtEvent { [pscustomobject]@{ params = @{ tab_id = 'owned-tab' } } }
        Mock -ModuleName ItE2E Stop-WtEventListener {}
        Resolve-AgentOwnerTabId -App ([pscustomobject]@{}) -OwnerPaneSessionId 'owned-pane' | Should -Be 'owned-tab'
        Should -Invoke -ModuleName ItE2E Start-WtEventListener -Times 1 -Exactly -ParameterFilter { $WaitForReady }
        Should -Invoke -ModuleName ItE2E Stop-WtEventListener -Times 1 -Exactly
    }

    It 'finds package helpers without matching a neighboring installation' -Tag 'PackageRefusal' {
        $script:packageRoot = Join-Path $TestDrive 'selected-package'
        Mock -ModuleName ItE2E Get-Process {
            @(
                [pscustomobject]@{ Id = 41; Path = (Join-Path $script:packageRoot 'wta.exe') },
                [pscustomobject]@{ Id = 42; Path = (Join-Path ($script:packageRoot + '-other') 'wta.exe') }
            )
        }
        Mock -ModuleName ItE2E Stop-Process { throw 'Discovery must not terminate anything.' }
        $matches = @(Get-WtProcessesForApp -App ([pscustomobject]@{ InstallLocation = $script:packageRoot }) -IncludePackageExecutables)
        $matches.Count | Should -Be 1
        $matches[0].Id | Should -Be 41
        Should -Invoke -ModuleName ItE2E Stop-Process -Times 0 -Exactly
    }

    It 'does not substitute Store when the explicitly selected Dev package is absent' -Tag 'SelectedPackage' {
        Mock -ModuleName ItE2E Get-AppxPackage {
            [pscustomobject]@{ PackageFamilyName = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe' }
        }
        Resolve-ItApp -Package Dev -IfInstalled | Should -BeNullOrEmpty
    }

    It 'does not infer process ownership when startup returned no launch context' -Tag 'StartupRecovery' {
        $target = [pscustomobject]@{ WindowsTerminal = (Join-Path $TestDrive 'WindowsTerminal.exe') }
        Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 77 } }
        Mock Stop-Terminal {}
        Mock Restore-WtConfig {}
        { Stop-TestTerminal -Target $target -LaunchStarted ([DateTime]::UtcNow) } |
            Should -Throw '*no returned launch context*'
        Should -Invoke Stop-Terminal -Times 0 -Exactly
        Should -Invoke Restore-WtConfig -Times 0 -Exactly
    }

    It 'retains configuration backups if the package remains active after teardown' -Tag 'RecoveryPostcondition' {
        $app = [pscustomobject]@{ Pid = 77; Launched = $true; InstallLocation = $TestDrive }
        Mock Stop-Terminal {}
        Mock Test-Until { $false }
        Mock Restore-WtConfig {}
        { Stop-TestTerminal -App $app } | Should -Throw '*still active*'
        Should -Invoke Stop-Terminal -Times 1 -Exactly -ParameterFilter { -not $RestoreSettings }
        Should -Invoke Restore-WtConfig -Times 0 -Exactly
    }
}

Describe 'Representative fixture cleanup regressions' -Tag 'Unit' {
    BeforeEach {
        $script:app = $null
        $script:target = $null
        $script:launchStarted = $null
        $script:clipboardSaved = $true
        $script:cursorSaved = $false
        $script:fixtureDir = $null
        $script:fixtureLog = $null
        $script:originalClipboard = [pscustomobject]@{ Formats = @('offline-non-text-format') }
        Mock Stop-TestTerminal {}
        Mock Restore-ClipboardSnapshot {}
    }

    It 'archives mouse fixture diagnostics even when clipboard restoration fails' -Tag 'MouseCleanup' {
        $cleanup = Get-FixtureCleanup 'Feature.AgentMouse.Tests.ps1' 'Feature: completed-turn triangle mouse click'
        $script:fixtureDir = Join-Path $TestDrive 'fixture'
        $script:evidenceDir = Join-Path $TestDrive 'evidence'
        New-Item -ItemType Directory -Path $script:fixtureDir, $script:evidenceDir | Out-Null
        $script:fixtureLog = Join-Path $script:fixtureDir 'fixture.log'
        'offline fixture evidence' | Set-Content $script:fixtureLog
        Mock Restore-ClipboardSnapshot { throw 'synthetic clipboard restore failure' }
        { & $cleanup } | Should -Throw '*synthetic clipboard restore failure*'
        Get-Content (Join-Path $script:evidenceDir 'fixture.log') -Raw | Should -Match 'offline fixture evidence'
    }

    It 'restores the full paste clipboard snapshot despite terminal cleanup failure' -Tag 'PasteCleanup' {
        $cleanup = Get-FixtureCleanup 'Feature.Paste.Tests.ps1' ('Feature ' + [char]0x00A7 + '2 agent pane paste')
        Mock Stop-TestTerminal { throw 'synthetic terminal cleanup failure' }
        { & $cleanup } | Should -Throw '*synthetic terminal cleanup failure*'
        Should -Invoke Restore-ClipboardSnapshot -Times 1 -Exactly -ParameterFilter {
            $Snapshot -eq $script:originalClipboard
        }
    }
}
