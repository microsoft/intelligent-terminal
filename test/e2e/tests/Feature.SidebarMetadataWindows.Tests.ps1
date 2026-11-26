#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: sidebar metadata belongs to its window' -Tag @('Feature', 'SidebarMetadata') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        & (Get-Module ItE2E) { Get-WinAppPath } | Out-Null
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        if ($env:ITE2E_PACKAGE -ne 'Dev' -or
            -not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Select the exact-source Dev package and provide its App and WTA hashes.'
        }
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:app = $null
        $script:repository = $null

        function Get-RenderedMetadata {
            param($Window)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$Window.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HeaderMetadataTextBlock')
            @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                $_.Current.ProcessId -eq $Window.Pid -and -not $_.Current.IsOffscreen -and
                    $_.Current.BoundingRectangle.Width -gt 0
            } | ForEach-Object { $_.Current.Name })
        }

        function Get-RenderedTabBranch {
            param($Window, [string]$Title)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$Window.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HeaderMetadataTextBlock')
            foreach ($group in $root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)) {
                $row = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($group)
                if (-not $row -or -not $row.Current.Name.StartsWith($Title, [StringComparison]::Ordinal) -or
                    $group.Current.IsOffscreen) { continue }
                $text = $group.FindFirst([Windows.Automation.TreeScope]::Children,
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::ControlTypeProperty,
                        [Windows.Automation.ControlType]::Text))
                if ($text -and -not $text.Current.IsOffscreen) { return $text.Current.Name }
            }
            return $null
        }

        function Set-OnlyMetadataField {
            param($Window, [string]$Field)
            Set-WtWindowForeground -App $Window | Should -BeTrue
            foreach ($selector in @('RichTabAgentStatusVisibleItem', 'RichTabWorkingDirectoryVisibleItem', $Field)) {
                Invoke-UiElement -App $Window -Selector FilterTabsButton | Out-Null
                Invoke-UiElement -App $Window -Selector $selector | Out-Null
                Send-WtWindowKey -App $Window -Vk 0x1B -RequireForeground | Out-Null
            }
        }

        function Report-ShellWorkingDirectory {
            param($Window)
            $pane = Wait-Until -TimeoutSec 15 -Because 'the window shell to start' -Condition {
                @(Get-WtPanes -App $Window -WindowId $Window.WindowId | Where-Object { $_.session_id }) |
                    Select-Object -First 1
            }
            Invoke-RunCommand -App $Window -SessionId $pane.session_id -Command `
                'Write-Host -NoNewline ([char]27 + '']9;9;"'' + (Get-Location).Path + ''"'' + [char]7)' | Out-Null
        }
    }

    It 'shows the native agent session worktree branch instead of its launch directory' {
        if ($script:app) { Stop-Terminal -App $script:app; $script:app = $null }
        $sandbox = Join-Path $PSScriptRoot ('..\artifacts\native-worktree-' + [guid]::NewGuid().ToString('N'))
        $main = Join-Path $sandbox 'main'
        $linked = Join-Path $sandbox 'linked'
        $second = Join-Path $sandbox 'second'
        $fixtureHome = Join-Path $sandbox 'home'
        $bin = Join-Path $sandbox 'agent-bin'
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $git = (Get-Command git.exe -ErrorAction Stop).Source
        $savedEnvironment = @{}
        foreach ($name in @('USERPROFILE', 'HOME', 'COPILOT_HOME', 'XDG_CONFIG_HOME',
                'CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'GEMINI_CLI_HOME', 'OPENCODE_CONFIG_DIR', 'PATH')) {
            $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        }
        try {
            New-Item -ItemType Directory -Path $bin, $fixtureHome -Force | Out-Null
            $stubSource = Join-Path $bin 'RejectAgent.cs'
            'class RejectAgent { static int Main() { return 3; } }' |
                Set-Content -LiteralPath $stubSource -Encoding ascii
            $compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
            $stub = Join-Path $bin 'RejectAgent.exe'
            (Invoke-Native -FilePath $compiler -Arguments @(
                '/nologo', '/target:exe', "/out:$stub", $stubSource) -TimeoutSec 20).ExitCode | Should -Be 0
            foreach ($provider in @('copilot', 'claude', 'codex', 'gemini', 'opencode', 'npx', 'antigravity')) {
                Copy-Item -LiteralPath $stub -Destination (Join-Path $bin "$provider.exe")
                "@echo off`r`nexit /b 3" | Set-Content -LiteralPath (Join-Path $bin "$provider.cmd") -Encoding ascii
            }
            $environment = @{
                USERPROFILE = $fixtureHome; HOME = $fixtureHome; COPILOT_HOME = (Join-Path $fixtureHome '.copilot')
                XDG_CONFIG_HOME = (Join-Path $fixtureHome 'config')
                CLAUDE_CONFIG_DIR = (Join-Path $fixtureHome '.claude')
                CODEX_HOME = (Join-Path $fixtureHome '.codex')
                GEMINI_CLI_HOME = (Join-Path $fixtureHome '.gemini')
                OPENCODE_CONFIG_DIR = (Join-Path $fixtureHome 'opencode')
                PATH = (@($bin, (Split-Path $pwsh), (Split-Path $git), $script:target.InstallLocation,
                    (Join-Path $env:WINDIR 'System32'), $env:WINDIR) -join ';')
            }
            foreach ($name in $environment.Keys) {
                [Environment]::SetEnvironmentVariable($name, $environment[$name], 'Process')
            }
            foreach ($name in @('USERPROFILE', 'COPILOT_HOME', 'XDG_CONFIG_HOME', 'CLAUDE_CONFIG_DIR',
                    'CODEX_HOME', 'GEMINI_CLI_HOME', 'OPENCODE_CONFIG_DIR')) {
                New-Item -ItemType Directory -Path $environment[$name] -Force | Out-Null
            }
            & $git init -q -b main $main
            $LASTEXITCODE | Should -Be 0
            & $git -C $main -c user.name=Test -c user.email=test@example.invalid -c commit.gpgsign=false `
                -c core.hooksPath=NUL commit -q --allow-empty -m initial
            $LASTEXITCODE | Should -Be 0
            & $git -C $main worktree add -q -b agent-one $linked
            $LASTEXITCODE | Should -Be 0
            & $git -C $main worktree add -q -b agent-two $second
            $LASTEXITCODE | Should -Be 0

            $profile = '{' + [guid]::NewGuid() + '}'
            $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = 'en-US'; tabLayout = 'vertical'; defaultProfile = $profile; startupActions = ''
                firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                agentSessionManagementEnabled = $true; autoErrorDetectionEnabled = $false
                autoFixEnabled = $false; acpAgent = 'custom:metadata-fixture'
                acpCustomCommand = 'missing-metadata-fixture-acp.exe'
                profiles = @{ list = @(@{
                    guid = $profile; name = 'Native metadata shell'
                    commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                    startingDirectory = $main; shellIntegrationEnabled = $false
                }) }
            }
            $loadedSettings = Invoke-WtCli -App $script:app -Arguments @('get-settings')
            $loadedSettings.defaultProfile | Should -Be $profile
            $loadedSettings.agentSessionManagementEnabled | Should -BeTrue
            $pipe = Get-WtaMasterPipe -App $script:app
            Set-OnlyMetadataField -Window $script:app -Field RichTabBranchVisibleItem
            $title = 'Native worktree fixture'
            $command = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
            $created = Invoke-WtCli -App $script:app -Arguments @(
                'new-tab', '--window-id', [string]$script:app.WindowId, '--agent-provider', 'copilot',
                '--command', $command, '--cwd', $main, '--title', $title)
            $pane = Wait-Until -TimeoutSec 20 -Because 'the fixture has a native agent pane' -Condition {
                Get-WtPanes -App $script:app -TabId ([string]$created.tab_id) |
                    Where-Object { $_.session_id -eq $created.session_id -and
                        $_.native_agent_provider_id -eq 'copilot' }
            }
            $pane.cwd | Should -Be $main -Because 'the terminal launch directory differs from the agent session directory'
            $sendSession = {
                param([string]$SessionId, [string]$Directory)
                $payload = Join-Path $sandbox "$SessionId.json"
                @{ session_id = $SessionId; cwd = $Directory } | ConvertTo-Json -Compress |
                    Set-Content -LiteralPath $payload
                $scriptText = "Set-Location -LiteralPath '$($Directory.Replace("'", "''"))'; " +
                    "Get-Content -Raw -LiteralPath '$($payload.Replace("'", "''"))' | " +
                    "& '$($script:app.WtcliPath.Replace("'", "''"))' agent-hook --cli-source copilot --event agent.session.start"
                Invoke-RunCommand -App $script:app -SessionId $created.session_id -Command $scriptText | Out-Null
            }
            foreach ($case in @(
                    @{ Session = [guid]::NewGuid().ToString(); Directory = $linked; Branch = 'agent-one' },
                    @{ Session = [guid]::NewGuid().ToString(); Directory = $second; Branch = 'agent-two' })) {
                & $sendSession $case.Session $case.Directory
                try {
                    Wait-Until -TimeoutSec 15 -Because "WTA binds $($case.Branch) to the native pane" -Condition {
                        $snapshot = Invoke-Wta -App $script:app -Arguments @(
                            'sessions', 'list', '--json', '--master', $pipe, '--include-status', '--origin', 'all')
                        $script:observedPaneStatuses = @($snapshot.pane_statuses | Where-Object {
                            [guid]$_.pane_id -eq [guid]$created.session_id
                        })
                        @($script:observedPaneStatuses | Where-Object {
                            $_.agent_session_id -eq $case.Session -and $_.cwd -eq $case.Directory -and
                            $null -eq $_.mode_epoch
                        }).Count -eq 1
                    } | Out-Null
                }
                catch {
                    throw "Native pane status: $($script:observedPaneStatuses | ConvertTo-Json -Depth 8 -Compress); $($_.Exception.Message)"
                }
                Wait-Until -TimeoutSec 15 -Because "the native tab renders $($case.Branch) instead of the launch branch" -Condition {
                    (Get-RenderedTabBranch -Window $script:app -Title $title) -eq $case.Branch
                } | Out-Null
            }
        }
        finally {
            try {
                if ($script:app) { Stop-Terminal -App $script:app; $script:app = $null }
            }
            finally {
                foreach ($name in $savedEnvironment.Keys) {
                    [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name], 'Process')
                }
                if (Test-Path -LiteralPath $sandbox) {
                    Remove-Item -LiteralPath $sandbox -Recurse -Force
                }
            }
        }
    }

    AfterAll {
        try {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
        finally {
            if ($script:repository -and (Test-Path -LiteralPath $script:repository)) {
                Remove-Item -LiteralPath $script:repository -Recurse -Force
            }
        }
    }

    It 'keeps Repository in window one while window two displays Branch' {
        $script:repository = Join-Path $PSScriptRoot ('..\artifacts\metadata-window-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:repository -Force | Out-Null
        & git -C $script:repository init --initial-branch fixture-branch --quiet
        $LASTEXITCODE | Should -Be 0
        & git -C $script:repository -c user.name='Metadata test' -c user.email='metadata-test@example.invalid' `
            -c "core.hooksPath=$script:repository" commit --allow-empty --quiet -m 'Create metadata fixture'
        $LASTEXITCODE | Should -Be 0
        $profile = '{' + [guid]::NewGuid() + '}'
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; defaultProfile = $profile; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            profiles = @{ list = @(@{
                guid = $profile; name = 'Metadata window shell'
                commandline = 'pwsh -NoLogo -NoProfile'; startingDirectory = $script:repository
            }) }
        }
        $firstId = [string]$script:app.WindowId
        $previousHwnds = @(Get-WtWindowHwnds -App $script:app | ForEach-Object { [string]$_.hwnd })
        Report-ShellWorkingDirectory -Window $script:app
        Set-OnlyMetadataField -Window $script:app -Field RichTabRepositoryVisibleItem
        $expectedRepository = [regex]::Escape((Split-Path $script:repository -Leaf))
        Wait-Until -TimeoutSec 20 -Because 'the first window renders its selected repository' -Condition {
            @(Get-RenderedMetadata -Window $script:app | Where-Object { $_ -match "Repository: $expectedRepository" }).Count -eq 1
        } | Out-Null

        Send-WtWindowKey -App $script:app -Vk 0x4E -Ctrl -Shift -RequireForeground | Out-Null
        $secondId = Wait-Until -TimeoutSec 20 -Because 'the app opens a second window' -Condition {
            Get-WtWindows -App $script:app | Where-Object { [string]$_.window_id -ne $firstId } |
                Select-Object -First 1 -ExpandProperty window_id
        }
        $secondHwnd = Wait-Until -TimeoutSec 15 -Because 'the second window uses the same process' -Condition {
            Get-WtWindowHwnds -App $script:app | Where-Object {
                $_.pid -eq $script:app.Pid -and [string]$_.hwnd -notin $previousHwnds
            } | Select-Object -First 1 -ExpandProperty hwnd
        }
        $second = $script:app.PSObject.Copy()
        $second.WindowId = [string]$secondId
        $second.Hwnd = $secondHwnd
        Report-ShellWorkingDirectory -Window $second
        Set-OnlyMetadataField -Window $second -Field RichTabBranchVisibleItem
        $branch = 'fixture-branch'
        Wait-Until -TimeoutSec 20 -Because 'each window renders its own selection' -Condition {
            @(Get-RenderedMetadata -Window $script:app | Where-Object {
                $_ -match "Repository: $expectedRepository" -and $_ -notmatch 'Branch:'
            }).Count -eq 1 -and
            @(Get-RenderedMetadata -Window $second | Where-Object {
                $_ -match ([regex]::Escape("Branch: $branch")) -and $_ -notmatch 'Repository:'
            }).Count -eq 1
        } | Out-Null
        foreach ($window in @($script:app, $second)) {
            Invoke-UiElement -App $window -Selector FilterTabsButton | Out-Null
            (Get-UiElement -App $window -Selector RichTabRepositoryVisibleItem).toggleState |
                Should -Be $(if ($window.WindowId -eq $firstId) { 'on' } else { 'off' })
            (Get-UiElement -App $window -Selector RichTabBranchVisibleItem).toggleState |
                Should -Be $(if ($window.WindowId -eq $firstId) { 'off' } else { 'on' })
            Send-WtWindowKey -App $window -Vk 0x1B -RequireForeground | Out-Null
        }
    }

    It 'records an OSC 7 directory report in the CWD diagnostic log' {
        if ($script:app) { Stop-Terminal -App $script:app }
        $script:app = $null
        $directory = Join-Path $PSScriptRoot ('..\artifacts\osc7-directory-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path (Join-Path $directory 'child') -Force | Out-Null
        $profile = '{' + [guid]::NewGuid() + '}'
        try {
            $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
                sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
            } -Settings @{
                language = 'en-US'; tabLayout = 'vertical'; defaultProfile = $profile; startupActions = ''
                actions = @(@{
                    command = @{ action = 'splitPane'; splitMode = 'duplicate'; split = 'right' }
                    keys = 'ctrl+shift+f12'
                })
                profiles = @{ list = @(@{
                    guid = $profile; name = 'OSC 7 diagnostic shell'
                    commandline = 'pwsh -NoLogo -NoProfile'; startingDirectory = $directory
                }) }
            }
            $pane = Wait-Until -TimeoutSec 15 -Because 'the PowerShell pane to start' -Condition {
                @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId | Where-Object { $_.session_id }) |
                    Select-Object -First 1
            }
            $logDir = Get-ItLogDir -App $script:app
            $logPattern = "rich-tab-provider-*-$($script:app.Pid)-*.jsonl"
            $records = {
                @(Get-ChildItem $logDir -Filter $logPattern -ErrorAction SilentlyContinue |
                    Get-Content -ErrorAction SilentlyContinue |
                    ForEach-Object { $_ | ConvertFrom-Json -ErrorAction SilentlyContinue })
            }
            $before = @(& $records | Where-Object { $_.event -eq 'cwd' -and $_.stage -eq 'osc-received' -and
                $_.outcome -eq 'matched' -and $_.cwd -ne 0 }).Count
            Invoke-RunCommand -App $script:app -SessionId $pane.session_id -Command `
                'Write-Host -NoNewline ([char]27 + '']7;'' + [Uri]::new((Get-Location).Path).AbsoluteUri + [char]7)' | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'OSC 7 to be logged as a matched directory report' -Condition {
                @(& $records | Where-Object { $_.event -eq 'cwd' -and $_.stage -eq 'osc-received' -and
                    $_.outcome -eq 'matched' -and $_.cwd -ne 0 }).Count -gt $before
            } | Out-Null

            Send-WtWindowKey -App $script:app -Vk 0x7B -Ctrl -Shift -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'a second active shell pane' -Condition {
                @(Get-WtPanes -App $script:app -WindowId $script:app.WindowId |
                    Where-Object { $_.session_id -and $_.session_id -ne $pane.session_id }).Count -ge 1 -and
                (Get-ActivePane -App $script:app).session_id -ne $pane.session_id
            } | Out-Null
            $lastSequence = @(& $records | Measure-Object -Property recordSequence -Maximum)[0].Maximum
            Invoke-RunCommand -App $script:app -SessionId $pane.session_id -Command `
                'Set-Location (Join-Path (Get-Location).Path ''child''); Write-Host -NoNewline ([char]27 + '']7;'' + [Uri]::new((Get-Location).Path).AbsoluteUri + [char]7)' | Out-Null
            (Get-ActivePane -App $script:app).session_id | Should -Not -Be $pane.session_id
            Wait-Until -TimeoutSec 20 -Because 'the background pane row to apply its changed directory' -Condition {
                $rows = & $records
                $osc = @($rows | Where-Object { $_.event -eq 'cwd' -and $_.stage -eq 'osc-received' -and
                    $_.recordSequence -gt $lastSequence -and $_.cwd -ne 0 } | Select-Object -Last 1)
                $osc.Count -eq 1 -and
                @($rows | Where-Object { $_.event -eq 'cwd' -and $_.stage -eq 'ui-result' -and
                    $_.session -eq $osc[0].session -and $_.recordSequence -gt $osc[0].recordSequence -and
                    $_.outcome -eq 'applied' -and $_.cwd -eq $osc[0].cwd }).Count -ge 1
            } | Out-Null

            Invoke-UiElement -App $script:app -Selector SearchTabsButton | Out-Null
            Set-UiValue -App $script:app -Selector SearchTextBox -Value ('hidden-' + [guid]::NewGuid().ToString('N')) | Out-Null
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            $listCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'ItemsList')
            $rowCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::TreeItem)
            Wait-Until -TimeoutSec 10 -Because 'search to hide the parent tab and its pane rows' -Condition {
                $list = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $listCondition)
                -not $list -or $list.Current.IsOffscreen -or
                @($list.FindAll([Windows.Automation.TreeScope]::Descendants, $rowCondition) |
                    Where-Object { -not $_.Current.IsOffscreen -and $_.Current.BoundingRectangle.Width -gt 0 -and
                        $_.Current.AutomationId -ne 'HistoryHeaderButton' }).Count -eq 0
            } | Out-Null
            $lastSequence = @(& $records | Measure-Object -Property recordSequence -Maximum)[0].Maximum
            Invoke-RunCommand -App $script:app -SessionId $pane.session_id -Command `
                'Set-Location ..; Write-Host -NoNewline ([char]27 + '']7;'' + [Uri]::new((Get-Location).Path).AbsoluteUri + [char]7)' | Out-Null
            Wait-Until -TimeoutSec 20 -Because 'a filtered-out pane row to remain unmatched' -Condition {
                $rows = & $records
                $osc = @($rows | Where-Object { $_.event -eq 'cwd' -and $_.stage -eq 'osc-received' -and
                    $_.recordSequence -gt $lastSequence -and $_.cwd -ne 0 } | Select-Object -Last 1)
                $osc.Count -eq 1 -and
                @($rows | Where-Object { $_.event -eq 'cwd' -and $_.stage -eq 'ui-result' -and
                    $_.session -eq $osc[0].session -and $_.recordSequence -gt $osc[0].recordSequence -and
                    $_.outcome -eq 'unmatched' -and $_.cwd -eq $osc[0].cwd }).Count -ge 1
            } | Out-Null
        }
        finally {
            try {
                if ($script:app) { Stop-Terminal -App $script:app; $script:app = $null }
            }
            finally {
                if (Test-Path -LiteralPath $directory) {
                    Remove-Item -LiteralPath $directory -Recurse -Force
                }
            }
        }
    }
}
