#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Hermetic unit tests for the ItE2E framework core. No deployed terminal required.
#   Invoke-Pester test/e2e/selftests -Tag Unit

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
}

Describe 'Window keyboard-layout guard' -Tag 'Unit' {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\tests\helpers\TestWindowKeyboardLayout.ps1')
    }

    It 'rejects a window not owned by the test application before changing its layout' {
        {
            Enable-TestWindowEnglishKeyboardLayout -App ([pscustomobject]@{ Hwnd = 0; Pid = 1 })
        } | Should -Throw '*Keyboard-layout target does not belong to the test window*'
    }
}

Describe 'Invoke-Native' -Tag 'Unit' {
    It 'captures stdout and a zero exit code' {
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'echo', 'hello-ite2e')
        $r.ExitCode | Should -Be 0
        $r.StdOut | Should -Match 'hello-ite2e'
    }

    It 'propagates a non-zero exit code' {
        (Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'exit', '7')).ExitCode | Should -Be 7
    }

    It 'does NOT truncate large output (regression for the async-read race)' {
        # Produce ~500 lines; the old Register-ObjectEvent impl truncated to a few bytes.
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'for /L %i in (1,1,500) do @echo line%i')
        $r.ExitCode | Should -Be 0
        ([regex]::Matches($r.StdOut, 'line\d+')).Count | Should -Be 500
    }

    It 'times out and flags TimedOut' {
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'ping', '-n', '10', '127.0.0.1') -TimeoutSec 1
        $r.TimedOut | Should -BeTrue
        $r.ExitCode | Should -Be -1
    }
}

Describe 'Wait-Until / Test-Until' -Tag 'Unit' {
    It 'returns the truthy value once the condition holds' {
        $script:n = 0
        $v = Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Condition { $script:n++; if ($script:n -ge 3) { 'ready' } else { $null } }
        $v | Should -Be 'ready'
    }
    It 'throws on timeout' {
        { Wait-Until -TimeoutSec 1 -IntervalSec 0.2 -Condition { $false } -Because 'never' } | Should -Throw
    }
    It 'Test-Until returns a boolean and never throws' {
        Test-Until -TimeoutSec 1 -IntervalSec 0.2 -Condition { $false } | Should -BeFalse
        Test-Until -TimeoutSec 2 -IntervalSec 0.1 -Condition { $true } | Should -BeTrue
    }
}

Describe 'JSON helpers' -Tag 'Unit' {
    It 'ConvertFrom-JsonSafe returns $null on garbage' {
        ConvertFrom-JsonSafe -InputObject 'not json {' | Should -BeNullOrEmpty
        ConvertFrom-JsonSafe -InputObject '' | Should -BeNullOrEmpty
    }
    It 'ConvertFrom-JsonSafe parses valid JSON' {
        (ConvertFrom-JsonSafe -InputObject '{"a":1,"b":[2,3]}').b[1] | Should -Be 3
    }
    It 'ConvertFrom-JsonC strips // comments and trailing commas' {
        $jsonc = @"
{
    // a comment
    "acpAgent": "copilot",
    "autoFixEnabled": true,
}
"@
        $o = ConvertFrom-JsonC -Text $jsonc
        $o.acpAgent | Should -Be 'copilot'
        $o.autoFixEnabled | Should -BeTrue
    }

    It 'Set-WtSetting verifies nested object arrays structurally' {
        $settingsPath = Join-Path $TestDrive 'nested-settings.json'
        '{}' | Set-Content -LiteralPath $settingsPath -Encoding utf8
        $app = [pscustomobject]@{ SettingsPath = $settingsPath }
        $providers = @(@{
            id = 'provider-local'
            models = @(@{ id = 'test-model'; name = 'Test Model' })
        })

        Set-WtSetting -App $app -Key 'customModelProviders' -Value $providers | Out-Null

        $stored = Get-WtSetting -App $app -Key 'customModelProviders'
        @($stored) | Should -HaveCount 1
        $stored[0].id | Should -Be 'provider-local'
        $stored[0].models[0].name | Should -Be 'Test Model'
    }
}

Describe 'Localized WTA text matching' -Tag 'Unit' {
    It 'matches action labels without depending on the trailing Enter glyph encoding' {
        $pattern = Get-WtaLocalizedTextRegex -Key 'recommendations.button_open_tab'

        $pattern | Should -Not -BeNullOrEmpty
        'Open Tab' | Should -Match $pattern
    }

    It 'matches a substituted provider command in localized policy text' {
        $pattern = Get-WtaLocalizedTextRegex -Key 'system.provider_command_blocked_by_policy'
        $pattern = $pattern.Replace(
            [regex]::Escape('%{command}'),
            [regex]::Escape('/allow_all'))

        $pattern | Should -Not -BeNullOrEmpty
        "/allow_all: Automatic approval is disabled by your organization's policy." | Should -Match $pattern
    }
}

