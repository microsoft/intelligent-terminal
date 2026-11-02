#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Sidebar telemetry container modes' -Tag Unit {
    BeforeAll {
        $script:suite = Join-Path $PSScriptRoot '..\tests\Feature.SidebarTelemetry.Tests.ps1'
        $errors = $null
        $script:suiteAst = [Management.Automation.Language.Parser]::ParseFile(
            $script:suite, [ref]$null, [ref]$errors)
        @($errors) | Should -HaveCount 0
        . (Join-Path $PSScriptRoot '..\tests\helpers\TelemetryFunnels.Scenarios.ps1')
        $schema = $script:suiteAst.Find({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Assert-SidebarSchema'
        }, $true)
        . ([scriptblock]::Create($schema.Extent.Text))
        $search = $script:suiteAst.Find({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'It' -and
                $node.CommandElements[1].Value -eq 'Sidebar search telemetry counts opening rather than editing'
        }, $true)
        $script:searchAssertion = $search.CommandElements[-1].ScriptBlock.GetScriptBlock()
    }

    It 'discovers <Count> cases without setup in <Mode> mode' -ForEach @(
        @{ Mode = 'default'; Data = @{}; Count = 14 }
        @{ Mode = 'core'; Data = @{ CoreOnly = $true }; Count = 9 }
        @{ Mode = 'keep-running'; Data = @{ KeepRunningOnly = $true }; Count = 7 }
    ) {
        $configuration = New-PesterConfiguration
        $configuration.Run.Container = New-PesterContainer -Path $script:suite -Data $Data
        $configuration.Run.SkipRun = $true
        $configuration.Run.PassThru = $true
        $configuration.Output.Verbosity = 'None'
        $result = Invoke-Pester -Configuration $configuration
        $result.TotalCount | Should -Be $Count
        $result.FailedCount | Should -Be 0
        @($result.Tests | Where-Object Executed) | Should -HaveCount 0
        foreach ($name in @(
            'Sidebar pin telemetry counts explicit keep-running opt-ins'
            'Keep-running telemetry correlates opt-in, retention, and live reattachment'
            'Restored agent prompt telemetry identifies the surviving ACP session'
        )) {
            $result.Tests.Name | Should -Contain $name
        }
        if ($Mode -eq 'core') {
            $result.Tests.Name | Should -Contain 'Sidebar resume telemetry counts native launches rather than focus or rejection'
            $result.Tests.Name | Should -Contain 'Sidebar Agent launch telemetry preserves plus and split profile selection'
        }
    }

    It 'checks only executed search phases and rejects extra openings in <Mode> mode' -ForEach @(
        @{ Mode = 'default'; CoreOnly = $false; KeepRunningOnly = $false }
        @{ Mode = 'core'; CoreOnly = $true; KeepRunningOnly = $false }
        @{ Mode = 'keep-running'; CoreOnly = $false; KeepRunningOnly = $true }
    ) {
        $script:appProvider = 'fixture-provider'
        $script:app = @{ Pid = 123 }
        $script:phaseErrors = @{}
        $script:traceClockCorrection = [TimeSpan]::Zero
        $script:phases = @{}
        $script:records = @(@{
            Name = 'AppCreated'; Provider = $script:appProvider
            Fields = @{ PartA_PrivTags = '1' }; Timestamp = '2026-09-23T00:00:00Z'
        })
        $opening = @('pin-second-hidden-first')
        $silent = @('layout-refresh')
        if ($Mode -eq 'default') {
            $opening = @('keyboard-enter', 'search-open', 'search-reopen', 'filter-all', 'filter-live-search', 'filter-live-no-match') + $opening
            $silent = @('search-edit', 'search-close', 'filter-search-edit') + $silent
        }
        $time = [DateTimeOffset]'2026-09-23T00:01:00Z'
        foreach ($phase in @($opening) + @($silent)) {
            $script:phases[$phase] = @{ StartedUtc = $time.ToString('o'); EndedUtc = $time.AddSeconds(1).ToString('o') }
            if ($phase -in $opening) {
                $script:records += @{
                    Name = 'SidebarSearchOpened'; Provider = $script:appProvider; ProcessId = 123
                    Timestamp = $time.ToString('o')
                    Fields = @{ PartA_PrivTags = '1' }; Types = @{ PartA_PrivTags = 'UInt64' }
                }
            }
            $time = $time.AddSeconds(10)
        }
        & $script:searchAssertion
        $script:records += $script:records[-1]
        { & $script:searchAssertion } | Should -Throw
    }
}
