#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    . (Join-Path $PSScriptRoot '..\tests\helpers\TestTerminalCleanup.ps1')
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    if (-not ('ItE2ETests.TextRange' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Linq;

namespace ItE2ETests
{
    public sealed class TextRange
    {
        public string[] Units;
        public int Start;
        public int End;
        public int ExtraEndUnits = 1;
        public bool FreezeEnd;
        public bool WrongMatch;
        public string WrongText = "wrong text";
        public bool FindCalled;
        public object[] Rectangles = new object[] { "first rectangle", "second rectangle" };

        public TextRange(string[] units, int start, int end)
        {
            Units = units;
            Start = start;
            End = end;
        }

        public TextRange FindText(string text, bool backward, bool ignoreCase)
        {
            if (backward || ignoreCase) throw new InvalidOperationException("Unexpected search semantics");
            FindCalled = true;
            if (WrongMatch) return new TextRange(new[] { WrongText }, 0, 1);
            for (int start = Start; start < End; start++)
            {
                string value = "";
                for (int end = start; end < End; end++)
                {
                    value += Units[end];
                    if (String.Equals(value, text, StringComparison.Ordinal))
                    {
                        return new TextRange(Units, start, Math.Min(End, end + 1 + ExtraEndUnits))
                        {
                            FreezeEnd = FreezeEnd,
                            Rectangles = Rectangles
                        };
                    }
                }
            }
            return null;
        }

        public string GetText(int maxLength)
        {
            return String.Concat(Units.Skip(Start).Take(End - Start));
        }

        public int MoveEndpointByUnit(object endpoint, object unit, int count)
        {
            if (endpoint.ToString() != "End" || unit.ToString() != "Character")
                throw new InvalidOperationException("Only provider end-unit adjustment is expected");
            if (FreezeEnd) return 0;
            int old = End;
            End = Math.Max(Start, Math.Min(Units.Length, End + count));
            return End - old;
        }

        public object[] GetBoundingRectangles() { return Rectangles; }
    }
}
'@
    }
}

Describe 'Self-contained agent Escape input' -Tag 'Unit', 'MouseInput' {
    BeforeEach {
        Mock -ModuleName ItE2E Invoke-WtCli {}
        Mock -ModuleName ItE2E Send-AgentWin32Key {}
        Mock -ModuleName ItE2E Start-Sleep {}
    }

    It 'sends complete Escape events rather than a prefix that can consume the next character' {
        Send-AgentKey -App ([pscustomobject]@{ Pid = 1 }) -PaneSessionId 'owned-pane' -Key Escape -Count 2 | Out-Null
        Should -Invoke -ModuleName ItE2E Send-AgentWin32Key -Times 2 -Exactly -ParameterFilter {
            $PaneSessionId -eq 'owned-pane' -and $Vk -eq 0x1B -and $Sc -eq 1 -and $Uc -eq 27
        }
        Should -Invoke -ModuleName ItE2E Invoke-WtCli -Times 0 -Exactly
    }

    It 'preserves ordinary named-key routing' {
        Send-AgentKey -App ([pscustomobject]@{ Pid = 1 }) -PaneSessionId 'owned-pane' -Key Enter | Out-Null
        Should -Invoke -ModuleName ItE2E Invoke-WtCli -Times 1 -Exactly -ParameterFilter {
            $Arguments[-1] -eq 'Enter' -and $Arguments -contains 'owned-pane'
        }
        Should -Invoke -ModuleName ItE2E Send-AgentWin32Key -Times 0 -Exactly
    }
}

Describe 'Owner probe subscription readiness' -Tag 'Unit', 'OwnerProbeReady' {
    It 'confirms event subscription before emitting the owner-tab probe' {
        $script:ownerProbeReady = $false
        Mock -ModuleName ItE2E Start-WtEventListener {
            param($App, $WaitForReady)
            $script:ownerProbeReady = [bool]$WaitForReady
            [pscustomobject]@{ fixture = $true }
        }
        Mock -ModuleName ItE2E Start-Sleep {}
        Mock -ModuleName ItE2E Invoke-RunCommand {
            if (-not $script:ownerProbeReady) { throw 'Owner probe emitted before subscription readiness.' }
        }
        Mock -ModuleName ItE2E Wait-WtEvent { [pscustomobject]@{ params = @{ tab_id = 'owned-tab' } } }
        Mock -ModuleName ItE2E Stop-WtEventListener {}
        Resolve-AgentOwnerTabId -App ([pscustomobject]@{}) -OwnerPaneSessionId 'owned-pane' | Should -Be 'owned-tab'
        Should -Invoke -ModuleName ItE2E Start-WtEventListener -Times 1 -Exactly -ParameterFilter { $WaitForReady }
        Should -Invoke -ModuleName ItE2E Stop-WtEventListener -Times 1 -Exactly
    }
}

