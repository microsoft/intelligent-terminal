#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:ast = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
}

Describe 'Combined owning-tab exact displayed marker' -Tag 'Unit' {
    BeforeAll {
        Add-Type -AssemblyName PresentationFramework, UIAutomationClient, UIAutomationTypes
        Add-Type -ReferencedAssemblies ([System.Windows.Rect].Assembly.Location) @'
namespace CombinedDisplayMock {
    public class TextRange {
        public string Text = "PID=8765";
        public System.Windows.Rect[] Rectangles = { new System.Windows.Rect(110, 120, 60, 20) };
        public bool Error;
        public string GetText(int length) {
            if (Error) { throw new System.Exception("range-read-error"); }
            return Text;
        }
        public System.Windows.Rect[] GetBoundingRectangles() { return Rectangles; }
    }
}
'@
        Add-Type @'
namespace CombinedDisplayMock {
    public static class ItWtWin32Input {
        public static System.IntPtr GetAncestor(System.IntPtr h, int f) { return h; }
        public static int GetWindowProcessId(System.IntPtr h) { return 42; }
    }
    public static class AutomationElement {
        public static object Root;
        public static string ClassNameProperty = "Class";
        public static object FromHandle(System.IntPtr h) { return Root; }
    }
    public class PropertyCondition { public PropertyCondition(object p, string v) {} }
    public static class TreeScope { public static string Descendants = "Descendants"; }
    public static class TextPattern { public static string Pattern = "Text"; }
}
'@
        $definition = $script:ast.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-MoveDisplay'
        }, $true)
        . ([scriptblock]::Create($definition.Extent.Text.Replace(
            'Windows.Automation.', 'CombinedDisplayMock.').Replace(
            'ItE2E.ItWtWin32Input', 'CombinedDisplayMock.ItWtWin32Input')))
        $ui = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\ItE2E\Public\Ui.ps1'), [ref]$null, [ref]$null)
        $exact = $ui.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Find-ItExactTextRange'
        }, $true)
        $script:displayModule = New-Module -Name ItE2E -ScriptBlock {
            param($text)
            . ([scriptblock]::Create($text))
            Export-ModuleMember -Function @()
        } -ArgumentList $exact.Extent.Text
        Import-Module $script:displayModule -Force
        function New-DisplayBounds($X, $Y, $Width, $Height) {
            @{ X = $X; Y = $Y; Width = $Width; Height = $Height
                Left = $X; Top = $Y; Right = $X + $Width; Bottom = $Y + $Height }
        }
        function New-DisplayPeer([string]$Text) {
            $range = [CombinedDisplayMock.TextRange]::new()
            $document = [pscustomobject]@{ Text = $Text; Range = $range; Reads = 0; Finds = 0; Error = $false; NoHit = $false }
            $document | Add-Member ScriptMethod GetText { param($Length)
                $Length | Should -Be -1
                $this.Reads++
                if ($this.Error) { throw 'document-read-error' }; $this.Text
            }
            $document | Add-Member ScriptMethod FindText { param($Text, $Backward, $IgnoreCase)
                $Text | Should -Be 'PID=8765'
                $Backward | Should -BeFalse; $IgnoreCase | Should -BeFalse
                $this.Finds++
                if (-not $this.NoHit) { $this.Range }
            }
            $peer = [pscustomobject]@{
                Current = @{ ProcessId = 42; IsOffscreen = $false; ClassName = 'TermControl'; Name = 'Owner'
                    BoundingRectangle = (New-DisplayBounds 100 100 400 300) }
                Document = $document; DocumentGets = 0
            }
            $peer | Add-Member ScriptMethod GetCurrentPattern {
                param($Pattern)
                $patternObject = [pscustomobject]@{ Peer = $this }
                $patternObject | Add-Member ScriptProperty DocumentRange {
                    $this.Peer.DocumentGets++; $this.Peer.Document
                }
                $patternObject
            }
            $peer | Add-Member ScriptMethod GetRuntimeId { @(1, 2, 3) }
            $peer
        }
    }
    AfterAll { Remove-Module $script:displayModule }
    BeforeEach {
        $script:app = @{ Pid = 42; Hwnd = 123; Launched = $true; InstallLocation = 'Q:\fixture'
            OwnedProcess = @{ Id = 42; HasExited = $false; StartTime = 123 } }
        $script:launch = @{ pid = 8765 }
        Mock Get-Process { @{ Id = 42; StartTime = 123; Path = 'Q:\fixture\WindowsTerminal.exe' } }
        $script:peer = New-DisplayPeer 'banner PID=8765 tail'
        $script:displayWindow = [pscustomobject]@{
            Current = @{ ProcessId = 42; IsOffscreen = $false; BoundingRectangle = (New-DisplayBounds 0 0 800 600) }
            Peers = @($script:peer)
        }
        $script:displayWindow | Add-Member ScriptMethod FindAll { param($Scope, $Condition) $this.Peers }
        [CombinedDisplayMock.AutomationElement]::Root = $script:displayWindow
    }
    It 'does not call FindText for proven nonmatching <Text> documents' -ForEach @(
        @{ Text = 'pwsh.exe banner' }, @{ Text = 'Agent Pane transcript' }, @{ Text = 'pid=8765' }, @{ Text = '' }
    ) {
        $peer.Document.Text = $Text
        $peer.Document.Range.Error = $true
        Get-MoveDisplay | Should -BeNullOrEmpty
        $peer.Document.Reads | Should -Be 1
        $peer.Document.Finds | Should -Be 0
        $peer.DocumentGets | Should -Be 1
    }
    It 'keeps a matching document on the real exact helper and captured range' {
        [System.Windows.Automation.Text.TextPatternRange].GetMethod('GetBoundingRectangles').ReturnType |
            Should -Be ([System.Windows.Rect[]])
        $peer.Document.Range.GetBoundingRectangles() | Should -BeOfType ([System.Windows.Rect])
        $peer.Document.Range.GetBoundingRectangles().GetType() | Should -Be ([System.Windows.Rect[]])
        $display = Get-MoveDisplay
        $display.marker | Should -Be 'PID=8765'
        $display.range_rectangles | Should -Be @(110, 120, 60, 20)
        $peer.DocumentGets | Should -Be 1
        $peer.Document.Reads | Should -Be 1
        $peer.Document.Finds | Should -Be 2
    }
    It 'surfaces document GetText failure without searching or excluding it' {
        $peer.Document.Error = $true
        { Get-MoveDisplay } | Should -Throw '*document-read-error*'
        $peer.Document.Finds | Should -Be 0
    }
    It 'surfaces matching range GetText failure' {
        $peer.Document.Range.Error = $true
        { Get-MoveDisplay } | Should -Throw '*range-read-error*'
    }
    It 'rejects a mismatching exact range even with a matching document' {
        $peer.Document.Range.Text = 'not the marker'
        { Get-MoveDisplay } | Should -Throw '*does not exactly match*'
    }
    It 'does not accept document presence alone when FindText returns null' {
        $peer.Document.NoHit = $true
        Get-MoveDisplay | Should -BeNullOrEmpty
        $peer.Document.Finds | Should -Be 1
    }
    It 'rejects matching marker with <Kind> rectangles' -ForEach @(
        @{ Kind = 'zero'; Bounds = ,@(110, 120, 0, 20) },
        @{ Kind = 'outside control'; Bounds = ,@(50, 120, 60, 20) },
        @{ Kind = 'outside window'; Bounds = ,@(110, 620, 60, 20) },
        @{ Kind = 'NaN'; Bounds = ,@([double]::NaN, 120, 60, 20) },
        @{ Kind = 'infinite X'; Bounds = ,@([double]::PositiveInfinity, 120, 60, 20) },
        @{ Kind = 'infinite height'; Bounds = ,@(110, 120, 60, [double]::PositiveInfinity) },
        @{ Kind = 'zero height'; Bounds = ,@(110, 120, 60, 0) },
        @{ Kind = 'negative position'; Bounds = ,@(-110, 120, 60, 20) },
        @{ Kind = 'right overflow'; Bounds = ,@(490, 120, 60, 20) },
        @{ Kind = 'bottom overflow'; Bounds = ,@(110, 390, 60, 20) },
        @{ Kind = 'empty collection'; Bounds = @() },
        @{ Kind = 'invalid wrapped segment'; Bounds = @(@(110, 120, 60, 20), @(490, 140, 60, 20)) }
    ) {
        $peer.Document.Range.Rectangles = [System.Windows.Rect[]]@(
            foreach ($bound in $Bounds) { [System.Windows.Rect]::new($bound[0], $bound[1], $bound[2], $bound[3]) }
        )
        Get-MoveDisplay | Should -BeNullOrEmpty
        $peer.Document.Finds | Should -Be 2
    }
    It 'rejects the actual empty Rect with negative infinite dimensions' {
        $peer.Document.Range.Rectangles = [System.Windows.Rect[]]@([System.Windows.Rect]::Empty)
        Get-MoveDisplay | Should -BeNullOrEmpty
    }
    It 'accepts all contained wrapped segments and serializes ordered flat tuples' {
        $peer.Document.Range.Rectangles = [System.Windows.Rect[]]@(
            [System.Windows.Rect]::new(110, 120, 60, 20),
            [System.Windows.Rect]::new(100, 140, 40, 20)
        )
        $display = Get-MoveDisplay
        $display.marker | Should -Be 'PID=8765'
        $display.range_rectangles | Should -Be @(110, 120, 60, 20, 100, 140, 40, 20)
        $roundTrip = $display | ConvertTo-Json -Depth 5 | ConvertFrom-Json
        $roundTrip.range_rectangles | Should -Be $display.range_rectangles
    }
    It 'accepts full containment at the control boundary with negative screen coordinates' {
        $displayWindow.Current.BoundingRectangle = [System.Windows.Rect]::new(-800, -600, 800, 600)
        $peer.Current.BoundingRectangle = [System.Windows.Rect]::new(-500, -400, 400, 300)
        $peer.Document.Range.Rectangles = [System.Windows.Rect[]]@([System.Windows.Rect]::new(-500, -400, 400, 300))
        (Get-MoveDisplay).marker | Should -Be 'PID=8765'
    }
    It 'rejects a range inside its control but outside the owned window' {
        $displayWindow.Current.BoundingRectangle = [System.Windows.Rect]::new(0, 0, 100, 100)
        Get-MoveDisplay | Should -BeNullOrEmpty
    }
    It 'accepts the two wrapped rectangles observed in the live failure' {
        $displayWindow.Current.BoundingRectangle = [System.Windows.Rect]::new(0, 0, 900, 650)
        $peer.Current.BoundingRectangle = [System.Windows.Rect]::new(328, 41, 280, 278)
        $peer.Document.Range.Rectangles = [System.Windows.Rect[]]@(
            [System.Windows.Rect]::new(516, 106, 63, 19),
            [System.Windows.Rect]::new(336, 125, 27, 19)
        )
        (Get-MoveDisplay).range_rectangles | Should -Be @(516, 106, 63, 19, 336, 125, 27, 19)
    }
    It 'retains unique-owner rejection across two matching terminals' {
        $displayWindow.Peers = @($peer, (New-DisplayPeer 'PID=8765'))
        { Get-MoveDisplay } | Should -Throw '*ambiguous across terminal peers*'
    }
    It 'ignores the nonmatching sentinel beside one exact visible owner' {
        $other = New-DisplayPeer 'Agent transcript without marker'
        $other.Document.Range.Error = $true
        $displayWindow.Peers = @($other, $peer)
        (Get-MoveDisplay).marker | Should -Be 'PID=8765'
        $other.Document.Finds | Should -Be 0
        $peer.Document.Finds | Should -Be 2
    }
}

