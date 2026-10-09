#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# New sidebar action coverage only. Search/navigation remains in CombinedAgentsSidebar.
# A native canonical shim and custom interactive CLI avoid provider quota.

Describe 'Feature: Agents mode actions' -Tag @('Feature', 'AgentsModeActions') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $cleanupTokens = $null
        $cleanupErrors = $null
        $cleanupAst = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot 'Feature.CombinedAgentsSidebar.Tests.ps1'),
            [ref]$cleanupTokens, [ref]$cleanupErrors)
        if ($cleanupErrors.Count) { throw 'Shared owned headless cleanup helpers must parse.' }
        foreach ($name in @('Initialize-CombinedCleanupNative', 'Get-CombinedVisibleProcessIds',
            'Start-CombinedCleanupTab', 'Invoke-CombinedHeadlessRecovery')) {
            $definition = $cleanupAst.Find({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
            }, $true)
            if (-not $definition) { throw "Missing existing cleanup helper: $name" }
            . ([scriptblock]::Create($definition.Extent.Text))
        }
        $script:app = $null
        $script:ownsConfig = $false
        $script:resolverShimOwned = $false
        if ($env:ITE2E_PACKAGE -ne 'Dev') { throw 'Agents mode actions require explicit Dev selection.' }
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        $head = (& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $head -or -not $env:ITE2E_SOURCE_COMMIT -or
            -not $env:ITE2E_SOURCE_COMMIT.StartsWith($head, [StringComparison]::Ordinal)) {
            throw 'Source provenance must come from the new exact-source build receipt.'
        }
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply both deployed application and WTA hashes from the new build receipt.'
        }
        $appHash = (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash
        $wtaHash = (Get-FileHash -LiteralPath $script:target.WtaPath).Hash
        $appHash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        $wtaHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Existing backup requires recovery before testing: $path"
            }
        }
        $script:originalConfigHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            $script:originalConfigHashes[$path] = if (Test-Path -LiteralPath $path) {
                (Get-FileHash -LiteralPath $path).Hash
            } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:runId = [guid]::NewGuid().ToString('N')
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) "agents actions $script:runId"
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null
        $script:launchLog = Join-Path $script:evidence 'interactive-launch.jsonl'
        @{
            source_commit = $env:ITE2E_SOURCE_COMMIT; app_sha256 = $appHash; wta_sha256 = $wtaHash
            package = $script:target.Package; install_location = $script:target.InstallLocation
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json')
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-InteractiveDelegate.ps1')).Path
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $script:canonicalLog = Join-Path $script:evidence 'canonical-launch.jsonl'
        $script:shimDirectory = Join-Path $script:evidence 'native-shim'
        New-Item -ItemType Directory -Path $script:shimDirectory | Out-Null
        $script:shim = Join-Path $script:shimDirectory 'copilot.exe'
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $vs = Invoke-Native -FilePath $vswhere -Arguments @(
            '-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
            '-property', 'installationPath') -TimeoutSec 10
        if ($vs.ExitCode -ne 0 -or -not $vs.StdOut.Trim()) { throw 'Native CLI fixture requires the installed MSVC toolchain.' }
        $vcvars = Join-Path $vs.StdOut.Trim() 'VC\Auxiliary\Build\vcvars64.bat'
        $nativeSource = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-CopilotDelegate.cpp')).Path
        $shimConfig = @{
            ITE2E_SHIM_PWSH = $pwsh; ITE2E_SHIM_FIXTURE = $fixture
            ITE2E_SHIM_LOG = $script:canonicalLog; ITE2E_SHIM_RUN = $script:runId
            ITE2E_SHIM_WTCLI = $script:target.WtcliPath
            ITE2E_SHIM_SESSION_START_GATE = (Join-Path $script:evidence 'release-session-start')
        }
        $shimConfig | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:shimDirectory 'config.json')
        $configHeader = Join-Path $script:shimDirectory 'config.h'
        @($shimConfig.Keys | ForEach-Object {
            "#define $_ LR`"ite2e($($shimConfig[$_]))ite2e`""
        }) | Set-Content -LiteralPath $configHeader -Encoding ascii
        $buildCommand = "call `"$vcvars`" >nul && cl /nologo /EHsc /std:c++17 /FI`"$configHeader`" `"$nativeSource`" /Fe:`"$script:shim`" /Fo:`"$script:shimDirectory\copilot.obj`" /link /INCREMENTAL:NO"
        $buildScript = "& `$env:ComSpec /d /c '$($buildCommand.Replace("'", "''"))'; exit `$LASTEXITCODE"
        $buildEncoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($buildScript))
        $build = Invoke-Native -FilePath $pwsh -Arguments @('-NoProfile', '-EncodedCommand', $buildEncoded) -TimeoutSec 60 -WorkingDirectory $script:shimDirectory
        if ($build.TimedOut -or $build.ExitCode -ne 0) { throw "Native CLI fixture build failed: $($build.StdOut) $($build.StdErr)" }
        $delegate = "`"$pwsh`" -NoLogo -NoProfile -File `"$fixture`" -LogPath `"$script:launchLog`" -RunId $script:runId"
        $acp = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1')).Path
        $invocation = "& '$($acp.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\acp.log'"
        $acpCommand = 'pwsh -NoProfile -EncodedCommand ' +
            [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $script:ownsConfig = $true
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            acpAgent = 'custom:agents-actions-acp'; acpCustomCommand = $acpCommand; acpModel = ''
            delegateAgent = 'custom:agents-actions-cli'; delegateCustomCommand = $delegate; delegateModel = ''
            autoFixEnabled = $false; 'warning.confirmOnClose' = 'never'
            actions = @(); keybindings = @()
            defaultProfile = '{7d075d6b-6625-49fe-8a57-c31f362b7fce}'
            profiles = @{
                list = @(@{
                    guid = '{7d075d6b-6625-49fe-8a57-c31f362b7fce}'
                    name = 'ite2e-agents-actions'; commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                    startingDirectory = $script:evidence; commandPaletteAgent = ''
                    reloadEnvironmentVariables = $false
                })
            }
        }
        $script:app.Launched | Should -BeTrue
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true
        Test-WtWindowKeyFocusable -App $script:app | Should -BeTrue

        function Get-ActionElement {
            param([string]$Id)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $window.Current.ProcessId | Should -Be $script:app.Pid
            $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id))
        }
        function Install-ActionCanonicalResolver {
            if ($script:resolverShimOwned) { return }
            if (-not $env:ITE2E_CANONICAL_SHIM_DIRECTORY) {
                throw 'Supply an explicitly approved existing PATH directory for the temporary canonical fixture.'
            }
            $approved = [IO.Path]::GetFullPath($env:ITE2E_CANONICAL_SHIM_DIRECTORY)
            $allPath = ([Environment]::GetEnvironmentVariable('PATH', 'Machine') + ';' +
                [Environment]::GetEnvironmentVariable('PATH', 'User')).Split(';') |
                Where-Object { $_ } | ForEach-Object { [IO.Path]::GetFullPath($_) }
            $approved | Should -BeIn $allPath
            Test-Path -LiteralPath $approved -PathType Container | Should -BeTrue
            $script:resolverShim = Join-Path $approved 'copilot.exe'
            foreach ($path in @($script:resolverShim, (Join-Path $script:target.InstallLocation 'copilot.exe'),
                (Join-Path $script:evidence 'copilot.exe'))) {
                if (Test-Path -LiteralPath $path) { throw "Refusing to replace any existing canonical executable: $path" }
            }
            $script:resolverShimHash = (Get-FileHash -LiteralPath $script:shim).Hash
            foreach ($name in @('copilot.exe', 'copilot.cmd', 'copilot.ps1', 'copilot.bat', 'copilot.com', 'copilot')) {
                $path = Join-Path $approved $name
                if (Test-Path -LiteralPath $path) { throw "Canonical fixture collision: $path" }
            }
            [IO.File]::Copy($script:shim, $script:resolverShim, $false)
            $script:resolverShimOwned = $true
            $script:resolverShimCreatedUtc = (Get-Item -LiteralPath $script:resolverShim).CreationTimeUtc
            @{
                path = $script:resolverShim; source = $script:shim; sha256 = $script:resolverShimHash
                created_utc = $script:resolverShimCreatedUtc.ToString('o'); existed_before_create = $false
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'canonical-resolver-file-lease.json')
        }
        function Save-ActionHeldRowIconEvidence {
            param($Receipt, [string]$ProviderId, [string]$Phase)
            Test-Path -LiteralPath $shimConfig.ITE2E_SHIM_SESSION_START_GATE | Should -BeFalse
            Test-Path -LiteralPath "$($shimConfig.ITE2E_SHIM_SESSION_START_GATE).emitted-$($Receipt.session_id).json" | Should -BeFalse
            $script:heldRow = $null
            Wait-Until -TimeoutSec 10 -Because 'the exact fixture-owned newly created sidebar row is visible before its hook' -Condition {
                $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
                $window.Current.ProcessId | Should -Be $script:app.Pid
                $title = $window.FindFirst([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::NameProperty, [string]$Receipt.title))
                if (-not $title -or $title.Current.IsOffscreen) { return $false }
                $row = $title
                while ($row -and $row.Current.ControlType -notin @(
                    [Windows.Automation.ControlType]::ListItem, [Windows.Automation.ControlType]::TabItem)) {
                    $row = [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($row)
                }
                if (-not $row -or $row.Current.IsOffscreen) { return $false }
                $script:heldRow = $row
                $row.Current.BoundingRectangle.Width -gt 0 -and $row.Current.BoundingRectangle.Height -gt 0
            } | Out-Null
            $bounds = $script:heldRow.Current.BoundingRectangle
            Add-Type -AssemblyName System.Drawing
            $image = [Drawing.Bitmap]::new([int][Math]::Ceiling($bounds.Width), [int][Math]::Ceiling($bounds.Height))
            $graphics = [Drawing.Graphics]::FromImage($image)
            try {
                $graphics.CopyFromScreen([int]$bounds.Left, [int]$bounds.Top, 0, 0, $image.Size)
                $image.Save((Join-Path $script:evidence "$Phase.row.png"), [Drawing.Imaging.ImageFormat]::Png)
            }
            finally { $graphics.Dispose(); $image.Dispose() }
            @{
                phase = $Phase; provider_id = $ProviderId; pane_session_id = $Receipt.pane_session_id
                session_id = $Receipt.session_id; title = $Receipt.title; pid = $script:app.Pid
                row_bounds = $bounds.ToString()
                expected_icon = if ($ProviderId -eq 'copilot') { 'Copilot provider glyph' } else { 'existing generic Message glyph for custom provider without a configured icon' }
                rendered_icon_review = 'pending parent visual review of row.png; automated assertions cover exact visible row and canonical provider data'
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.row.json")
            Test-Path -LiteralPath $shimConfig.ITE2E_SHIM_SESSION_START_GATE | Should -BeFalse
        }
        function Save-ActionSessionSnapshot {
            param([string]$Phase)
            $rows = @(Get-ActionSessions)
            ConvertTo-Json -InputObject $rows -Depth 20 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.sessions.json")
            return $rows
        }
        function Assert-ActionPriorSessionsPreserved {
            param([array]$Before, [array]$During)
            foreach ($row in $Before) {
                $matches = @($During | Where-Object {
                    $_.session_id -eq $row.session_id -and $_.provider_id -eq $row.provider_id -and $_.location -eq $row.location
                })
                $matches.Count | Should -Be 1
                $matches[0].status | Should -Be $row.status
                $matches[0].pane_session_id | Should -Be $row.pane_session_id
            }
        }
        function Set-ActionView {
            param([bool]$Agents)
            $header = Get-ActionElement VerticalTabsHeader
            if (($header.Current.Name -eq 'Agents') -ne $Agents) {
                Invoke-UiClick -App $script:app -Selector VerticalTabsHeaderButton | Out-Null
            }
            $expected = if ($Agents) { 'Agents' } else { 'Tabs' }
            Wait-Until -TimeoutSec 10 -Because 'requested sidebar page renders' -Condition {
                (Get-ActionElement VerticalTabsHeader).Current.Name -eq $expected
            } | Out-Null
        }
        function Get-ActionLaunches {
            if (Test-Path -LiteralPath $script:launchLog) {
                @(Get-Content -LiteralPath $script:launchLog | ForEach-Object { $_ | ConvertFrom-Json })
            }
        }
        function Get-ActionTabs {
            @(Get-WtTabs -App $script:app -WindowId ([string]$script:app.WindowId))
        }
        function Get-CanonicalLaunches {
            if (Test-Path -LiteralPath $script:canonicalLog) {
                @(Get-Content -LiteralPath $script:canonicalLog | ForEach-Object { $_ | ConvertFrom-Json })
            }
        }
        function Get-ActionSessions {
            $pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
            (Invoke-Wta -App $script:app -TimeoutSec 10 -Arguments @(
                'sessions', 'list', '--master', $pipe, '--json', '--include-status')).sessions
        }
        function Invoke-ActionPlus {
            $before = @(Get-ActionTabs).tab_id
            $label = if ((Get-ActionElement VerticalTabsHeader).Current.Name -eq 'Agents') {
                'Open background agent in a new tab'
            } else { 'New tab' }
            Invoke-UiClick -App $script:app -Selector $label | Out-Null
            Wait-Until -TimeoutSec 25 -Because 'plus creates exactly one new tab' -Condition {
                @(Get-ActionTabs | Where-Object tab_id -NotIn $before).Count -eq 1
            } | Out-Null
            @(Get-ActionTabs | Where-Object tab_id -NotIn $before)[0]
        }
        function Invoke-ActionRejectedSplit {
            Initialize-LogOffsets -App $script:app | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0xBB -Alt -Shift | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'unidentified split reports a visible failure before retry' -Condition {
                $message = Get-ActionElement HistoryMessage
                $message -and -not $message.Current.IsOffscreen -and $message.Current.Name -and
                    (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) -match
                        'agent split rejected: missing live identity, unsupported provider, or policy'
            } | Out-Null
            Save-ActionUiEvidence "retry-failure-$script:caseIndex"
            (Get-ActionElement HistoryMessage).Current.Name
        }
        function Assert-ActionRetryClearsError {
            param([string]$PreviousError)
            Wait-Until -TimeoutSec 10 -Because 'successful retry removes the old error without closing Agents' -Condition {
                $message = Get-ActionElement HistoryMessage
                -not $message -or $message.Current.IsOffscreen -or
                    $message.Current.BoundingRectangle.Height -le 0 -or
                    $message.Current.BoundingRectangle.Width -le 0
            } | Out-Null
            (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        }
        function Save-ActionUiEvidence {
            param([string]$Phase)
            $toggle = (Get-ActionElement SearchTabsButton).GetCurrentPattern(
                [Windows.Automation.TogglePattern]::Pattern)
            @{
                phase = $Phase; at = [DateTimeOffset]::UtcNow.ToString('o')
                header = (Get-ActionElement VerticalTabsHeader).Current.Name
                search_toggle = $toggle.Current.ToggleState.ToString()
                active_pane = Get-ActivePane -App $script:app
                tabs = @(Get-ActionTabs)
                custom_launches = @(Get-ActionLaunches)
                canonical_launches = @(Get-CanonicalLaunches)
            } | ConvertTo-Json -Depth 10 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.json")
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "$Phase.png") | Out-Null
            Get-UiTree -App $script:app -Depth 8 |
                Set-Content -LiteralPath (Join-Path $script:evidence "$Phase.tree.txt")
        }
        $script:caseIndex = 0
    }

    BeforeEach {
        $script:caseIndex++
        Save-ActionUiEvidence "before-case-$script:caseIndex"
    }

    AfterEach {
        Save-ActionUiEvidence "after-case-$script:caseIndex"
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app -RestoreSettings $false
            if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
                Invoke-CombinedHeadlessRecovery -App $script:app -Target $script:target -InitiallyInactive $true |
                    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'headless-recovery.json')
            }
            Restore-WtConfig -App $script:target
        }
        # A failed launch provides no owned host authority. Retain backups if Dev is active.
        elseif ($script:ownsConfig -and $script:target -and -not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            Restore-WtConfig -App $script:target
        }
        if ($script:ownsConfig -and -not @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            if ($script:resolverShimOwned) {
                (Get-FileHash -LiteralPath $script:resolverShim).Hash | Should -Be $script:resolverShimHash
                (Get-Item -LiteralPath $script:resolverShim).CreationTimeUtc | Should -Be $script:resolverShimCreatedUtc
                if (@(Get-Process -ErrorAction Stop | Where-Object Path -eq $script:resolverShim).Count) {
                    throw 'Canonical fixture remains active outside the closed package; retain executable.'
                }
                Remove-Item -LiteralPath $script:resolverShim
                $script:resolverShimOwned = $false
            }
            foreach ($path in $script:originalConfigHashes.Keys) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
                $actual | Should -Be $script:originalConfigHashes[$path] -Because 'restore exact pre-run configuration bytes'
            }
            foreach ($name in @('copilot.exe', 'copilot.obj', 'interactive-fixture.exe')) {
                $path = Join-Path $script:shimDirectory $name
                if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
            }
        }
    }

    It 'Agents plus creates a fresh interactive delegate' {
        Set-ActionView $true
        $original = Get-ActivePane -App $script:app
        $tabsBeforeFailure = @(Get-ActionTabs).Count
        $retryError = Invoke-ActionRejectedSplit
        @(Get-ActionTabs).Count | Should -Be $tabsBeforeFailure
        @(Get-ActionLaunches).Count | Should -Be 0
        $first = Invoke-ActionPlus
        Wait-Until -TimeoutSec 20 -Because 'interactive CLI logs its own launch' -Condition {
            @(Get-ActionLaunches).Count -eq 1
        } | Out-Null
        $one = @(Get-ActionLaunches)[0]
        $panes = @(Get-WtPanes -App $script:app -TabId ([string]$first.tab_id) -WindowId ([string]$script:app.WindowId))
        $matches = @($panes | Where-Object session_id -EQ $one.pane_session_id)
        $matches.Count | Should -Be 1
        $one.pane_session_id | Should -Not -Be $original.session_id
        $first.tab_id | Should -Not -Be $original.tab_id
        $one.cwd | Should -Be $script:evidence
        $one.source | Should -Be 'host'
        @($one.args).Count | Should -Be 0 -Because 'no startup prompt or resume ID is allowed'
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($one.pid)" -ErrorAction Stop
        $process.ExecutablePath | Should -Be $pwsh
        $process.CommandLine | Should -Match ([regex]::Escape($fixture))
        Get-WtCapture -App $script:app -SessionId $one.pane_session_id -MaxLines 30 |
            Should -Match ([regex]::Escape("ITE2E-INTERACTIVE-DELEGATE $script:runId $($one.session_id)"))
        Assert-ActionRetryClearsError $retryError
        (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        $second = Invoke-ActionPlus
        Wait-Until -TimeoutSec 20 -Because 'second plus launches a fresh CLI' -Condition {
            @(Get-ActionLaunches).Count -eq 2
        } | Out-Null
        $two = @(Get-ActionLaunches)[1]
        $two.session_id | Should -Not -Be $one.session_id
        $two.pid | Should -Not -Be $one.pid
        $two.pane_session_id | Should -Not -Be $one.pane_session_id
        $second.tab_id | Should -Not -Be $first.tab_id
        $two.cwd | Should -Be $one.cwd
        $two.source | Should -Be $one.source
        @($two.args).Count | Should -Be 0
        (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Wait-Until -TimeoutSec 10 -Because 'second delegate renders its unique interactive banner' -Condition {
            (Get-WtCapture -App $script:app -SessionId $two.pane_session_id -MaxLines 30) -match
                [regex]::Escape("ITE2E-INTERACTIVE-DELEGATE $script:runId $($two.session_id)")
        } | Out-Null
        Send-WtInput -App $script:app -SessionId $one.pane_session_id -Text "alive-$script:runId"
        Send-WtKeys -App $script:app -SessionId $one.pane_session_id -Keys Enter
        Wait-Until -TimeoutSec 10 -Because 'original delegate remains responsive' -Condition {
            (Get-WtCapture -App $script:app -SessionId $one.pane_session_id -MaxLines 30) -match
                [regex]::Escape("ITE2E-DELEGATE-ALIVE $($one.session_id) alive-$script:runId")
        } | Out-Null
    }

    It 'Tabs plus and split retain ordinary terminal behavior' {
        Set-ActionView $false
        $count = @(Get-ActionLaunches).Count
        $tab = Invoke-ActionPlus
        $source = Get-ActivePane -App $script:app
        Invoke-RunCommand -App $script:app -SessionId $source.session_id -Command "Write-Output 'NORMAL-TABS-$script:runId'" -SettleSec 1 | Out-Null
        Get-WtCapture -App $script:app -SessionId $source.session_id -MaxLines 30 |
            Should -Match "NORMAL-TABS-$script:runId"
        Send-WtWindowKey -App $script:app -Vk 0xBB -Alt -Shift | Out-Null
        Wait-Until -TimeoutSec 15 -Because 'ordinary duplicate split creates another terminal' -Condition {
            @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId)).Count -eq 2
        } | Out-Null
        $split = Get-ActivePane -App $script:app
        $split.session_id | Should -Not -Be $source.session_id
        Invoke-RunCommand -App $script:app -SessionId $split.session_id -Command "Write-Output 'NORMAL-SPLIT-$script:runId'" -SettleSec 1 | Out-Null
        Get-WtCapture -App $script:app -SessionId $split.session_id -MaxLines 30 |
            Should -Match "NORMAL-SPLIT-$script:runId"
        @(Get-ActionLaunches).Count | Should -Be $count
        (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Tabs'
    }

    It 'Native agent provider is visible before session startup' {
        Set-ActionView $true
        Install-ActionCanonicalResolver
        $gate = $shimConfig.ITE2E_SHIM_SESSION_START_GATE
        $prior = Get-WtSetting -App $script:app -Key delegateAgent
        $receiptsBefore = @(Get-ChildItem -LiteralPath $script:evidence -Filter 'release-session-start.waiting-*.json').FullName
        $rowsBefore = @(Save-ActionSessionSnapshot 'copilot-before-launch')
        try {
            Set-WtDelegateAgent -App $script:app -Agent copilot | Out-Null
            Wait-Until -TimeoutSec 15 -Because 'Terminal settings select the canonical fixture provider' -Condition {
                $settings = Invoke-Native -FilePath $script:target.WtcliPath -Arguments @('--json', 'get-settings') -TimeoutSec 10
                $settings.ExitCode -eq 0 -and ($settings.StdOut | ConvertFrom-Json).delegateAgent -eq 'copilot'
            } | Out-Null
            $tab = Invoke-ActionPlus
            Wait-Until -TimeoutSec 20 -Because 'native fixture is held before publishing any session hook' -Condition {
                @(Get-ChildItem -LiteralPath $script:evidence -Filter 'release-session-start.waiting-*.json' |
                    Where-Object FullName -NotIn $receiptsBefore).Count -eq 1
            } | Out-Null
            $receipt = Get-Content -LiteralPath @(Get-ChildItem -LiteralPath $script:evidence -Filter 'release-session-start.waiting-*.json' |
                Where-Object FullName -NotIn $receiptsBefore)[0].FullName -Raw | ConvertFrom-Json
            $sid = $receipt.session_id
            $pane = @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId)) |
                Where-Object session_id -EQ $receipt.pane_session_id
            $pane.native_agent_provider_id | Should -Be 'copilot'
            $pane.is_agent_pane | Should -BeFalse
            $rowsDuring = @(Save-ActionSessionSnapshot 'copilot-before-hook')
            $bornRows = @($rowsDuring | Where-Object session_id -EQ $sid)
            $bornRows.Count | Should -Be 1 -Because 'Copilot explicitly receives this preassigned SID and its existing born-bound registration remains valid'
            $bornRows[0].pane_session_id | Should -Be $receipt.pane_session_id
            $bornRows[0].provider_id | Should -Be 'copilot'
            $bornRows[0].location | Should -Be 'host'
            @($rowsDuring | Where-Object pane_session_id -EQ $receipt.pane_session_id).Count |
                Should -Be 1 -Because 'only the exact pinned native conversation belongs to this pane; new stashed ACP helpers have different pane GUIDs'
            Assert-ActionPriorSessionsPreserved -Before $rowsBefore -During $rowsDuring
            (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $visibleTitle = @($window.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::NameProperty, $pane.title)) |
                Where-Object { -not $_.Current.IsOffscreen })
            $visibleTitle.Count | Should -BeGreaterThan 0 -Because 'the held native CLI remains visible in Agents before any title or hook startup'
            Save-ActionUiEvidence 'provider-before-session-hook'
            Save-ActionHeldRowIconEvidence -Receipt $receipt -ProviderId copilot -Phase 'provider-before-session-hook'
        }
        finally {
            Set-Content -LiteralPath $gate -Value 'released'
            Set-WtDelegateAgent -App $script:app -Agent $prior | Out-Null
        }
        Wait-Until -TimeoutSec 20 -Because 'real session hook binds the fixture conversation after release' -Condition {
            (Test-Path -LiteralPath "$gate.emitted-$sid.json") -and @(Get-ActionSessions | Where-Object {
                $_.session_id -eq $sid -and $_.pane_session_id -eq $receipt.pane_session_id -and $_.provider_id -eq 'copilot'
            }).Count -eq 1
        } | Out-Null
        $rowsAfter = @(Save-ActionSessionSnapshot 'copilot-after-hook')
        @($rowsAfter | Where-Object pane_session_id -EQ $receipt.pane_session_id).Count | Should -Be 1
        Assert-ActionPriorSessionsPreserved -Before $rowsBefore -During $rowsAfter
    }

    It 'Custom native provider intent creates no conversation before its hook' {
        Set-ActionView $true
        $sid = [guid]::NewGuid().ToString()
        $gate = $shimConfig.ITE2E_SHIM_SESSION_START_GATE
        $provider = 'custom:agents-actions-no-pin'
        $customShim = Join-Path $script:shimDirectory 'interactive-fixture.exe'
        [IO.File]::Copy($script:shim, $customShim, $false)
        $priorAgent = Get-WtSetting -App $script:app -Key delegateAgent
        $priorCommand = Get-WtSetting -App $script:app -Key delegateCustomCommand
        $rowsBefore = @(Save-ActionSessionSnapshot 'custom-before-launch')
        if (Test-Path -LiteralPath $gate) { Remove-Item -LiteralPath $gate }
        try {
            Set-WtSetting -App $script:app -Key delegateCustomCommand -Value "`"$customShim`" --session-id $sid" | Out-Null
            Set-WtDelegateAgent -App $script:app -Agent $provider | Out-Null
            $tab = Invoke-ActionPlus
            Wait-Until -TimeoutSec 20 -Because 'no-pin native fixture waits before publishing its genuine session hook' -Condition {
                Test-Path -LiteralPath "$gate.waiting-$sid.json"
            } | Out-Null
            $receipt = Get-Content -LiteralPath "$gate.waiting-$sid.json" -Raw | ConvertFrom-Json
            $pane = @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId)) |
                Where-Object session_id -EQ $receipt.pane_session_id
            $pane.native_agent_provider_id | Should -Be $provider
            $pane.is_agent_pane | Should -BeFalse
            $rowsDuring = @(Save-ActionSessionSnapshot 'custom-before-hook')
            @($rowsDuring | Where-Object pane_session_id -EQ $receipt.pane_session_id).Count | Should -Be 0
            @($rowsDuring | Where-Object session_id -EQ $sid).Count | Should -Be 0
            Assert-ActionPriorSessionsPreserved -Before $rowsBefore -During $rowsDuring
            (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            @($window.FindAll([Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::NameProperty, $pane.title)) |
                Where-Object { -not $_.Current.IsOffscreen }).Count | Should -BeGreaterThan 0
            Save-ActionUiEvidence 'custom-provider-before-session-hook'
            Save-ActionHeldRowIconEvidence -Receipt $receipt -ProviderId $provider -Phase 'custom-provider-before-session-hook'
        }
        finally {
            Set-Content -LiteralPath $gate -Value 'released'
            Set-WtSetting -App $script:app -Key delegateCustomCommand -Value $priorCommand | Out-Null
            Set-WtDelegateAgent -App $script:app -Agent $priorAgent | Out-Null
        }
        Wait-Until -TimeoutSec 20 -Because 'the real hook supplies exactly one conversation for the same custom-launched pane' -Condition {
            (Test-Path -LiteralPath "$gate.emitted-$sid.json") -and @(Get-ActionSessions | Where-Object {
                $_.session_id -eq $sid -and $_.pane_session_id -eq $receipt.pane_session_id -and $_.provider_id -eq 'copilot'
            }).Count -eq 1
        } | Out-Null
        $rowsAfter = @(Save-ActionSessionSnapshot 'custom-after-hook')
        @($rowsAfter | Where-Object pane_session_id -EQ $receipt.pane_session_id).Count | Should -Be 1
        Assert-ActionPriorSessionsPreserved -Before $rowsBefore -During $rowsAfter
    }

    It 'Agents split creates a fresh same-provider interactive CLI' {
        Set-Content -LiteralPath $shimConfig.ITE2E_SHIM_SESSION_START_GATE -Value 'released'
        Set-ActionView $false
        Install-ActionCanonicalResolver
        $sid = [guid]::NewGuid().ToString()
        $beforeCount = @(Get-CanonicalLaunches).Count
        $tab = New-WtTab -App $script:app -Command "`"$script:shim`" --session-id $sid" -Cwd $script:evidence
        Wait-Until -TimeoutSec 20 -Because 'owned canonical shim delivers a real root session-start hook' -Condition {
            @(Get-CanonicalLaunches).Count -eq $beforeCount + 1
        } | Out-Null
        $original = @(Get-CanonicalLaunches)[-1]
        $original.session_id | Should -Be $sid
        $original.pane_session_id | Should -Be $tab.session_id
        $original.cwd | Should -Be $script:evidence
        $original.provider | Should -Be 'copilot'
        $originalProcess = Get-Process -Id $original.native_pid -ErrorAction Stop
        $originalProcess.Path | Should -Be $script:shim
        $probe = New-WtTab -App $script:app -Command "`"$pwsh`" -NoLogo -NoProfile -NoExit" -Cwd $script:evidence
        $probeLog = Join-Path $script:evidence 'resolver-proof.json'
        $probeCommand = "[IO.File]::WriteAllText('$($probeLog.Replace("'", "''"))', ((Get-Command copilot.exe -CommandType Application | Select-Object -First 1).Source | ConvertTo-Json))"
        Invoke-RunCommand -App $script:app -SessionId $probe.session_id -Command $probeCommand -SettleSec 1 | Out-Null
        (Get-Content -LiteralPath $probeLog -Raw | ConvertFrom-Json) | Should -Be $script:resolverShim
        Wait-Until -TimeoutSec 20 -Because 'canonical Host identity is live in the owning master before splitting' -Condition {
            $rows = @(Get-ActionSessions | Where-Object session_id -EQ $sid)
            $rows.Count -eq 1 -and $rows[0].pane_session_id -eq $tab.session_id -and
                $rows[0].provider_id -eq 'copilot' -and $rows[0].location -eq 'host'
        } | Out-Null
        Set-WtPaneFocus -App $script:app -SessionId $probe.session_id | Out-Null
        Set-ActionView $true
        $retryError = Invoke-ActionRejectedSplit
        @(Get-CanonicalLaunches).Count | Should -Be ($beforeCount + 1)
        Set-WtPaneFocus -App $script:app -SessionId $tab.session_id | Out-Null
        (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        (Get-ActionElement HistoryMessage).Current.Name | Should -Be $retryError
        Send-WtWindowKey -App $script:app -Vk 0xBB -Alt -Shift | Out-Null
        Wait-Until -TimeoutSec 30 -Because 'UI same-provider split launches the canonical native shim' -Condition {
            @(Get-CanonicalLaunches).Count -eq $beforeCount + 2
        } | Out-Null
        $fresh = @(Get-CanonicalLaunches)[-1]
        $fresh.session_id | Should -Not -Be $sid
        $fresh.native_pid | Should -Not -Be $original.native_pid
        $fresh.pane_session_id | Should -Not -Be $tab.session_id
        $fresh.cwd | Should -Be $original.cwd
        $fresh.source | Should -Be 'host'
        $fresh.provider | Should -Be 'copilot'
        (Get-CimInstance Win32_Process -Filter "ProcessId=$($fresh.native_pid)" -ErrorAction Stop).ExecutablePath |
            Should -Be $script:resolverShim -Because 'the real provider executable must never launch'
        $fresh.native_command_line | Should -Match ([regex]::Escape("--session-id $($fresh.session_id)"))
        $fresh.native_command_line | Should -Not -Match '--resume|--acp|--stdio|(?:^|\s)-i(?:\s|$)'
        $fresh.native_command_line | Should -Not -Match ([regex]::Escape($sid))
        $panes = @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId))
        @($panes | Where-Object session_id -In @($tab.session_id, $fresh.pane_session_id)).Count | Should -Be 2
        (Get-ActionTabs | Where-Object tab_id -EQ $tab.tab_id) | Should -Not -BeNullOrEmpty
        (Get-ActionElement VerticalTabsHeader).Current.Name | Should -Be 'Agents'
        Wait-Until -TimeoutSec 20 -Because 'both fresh and original canonical identities remain live' -Condition {
            $rows = @(Get-ActionSessions | Where-Object session_id -In @($sid, $fresh.session_id))
            $rows.Count -eq 2 -and @($rows | Where-Object provider_id -NE 'copilot').Count -eq 0
        } | Out-Null
        $originalProcess.Refresh()
        $originalProcess.HasExited | Should -BeFalse
        Get-WtCapture -App $script:app -SessionId $fresh.pane_session_id -MaxLines 30 |
            Should -Match ([regex]::Escape("ITE2E-INTERACTIVE-DELEGATE $script:runId $($fresh.session_id)"))
        Assert-ActionRetryClearsError $retryError
        Send-WtInput -App $script:app -SessionId $tab.session_id -Text "split-alive-$script:runId"
        Send-WtKeys -App $script:app -SessionId $tab.session_id -Keys Enter
        Wait-Until -TimeoutSec 10 -Because 'original CLI remains responsive with unchanged session identity' -Condition {
            (Get-WtCapture -App $script:app -SessionId $tab.session_id -MaxLines 30) -match
                [regex]::Escape("ITE2E-DELEGATE-ALIVE $sid split-alive-$script:runId")
        } | Out-Null
    }

    It 'Agents split rejects an unidentified terminal without fallback' {
        Set-ActionView $false
        $normalTab = New-WtTab -App $script:app -Command "`"$pwsh`" -NoLogo -NoProfile -NoExit" -Cwd $script:evidence
        Set-WtPaneFocus -App $script:app -SessionId $normalTab.session_id | Out-Null
        Set-ActionView $true
        $before = @(Get-WtPanes -App $script:app -TabId ([string]$normalTab.tab_id) -WindowId ([string]$script:app.WindowId)).session_id
        $count = @(Get-ActionLaunches).Count
        Initialize-LogOffsets -App $script:app | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0xBB -Alt -Shift | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'product explicitly rejects missing live agent identity' -Condition {
            (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) -match
                'agent split rejected: missing live identity, unsupported provider, or policy'
        } | Out-Null
        $error = Get-ActionElement HistoryMessage
        $error | Should -Not -BeNullOrEmpty
        $error.Current.IsOffscreen | Should -BeFalse
        $error.Current.Name | Should -Not -BeNullOrEmpty
        Start-Sleep -Seconds 2
        $after = @(Get-WtPanes -App $script:app -TabId ([string]$normalTab.tab_id) -WindowId ([string]$script:app.WindowId)).session_id
        @($after | Where-Object { $_ -notin $before }).Count | Should -Be 0
        $after.Count | Should -Be $before.Count
        @(Get-ActionLaunches).Count | Should -Be $count
        Invoke-RunCommand -App $script:app -SessionId $normalTab.session_id -Command "Write-Output 'ORIGINAL-ALIVE-$script:runId'" -SettleSec 1 | Out-Null
        Get-WtCapture -App $script:app -SessionId $normalTab.session_id -MaxLines 30 |
            Should -Match "ORIGINAL-ALIVE-$script:runId"
    }

    It 'Agents split rejects an unsupported custom provider without fallback' {
        Set-ActionView $false
        $tab = New-WtTab -App $script:app -Command "`"$pwsh`" -NoLogo -NoProfile -NoExit" -Cwd $script:evidence
        Set-WtPaneFocus -App $script:app -SessionId $tab.session_id | Out-Null
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 30 | Should -BeTrue
        $helper = Get-AgentPaneSession -App $script:app
        $helper.AcpSessionId | Should -Match '^chat-fixture-'
        Set-AgentPaneFocus -App $script:app | Out-Null
        Set-ActionView $true
        $before = @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId)).session_id
        $count = @(Get-CanonicalLaunches).Count + @(Get-ActionLaunches).Count
        Initialize-LogOffsets -App $script:app | Out-Null
        Send-WtWindowKey -App $script:app -Vk 0xBB -Alt -Shift | Out-Null
        Wait-Until -TimeoutSec 10 -Because 'custom provider split fails visibly instead of launching a different agent' -Condition {
            (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) -match
                'agent split rejected: missing live identity, unsupported provider, or policy'
        } | Out-Null
        $message = Get-ActionElement HistoryMessage
        $message | Should -Not -BeNullOrEmpty
        $message.Current.IsOffscreen | Should -BeFalse
        $message.Current.Name | Should -Not -BeNullOrEmpty
        $after = @(Get-WtPanes -App $script:app -TabId ([string]$tab.tab_id) -WindowId ([string]$script:app.WindowId)).session_id
        @($after | Where-Object { $_ -notin $before }).Count | Should -Be 0
        $after.Count | Should -Be $before.Count
        (@(Get-CanonicalLaunches).Count + @(Get-ActionLaunches).Count) | Should -Be $count
        (Get-AgentPaneSession -App $script:app).AcpSessionId | Should -Be $helper.AcpSessionId
    }
}