Describe 'Paired fixture listener readiness' -Tag 'Unit', 'FixtureListeners' {
    It 'confirms subscription for every mouse and paste fixture listener' {
        $targets = @(
            @{ File = 'Feature.AgentMouse.Tests.ps1'; Name = 'Feature: completed-turn triangle mouse click' },
            @{ File = 'Feature.Paste.Tests.ps1'; Name = ('Feature ' + [char]0x00A7 + '2 agent pane paste') }
        )
        foreach ($target in $targets) {
            $tokens = $null
            $errors = $null
            $path = Join-Path $PSScriptRoot ("..\tests\" + $target.File)
            $ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
            $describe = $ast.FindAll({
                param($node)
                $node -is [System.Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Describe' -and $node.CommandElements[1].Value -eq $target.Name
            }, $true)[0]
            $listeners = @($describe.FindAll({
                param($node)
                $node -is [System.Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Start-WtEventListener'
            }, $true))
            $listeners.Count | Should -BeGreaterThan 0
            foreach ($listener in $listeners) {
                @($listener.CommandElements | Where-Object {
                    $_ -is [System.Management.Automation.Language.CommandParameterAst] -and $_.ParameterName -eq 'WaitForReady'
                }).Count | Should -Be 1 -Because "$($target.File) must subscribe before testing an event or its absence"
            }
        }
    }
}

Describe 'Package-wide refusal discovery' -Tag 'Unit', 'PackageRefusal' {
    BeforeEach {
        $script:packageRoot = Join-Path $TestDrive 'selected-package'
        Mock -ModuleName ItE2E Get-Process {
            @(
                [pscustomobject]@{ Id = 41; Path = (Join-Path $script:packageRoot 'wta.exe') },
                [pscustomobject]@{ Id = 42; Path = (Join-Path ($script:packageRoot + '-other') 'wta.exe') },
                [pscustomobject]@{ Id = 43; Path = (Join-Path $TestDrive 'ordinary-terminal\WindowsTerminal.exe') }
            )
        }
        Mock -ModuleName ItE2E Stop-Process { throw 'Process discovery must not terminate anything.' }
    }

    It 'finds orphan helpers only beneath the exact selected package directory' {
        $app = [pscustomobject]@{ InstallLocation = $script:packageRoot }
        $matches = @(Get-WtProcessesForApp -App $app -IncludePackageExecutables)
        $matches.Count | Should -Be 1
        $matches[0].Id | Should -Be 41
        Should -Invoke -ModuleName ItE2E Get-Process -Times 1 -Exactly -ParameterFilter { -not $Name }
        Should -Invoke -ModuleName ItE2E Stop-Process -Times 0 -Exactly
    }

    It 'preserves the default terminal-only discovery contract' {
        Get-WtProcessesForApp -App ([pscustomobject]@{ InstallLocation = $script:packageRoot }) | Out-Null
        Should -Invoke -ModuleName ItE2E Get-Process -Times 1 -Exactly -ParameterFilter { $Name -eq 'WindowsTerminal' }
        Should -Invoke -ModuleName ItE2E Stop-Process -Times 0 -Exactly
    }

    It 'continues to discover package helpers after an inaccessible process path' {
        Mock -ModuleName ItE2E Get-Process {
            $inaccessible = [pscustomobject]@{ Id = 40 }
            $inaccessible | Add-Member -MemberType ScriptProperty -Name Path -Value {
                throw [UnauthorizedAccessException]::new('Synthetic inaccessible process.')
            }
            $inaccessible
            [pscustomobject]@{ Id = 41; Path = (Join-Path $script:packageRoot 'wta.exe') }
        }
        $matches = @(Get-WtProcessesForApp -App ([pscustomobject]@{ InstallLocation = $script:packageRoot }) -IncludePackageExecutables)
        $matches.Count | Should -Be 1
        $matches[0].Id | Should -Be 41
    }
}

Describe 'Selected package discovery' -Tag 'Unit', 'SelectedPackage' {
    BeforeEach {
        Mock -ModuleName ItE2E Get-AppxPackage {
            [pscustomobject]@{ PackageFamilyName = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe' }
        }
    }

    It 'does not substitute another installed brand for the selected package' {
        Resolve-ItApp -Package Dev -IfInstalled | Should -BeNullOrEmpty
    }

    It 'preserves required resolution failure when the selected package is missing' {
        { Resolve-ItApp -Package Dev } | Should -Throw '*No Intelligent Terminal package found*'
    }

    It 'returns the explicitly selected installed package' {
        $script:selectedInstall = Join-Path $TestDrive 'dev-layout'
        New-Item -ItemType Directory -Path $script:selectedInstall | Out-Null
        foreach ($name in 'wtcli.exe', 'wta.exe', 'WindowsTerminal.exe') {
            'offline file' | Set-Content (Join-Path $script:selectedInstall $name)
        }
        Mock -ModuleName ItE2E Get-AppxPackage {
            [pscustomobject]@{
                PackageFamilyName = 'IntelligentTerminal_rd9vj3e6a2mbr'
                PackageFullName = 'offline-dev'; Version = [version]'1.0.0'
                InstallLocation = $script:selectedInstall
            }
        }
        Mock -ModuleName ItE2E Get-StartApps { @() }
        Mock -ModuleName ItE2E Get-Command { [pscustomobject]@{ Source = 'offline-alias' } } -ParameterFilter { $Name -eq 'wtai' }
        $app = Resolve-ItApp -Package Dev -IfInstalled
        $app.Package | Should -Be 'IntelligentTerminal_rd9vj3e6a2mbr'
        $app.InstallLocation | Should -Be $script:selectedInstall
    }

    It 'does not turn an invalid implicit selection into a skip' {
        $previous = $env:ITE2E_PACKAGE
        try {
            foreach ($value in @('', 'Auto')) {
                $env:ITE2E_PACKAGE = $value
                { Get-ItTestPackage } | Should -Throw '*Choose the live integration-test package explicitly*'
            }
        }
        finally { $env:ITE2E_PACKAGE = $previous }
        { Resolve-ItApp -Package Auto -IfInstalled } | Should -Throw "*'Auto' is not allowed*"
    }
}

Describe 'Failed-startup terminal recovery' -Tag 'Unit', 'StartupRecovery' {
    BeforeEach {
        $script:recoveryStart = [DateTime]::UtcNow.AddMinutes(-1)
        $script:recoveryTarget = [pscustomobject]@{
            WindowsTerminal = (Join-Path $TestDrive 'package\WindowsTerminal.exe')
            InstallLocation = (Join-Path $TestDrive 'package')
            Pid = $null
        }
        Mock Get-CimInstance { [pscustomobject]@{ ParentProcessId = 0 } }
        Mock Get-WtProcessesForApp { @() }
        Mock Restore-WtConfig {}
        Mock Stop-Terminal {}
    }

    It 'restores configuration after failure before a process was launched' {
        Stop-TestTerminal -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart
        Should -Invoke Restore-WtConfig -Times 1 -Exactly
        Should -Invoke Stop-Terminal -Times 0 -Exactly
    }

    It 'does nothing when startup was never attempted' {
        Stop-TestTerminal -Target $script:recoveryTarget
        Should -Invoke Restore-WtConfig -Times 0 -Exactly
        Should -Invoke Stop-Terminal -Times 0 -Exactly
    }

    It 'refuses restoration when an attempted stop leaves the package active' -Tag 'RecoveryPostcondition' {
        $app = [pscustomobject]@{ Pid = 77; Launched = $true; InstallLocation = $script:recoveryTarget.InstallLocation }
        Mock Test-Until { $false }
        { Stop-TestTerminal -App $app -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart } |
            Should -Throw '*still active*'
        Should -Invoke Restore-WtConfig -Times 0 -Exactly
    }

    It 'restores configuration after an owned launch is confirmed stopped' {
        $app = [pscustomobject]@{ Pid = 77; Launched = $true; InstallLocation = $script:recoveryTarget.InstallLocation }
        Mock Test-Until { $true }
        Stop-TestTerminal -App $app -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart
        Should -Invoke Stop-Terminal -Times 1 -Exactly -ParameterFilter { -not $RestoreSettings }
        Should -Invoke Restore-WtConfig -Times 1 -Exactly
    }

    It 'does not treat an attached context as permission to stop a process' {
        $app = [pscustomobject]@{ Pid = 77; Launched = $false }
        { Stop-TestTerminal -App $app -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart } |
            Should -Throw '*owned launch context*'
        Should -Invoke Stop-Terminal -Times 0 -Exactly
    }

    It 'refuses an older process rather than assuming ownership from its path' {
        Mock Get-WtProcessesForApp {
            [pscustomobject]@{ Id = 77; Path = $script:recoveryTarget.WindowsTerminal; StartTime = $script:recoveryStart.AddSeconds(-1) }
        }
        { Stop-TestTerminal -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart } |
            Should -Throw '*no returned launch context*'
        Should -Invoke Stop-Terminal -Times 0 -Exactly
    }

    It 'does not infer ownership from a new stable matching process' -Tag 'RecoveryOwnership' {
        $script:recoveryProcess = [pscustomobject]@{
            Id = 77; Path = $script:recoveryTarget.WindowsTerminal; StartTime = $script:recoveryStart.AddSeconds(1)
        }
        Mock Get-WtProcessesForApp { $script:recoveryProcess }
        Mock Get-Process { $script:recoveryProcess } -ParameterFilter { $Id -eq 77 }
        { Stop-TestTerminal -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart } |
            Should -Throw '*no returned launch context*'
        Should -Invoke Stop-Terminal -Times 0 -Exactly
        Should -Invoke Restore-WtConfig -Times 0 -Exactly
    }

    It 'refuses a reused PID before cleanup' {
        Mock Get-WtProcessesForApp {
            [pscustomobject]@{ Id = 77; Path = $script:recoveryTarget.WindowsTerminal; StartTime = $script:recoveryStart.AddSeconds(1) }
        }
        Mock Get-Process {
            [pscustomobject]@{ Id = 77; Path = $script:recoveryTarget.WindowsTerminal; StartTime = $script:recoveryStart.AddSeconds(2) }
        }
        { Stop-TestTerminal -Target $script:recoveryTarget -LaunchStarted $script:recoveryStart } |
            Should -Throw '*no returned launch context*'
        Should -Invoke Stop-Terminal -Times 0 -Exactly
    }
}

Describe 'Mouse cleanup evidence preservation' -Tag 'Unit', 'MouseCleanup' {
    BeforeEach {
        $suite = (Resolve-Path (Join-Path $PSScriptRoot '..\tests\Feature.AgentMouse.Tests.ps1')).Path
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$errors)
        $describe = $ast.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst] -and
            $node.GetCommandName() -eq 'Describe' -and
            $node.CommandElements[1].Value -eq 'Feature: completed-turn triangle mouse click'
        }, $true)[0]
        $cleanup = $describe.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'AfterAll'
        }, $true)[0]
        $body = $cleanup.CommandElements[1].ScriptBlock.Extent.Text
        $script:cleanupBlock = [scriptblock]::Create($body.Substring(1, $body.Length - 2))
        $script:app = [pscustomobject]@{ Pid = 1 }
        $script:target = $null
        $script:launchStarted = $null
        $script:clipboardSaved = $true
        $script:originalClipboard = [pscustomobject]@{ offline = $true }
        $script:cursorSaved = $false
        $script:fixtureDir = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $script:evidenceDir = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:fixtureDir, $script:evidenceDir | Out-Null
        $script:fixtureLog = Join-Path $script:fixtureDir 'fixture.log'
        'offline fixture evidence' | Set-Content $script:fixtureLog
        Mock Stop-Terminal {}
        Mock Stop-TestTerminal {}
        Mock Restore-ClipboardSnapshot {}
    }

    It 'archives the fixture log even when clipboard restoration fails' {
        Mock Restore-ClipboardSnapshot { throw 'synthetic clipboard restore failure' }
        { & $script:cleanupBlock } | Should -Throw '*synthetic clipboard restore failure*'
        Get-Content (Join-Path $script:evidenceDir 'fixture.log') -Raw | Should -Match 'offline fixture evidence'
    }

    It 'restores clipboard and retains the source log when archival fails' {
        Mock Copy-Item { throw 'synthetic archival failure' }
        { & $script:cleanupBlock } | Should -Throw '*synthetic archival failure*'
        Should -Invoke Restore-ClipboardSnapshot -Times 1 -Exactly
        Test-Path $script:fixtureLog | Should -BeTrue
    }
}

