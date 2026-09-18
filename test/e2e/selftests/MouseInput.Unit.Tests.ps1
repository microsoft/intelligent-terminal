#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
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
