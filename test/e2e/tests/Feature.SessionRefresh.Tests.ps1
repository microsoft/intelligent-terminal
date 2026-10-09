#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: master-owned session refresh' -Tag @('Feature', 'SessionRefresh') {
    BeforeAll {
        $script:ownsConfig = $false
        $script:app = $null
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'This PR validation requires an explicitly selected Dev build.' }
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        if (-not $env:ITE2E_EXPECTED_WTA_SHA256 -or -not $env:ITE2E_EXPECTED_APP_SHA256) {
            throw 'Supply WTA and TerminalApp hashes from the intended feature build.'
        }
        (Get-FileHash -LiteralPath $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Existing configuration backup requires manual recovery: $path"
            }
            $script:originalHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('session-refresh-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $script:historyPath = Join-Path $script:evidence 'history.json'
        $script:fixtureLog = Join-Path $script:evidence 'fixture.log'
        $script:historyId = 'refresh-history-' + [guid]::NewGuid().ToString('N')
        $script:provider = 'custom:session-refresh-fixture'

        function Set-FixtureHistory {
            param([string]$Title)
            $json = @{ sessions = @(@{
                sessionId = $script:historyId
                cwd = $script:evidence
                title = $Title
                updatedAt = [DateTimeOffset]::UtcNow.ToString('o')
            }) } | ConvertTo-Json -Depth 6 -Compress
            $temp = Join-Path $script:evidence ('history-' + [guid]::NewGuid().ToString('N') + '.tmp')
            [IO.File]::WriteAllText($temp, $json, [Text.UTF8Encoding]::new($false))
            Move-Item -LiteralPath $temp -Destination $script:historyPath -Force
        }
        function Get-FixtureCalls {
            param([string]$Method = 'list')
            if (-not (Test-Path -LiteralPath $script:fixtureLog)) { return }
            foreach ($line in Get-Content -LiteralPath $script:fixtureLog) {
                if ($line -match ('^(?<pid>\d+)\|' + [regex]::Escape($Method) + '\|(?<at>[^|]+)')) {
                    [pscustomobject]@{ Pid = [int]$Matches.pid; At = [DateTimeOffset]::Parse($Matches.at) }
                }
            }
        }
        function Read-MasterSnapshot {
            $result = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $script:pipe, '--json', '--include-status') -Raw
            $result.ExitCode | Should -Be 0 -Because $result.StdErr
            $result.StdOut | ConvertFrom-Json -Depth 32
        }
        function Get-MasterTrace {
            Get-ItLogText -App $script:logApp -Name 'wta-main_master.log' -SinceStart
        }
        function Get-HelperTrace {
            Get-ItLogText -App $script:logApp -Name "wta-main_helper-$($script:pane.HelperProcessId).log" -SinceStart
        }
        function Test-NativeVisible {
            param([string]$Id)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            if ($window.Current.ProcessId -ne $script:app.Pid) { throw 'Session view window changed ownership.' }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            $element = $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
            $element -and $element.Current.ProcessId -eq $script:app.Pid -and -not $element.Current.IsOffscreen -and
                $element.Current.BoundingRectangle.Width -gt 0 -and $element.Current.BoundingRectangle.Height -gt 0
        }
        function Set-TestLayout {
            param([ValidateSet('vertical', 'horizontal')][string]$Layout)
            Set-WtSetting -App $script:app -Key tabLayout -Value $Layout | Out-Null
            $expected = if ($Layout -eq 'vertical') { 'true' } else { 'false' }
            Wait-Until -TimeoutSec 20 -Because "helper receives actual $Layout window layout" -Condition {
                $matches = [regex]::Matches((Get-HelperTrace), '"sessions_in_sidebar":(true|false)')
                $matches.Count -gt 0 -and $matches[$matches.Count - 1].Groups[1].Value -eq $expected
            } | Out-Null
            $selector = if ($Layout -eq 'vertical') { 'VerticalTabsHeaderButton' } else { 'SessionToggleButton' }
            Wait-Until -TimeoutSec 15 -Because "$Layout session entry point is visible" -Condition {
                Test-NativeVisible -Id $selector
            } | Out-Null
        }
        function Open-TestSidebar {
            if (-not (Test-NativeVisible -Id HistoryHeaderButton)) {
                Invoke-UiClick -App $script:app -Selector VerticalTabsHeaderButton | Out-Null
            }
            Wait-Until -TimeoutSec 15 -Because 'native history panel is open' -Condition {
                Test-NativeVisible -Id HistoryHeaderButton
            } | Out-Null
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $header = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'VerticalTabsHeader'))
            $header.Current.Name | Should -Be 'Agents'
            $search = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, 'SearchTabsButton'))
            $search.GetCurrentPattern([Windows.Automation.TogglePattern]::Pattern).Current.ToggleState |
                Should -Be ([Windows.Automation.ToggleState]::Off)
            Test-NativeVisible -Id SearchTextBox | Should -BeFalse -Because 'passive Agents navigation must not enable search'
        }
        function Read-TraceRecords {
            param([DateTimeOffset]$Since)
            foreach ($line in (Get-MasterTrace) -split '\r?\n') {
                if ($line -notmatch '^(?<at>\S+).*helper_id=HelperId\((?<helper>\d+)\)') { continue }
                $at = [DateTimeOffset]::Parse($Matches.at)
                $helper = [int]$Matches.helper
                if ($at -lt $Since) { continue }
                if ($line -match 'routing ext_method.*method=intellterm.wta/sessions/list\b') {
                    [pscustomobject]@{ At = $at; Helper = $helper; Kind = 'read' }
                }
                elseif ($line -match 'writing live-set ext-notification.*method=_intellterm.wta/sessions/changed\b') {
                    [pscustomobject]@{ At = $at; Helper = $helper; Kind = 'push' }
                }
            }
        }
        function Measure-QuietFallback {
            param([string]$Label, [bool]$Sidebar, [bool]$Helper)
            # A complete, real 65-second quiet interval distinguishes a fallback
            # from push-driven reads without shortening production timers.
            $since = [DateTimeOffset]::UtcNow.AddSeconds(2)
            $deadline = [DateTimeOffset]::UtcNow.AddSeconds(190)
            do {
                Start-Sleep -Seconds 1
                $records = @(Read-TraceRecords -Since $since)
                $pushes = @($records | Where-Object Kind -eq push)
                if ($pushes.Count) { $since = ($pushes | Sort-Object At | Select-Object -Last 1).At.AddSeconds(2) }
                if (([DateTimeOffset]::UtcNow - $since).TotalSeconds -ge 65) { break }
                if ([DateTimeOffset]::UtcNow -ge $deadline) {
                    throw "No complete quiet observation window for $Label; background pushes remain active."
                }
            } while ($true)
            $until = [DateTimeOffset]::UtcNow
            $records = @(Read-TraceRecords -Since $since)
            $reads = @($records | Where-Object Kind -eq read)
            $helperReads = @($reads | Where-Object Helper -eq $script:helperId)
            $sidebarReads = @($reads | Where-Object Helper -ne $script:helperId)
            $fixtureReads = @(Get-FixtureCalls | Where-Object { $_.At -ge $since -and $_.At -le $until })
            @{
                phase = $Label; since = $since.ToString('o'); until = $until.ToString('o')
                helper_reads = $helperReads; sidebar_reads = $sidebarReads
                upstream_reads = $fixtureReads
            } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence "$Label.json") -Encoding utf8
            @($records | Where-Object Kind -eq push).Count | Should -Be 0
            if ($Helper) {
                $helperReads.Count | Should -BeGreaterOrEqual 1
                $helperReads.Count | Should -BeLessOrEqual 2
            } else { $helperReads.Count | Should -Be 0 -Because "$Label must not poll through the helper" }
            if ($Sidebar) {
                $sidebarReads.Count | Should -BeGreaterOrEqual 1
                $sidebarReads.Count | Should -BeLessOrEqual 2
            } else { $sidebarReads.Count | Should -Be 0 -Because "$Label must not poll through Sidebar" }
            $fixtureReads.Count | Should -BeGreaterOrEqual 11 -Because 'master keeps its five-second cadence independently of visible views'
            $fixtureReads.Count | Should -BeLessOrEqual 15 -Because 'frontend reads must not multiply upstream queries'
        }

        Set-FixtureHistory -Title 'Refresh fixture initial'
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))' -HistoryPath '$($script:historyPath.Replace("'", "''"))'"
        $command = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
        $settings = Get-WtSettingsObject -App $script:target
        if (-not $settings) { throw 'Refusing to replace unparseable user settings for a test.' }
        $profileId = '{' + [guid]::NewGuid().ToString() + '}'
        $profile = [pscustomobject]@{
            guid = $profileId; name = 'ItE2E session refresh'
            commandline = '"' + (Get-Command pwsh).Source + '" -NoLogo -NoProfile -NoExit'
            startingDirectory = $script:evidence
            acpAgent = $script:provider; acpCustomCommand = $command; acpModel = ''; acpSource = 'host'
        }
        if ($settings.profiles -is [array]) {
            $profiles = @($settings.profiles) + $profile
        } else {
            $profiles = $settings.profiles
            if (-not $profiles) { $profiles = [pscustomobject]@{} }
            $list = if ($profiles.PSObject.Properties.Name -contains 'list') { @($profiles.list) } else { @() }
            $profiles | Add-Member -NotePropertyName list -NotePropertyValue @($list + $profile) -Force
        }
        Stop-StaleItInstances -App $script:target
        $script:ownsConfig = $true
        $script:app = Start-Terminal -Package Dev -CleanSettings $false -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            profiles = $profiles; defaultProfile = $profileId
            firstWindowPreference = 'defaultProfile'; startupActions = ''
            tabLayout = 'vertical'; tabLayoutVerticalWidth = 320; language = 'en-US'
            acpAgent = $script:provider; acpCustomCommand = $command; acpModel = ''
            customModelSelection = ''; autoFixEnabled = $false
        }
        $script:logApp = $script:app.PSObject.Copy()
        $script:logApp.LogStartOffset = $script:app.PreLaunchLogStartOffset
        $script:pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
        (Wait-AgentReady -App $script:app -TimeoutSec 90) | Should -BeTrue
        $script:pane = Get-AgentPaneSession -App $script:app
        $script:pane.AcpSessionId | Should -Match '^chat-fixture-'
        $script:helperId = Wait-Until -TimeoutSec 15 -Because 'the fixture session is bound to its helper' -Condition {
            $pattern = 'session bound to helper.*helper_id=HelperId\((\d+)\).*session_id=SessionId\("' +
                [regex]::Escape($script:pane.AcpSessionId) + '"\)'
            $match = [regex]::Match((Get-MasterTrace), $pattern)
            if ($match.Success) { [int]$match.Groups[1].Value }
        }
        Wait-Until -TimeoutSec 120 -Because 'startup history discovery has finished' -Condition {
            (Read-MasterSnapshot).history_status -in @('ready', 'error')
        } | Out-Null
        Set-TestLayout vertical
        @{
            package = $script:app.Package; version = $script:app.Version
            install_location = $script:app.InstallLocation; source_commit = $env:ITE2E_SOURCE_COMMIT
            wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256; app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256
            helper_pid = $script:pane.HelperProcessId; helper_id = $script:helperId
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json') -Encoding utf8
    }

    AfterAll {
        if ($script:app) {
            try {
                Get-MasterTrace | Set-Content -LiteralPath (Join-Path $script:evidence 'master.log') -Encoding utf8
                Get-HelperTrace | Set-Content -LiteralPath (Join-Path $script:evidence 'helper.log') -Encoding utf8
            } finally {
                Stop-Terminal -App $script:app -RestoreSettings $false
            }
        }
        if ($script:target -and $script:ownsConfig) {
            if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                throw 'Package is still active; retaining configuration backups rather than racing a live writer.'
            }
            Restore-WtConfig -App $script:target
            foreach ($path in $script:originalHashes.Keys) {
                $hash = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                $hash | Should -Be $script:originalHashes[$path] -Because 'user configuration must be restored byte-for-byte'
            }
            'settings.json and state.json restored byte-for-byte' |
                Set-Content -LiteralPath (Join-Path $script:evidence 'cleanup.txt')
        }
    }

    It 'Master refreshes history without an open session view' {
        Stop-AgentPane -App $script:app | Out-Null
        Set-FixtureHistory -Title 'Refresh fixture background'
        Wait-Until -TimeoutSec 20 -Because 'closed views still receive newly synchronized history' -Condition {
            @((Read-MasterSnapshot).sessions | Where-Object {
                $_.session_id -eq $script:historyId -and $_.title -eq 'Refresh fixture background'
            }).Count -eq 1
        } | Out-Null
        $before = @(Get-FixtureCalls).Count
        $watch = [Diagnostics.Stopwatch]::StartNew()
        foreach ($i in 1..12) {
            @((Read-MasterSnapshot).sessions | Where-Object session_id -eq $script:historyId).Count | Should -Be 1
        }
        $watch.Stop()
        (@(Get-FixtureCalls).Count - $before) | Should -BeLessOrEqual ([math]::Ceiling($watch.Elapsed.TotalSeconds / 5) + 1)
        @(Get-FixtureCalls -Method initialize).Count | Should -Be 1
    }

    It 'Session fallback follows vertical and horizontal layouts' {
        Set-TestLayout vertical
        Open-SessionList -App $script:app | Out-Null
        Test-SessionListShown -App $script:app | Should -BeTrue
        Open-TestSidebar
        Measure-QuietFallback -Label vertical -Sidebar $true -Helper $false

        Set-TestLayout horizontal
        Test-SessionListShown -App $script:app | Should -BeTrue
        (Get-AgentPaneSession -App $script:app).AcpSessionId | Should -Be $script:pane.AcpSessionId
        Measure-QuietFallback -Label horizontal -Sidebar $false -Helper $true

        Set-TestLayout vertical
        Open-TestSidebar
        Measure-QuietFallback -Label vertical-return -Sidebar $true -Helper $false
        @(Get-FixtureCalls -Method initialize).Count | Should -Be 1 -Because 'layout switches must reuse the ACP connection'
    }

    It 'Closed session views suppress fallback reads' {
        Set-TestLayout horizontal
        Open-SessionList -App $script:app | Out-Null
        Close-SessionList -App $script:app | Out-Null
        Test-SessionListShown -App $script:app -TimeoutSec 1 | Should -BeFalse
        Measure-QuietFallback -Label closed -Sidebar $false -Helper $false
    }

    It 'Explicit history refresh preserves connection and updates visible rows' {
        Set-TestLayout horizontal
        Open-SessionList -App $script:app | Out-Null
        Set-FixtureHistory -Title 'Refresh fixture manual'
        $since = [DateTimeOffset]::UtcNow
        Send-AgentWin32Key -App $script:app -Vk 0x74 -Sc 0x3F -PaneSessionId $script:pane.PaneSessionId | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'explicit helper refresh completes through master' -Condition {
            (Get-MasterTrace) -split '\r?\n' | Where-Object {
                $_ -match '^(\S+).*sessions/list rescan:' -and [DateTimeOffset]::Parse($Matches[1]) -ge $since
            }
        } | Out-Null
        Assert-AgentPaneText -App $script:app -Pattern 'Refresh fixture manual' -TimeoutSec 15
        $snapshot = Invoke-Wta -App $script:app -Arguments @('sessions', 'refresh', '--master', $script:pipe, '--json')
        $snapshot.PSObject.Properties.Name | Should -Contain history_status
        @($snapshot.sessions | Where-Object session_id -eq $script:historyId).Count | Should -Be 1
        $removed = Invoke-Wta -App $script:app -Arguments @('sessions', 'list', '--all-agents') -Raw
        $removed.ExitCode | Should -Be 2
        $removed.StdErr | Should -Match 'unexpected argument'
        (Get-AgentPaneSession -App $script:app).AcpSessionId | Should -Be $script:pane.AcpSessionId
        @(Get-FixtureCalls -Method initialize).Count | Should -Be 1
    }
}