Describe 'Paste clipboard preservation' -Tag 'Unit', 'PasteCleanup' {
    It 'restores a full snapshot independently of terminal cleanup' {
        $suite = (Resolve-Path (Join-Path $PSScriptRoot '..\tests\Feature.Paste.Tests.ps1')).Path
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$errors)
        $cleanup = $ast.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'AfterAll'
        }, $true)[0]
        $body = $cleanup.CommandElements[1].ScriptBlock.Extent.Text
        $block = [scriptblock]::Create($body.Substring(1, $body.Length - 2))
        $script:app = $null
        $script:target = $null
        $script:launchStarted = $null
        $script:clipboardSaved = $true
        $script:cursorSaved = $false
        $script:fixtureDir = $null
        $script:fixtureLog = $null
        $script:originalClipboard = [pscustomobject]@{ Formats = @('offline-non-text-format') }
        Mock Stop-TestTerminal { throw 'synthetic terminal cleanup failure' }
        Mock Restore-ClipboardSnapshot {}
        { & $block } | Should -Throw '*synthetic terminal cleanup failure*'
        Should -Invoke Restore-ClipboardSnapshot -Times 1 -Exactly -ParameterFilter {
            $Snapshot -eq $script:originalClipboard
        }
    }
}

Describe 'Paste evidence isolation' -Tag 'Unit', 'PasteEvidence' {
    It 'verifies an explicitly supplied WTA hash before state mutation' -Tag 'PasteHash' {
        $suite = (Resolve-Path (Join-Path $PSScriptRoot '..\tests\Feature.Paste.Tests.ps1')).Path
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$errors)
        $guards = @($ast.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.IfStatementAst] -and
            $node.Clauses[0].Item1.Extent.Text -eq '$env:ITE2E_EXPECTED_WTA_SHA256'
        }, $true))
        $guards.Count | Should -Be 1
        $source = [IO.File]::ReadAllText($suite)
        $guards[0].Extent.StartOffset | Should -BeLessThan $source.IndexOf('$script:originalClipboard = Get-ClipboardSnapshot')
        $previous = $env:ITE2E_EXPECTED_WTA_SHA256
        try {
            $env:ITE2E_EXPECTED_WTA_SHA256 = 'a' * 64
            $binaryHash = 'b' * 64
            $block = [scriptblock]::Create($guards[0].Extent.Text)
            { & $block } | Should -Throw
            $binaryHash = $env:ITE2E_EXPECTED_WTA_SHA256
            { & $block } | Should -Not -Throw
        }
        finally { $env:ITE2E_EXPECTED_WTA_SHA256 = $previous }
    }

    It 'uses a unique directory beneath the explicitly selected run root' {
        $suite = (Resolve-Path (Join-Path $PSScriptRoot '..\tests\Feature.Paste.Tests.ps1')).Path
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
        $assignments = @($ast.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.AssignmentStatementAst]
        }, $true))
        $evidence = @($assignments | Where-Object {
            $_.Left.Extent.Text -eq '$script:evidenceDir' -and $_.Right.Extent.Text -match '^Join-Path\s'
        })
        $evidence.Count | Should -Be 1
        $rootAssignment = @($assignments | Where-Object { $_.Left.Extent.Text -eq '$artifactRoot' })
        $code = (@($rootAssignment | ForEach-Object { $_.Extent.Text }) + @($evidence[0].Right.Extent.Text)) -join "`n"
        $code = $code.Replace('$PSScriptRoot', ("'" + (Split-Path $suite -Parent).Replace("'", "''") + "'"))
        $previous = $env:ITE2E_ARTIFACT_ROOT
        try {
            $env:ITE2E_ARTIFACT_ROOT = Join-Path $TestDrive 'selected-run'
            $first = & ([scriptblock]::Create($code))
            $second = & ([scriptblock]::Create($code))
            $prefix = [IO.Path]::GetFullPath($env:ITE2E_ARTIFACT_ROOT).TrimEnd('\') + '\'
            $first.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) | Should -BeTrue
            $second.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase) | Should -BeTrue
            $first | Should -Not -Be $second
        }
        finally { $env:ITE2E_ARTIFACT_ROOT = $previous }
    }
}