Describe 'Log observation' -Tag 'Unit' {
    It 'aggregates appended slices across exact-name dated rotation candidates' {
        $version = 'aggregate-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $fixedPath = Join-Path $dir 'wta-main_master.log'
        $oldPath = Join-Path $dir 'wta-main_master.2026-09-10.log'
        $newPath = Join-Path $dir 'wta-main_master.2026-09-11.log'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($fixedPath, "fixed-before`n", $utf8)
        [System.IO.File]::WriteAllText($oldPath, "old-before`n", $utf8)
        $app = [pscustomobject]@{
            LogRootDir      = $TestDrive
            Version         = $version
            LogStartOffset  = @{}
        }

        Initialize-LogOffsets -App $app | Out-Null
        [System.IO.File]::AppendAllText($fixedPath, "fixed-after`n", $utf8)
        [System.IO.File]::AppendAllText($oldPath, "old-after`n", $utf8)
        [System.IO.File]::WriteAllText($newPath, "new-after`n", $utf8)
        [System.IO.File]::SetLastWriteTimeUtc($fixedPath, [DateTime]::Parse('2026-09-09T00:00:00Z'))
        [System.IO.File]::SetLastWriteTimeUtc($oldPath, [DateTime]::Parse('2026-09-10T00:00:00Z'))
        [System.IO.File]::SetLastWriteTimeUtc($newPath, [DateTime]::Parse('2026-09-11T00:00:00Z'))

        $text = Get-ItLogText -App $app -Name 'wta-main_master.log' -SinceStart

        $text | Should -Not -Match 'fixed-before'
        $text | Should -Not -Match 'old-before'
        $text | Should -Match '(?s)# ==== wta-main_master\.log ====.*fixed-after.*# ==== wta-main_master\.2026-09-10\.log ====.*old-after.*# ==== wta-main_master\.2026-09-11\.log ====.*new-after'
    }

    It 'matches mixed-case exact-name fixed and dated rotation candidates' {
        $version = 'case-insensitive-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $fixedPath = Join-Path $dir 'wta-main_master.log'
        $datedPath = Join-Path $dir 'WtA-MaIn_MaStEr.2026-09-11.LoG'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($fixedPath, "fixed-content`n", $utf8)
        [System.IO.File]::WriteAllText($datedPath, "dated-content`n", $utf8)
        $app = [pscustomobject]@{
            LogRootDir     = $TestDrive
            Version        = $version
            LogStartOffset = @{}
        }

        $text = Get-ItLogText -App $app -Name 'WTA-MAIN_MASTER.LOG' -SinceStart

        $text | Should -Match 'fixed-content'
        $text | Should -Match 'dated-content'
    }

    It 'keeps exact-name reads without SinceStart limited to the newest candidate' {
        $version = 'newest-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $oldPath = Join-Path $dir 'wta-main_master.2026-09-10.log'
        $newPath = Join-Path $dir 'wta-main_master.2026-09-11.log'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($oldPath, "old-content`n", $utf8)
        [System.IO.File]::WriteAllText($newPath, "new-content`n", $utf8)
        [System.IO.File]::SetLastWriteTimeUtc($oldPath, [DateTime]::Parse('2026-09-10T00:00:00Z'))
        [System.IO.File]::SetLastWriteTimeUtc($newPath, [DateTime]::Parse('2026-09-11T00:00:00Z'))
        $app = [pscustomobject]@{
            LogRootDir     = $TestDrive
            Version        = $version
            LogStartOffset = @{}
        }

        $text = Get-ItLogText -App $app -Name 'wta-main_master.log'

        $text | Should -Match 'new-content'
        $text | Should -Not -Match 'old-content'
    }

    It 'ignores a newer dated directory when selecting the newest exact-name file' {
        $version = 'directory-filter-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $fixedPath = Join-Path $dir 'wta-main_master.log'
        $datedDirectory = Join-Path $dir 'wta-main_master.2026-09-11.log'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($fixedPath, "fixed-content`n", $utf8)
        New-Item -ItemType Directory -Path $datedDirectory | Out-Null
        [System.IO.File]::SetLastWriteTimeUtc($fixedPath, [DateTime]::Parse('2026-09-10T00:00:00Z'))
        [System.IO.Directory]::SetLastWriteTimeUtc($datedDirectory, [DateTime]::Parse('2026-09-11T00:00:00Z'))
        $app = [pscustomobject]@{
            LogRootDir     = $TestDrive
            Version        = $version
            LogStartOffset = @{}
        }

        Initialize-LogOffsets -App $app | Out-Null
        $text = Get-ItLogText -App $app -Name 'wta-main_master.log'

        $text | Should -Match 'fixed-content'
        $app.LogStartOffset.Keys | Should -Not -Contain 'wta-main_master.2026-09-11.log'
    }

    It 'excludes malformed and extra-segment exact-name candidates even when newest' {
        $version = 'candidate-filter-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $validPath = Join-Path $dir 'wta-main_master.2026-09-10.log'
        $malformedPath = Join-Path $dir 'wta-main_master.not-a-date.log'
        $extraSegmentPath = Join-Path $dir 'wta-main_master.2026-09-11.debug.log'
        $invalidDatePath = Join-Path $dir 'wta-main_master.2026-02-30.log'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($validPath, "valid-content`n", $utf8)
        [System.IO.File]::WriteAllText($malformedPath, "malformed-content`n", $utf8)
        [System.IO.File]::WriteAllText($extraSegmentPath, "extra-segment-content`n", $utf8)
        [System.IO.File]::WriteAllText($invalidDatePath, "invalid-date-content`n", $utf8)
        [System.IO.File]::SetLastWriteTimeUtc($validPath, [DateTime]::Parse('2026-09-10T00:00:00Z'))
        [System.IO.File]::SetLastWriteTimeUtc($malformedPath, [DateTime]::Parse('2026-09-12T00:00:00Z'))
        [System.IO.File]::SetLastWriteTimeUtc($extraSegmentPath, [DateTime]::Parse('2026-09-13T00:00:00Z'))
        [System.IO.File]::SetLastWriteTimeUtc($invalidDatePath, [DateTime]::Parse('2026-09-14T00:00:00Z'))
        $app = [pscustomobject]@{
            LogRootDir     = $TestDrive
            Version        = $version
            LogStartOffset = @{}
        }

        $text = Get-ItLogText -App $app -Name 'wta-main_master.log'

        $text | Should -Match 'valid-content'
        $text | Should -Not -Match 'malformed-content'
        $text | Should -Not -Match 'extra-segment-content'
        $text | Should -Not -Match 'invalid-date-content'
    }

    It 'aggregates all files matched by a bracket-only wildcard' {
        $version = 'bracket-wildcard-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $masterPath = Join-Path $dir 'wta-main_master.log'
        $mhsterPath = Join-Path $dir 'wta-main_mhster.log'
        $nonMatchPath = Join-Path $dir 'wta-main_mxster.log'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($masterPath, "master-content`n", $utf8)
        [System.IO.File]::WriteAllText($mhsterPath, "mhster-content`n", $utf8)
        [System.IO.File]::WriteAllText($nonMatchPath, "non-match-content`n", $utf8)
        $app = [pscustomobject]@{
            LogRootDir     = $TestDrive
            Version        = $version
            LogStartOffset = @{}
        }

        $text = Get-ItLogText -App $app -Name 'wta-main_m[ah]ster.log'

        $text | Should -Match 'master-content'
        $text | Should -Match 'mhster-content'
        $text | Should -Not -Match 'non-match-content'
    }

    It 'emits no header or result for a known-offset file with no appended content' {
        $version = 'empty-slice-1.0'
        $dir = Join-Path $TestDrive $version
        New-Item -ItemType Directory -Path $dir | Out-Null
        $path = Join-Path $dir 'wta-main_master.log'
        $utf8 = [System.Text.UTF8Encoding]::new($false)
        [System.IO.File]::WriteAllText($path, "before-content`n", $utf8)
        $app = [pscustomobject]@{
            LogRootDir     = $TestDrive
            Version        = $version
            LogStartOffset = @{}
        }

        Initialize-LogOffsets -App $app | Out-Null
        $text = Get-ItLogText -App $app -Name 'wta-main_master.log' -SinceStart

        $text | Should -Be ''
    }
}