Describe 'Combined sidebar supported filter actions' -Tag 'Unit' {
    BeforeAll {
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $definition = $script:ast.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Set-CombinedFilters'
        }, $true)
        . ([scriptblock]::Create($definition.Extent.Text))
        $core = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\ItE2E\Private\Core.ps1'), [ref]$null, [ref]$null)
        $wait = $core.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Wait-Until'
        }, $true)
        . ([scriptblock]::Create($wait.Extent.Text))
        function Get-CombinedFilterState { $script:filterState.Clone() }
        function Invoke-UiElement { param($App, $Selector) }
        function Get-CombinedElement {
            param($Id)
            $script:peerRequests.Add($Id)
            $script:peerReads++
            if ($script:peerReads -le $script:pendingReads) { return $null }
            $peer = [pscustomobject]@{
                Id = $Id
                Current = [pscustomobject]@{
                    ProcessId = $script:peerPid
                    ControlType = $script:peerType
                    IsOffscreen = $script:peerReads -le ($script:pendingReads + $script:offscreenReads)
                }
            }
            $peer | Add-Member ScriptMethod GetCurrentPattern {
                param($pattern)
                if ($script:unsupportedPattern -or $pattern -ne [Windows.Automation.TogglePattern]::Pattern) {
                    throw 'Unsupported Pattern'
                }
                $script:patternRequests.Add($this.Id)
                $toggle = [pscustomobject]@{ Id = $this.Id }
                $toggle | Add-Member ScriptMethod Toggle {
                    $script:toggleCalls.Add($this.Id)
                    if (-not $script:ignoreToggle) {
                        $script:filterState[$this.Id] = -not $script:filterState[$this.Id]
                    }
                }
                $toggle
            }
            $peer
        }
        function Assert-CombinedHeaderCue { param($Expected) }
    }

    BeforeEach {
        $script:app = @{ Pid = 42; Hwnd = 123 }
        $script:filterState = @{
            AgentsOnlyFilterMenuItem = $false
            RecentAgentSessionsFilterMenuItem = $false
        }
        $script:patternRequests = [Collections.Generic.List[string]]::new()
        $script:toggleCalls = [Collections.Generic.List[string]]::new()
        $script:ignoreToggle = $false
        $script:peerRequests = [Collections.Generic.List[string]]::new()
        $script:peerReads = 0
        $script:pendingReads = 0
        $script:offscreenReads = 0
        $script:peerPid = 42
        $script:peerType = [Windows.Automation.ControlType]::MenuItem
        $script:unsupportedPattern = $false
        Mock Invoke-UiElement {
            $Selector | Should -Be FilterTabsButton
            $App.Pid | Should -Be 42
            $App.Hwnd | Should -Be 123
        }
        Mock Assert-CombinedHeaderCue { $Expected | Should -Be Tabs }
    }

    It 'toggles each mismatching named filter once and leaves matching state untouched' {
        Set-CombinedFilters -AgentsOnly $true -Recent $false
        $script:toggleCalls.ToArray() | Should -Be @('AgentsOnlyFilterMenuItem')
        Set-CombinedFilters -AgentsOnly $true -Recent $true
        $script:toggleCalls.ToArray() | Should -Be @(
            'AgentsOnlyFilterMenuItem', 'RecentAgentSessionsFilterMenuItem')
        Set-CombinedFilters -AgentsOnly $true -Recent $true
        $script:toggleCalls.Count | Should -Be 2
        Set-CombinedFilters -AgentsOnly $false -Recent $false
        $script:toggleCalls.ToArray() | Should -Be @(
            'AgentsOnlyFilterMenuItem', 'RecentAgentSessionsFilterMenuItem',
            'AgentsOnlyFilterMenuItem', 'RecentAgentSessionsFilterMenuItem')
        $script:patternRequests.ToArray() | Should -Be $script:toggleCalls.ToArray()
        $script:filterState.AgentsOnlyFilterMenuItem | Should -BeFalse
        $script:filterState.RecentAgentSessionsFilterMenuItem | Should -BeFalse
        Should -Invoke Invoke-UiElement -Exactly -Times 4
        Should -Invoke Assert-CombinedHeaderCue -Exactly -Times 4
    }

    It 'fails the retained final state assertion when <Id> does not change' -ForEach @(
        @{ Id = 'AgentsOnlyFilterMenuItem'; AgentsOnly = $true; Recent = $false },
        @{ Id = 'RecentAgentSessionsFilterMenuItem'; AgentsOnly = $false; Recent = $true }
    ) {
        $script:ignoreToggle = $true
        { Set-CombinedFilters -AgentsOnly $AgentsOnly -Recent $Recent } | Should -Throw
        $script:toggleCalls.ToArray() | Should -Be @($Id)
        Should -Invoke Assert-CombinedHeaderCue -Exactly -Times 0
    }

    It 'waits for the exact <Id> peer to become present and visible before toggling' -ForEach @(
        @{ Id = 'AgentsOnlyFilterMenuItem'; AgentsOnly = $true; Recent = $false },
        @{ Id = 'RecentAgentSessionsFilterMenuItem'; AgentsOnly = $false; Recent = $true }
    ) {
        $script:pendingReads = 1
        $script:offscreenReads = 1
        Set-CombinedFilters -AgentsOnly $AgentsOnly -Recent $Recent
        $script:peerRequests.ToArray() | Should -Be @($Id, $Id, $Id)
        $script:toggleCalls.ToArray() | Should -Be @($Id)
        $script:filterState[$Id] | Should -BeTrue
        Should -Invoke Invoke-UiElement -Exactly -Times 1
        Should -Invoke Assert-CombinedHeaderCue -Exactly -Times 1
    }

    It 'rejects a visible peer from another process before requesting its pattern' {
        $script:peerPid = 43
        { Set-CombinedFilters -AgentsOnly $true -Recent $false } | Should -Throw
        $script:patternRequests.Count | Should -Be 0
        $script:toggleCalls.Count | Should -Be 0
    }

    It 'rejects a visible non-menu peer before requesting its pattern' {
        $script:peerType = [Windows.Automation.ControlType]::Button
        { Set-CombinedFilters -AgentsOnly $true -Recent $false } | Should -Throw
        $script:patternRequests.Count | Should -Be 0
        $script:toggleCalls.Count | Should -Be 0
    }

    It 'fails explicitly when the visible menu peer does not support TogglePattern' {
        $script:unsupportedPattern = $true
        { Set-CombinedFilters -AgentsOnly $true -Recent $false } | Should -Throw '*Unsupported Pattern*'
        $script:toggleCalls.Count | Should -Be 0
        Should -Invoke Assert-CombinedHeaderCue -Exactly -Times 0
    }

    It 'fails an unsupported scoped opener before looking up or toggling a menu peer' {
        Mock Invoke-UiElement { throw 'Unsupported Pattern' }
        { Set-CombinedFilters -AgentsOnly $true -Recent $false } | Should -Throw '*Unsupported Pattern*'
        $script:peerRequests.Count | Should -Be 0
        $script:toggleCalls.Count | Should -Be 0
        Should -Invoke Assert-CombinedHeaderCue -Exactly -Times 0
    }

    It 'retains the 30-second bound when the peer never becomes ready' {
        Mock Wait-Until {
            $TimeoutSec | Should -Be 30
            $Because | Should -Be 'AgentsOnlyFilterMenuItem is visible in the owned filter flyout'
            & $Condition | Should -BeNullOrEmpty
            throw 'Wait-Until timed out after 30s waiting for: AgentsOnlyFilterMenuItem'
        }
        $script:pendingReads = 1
        { Set-CombinedFilters -AgentsOnly $true -Recent $false } | Should -Throw '*timed out after 30s*'
        $script:toggleCalls.Count | Should -Be 0
        Should -Invoke Wait-Until -Exactly -Times 1
        Should -Invoke Assert-CombinedHeaderCue -Exactly -Times 0
    }
}