Describe 'Exact UIA text ranges' -Tag 'Unit', 'UiTextBounds' {
    It 'finds text after UTF-16 and UIA unit counts diverge' {
        $units = @([char]::ConvertFromUtf32(0x1F600), "`r`n", 'A', 'C', 'K', ' ')
        $document = [ItE2ETests.TextRange]::new($units, 0, $units.Count)
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            $range = Find-ItExactTextRange -DocumentRange $Document -Text 'ACK'
            $Document.FindCalled | Should -BeTrue
            $range.Start | Should -Be 2
            $range.GetText(-1) | Should -BeExactly 'ACK'
        }
    }

    It 'bounds complete Unicode units without using UTF-16 length as a movement count' {
        $text = ([char]::ConvertFromUtf32(0x1F600)) + 'e' + [char]0x0301
        $units = @('prefix', [char]::ConvertFromUtf32(0x1F600), ('e' + [char]0x0301), ' ')
        $document = [ItE2ETests.TextRange]::new($units, 0, $units.Count)
        InModuleScope ItE2E -Parameters @{ Document = $document; Text = $text } {
            param($Document, $Text)
            $range = Find-ItExactTextRange -DocumentRange $Document -Text $Text
            $range.Start | Should -Be 1
            $range.End | Should -Be 3
            $range.GetText(-1) | Should -BeExactly $Text
        }
    }

    It 'preserves multiline text and multiple bounding rectangles' {
        $units = @('A', "`r`n", 'B', ' ')
        $document = [ItE2ETests.TextRange]::new($units, 0, $units.Count)
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            $range = Find-ItExactTextRange -DocumentRange $Document -Text "A`r`nB"
            $range.GetText(-1) | Should -BeExactly "A`r`nB"
            @($range.GetBoundingRectangles()) | Should -Be @('first rectangle', 'second rectangle')
        }
    }

    It 'preserves first literal match semantics and an already exact range' {
        $units = @('A', ' ', 'A')
        $document = [ItE2ETests.TextRange]::new($units, 0, $units.Count)
        $document.ExtraEndUnits = 0
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            $range = Find-ItExactTextRange -DocumentRange $Document -Text 'A'
            $range.Start | Should -Be 0
            $range.End | Should -Be 1
        }
    }

    It 'rejects a provider range whose text does not match' {
        $document = [ItE2ETests.TextRange]::new(@('A'), 0, 1)
        $document.WrongMatch = $true
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            { Find-ItExactTextRange -DocumentRange $Document -Text 'A' } | Should -Throw '*UIA range does not exactly match*'
        }
    }

    It 'fails explicitly when the provider cannot make endpoint progress' {
        $document = [ItE2ETests.TextRange]::new(@('A', ' '), 0, 2)
        $document.FreezeEnd = $true
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            { Find-ItExactTextRange -DocumentRange $Document -Text 'A' } | Should -Throw '*endpoint*'
        }
    }

    It 'does not accept canonically equivalent but different source text' -Tag 'OrdinalText' {
        $document = [ItE2ETests.TextRange]::new(@('unused'), 0, 1)
        $document.WrongMatch = $true
        $document.WrongText = 'e' + [char]0x0301
        InModuleScope ItE2E -Parameters @{ Document = $document; Text = ([string][char]0x00E9) } {
            param($Document, $Text)
            { Find-ItExactTextRange -DocumentRange $Document -Text $Text } | Should -Throw '*UIA range does not exactly match*'
        }
    }

    It 'fails explicitly when text is absent' {
        $document = [ItE2ETests.TextRange]::new(@('A'), 0, 1)
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            { Find-ItExactTextRange -DocumentRange $Document -Text 'B' } | Should -Throw '*not found*'
        }
    }

    It 'stops when one provider step removes a surplus surrogate pair' {
        $units = @('A', [char]::ConvertFromUtf32(0x1F600))
        $document = [ItE2ETests.TextRange]::new($units, 0, $units.Count)
        InModuleScope ItE2E -Parameters @{ Document = $document } {
            param($Document)
            $range = Find-ItExactTextRange -DocumentRange $Document -Text 'A'
            $range.End | Should -Be 1
            [string]::Equals($range.GetText(-1), 'A', [StringComparison]::Ordinal) | Should -BeTrue
        }
    }
}