Describe 'Terminal action proposal permission gates' -Tag 'Unit' {
    BeforeAll {
        Mock Wait-Until -ModuleName ItE2E { param($Condition) & $Condition }
        Mock Get-AgentPaneSession -ModuleName ItE2E {
            [pscustomobject]@{ PaneSessionId = 'source-pane'; AcpSessionId = 'session-1'; HelperProcessId = 123 }
        }
        Mock Get-ItLogText -ModuleName ItE2E {
            @'
permission_ui: permission queued for user selection request={"session_id":"session-1","tool_call_id":"lookup-1","kind":"command_lookup"}
permission_ui: current permission snapshot={"session_id":"session-1","tool_call_id":"lookup-1"}
'@
        }
        Mock Get-AgentPaneText -ModuleName ItE2E { 'wta resolve-command gti --shell pwsh [Y] Allow [N] Deny' }
        Mock Send-AgentKey -ModuleName ItE2E { throw 'Waiting must not select a permission option' }
    }

    It 'returns an explicit command lookup permission without approving it' {
        $gate = Wait-TerminalActionProposal -App @{} -PaneSessionId 'source-pane' -ReturnOnPermission
        $gate.Mode | Should -Be 'Permission'
        $gate.Ready | Should -BeFalse
        Should -Invoke Get-AgentPaneText -ModuleName ItE2E -Times 1 -Exactly -ParameterFilter { $PaneSessionId -eq 'source-pane' }
        Should -Invoke Send-AgentKey -ModuleName ItE2E -Times 0
        Should -Invoke Get-ItLogText -ModuleName ItE2E -Times 1 -Exactly -ParameterFilter {
            $Name -eq 'wta-main_helper-123.log' -and -not $SinceStart
        }
    }

    It 'does not return command lookup permissions without opt-in' {
        Wait-TerminalActionProposal -App @{} | Should -BeNullOrEmpty
    }

    It 'does not treat unrelated tool permissions as proposals' {
        Mock Get-ItLogText -ModuleName ItE2E {
            @'
permission_ui: permission queued for user selection request={"session_id":"session-1","tool_call_id":"lookup-1","kind":"command_lookup"}
permission_ui: permission queued for user selection request={"session_id":"session-1","tool_call_id":"other-1","kind":"other"}
permission_ui: current permission snapshot={"session_id":"session-1","tool_call_id":"other-1"}
'@
        }
        Wait-TerminalActionProposal -App @{} -ReturnOnPermission | Should -BeNullOrEmpty
    }

    It 'requires rendered permission choices rather than command text alone' {
        Mock Get-AgentPaneText -ModuleName ItE2E { 'Completed wta resolve-command gti' }
        Wait-TerminalActionProposal -App @{} -ReturnOnPermission | Should -BeNullOrEmpty
    }

    It 'still recognizes the session MCP permission gate' {
        Mock Get-ItLogText -ModuleName ItE2E {
            @'
permission_ui: permission queued for user selection request={"session_id":"session-1","tool_call_id":"mcp-1","kind":"session_mcp"}
permission_ui: current permission snapshot={"session_id":"session-1","tool_call_id":"mcp-1"}
'@
        }
        Mock Get-AgentPaneText -ModuleName ItE2E { 'Run command in current shell [Y] Allow [N] Deny' }
        (Wait-TerminalActionProposal -App @{} -ReturnOnPermission).Mode | Should -Be 'Permission'
    }

    It 'still recognizes a rendered recommendation without a permission request' {
        Mock Get-ItLogText -ModuleName ItE2E { '' }
        Mock Get-AgentPaneText -ModuleName ItE2E { 'Insert in Terminal' }
        $gate = Wait-TerminalActionProposal -App @{}
        $gate.Mode | Should -Be 'Mcp'
        $gate.Ready | Should -BeTrue
    }

    It 'accepts a request already pending before waiting starts' {
        $gate = Wait-TerminalActionProposal -App @{} -ReturnOnPermission
        $gate.ToolCallId | Should -Be 'lookup-1'
        $gate.AcpSessionId | Should -Be 'session-1'
    }

    It 'ignores a cleared <Reason> request despite old resolver text' -TestCases @(
        @{ Reason = 'resolved' }, @{ Reason = 'rejected' }, @{ Reason = 'cancelled' }, @{ Reason = 'turn-ended' }
    ) {
        Mock Get-ItLogText -ModuleName ItE2E {
            @'
permission_ui: permission queued for user selection request={"session_id":"session-1","tool_call_id":"lookup-1","kind":"command_lookup"}
permission_ui: current permission snapshot={"session_id":"session-1","tool_call_id":"lookup-1"}
permission_ui: current permission snapshot={"session_id":"session-1","tool_call_id":null}
'@
        }
        Wait-TerminalActionProposal -App @{} -ReturnOnPermission | Should -BeNullOrEmpty
    }

    It 'ignores old MCP validation that was auto-approved or rejected' {
        Mock Get-ItLogText -ModuleName ItE2E {
            'session_mcp_permission: validating session MCP permission before user selection'
        }
        Wait-TerminalActionProposal -App @{} -ReturnOnPermission | Should -BeNullOrEmpty
    }

    It 'ignores a previous session in the same helper' {
        Mock Get-ItLogText -ModuleName ItE2E {
            @'
permission_ui: permission queued for user selection request={"session_id":"old-session","tool_call_id":"lookup-1","kind":"command_lookup"}
permission_ui: current permission snapshot={"session_id":"old-session","tool_call_id":"lookup-1"}
'@
        }
        Wait-TerminalActionProposal -App @{} -ReturnOnPermission | Should -BeNullOrEmpty
    }

    It 'does not read another helpers permission evidence' {
        Mock Get-ItLogText -ModuleName ItE2E {
            param($Name)
            if ($Name -ne 'wta-main_helper-123.log') { throw 'Cross-helper log read' }
            ''
        }
        Wait-TerminalActionProposal -App @{} -PaneSessionId 'source-pane' -ReturnOnPermission | Should -BeNullOrEmpty
    }

    It 'pins the implicit target across polls' {
        Mock Wait-Until -ModuleName ItE2E {
            param($Condition, $Because)
            if ($Because -eq 'the target agent pane') { return & $Condition }
            $null = & $Condition
            & $Condition
        }
        $gate = Wait-TerminalActionProposal -App @{} -ReturnOnPermission
        $gate.PaneSessionId | Should -Be 'source-pane'
        Should -Invoke Get-AgentPaneSession -ModuleName ItE2E -Times 1 -Exactly -ParameterFilter { -not $PaneSessionId }
        Should -Invoke Get-AgentPaneSession -ModuleName ItE2E -Times 2 -Exactly -ParameterFilter { $PaneSessionId -eq 'source-pane' }
    }

    It 'does not let a stale legacy process gate hide a target card' {
        Mock Get-ItLogText -ModuleName ItE2E { 'proposal_permission: armed=true' }
        Mock Get-PendingTerminalActionProposal -ModuleName ItE2E { throw 'Global legacy process lookup' }
        Mock Get-AgentPaneText -ModuleName ItE2E { 'Insert in Terminal' }
        (Wait-TerminalActionProposal -App @{}).Ready | Should -BeTrue
        Should -Invoke Get-PendingTerminalActionProposal -ModuleName ItE2E -Times 0
    }

    It 'requires a target card even when legacy transport was armed elsewhere' {
        Mock Get-ItLogText -ModuleName ItE2E { 'proposal_permission: armed=true' }
        Wait-TerminalActionProposal -App @{} -ReturnOnPermission | Should -BeNullOrEmpty
    }
}

Describe 'Agent settings cleanup' -Tag 'Unit' {
    It 'removes showTokenUsageAndCost while preserving profiles' {
        $settingsPath = Join-Path $TestDrive 'settings.json'
        @{
            defaultProfile = '{6239a42c-1111-49a3-80bd-e8fdd045185c}'
            profiles = @(@{
                name = 'p0'
                guid = '{6239a42c-1111-49a3-80bd-e8fdd045185c}'
            })
            acpAgent = 'copilot'
            showTokenUsageAndCost = $true
        } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $settingsPath -Encoding utf8
        $app = [pscustomobject]@{ SettingsPath = $settingsPath }

        Clear-WtConfig -App $app

        $settings = Get-WtSettingsObject -App $app
        $settings.PSObject.Properties.Name | Should -Not -Contain 'showTokenUsageAndCost'
        $settings.PSObject.Properties.Name | Should -Not -Contain 'acpAgent'
        $settings.profiles[0].name | Should -Be 'p0'
    }
}