Describe 'Combined filter state scoped opening' -Tag 'Unit' {
    BeforeAll {
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $definition = $script:ast.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Get-CombinedFilterState'
        }, $true)
        . ([scriptblock]::Create($definition.Extent.Text))
        function Invoke-UiElement { param($App, $Selector) }
        function Invoke-UiClick { param($App, $Selector) throw 'Physical opening is forbidden' }
        function Send-WtWindowKey { param($App, $Vk, [switch]$RequireForeground) }
        function Wait-Until { param($TimeoutSec, $Because, $Condition) & $Condition }
        function Get-CombinedElement {
            param($Id)
            $script:statePeers.Add($Id)
            $peer = [pscustomobject]@{
                Current = @{ ProcessId = $script:statePid; IsOffscreen = $false
                    ControlType = [Windows.Automation.ControlType]::MenuItem }
                Id = $Id
            }
            $peer | Add-Member ScriptMethod GetCurrentPattern {
                param($Pattern)
                $Pattern | Should -Be ([Windows.Automation.TogglePattern]::Pattern)
                @{ Current = @{ ToggleState = $script:states[$this.Id] } }
            }
            $peer
        }
    }
    BeforeEach {
        $script:app = @{ Pid = 42; Hwnd = 123 }
        $script:statePid = 42
        $script:statePeers = [Collections.Generic.List[string]]::new()
        $script:states = @{
            AgentsOnlyFilterMenuItem = [Windows.Automation.ToggleState]::On
            RecentAgentSessionsFilterMenuItem = [Windows.Automation.ToggleState]::Off
        }
        Mock Invoke-UiElement {
            $App.Hwnd | Should -Be 123
            $App.Pid | Should -Be 42
            $Selector | Should -Be FilterTabsButton
        }
        Mock Send-WtWindowKey {
            $App.Pid | Should -Be 42
            $Vk | Should -Be 0x1B
            $RequireForeground | Should -BeTrue
        }
        Mock Wait-Until {
            $TimeoutSec | Should -Be 30
            $Because | Should -BeLike '*is visible in the owned filter flyout'
            & $Condition
        }
    }
    It 'opens with the scoped action and reads both original toggle states before retained dismissal' {
        $state = Get-CombinedFilterState
        $state.AgentsOnlyFilterMenuItem | Should -BeTrue
        $state.RecentAgentSessionsFilterMenuItem | Should -BeFalse
        $script:statePeers.ToArray() | Should -Be @('AgentsOnlyFilterMenuItem', 'RecentAgentSessionsFilterMenuItem')
        Should -Invoke Invoke-UiElement -Exactly -Times 1
        Should -Invoke Wait-Until -Exactly -Times 2
        Should -Invoke Send-WtWindowKey -Exactly -Times 1
    }
    It 'retains process rejection and finally dismissal' {
        $script:statePid = 43
        { Get-CombinedFilterState } | Should -Throw
        Should -Invoke Invoke-UiElement -Exactly -Times 1
        Should -Invoke Send-WtWindowKey -Exactly -Times 1
    }
    It 'does not turn scoped invocation failure into a physical fallback' {
        Mock Invoke-UiElement { throw 'Unsupported Pattern' }
        { Get-CombinedFilterState } | Should -Throw '*Unsupported Pattern*'
        $script:statePeers.Count | Should -Be 0
        Should -Invoke Send-WtWindowKey -Exactly -Times 0
    }
}

