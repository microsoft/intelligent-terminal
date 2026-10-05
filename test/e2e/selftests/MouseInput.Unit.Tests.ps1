#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    . (Join-Path $PSScriptRoot '..\tests\helpers\TestTerminalCleanup.ps1')
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes

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