Describe 'Configuration backup and restore' -Tag 'Unit' {
    It 'restores existing configuration content' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'existing-settings.json'
            StatePath = Join-Path $TestDrive 'existing-state.json'
        }
        '{"original":"settings"}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{"original":"state"}' | Set-Content -LiteralPath $app.StatePath -Encoding utf8

        Backup-WtConfig -App $app
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{}' | Set-Content -LiteralPath $app.StatePath -Encoding utf8
        Restore-WtConfig -App $app

        Get-Content -LiteralPath $app.SettingsPath -Raw | Should -Match '"original":"settings"'
        Get-Content -LiteralPath $app.StatePath -Raw | Should -Match '"original":"state"'
    }

    It 'removes files created when the original configuration was absent' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'missing-settings.json'
            StatePath = Join-Path $TestDrive 'missing-state.json'
        }

        Backup-WtConfig -App $app
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{}' | Set-Content -LiteralPath $app.StatePath -Encoding utf8
        Restore-WtConfig -App $app

        Test-Path -LiteralPath $app.SettingsPath | Should -BeFalse
        Test-Path -LiteralPath $app.StatePath | Should -BeFalse
    }

    It 'records missing configuration when the fresh package directory is absent' {
        $localState = Join-Path $TestDrive 'fresh-package\LocalState'
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $localState 'settings.json'
            StatePath = Join-Path $localState 'state.json'
        }

        Backup-WtConfig -App $app

        Test-Path -LiteralPath "$($app.SettingsPath).e2ebak.missing" | Should -BeTrue
        Test-Path -LiteralPath "$($app.StatePath).e2ebak.missing" | Should -BeTrue
        Restore-WtConfig -App $app
        Test-Path -LiteralPath $app.SettingsPath | Should -BeFalse
        Test-Path -LiteralPath $app.StatePath | Should -BeFalse
    }

    It 'recovers a stale missing-file marker before taking a new snapshot' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'stale-settings.json'
            StatePath = Join-Path $TestDrive 'stale-state.json'
        }
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        [System.IO.File]::WriteAllBytes("$($app.SettingsPath).e2ebak.missing", [byte[]]::new(0))

        Backup-WtConfig -App $app

        Test-Path -LiteralPath $app.SettingsPath | Should -BeFalse
        Test-Path -LiteralPath "$($app.SettingsPath).e2ebak.missing" | Should -BeTrue
        Restore-WtConfig -App $app
    }

    It 'recovers a stale backup before taking a new snapshot' {
        $app = [pscustomobject]@{
            SettingsPath = Join-Path $TestDrive 'crashed-settings.json'
            StatePath = Join-Path $TestDrive 'crashed-state.json'
        }
        '{"test":"contaminated"}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        '{"original":"settings"}' | Set-Content -LiteralPath "$($app.SettingsPath).e2ebak" -Encoding utf8

        Backup-WtConfig -App $app

        Get-Content -LiteralPath $app.SettingsPath -Raw | Should -Match '"original":"settings"'
        '{}' | Set-Content -LiteralPath $app.SettingsPath -Encoding utf8
        Restore-WtConfig -App $app
        Get-Content -LiteralPath $app.SettingsPath -Raw | Should -Match '"original":"settings"'
    }
}