Describe 'Combined sidebar initialization order' -Tag 'Unit' {
    It 'keeps the C425 ordinary neighbor stashed while waiting for the two agent tabs' {
        $case = $script:ast.Find({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'It' -and
                $node.CommandElements[1].Value -eq 'Agents view moves whole owning tabs without changing sessions'
        }, $true)
        $case | Should -Not -BeNullOrEmpty
        $commands = @($case.FindAll({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Wait-AgentReady'
        }, $true))
        $commands | Should -HaveCount 2
        $targets = @(foreach ($command in $commands) {
            $parameter = @($command.CommandElements | Where-Object {
                $_ -is [Management.Automation.Language.CommandParameterAst] -and
                    $_.ParameterName -eq 'PaneSessionId'
            })
            $parameter | Should -HaveCount 1
            $argument = $command.CommandElements[$command.CommandElements.IndexOf($parameter[0]) + 1]
            $argument | Should -BeOfType ([Management.Automation.Language.MemberExpressionAst])
            $argument.Member.Value | Should -Be PaneSessionId
            $argument.Expression.VariablePath.UserPath
        })
        $targets | Should -Be @('neighborHelper', 'helper')
        $ordinaryCapture = @($case.FindAll({ param($node)
            $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                $node.Left.VariablePath.UserPath -eq 'ordinaryHelper'
        }, $true))
        $ordinaryCapture | Should -HaveCount 1
        @($ordinaryCapture[0].FindAll({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Wait-NewAgentPaneSession'
        }, $true)) | Should -HaveCount 1
    }

    It 'file-level BeforeAll defines helpers without accessing package paths' {
        $parent = $script:ast.EndBlock.Statements | Where-Object {
            $_ -is [Management.Automation.Language.PipelineAst] -and $_.PipelineElements[0].GetCommandName() -eq 'BeforeAll'
        } | Select-Object -First 1
        $body = $parent.PipelineElements[0].CommandElements[1].ScriptBlock
        @($body.EndBlock.Statements | Where-Object {
            $_ -isnot [Management.Automation.Language.FunctionDefinitionAst]
        }).Count | Should -Be 0
        $script:target = $null
        $script:evidence = $null
        $text = $body.Extent.Text
        { . ([scriptblock]::Create($text.Substring(1, $text.Length - 2))) } | Should -Not -Throw
    }

    It 'snapshots runtime state after provenance but before any fixture or app mutation' {
        $text = $script:ast.Extent.Text
        $call = $text.IndexOf("`n        Initialize-CombinedRuntimeBackup")
        $call | Should -BeGreaterOrEqual 0
        foreach ($marker in @(
            '$script:target = Resolve-ItApp',
            'Refusing to adopt or close an existing Dev process',
            'New-Item -ItemType Directory -Path $script:evidence',
            "'package-before-launch.json'"
        )) {
            $position = $text.IndexOf($marker)
            $position | Should -BeGreaterOrEqual 0 -Because "the prerequisite marker must exist: $marker"
            $call | Should -BeGreaterThan $position
        }
        foreach ($marker in @('$script:historyPath = Join-Path', 'Start-Terminal -Package Dev')) {
            $position = $text.IndexOf($marker)
            $position | Should -BeGreaterOrEqual 0 -Because "the mutation marker must exist: $marker"
            $call | Should -BeLessThan $position
        }
    }
}