Describe 'Resolve-ItApp' -Tag 'Unit' {
    It 'retains the packaged wta path when WindowsApps denies existence checks' {
        InModuleScope ItE2E {
            $pfn = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
            $install = 'C:\Program Files\WindowsApps\Microsoft.IntelligentTerminal_1.2.3.4_x64__8wekyb3d8bbwe'
            $expectedWta = Join-Path $install 'wta.exe'
            Mock Get-AppxPackage {
                [pscustomobject]@{
                    PackageFamilyName = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
                    PackageFullName = 'Microsoft.IntelligentTerminal_1.2.3.4_x64__8wekyb3d8bbwe'
                    Version = [version]'1.2.3.4'
                    InstallLocation = 'C:\Program Files\WindowsApps\Microsoft.IntelligentTerminal_1.2.3.4_x64__8wekyb3d8bbwe'
                }
            }
            Mock Get-StartApps { @() }
            Mock Get-Command { $null }
            Mock Test-Path { $true }
            Mock Test-Path { $false } -ParameterFilter { $Path -eq $expectedWta }

            $app = Resolve-ItApp -Package $pfn

            $app.WtaPath | Should -Be $expectedWta
            Should -Invoke Test-Path -ParameterFilter { $Path -eq $expectedWta } -Times 0
        }
    }

    It 'resolves a descriptor with the expected shape when a package is installed' {
        $installed = @(Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' })
        if (-not $installed) { Set-ItResult -Skipped -Because 'no IT package installed'; return }
        $app = Resolve-ItApp -Package $installed[0].PackageFamilyName
        $app.Package | Should -Match 'IntelligentTerminal'
        $app.AppUserModelId | Should -Match '!'
        $app.SettingsPath | Should -Match 'LocalState\\settings\.json$'
        $app.WtcliPath | Should -Not -BeNullOrEmpty
    }
}

Describe 'Live test package selection' -Tag 'Unit' {
    It 'requires ITE2E_PACKAGE to be set explicitly' {
        InModuleScope ItE2E {
            $saved = $env:ITE2E_PACKAGE
            try {
                Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue
                { Get-ItTestPackage } | Should -Throw '*Choose the live integration-test package explicitly*'
            }
            finally {
                if ($null -eq $saved) { Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue }
                else { $env:ITE2E_PACKAGE = $saved }
            }
        }
    }

    It 'rejects Auto and accepts an explicit package selector' {
        InModuleScope ItE2E {
            $saved = $env:ITE2E_PACKAGE
            try {
                $env:ITE2E_PACKAGE = 'Auto'
                { Get-ItTestPackage } | Should -Throw "*'Auto' is not allowed*"
                $env:ITE2E_PACKAGE = 'Dev'
                Get-ItTestPackage | Should -Be 'Dev'
            }
            finally {
                if ($null -eq $saved) { Remove-Item Env:\ITE2E_PACKAGE -ErrorAction SilentlyContinue }
                else { $env:ITE2E_PACKAGE = $saved }
            }
        }
    }

    It 'rejects Auto before resolving or launching a terminal' {
        InModuleScope ItE2E {
            Mock Resolve-ItApp { throw 'must not be called' }
            { Start-Terminal -Package Auto } | Should -Throw "*'Auto' is not allowed*"
            Should -Invoke Resolve-ItApp -Times 0
        }
    }
}

Describe 'Package-scoped process cleanup' -Tag 'Unit' {
    It 'finds WindowsTerminal processes only under the selected package install location' {
        InModuleScope ItE2E {
            $app = [pscustomobject]@{
                InstallLocation = 'C:\DevPackage\AppX'
            }
            Mock Get-Process {
                @(
                    [pscustomobject]@{ Id = 101; Path = 'C:\DevPackage\AppX\WindowsTerminal.exe' }
                    [pscustomobject]@{ Id = 202; Path = 'C:\Program Files\WindowsApps\Microsoft.IntelligentTerminal_1.0.0.0_x64__8wekyb3d8bbwe\WindowsTerminal.exe' }
                    [pscustomobject]@{ Id = 303; Path = 'C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_1.0.0.0_x64__8wekyb3d8bbwe\WindowsTerminal.exe' }
                )
            }

            @(Get-WtProcessesForApp -App $app).Id | Should -Be @(101)
        }
    }

    It 'scopes stale-instance discovery to the selected app descriptor' {
        InModuleScope ItE2E {
            $app = [pscustomobject]@{
                Package = 'IntelligentTerminal_rd9vj3e6a2mbr'
                InstallLocation = 'C:\DevPackage\AppX'
            }
            Mock Get-CimInstance { $null }
            Mock Get-WtProcessesForApp { @() }
            Mock Get-AppxPackage { throw 'must not enumerate other packages' }

            Stop-StaleItInstances -App $app

            Should -Invoke Get-WtProcessesForApp -Times 1 -ParameterFilter { $App -eq $app }
            Should -Invoke Get-AppxPackage -Times 0
        }
    }
}

Describe 'Get-RunnableWtaPath staging' -Tag 'Unit' {
    It 'isolates staged binaries independently by package family, version, and content hash' {
        InModuleScope ItE2E {
            $originalTemp = $env:TEMP
            $env:TEMP = $TestDrive
            try {
                function New-StagingApp {
                    param([string]$Name, [string]$Package, [string]$Version, [string]$Content)
                    $root = Join-Path $TestDrive "WindowsApps\$Name"
                    New-Item -ItemType Directory -Force -Path $root | Out-Null
                    $wta = Join-Path $root 'wta.exe'
                    Set-Content -LiteralPath $wta -Value $Content -NoNewline
                    [pscustomobject]@{
                        Package = $Package
                        Version = $Version
                        InstallLocation = $root
                        WtaPath = $wta
                    }
                }

                $baseline = New-StagingApp -Name Base -Package store-family -Version 1.2.3.4 -Content same-content
                $differentPackage = New-StagingApp -Name Package -Package dev-family -Version 1.2.3.4 -Content same-content
                $differentVersion = New-StagingApp -Name Version -Package store-family -Version 9.8.7.6 -Content same-content
                $differentContent = New-StagingApp -Name Content -Package store-family -Version 1.2.3.4 -Content different-content

                $baselinePath = Get-RunnableWtaPath -App $baseline
                $packagePath = Get-RunnableWtaPath -App $differentPackage
                $versionPath = Get-RunnableWtaPath -App $differentVersion
                $contentPath = Get-RunnableWtaPath -App $differentContent

                $packagePath | Should -Not -Be $baselinePath
                $versionPath | Should -Not -Be $baselinePath
                $contentPath | Should -Not -Be $baselinePath
                $baselinePath | Should -Match ([regex]::Escape($baseline.Package))
                $baselinePath | Should -Match ([regex]::Escape($baseline.Version))
                $baselinePath | Should -Match ([regex]::Escape((Get-FileHash -LiteralPath $baseline.WtaPath -Algorithm SHA256).Hash))
                Get-Content -LiteralPath $baselinePath -Raw | Should -Be 'same-content'
                Get-Content -LiteralPath $contentPath -Raw | Should -Be 'different-content'
            }
            finally {
                $env:TEMP = $originalTemp
            }
        }
    }

    It 'fails explicitly instead of returning an unreadable WindowsApps path' {
        InModuleScope ItE2E {
            $root = Join-Path $TestDrive 'WindowsApps\Unreadable'
            New-Item -ItemType Directory -Force -Path $root | Out-Null
            $wta = Join-Path $root 'wta.exe'
            Set-Content -LiteralPath $wta -Value 'packaged-wta' -NoNewline
            $app = [pscustomobject]@{
                Package = 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe'
                Version = '1.2.3.4'
                InstallLocation = $root
                WtaPath = $wta
            }
            Mock Test-Path { return $false } -ParameterFilter { $Path -eq $wta }
            Mock Get-FileHash { throw 'access denied' }

            {
                Get-RunnableWtaPath -App $app
            } | Should -Throw '*Could not stage packaged wta*ite2e-wta*Microsoft.IntelligentTerminal_8wekyb3d8bbwe*1.2.3.4*<sha256>*access denied*'
            $app.PSObject.Properties.Name | Should -Not -Contain 'WtaRunnable'
        }
    }

    Context 'hook bundle refresh' {
        BeforeEach {
            $script:originalHookTestTemp = $env:TEMP
            $env:TEMP = $TestDrive
            InModuleScope ItE2E {
                $install = Join-Path $TestDrive 'WindowsApps\Hooks'
                $bundleSource = Join-Path $install 'wt-agent-hooks'
                New-Item -ItemType Directory -Force -Path $bundleSource | Out-Null
                Set-Content -LiteralPath (Join-Path $bundleSource 'marker.txt') -Value 'new-hooks' -NoNewline
                $wta = Join-Path $install 'wta.exe'
                Set-Content -LiteralPath $wta -Value 'packaged-wta' -NoNewline
                $app = [pscustomobject]@{
                    Package = 'hook-test-family'
                    Version = '1.2.3.4'
                    InstallLocation = $install
                    WtaPath = $wta
                }
                $sourceHash = (Get-FileHash -LiteralPath $wta -Algorithm SHA256).Hash
                $stageDir = Join-Path $TestDrive "ite2e-wta\$($app.Package)\$($app.Version)\$sourceHash"
                New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
                Copy-Item -LiteralPath $wta -Destination (Join-Path $stageDir 'wta.exe')
                $bundleDest = Join-Path $stageDir 'wt-agent-hooks'
                New-Item -ItemType Directory -Force -Path $bundleDest | Out-Null
                Set-Content -LiteralPath (Join-Path $bundleDest 'marker.txt') -Value 'old-hooks' -NoNewline
                $script:hookFixture = [pscustomobject]@{
                    App = $app
                    BundleSource = $bundleSource
                    BundleDest = $bundleDest
                    StageDir = $stageDir
                }
            }
        }

        AfterEach {
            $env:TEMP = $script:originalHookTestTemp
        }

        It 'preserves the existing hook bundle when copying the replacement fails' {
            InModuleScope ItE2E {
                $fixture = $script:hookFixture
                Mock Copy-Item { throw 'transient copy failure' } -ParameterFilter { $LiteralPath -eq $fixture.BundleSource }
                Mock Write-ItLog

                Get-RunnableWtaPath -App $fixture.App | Should -Be (Join-Path $fixture.StageDir 'wta.exe')

                Get-Content -LiteralPath (Join-Path $fixture.BundleDest 'marker.txt') -Raw | Should -Be 'old-hooks'
                @(Get-ChildItem -LiteralPath $fixture.StageDir -Directory | Where-Object Name -Like 'wt-agent-hooks.*').Count | Should -Be 0
                Should -Invoke Write-ItLog -ParameterFilter { $Level -eq 'WARN' -and $Message -like '*transient copy failure*' }
            }
        }

        It 'replaces the hook bundle and cleans swap artifacts after success' {
            InModuleScope ItE2E {
                $fixture = $script:hookFixture

                Get-RunnableWtaPath -App $fixture.App | Should -Be (Join-Path $fixture.StageDir 'wta.exe')

                Get-Content -LiteralPath (Join-Path $fixture.BundleDest 'marker.txt') -Raw | Should -Be 'new-hooks'
                @(Get-ChildItem -LiteralPath $fixture.StageDir -Directory | Where-Object Name -Like 'wt-agent-hooks.*').Count | Should -Be 0
            }
        }

        It 'restores the existing hook bundle when activating the staged copy fails' {
            InModuleScope ItE2E {
                $fixture = $script:hookFixture
                Mock Move-Item { throw 'transient activation failure' } -ParameterFilter {
                    $LiteralPath -like "$($fixture.BundleDest).staging.*" -and $Destination -eq $fixture.BundleDest
                }

                Get-RunnableWtaPath -App $fixture.App | Should -Be (Join-Path $fixture.StageDir 'wta.exe')

                Get-Content -LiteralPath (Join-Path $fixture.BundleDest 'marker.txt') -Raw | Should -Be 'old-hooks'
                @(Get-ChildItem -LiteralPath $fixture.StageDir -Directory | Where-Object Name -Like 'wt-agent-hooks.*').Count | Should -Be 0
            }
        }
    }
}

Describe 'Feature suite package selection' -Tag 'Unit' {
    It 'resolves the Yolo suite package once and uses it for every app operation' {
        $suitePath = Join-Path $PSScriptRoot '..\tests\Feature.YoloMode.Tests.ps1'
        $suite = Get-Content -LiteralPath $suitePath -Raw

        ([regex]::Matches($suite, '\bGet-ItTestPackage\b')).Count | Should -Be 1
        ([regex]::Matches($suite, '(?m)^Describe ')).Count | Should -Be 6
        ([regex]::Matches($suite, '(?m)^Describe .* -ForEach \$script:PackageCase\b')).Count | Should -Be 6
        $suite | Should -Not -Match '-Package\s+Dev\b'
        $suite | Should -Not -Match 'Resolve-ItApp\s+-Package\s+(?!\$(?:script:Package|Package)\b)'
        $suite | Should -Not -Match 'Start-Terminal\s+-Package\s+(?!\$Package\b)'
        $suite | Should -Match 'openCodeInstalled\s*=.*Get-Command\s+opencode'
        $suite | Should -Not -Match 'Get-AgentAcpStatus.*opencode acp'
        $suite | Should -Not -Match 'AgentYoloStatusText|/yolo (?:on|off)'
        $suite | Should -Not -Match 'requires whitespace-free test paths'
        $suite | Should -Match '-EncodedCommand\s+\$encodedInvocation'
        $suite | Should -Match 'Profile automatic approval stays scoped to the Settings default provider'
    }

    It 'hides unavailable Settings controls and removes explanatory messages' {
        $settingsXamlPath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\AIAgents.xaml'
        $settingsXaml = Get-Content -LiteralPath $settingsXamlPath -Raw
        $setting = [regex]::Match(
            $settingsXaml,
            '(?s)<local:SettingContainer x:Name="AgentPaneYoloMode".*?</local:SettingContainer>').Value

        $setting | Should -Match 'Visibility="\{x:Bind ViewModel\.AgentPaneYoloModeVisibility, Mode=OneWay\}"'
        $setting | Should -Match 'IsEnabled="\{x:Bind ViewModel\.CanEnableAgentPaneYoloMode, Mode=OneWay\}"'
        $setting | Should -Not -Match 'AIAgents_PolicyLocked'
        $settingsXaml | Should -Not -Match 'OpenCodeYoloCompatibilityInfoBar'
        $settingsXaml | Should -Match 'GeminiYoloCompatibilityInfoBar'

        $viewModelIdlPath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\AIAgentsViewModel.idl'
        $viewModelIdl = Get-Content -LiteralPath $viewModelIdlPath -Raw
        $viewModelIdl | Should -Match 'Windows\.UI\.Xaml\.Visibility AgentPaneYoloModeVisibility \{ get; \};'
        $viewModelIdl | Should -Not -Match 'ShowOpenCodeYoloWarning'
        $viewModelCppPath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\AIAgentsViewModel.cpp'
        $viewModelCpp = Get-Content -LiteralPath $viewModelCppPath -Raw
        $viewModelCpp | Should -Match '!\(_isAddingCustomAcpAgent && _editingCustomAcpAgentId\.empty\(\)\)'
        $sameCustomRestore = [regex]::Match(
            $viewModelCpp,
            '(?s)if \(_GlobalSettings\.AcpAgent\(\) == value\.Id\(\)\).*?(?=const bool agentChanged)').Value
        $sameCustomRestore | Should -Match 'CanEnableAgentPaneYoloMode'
        $sameCustomRestore | Should -Match 'AgentPaneYoloModeVisibility'

        $resourceRoot = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\Resources'
        $localeDirectories = @(Get-ChildItem -LiteralPath $resourceRoot -Directory)
        foreach ($localeDirectory in $localeDirectories) {
            [xml]$xml = Get-Content -LiteralPath (Join-Path $localeDirectory.FullName 'Resources.resw') -Raw
            @($xml.root.data | Where-Object name -in @(
                'AIAgents_YoloOpenCodeWarning.Title',
                'AIAgents_YoloOpenCodeWarning.Message'
            )) | Should -HaveCount 0 -Because "OpenCode has no message when automatic approval is hidden in $($localeDirectory.Name)"
        }
    }

    It 'reuses Settings copy and model state in the first-run experience' {
        $freXamlPath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\FreOverlay.xaml'
        $freXaml = Get-Content -LiteralPath $freXamlPath -Raw
        $freXaml | Should -Match 'x:Name="AutomaticApprovalSetting"'
        $freXaml | Should -Match 'x:Name="AutomaticApprovalTitle"'
        $freXaml | Should -Match 'x:Name="AutomaticApprovalDescription"'
        $freXaml | Should -Match 'x:Name="AutomaticApprovalToggle"'
        $freXaml | Should -Match 'SelectionChanged="_OnAgentSelectionChanged"'

        $freCppPath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\FreOverlay.cpp'
        $freCpp = Get-Content -LiteralPath $freCppPath -Raw
        $freCpp | Should -Match 'ScopedResourceLoader\s+\w+\{\s*L"Microsoft\.Terminal\.Settings\.Editor/Resources"\s*\}'
        $freCpp | Should -Match 'AIAgents_YoloMode/Header'
        $freCpp | Should -Match 'AIAgents_YoloMode/HelpText'
        $freCpp | Should -Match '_UpdateAutomaticApprovalState\(\)'
        $freCpp | Should -Match '_refreshingAgentComboBox'
        $freCpp | Should -Match '(?s)_PopulateAgentComboBox\(\).*?scope_exit.*?_UpdateAutomaticApprovalState\(\)'
        $freCpp | Should -Match '(?s)_OnAgentSelectionChanged.*?!_refreshingAgentComboBox.*?_UpdateAutomaticApprovalState\(\)'
        $freCpp | Should -Match 'CanEnableAgentPaneYoloModeForAgent'
        $freCpp | Should -Match 'AgentPaneYoloMode\(AutomaticApprovalToggle\(\)\.IsOn\(\)\)'
        $freCpp | Should -Match 'ClearAgentPaneYoloModeIfUnavailableDefault\(\)'
        $freCpp | Should -Match 'ClearAgentPaneYoloModeIfPolicyBlocked\(\)'
        $freHeaderPath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\FreOverlay.h'
        $freHeader = Get-Content -LiteralPath $freHeaderPath -Raw
        $freHeader | Should -Match 'void UpdateSettings\(.*CascadiaSettings'
        $terminalPagePath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\TerminalPage.cpp'
        $terminalPage = Get-Content -LiteralPath $terminalPagePath -Raw
        $freRefresh = [regex]::Match(
            $terminalPage,
            '(?s)_settings = settings;(?<refresh>.*?)(?=        if \(!firstLoad && needRefreshUI\))').Groups['refresh'].Value
        $freRefresh | Should -Match 'FreOverlayElement\(\)'
        $freRefresh | Should -Match 'UpdateSettings\(_settings\)'
        $freRefresh | Should -Not -Match '_IsFreRequired\(\)'

        $terminalResourceRoot = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\Resources'
        foreach ($resource in Get-ChildItem -LiteralPath $terminalResourceRoot -Recurse -Filter Resources.resw) {
            [xml]$xml = Get-Content -LiteralPath $resource.FullName -Raw
            @($xml.root.data | Where-Object name -Like 'FreOverlay_AutomaticApproval*') |
                Should -HaveCount 0 -Because 'FRE must reuse the SettingsEditor localized copy instead of duplicating it'
        }

        $freTestsPath = Join-Path $PSScriptRoot '..\tests\Feature.FreAgentSetup.Tests.ps1'
        $freTests = Get-Content -LiteralPath $freTestsPath -Raw
        $freTests | Should -Match 'Send-WtWindowKey\s+-App \$script:app\s+-Vk 0x1B'
        $freTests | Should -Not -Match "Selector 'Light Dismiss'"
    }

    It 'round-trips Yolo control ownership across saved agent panes' {
        $repoRoot = Join-Path $PSScriptRoot '..\..\..'

        $restore = Get-Content -LiteralPath (Join-Path $repoRoot 'src\cascadia\inc\AgentPaneRestore.h') -Raw
        $restore | Should -Match 'YoloControlOwnerFlag'
        $restore | Should -Match 'std::wstring yoloControlOwner;'
        $restore | Should -Match 'IsValidYoloControlOwner'

        $contentHeader = Get-Content -LiteralPath (Join-Path $repoRoot 'src\cascadia\TerminalApp\AgentPaneContent.h') -Raw
        $contentSource = Get-Content -LiteralPath (Join-Path $repoRoot 'src\cascadia\TerminalApp\AgentPaneContent.cpp') -Raw
        $contentHeader | Should -Match 'SetYoloControlOwner'
        $contentHeader | Should -Match '_yoloControlOwner'
        $contentSource | Should -Match 'fields\.yoloControlOwner = _yoloControlOwner'
        $ownerSetter = [regex]::Match(
            $contentHeader,
            '(?s)void SetYoloControlOwner\(.*?\n        \}').Value
        $ownerSetter | Should -Match 'IsValidYoloControlOwner'
        $ownerSetter | Should -Match '\?\s*owner\s*:\s*winrt::hstring\{\}' `
            -Because 'empty or invalid owner projections must explicitly clear stale provenance'

        $terminalPage = Get-Content -LiteralPath (Join-Path $repoRoot 'src\cascadia\TerminalApp\TerminalPage.cpp') -Raw
        $terminalPage | Should -Match 'params\.isMember\("yolo_control_owner"\)'
        $terminalPage | Should -Match 'SetYoloControlOwner\(\*yoloControlOwner\)'
        $terminalPage | Should -Match '--initial-yolo-control-owner'
        $terminalPage | Should -Match 'fields\.yoloControlOwner'
        $ownerProjection = [regex]::Match(
            $terminalPage,
            '(?s)std::optional<winrt::hstring> yoloControlOwner;.*?(?=std::optional<bool> wantOpen;)').Value
        $ownerProjection | Should -Not -Match 'isMember\("yolo_control_owner"\)\s*&&' `
            -Because 'a present null or invalid field is an explicit clear, not an omitted update'
        $ownerProjection | Should -Match ':\s*winrt::hstring\{\};'

        $statusProjection = Get-Content -LiteralPath (Join-Path $repoRoot 'tools\wta\src\app_status_projection.rs') -Raw
        $appEvents = Get-Content -LiteralPath (Join-Path $repoRoot 'tools\wta\src\app_events.rs') -Raw
        $cliArgs = Get-Content -LiteralPath (Join-Path $repoRoot 'tools\wta\src\cli\args.rs') -Raw
        $statusProjection | Should -Match '"yolo_control_owner"'
        $statusProjection | Should -Match '\.owner\(session_id\)'
        $appEvents | Should -Match 'initial_yolo_control_owner\s*\.take\(\)'
        $cliArgs | Should -Match 'initial_yolo_control_owner'

        $wtProtocolEvents = Get-Content -LiteralPath (Join-Path $repoRoot 'tools\wta\src\wt_protocol_events.rs') -Raw
        $sendBody = [regex]::Match(
            $wtProtocolEvents,
            '(?s)pub fn send\(json_payload: String\).*?(?=\n\})').Value
        $sendBody | Should -Match '#\[cfg\(test\)\]'
        $sendBody | Should -Match '#\[cfg\(not\(test\)\)\]' `
            -Because 'unit-test event capture must not launch the external wtcli publisher'
    }
}

Describe 'Yolo Settings save normalization' -Tag 'Unit' {
    It 'clears unavailable and policy-blocked Yolo before writing Settings' {
        $mainPagePath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\MainPage.cpp'
        $source = Get-Content -LiteralPath $mainPagePath -Raw
        $saveHandler = [regex]::Match(
            $source,
            '(?s)void MainPage::SaveButton_Click.*?(?=void MainPage::ResetButton_Click)').Value

        $saveHandler | Should -Match 'ClearAgentPaneYoloModeIfUnavailableDefault\(\);'
        $saveHandler | Should -Match 'ClearAgentPaneYoloModeIfPolicyBlocked\(\);'
        $saveHandler | Should -Match '(?s)ClearAgentPaneYoloModeIfUnavailableDefault\(\);.*ClearAgentPaneYoloModeIfPolicyBlocked\(\);.*WriteSettingsToDisk\(\)'
    }
}

Describe 'Agent provider identity ownership' -Tag 'Unit' {
    It 'updates the current provider only from helper status' {
        $terminalPagePath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalApp\TerminalPage.cpp'
        $source = Get-Content -LiteralPath $terminalPagePath -Raw
        $rebindHandler = [regex]::Match(
            $source,
            '(?s)void TerminalPage::_RaiseAgentPaneRebindRequest.*?(?=TerminalPage::AgentRuntimeConfigSnapshot)').Value

        $rebindHandler | Should -Match '_RaiseProtocolEvent\("rebind_agent", params\);'
        $rebindHandler | Should -Not -Match 'AgentCurrentId\('
        $statusHandler = [regex]::Match(
            $source,
            '(?s)void TerminalPage::OnAgentStatusChanged\(.*?(?=void TerminalPage::OnAgentStateChanged)').Value
        $identityUpdate = [regex]::Match(
            $statusHandler,
            '(?s)const auto agentId = pickStr\("agent_id"\);.*?(?=const bool usesHostCatalog)').Value
        $statusHandler | Should -Match 'const auto agentIdSpecified = params\.isMember\("agent_id"\);'
        $identityUpdate | Should -Match 'statusTab->AgentCurrentId\(agentId\);'
        $identityUpdate | Should -Not -Match '!agentId\.empty\(\)' `
            -Because 'a present empty agent_id must clear stale per-tab provider identity'
    }
}

Describe 'Yolo Settings localization contract' -Tag 'Unit' {
    It 'uses the PM-approved English title and description' {
        $resourcePath = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\Resources\en-US\Resources.resw'
        [xml]$resources = Get-Content -LiteralPath $resourcePath -Raw

        [string]($resources.root.data |
                Where-Object name -eq 'AIAgents_YoloMode.Header' |
                Select-Object -First 1).value |
            Should -Be 'Automatic approval'
        [string]($resources.root.data |
                Where-Object name -eq 'AIAgents_YoloMode.HelpText' |
                Select-Object -First 1).value |
            Should -Be 'Your agent in the agent pane runs with full permissions provided by the agent CLI'
        @($resources.root.data |
                Where-Object name -eq 'AIAgents_YoloGeminiInfo.Title') |
            Should -HaveCount 0
        [string]($resources.root.data |
                Where-Object name -eq 'AIAgents_YoloGeminiInfo.Message' |
                Select-Object -First 1).value |
            Should -Be 'Automatic approval with Gemini is restricted to trusted workspaces'
    }

    It 'keeps every Settings locale structurally aligned' {
        $resourceRoot = Join-Path $PSScriptRoot '..\..\..\src\cascadia\TerminalSettingsEditor\Resources'
        $localeDirectories = @(Get-ChildItem -LiteralPath $resourceRoot -Directory)
        $keys = @(
            'AIAgents_YoloMode.Header',
            'AIAgents_YoloMode.HelpText',
            'AIAgents_YoloGeminiInfo.Message'
        )
        [xml]$english = Get-Content -LiteralPath (Join-Path $resourceRoot 'en-US\Resources.resw') -Raw

        foreach ($localeDirectory in $localeDirectories) {
            $resourcePath = Join-Path $localeDirectory.FullName 'Resources.resw'
            $bytes = [System.IO.File]::ReadAllBytes($resourcePath)
            ($bytes.Length -ge 3 -and
                $bytes[0] -eq 0xEF -and
                $bytes[1] -eq 0xBB -and
                $bytes[2] -eq 0xBF) | Should -BeTrue -Because "$resourcePath must retain its UTF-8 BOM"

            [xml]$localized = Get-Content -LiteralPath $resourcePath -Raw
            foreach ($key in $keys) {
                $source = @($english.root.data | Where-Object name -eq $key)
                $target = @($localized.root.data | Where-Object name -eq $key)

                $source | Should -HaveCount 1
                $target | Should -HaveCount 1 -Because "$key must exist exactly once in $($localeDirectory.Name)"
                [string]$target[0].value | Should -Not -BeNullOrEmpty
                [string]$target[0].comment | Should -Be ([string]$source[0].comment)

                if ($key -eq 'AIAgents_YoloMode.HelpText') {
                    [string]$target[0].comment | Should -MatchExactly '\{Locked="CLI"\}'
                    [string]$target[0].value | Should -MatchExactly '(?<![A-Za-z])CLI(?![A-Za-z])'
                }

                if ($key -eq 'AIAgents_YoloGeminiInfo.Message') {
                    [string]$target[0].comment | Should -MatchExactly '\{Locked="Gemini"\}'
                    [string]$target[0].value | Should -MatchExactly '(?<![A-Za-z])Gemini(?![A-Za-z])'
                    @($localized.root.data |
                            Where-Object name -eq 'AIAgents_YoloGeminiInfo.Title') |
                        Should -HaveCount 0
                }

                if ($localeDirectory.Name -notin @('en-US', 'qps-ploc', 'qps-ploca', 'qps-plocm')) {
                    [string]$target[0].value |
                        Should -Not -Be ([string]$source[0].value) -Because "$key must be translated in $($localeDirectory.Name)"
                }
            }
        }
    }
}

Describe 'Start-Terminal startup ordering' -Tag 'Unit' {
    It 'waits for the first window before probing COM' {
        InModuleScope ItE2E {
            $script:startupOrder = @()
            $root = Join-Path $env:TEMP "ite2e-startup-order-$PID"
            $fakeApp = [pscustomobject]@{
                Package = 'IntelligentTerminal_rd9vj3e6a2mbr'
                Version = '0.0.0.0'
                WtcliPath = 'wtcli.exe'
                LocalStateDir = $root
                SettingsPath = Join-Path $root 'settings.json'
                StatePath = Join-Path $root 'state.json'
                AppUserModelId = 'IntelligentTerminal_rd9vj3e6a2mbr!App'
                LaunchAlias = $null
                ComClsid = $null
                Pid = $null
                Hwnd = $null
            }

            Mock Resolve-ItApp { $fakeApp }
            Mock Stop-StaleItInstances
            Mock Get-WtProcessesForApp { [pscustomobject]@{ Id = 4242 } }
            Mock Start-Process
            Mock Get-WtWindowHwnds {
                $script:startupOrder += 'hwnd'
                [pscustomobject]@{ pid = 4242; hwnd = 9001; title = 'PowerShell' }
            }
            Mock Resolve-WtComClsid {
                $script:startupOrder += 'com'
                '{D5B7C9E1-4F6A-4B8C-D9E0-F1A2B3C4D5E6}'
            }
            Mock Get-ActivePane { [pscustomobject]@{ window_id = 1 } }
            Mock Initialize-LogOffsets
            Mock Write-ItLog

            $app = Start-Terminal -Package Dev -PassFre $false -Backup $false -CleanSettings $false

            $app.Hwnd | Should -Be 9001
            $script:startupOrder | Should -Be @('hwnd', 'com')
            Should -Invoke Stop-StaleItInstances -Times 1 -ParameterFilter { $App -eq $fakeApp }
        }
    }
}

Describe 'Agent readiness log boundary' -Tag 'Unit' {
    It 'checks failures only in the current launch log slice' {
        InModuleScope ItE2E {
            $script:observedOffset = $null
            $app = [pscustomobject]@{
                LogStartOffset = @{ 'wta-main_helper-old.log' = 200 }
                PreLaunchLogStartOffset = @{ 'wta-main_helper-old.log' = 100 }
            }

            Mock Open-AgentPane
            Mock Get-AgentConnectedPlaceholderRegex { 'connected-never-matches' }
            Mock Get-AgentPaneText { '' }
            Mock Get-ItLogText {
                param($App)
                $script:observedOffset = $App.LogStartOffset['wta-main_helper-old.log']
                'class=auth_required'
            }
            Mock Write-ItLog

            Wait-AgentReady -App $app -TimeoutSec 1 | Should -BeFalse
            $script:observedOffset | Should -Be 100
        }
    }

    It 'matches native Yolo updates by launch slice and ACP session' {
        InModuleScope ItE2E {
            $script:observedOffset = $null
            $app = [pscustomobject]@{
                LogStartOffset = @{ 'wta-main_helper-old.log' = 200 }
                PreLaunchLogStartOffset = @{ 'wta-main_helper-old.log' = 100 }
            }
            Mock Get-ItLogText {
                param($App)
                $script:observedOffset = $App.LogStartOffset['wta-main_helper-old.log']
                'session_id=session-a provider-native Yolo updated for live session enabled=true'
            }

            Test-AgentNativeYoloUpdate -App $app -AcpSessionId 'session-a' -Enabled $true | Should -BeTrue
            Test-AgentNativeYoloUpdate -App $app -AcpSessionId 'session-b' -Enabled $true | Should -BeFalse
            Test-AgentNativeYoloUpdate -App $app -AcpSessionId 'session-a' -Enabled $false | Should -BeFalse
            $script:observedOffset | Should -Be 100
        }
    }
}
