#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Contract/trigger: first sign-in and Esc/retry through the normal agent TUI.
# Boundary: packaged XAML/ConPTY -> helper/master -> the existing stdio ACP process.
# Oracles: owned UIA text/focus, safe protocol records, unchanged process leases,
# a real session/model and retained tab. No auth seeding, CLI login or fees.
# The fallback case emits a synthetic URL with an invalid client ID. The default
# browser may show a fixture error page; never sign in there or capture a browser.
# Negative controls: no authenticate before consent; an abandoned response cannot
# connect while a different advertised method is merely highlighted.
# Existing protection: Feature.AgentProtocolExperience / Feature.FreAgentSetup.
# Run only with ITE2E_PACKAGE=Dev and the parent's exact ITE2E_EXPECTED_WTA_SHA256.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = [bool]((Get-Command pwsh -ErrorAction SilentlyContinue) -and (Test-WinAppAvailable))
}

Describe 'Feature: ACP first-login authentication' -Tag 'Feature', 'AcpAuthentication' -Skip:(-not $script:Ready) {
    BeforeAll {
        $ErrorActionPreference = 'Stop'
        $script:setupPhase = 'Import ItE2E'
        try {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:setupPhase = 'Explicit Dev/hash prerequisites'
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'ACP first-login coverage requires explicit ITE2E_PACKAGE=Dev.' }
        if ($env:ITE2E_EXPECTED_WTA_SHA256 -notmatch '^[a-fA-F0-9]{64}$') {
            throw 'Set ITE2E_EXPECTED_WTA_SHA256 to the exact baseline/fixed packaged WTA hash.'
        }
        $script:setupPhase = 'Resolve Dev package'
        $script:target = Resolve-ItApp -Package Dev
        $script:setupPhase = 'Verify packaged WTA hash'
        (Get-FileHash -LiteralPath $script:target.WtaPath -Algorithm SHA256).Hash |
            Should -Be $env:ITE2E_EXPECTED_WTA_SHA256 -Because 'a different deployed build cannot establish RED/GREEN'
        $script:setupPhase = 'Refuse active or unknown package processes'
        Assert-WtPackageInactive -App $script:target
        $script:setupPhase = 'Resolve deterministic fixture'
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpAuthenticationAgent.ps1')).Path
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidenceRoot = Join-Path $artifactRoot ("acp-authentication-{0}" -f [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidenceRoot -Force | Out-Null
        $script:setupPhase = 'Load UI Automation assemblies'
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

        function Get-AuthenticationRecords {
            if (Test-Path -LiteralPath $script:requestLog) {
                Get-Content -LiteralPath $script:requestLog |
                    Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json }
            }
        }

        function Get-AuthenticationControl {
            $process = Get-Process -Id $script:app.Pid -ErrorAction Stop
            if ($process.StartTime -ne $script:app.OwnedProcess.StartTime -or
                $process.Path -ne $script:app.OwnedProcess.Path -or $script:app.OwnedProcess.HasExited) {
                throw 'The original owned Terminal process is no longer the input target.'
            }
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            if (-not $root -or $root.Current.ProcessId -ne $script:app.Pid) {
                throw 'The authentication HWND no longer belongs to the owned Terminal.'
            }
            $condition = [Windows.Automation.AndCondition]::new(
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Agent Pane'),
                [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ClassNameProperty, 'TermControl'))
            $controls = @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) |
                Where-Object { $_.Current.ProcessId -eq $script:app.Pid -and -not $_.Current.IsOffscreen })
            if ($controls.Count -ne 1) { throw 'Expected exactly one visible owned Agent Pane TermControl.' }
            $controls[0]
        }

        function Get-AuthenticationText {
            # No session JSONL is required before login, and no pane ID is invented.
            $control = Get-AuthenticationControl
            $pattern = $control.GetCurrentPattern([Windows.Automation.TextPattern]::Pattern)
            $pattern.DocumentRange.GetText(-1)
        }

        function Wait-AuthenticationText {
            param([string]$Pattern, [string]$Because, [int]$TimeoutSec = 20)
            Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 0.2 -Because $Because -Condition {
                $text = Get-AuthenticationText
                if ($text -match $Pattern) { $text }
            }
        }

        function Send-AuthenticationKey {
            param([int]$Vk)
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $control = Get-AuthenticationControl
            $control.SetFocus()
            Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'the owned pre-connect Agent Pane to receive physical input' -Condition {
                $focused = [Windows.Automation.AutomationElement]::FocusedElement
                $focused -and $focused.Current.ProcessId -eq $script:app.Pid -and
                    [Windows.Automation.Automation]::Compare($control, $focused)
            } | Out-Null
            Send-WtWindowKey -App $script:app -Vk $Vk -RequireForeground | Out-Null
        }

        function Assert-AuthenticationSelection {
            param([string]$Name)
            Wait-AuthenticationText -Pattern ("(?m)^[^\r\n]*>\s+[^\r\n]*" + [regex]::Escape($Name)) `
                -Because "the normal method picker to select $Name" | Out-Null
        }

        function Open-AuthenticationMethods {
            Send-AuthenticationKey -Vk 0x0D
            Wait-AuthenticationText -Pattern 'Personal OAuth' -Because 'the advertised authentication methods to render' | Out-Null
            (Get-AuthenticationText) | Should -Match 'Other method'
            @(Get-AuthenticationRecords | Where-Object method -eq 'authenticate') | Should -HaveCount 0
            # Exercise real navigation, rather than accepting an assumed first item.
            Send-AuthenticationKey -Vk 0x28
            Assert-AuthenticationSelection -Name 'Other method'
            Send-AuthenticationKey -Vk 0x26
            Assert-AuthenticationSelection -Name 'Personal OAuth'
        }

        function Start-PersonalAuthentication {
            Send-AuthenticationKey -Vk 0x0D
            $wait = [Diagnostics.Stopwatch]::StartNew()
            $record = Wait-Until -TimeoutSec 10 -IntervalSec 0.1 -Because 'Personal OAuth authenticate on the original ACP process' -Condition {
                Get-AuthenticationRecords | Where-Object {
                    $_.method -eq 'authenticate' -and $_.methodId -eq 'oauth-personal' -and -not $_.authenticated
                } | Select-Object -First 1
            }
            $record.pid | Should -Be $script:fixturePid
            Wait-AuthenticationText -Pattern $script:waitingRegex -Because 'the visible authorization wait' | Out-Null
            $wait
        }

        function Assert-AuthenticationOwnedProcesses {
            $script:app.OwnedProcess.HasExited | Should -BeFalse
            $helper = Get-Process -Id $script:helper.Id -ErrorAction Stop
            $helper.StartTime | Should -Be $script:helper.StartTime
            $helper.Path | Should -Be $script:target.WtaPath
            $fixtureProcess = Get-Process -Id $script:fixturePid -ErrorAction Stop
            $fixtureProcess.StartTime | Should -Be $script:fixtureProcess.StartTime
            $fixtureProcess.Path | Should -Be $script:fixtureProcess.Path
            $tabs = @(Get-WtTabs -App $script:app -WindowId $script:app.WindowId)
            $tabs | Should -HaveCount 1 -Because 'sign-in and Esc must not auto-close the tab'
            $tabs[0].tab_id | Should -BeExactly $script:tabIds[0] -Because 'authentication must retain the original tab'
            @(Get-AuthenticationRecords | Where-Object method -eq 'initialize') | Should -HaveCount 1
            @((Get-AuthenticationRecords).pid | Select-Object -Unique) | Should -HaveCount 1
            (Get-WtSettingsObject -App $script:app).acpAgent | Should -Be 'custom:pwsh'
        }

        function Assert-AuthenticationConnected {
            Wait-AuthenticationText -Pattern $script:connectedRegex `
                -Because 'the authenticated ACP session to enable the normal chat input' -TimeoutSec 35 | Out-Null
            $session = Wait-Until -TimeoutSec 15 -IntervalSec 0.2 -Because 'the genuine post-authentication pane session' -Condition {
                $sessions = @(Get-AgentPaneSessions -App $script:app)
                if ($sessions.Count -eq 1 -and $sessions[0].AcpSessionId) { $sessions[0] }
            }
            $session.HelperProcessId | Should -Be $script:helper.Id
            $session.AcpSessionId | Should -Match "^authentication-fixture-$($script:fixturePid)-\d+$"
            # AgentLabelText is a TextBlock: its UIA Name is the rendered title, not an editable Value.
            $label = Wait-Until -TimeoutSec 15 -IntervalSec 0.2 -Because 'the confirmed fixture model in the normal pane title' -Condition {
                $element = Get-UiElement -App $script:app -Selector AgentLabelText
                if ($element -and -not $element.isOffscreen -and $element.name -match 'Authentication Fixture Model') {
                    $element
                }
            }
            $label.name | Should -Match 'Authentication Fixture Model'
            $label | Select-Object automationId, name, isOffscreen |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:caseDir 'authenticated-agent-label.json') -Encoding utf8
            @(Get-AuthenticationRecords | Where-Object { $_.method -eq 'session/new' -and $_.authenticated }) |
                Should -HaveCount 1
            Assert-AuthenticationOwnedProcesses
        }

        $script:setupPhase = 'Resolve localized authentication text'
        # This helper is deliberately private; do not rely on Pester's command-not-found handling.
        $script:connectedRegex = & (Get-Module ItE2E) { Get-AgentConnectedPlaceholderRegex }
        $script:signInRegex = Get-WtaLocalizedTextRegex -Key 'setup.title.sign_in'
        if (-not $script:signInRegex) { $script:signInRegex = 'Sign in required' }
        $script:waitingRegex = Get-WtaLocalizedTextRegex -Key 'auth.waiting_for_authorization'
        if ($script:waitingRegex) {
            $script:waitingRegex = $script:waitingRegex.Replace([regex]::Escape('%{spinner}'), '[^\r\n]*')
        } else { $script:waitingRegex = 'Waiting for authorization' }
        }
        catch {
            $failure = $_
            $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
            $failurePath = Join-Path $artifactRoot ("acp-authentication-setup-failure-{0}.json" -f [guid]::NewGuid().ToString('N'))
            try {
                New-Item -ItemType Directory -Path $artifactRoot -Force | Out-Null
                @{
                    phase = $script:setupPhase
                    message = $failure.Exception.Message
                    errorId = $failure.FullyQualifiedErrorId
                    scriptStackTrace = $failure.ScriptStackTrace
                } | ConvertTo-Json | Set-Content -LiteralPath $failurePath -Encoding utf8
                Write-Warning "ACP authentication BeforeAll failed at '$($script:setupPhase)'; details: $failurePath"
            }
            catch { Write-Warning "Could not save authentication setup failure: $($_.Exception.Message)" }
            throw $failure
        }
    }

    BeforeEach {
        $ErrorActionPreference = 'Stop'
        $script:app = $null
        $script:fixtureProcess = $null
        $script:fixtureOwned = $false
        $script:helper = $null
        $script:caseDir = Join-Path $script:evidenceRoot ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:caseDir -Force | Out-Null
        $script:requestLog = Join-Path $script:caseDir 'authentication.jsonl'
        $script:browserProgressTrigger = Join-Path $script:caseDir 'emit-browser-progress.flag'
        $invocation = "& '$($script:fixture.Replace("'", "''"))' -LogPath '$($script:requestLog.Replace("'", "''"))' -BrowserProgressTriggerPath '$($script:browserProgressTrigger.Replace("'", "''"))'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $command = "pwsh -NoLogo -NoProfile -EncodedCommand $encoded"
        $profileId = "{$([guid]::NewGuid())}"
        Assert-WtPackageInactive -App $script:target
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            acpAgent = 'custom:pwsh'; acpCustomCommand = $command; acpCustomCommands = @(); acpModel = ''
            delegateAgent = ''; delegateCustomCommand = ''; delegateCustomCommands = @()
            autoErrorDetectionEnabled = $false; autoFixEnabled = $false
            agentSessionManagementEnabled = $false; agentPanePosition = 'bottom'
            defaultProfile = $profileId
            profiles = @{ list = @(@{
                guid = $profileId; name = 'ACP authentication shell'
                commandline = 'pwsh -NoLogo -NoProfile'; startingDirectory = (Split-Path $script:fixture)
            }) }
        }
        $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true -Force
        @(Get-WtWindows -App $script:app) | Should -HaveCount 1
        $script:tabIds = @((Get-WtTabs -App $script:app -WindowId $script:app.WindowId).tab_id)
        $script:tabIds | Should -HaveCount 1
        Open-AgentPane -App $script:app | Out-Null
        Wait-AuthenticationText -Pattern $script:signInRegex -Because 'fresh unauthenticated ACP sign-in setup' | Out-Null
        $initial = Wait-Until -TimeoutSec 15 -IntervalSec 0.2 -Because 'a real unauthenticated session/new request' -Condition {
            Get-AuthenticationRecords | Where-Object { $_.method -eq 'session/new' -and -not $_.authenticated } |
                Select-Object -First 1
        }
        $script:fixturePid = [int]$initial.pid
        $script:fixtureProcess = Get-Process -Id $script:fixturePid -ErrorAction Stop
        $null = $script:fixtureProcess.Handle
        $null = $script:fixtureProcess.StartTime
        $ownedWta = @(Get-DescendantWtaIds -RootPid $script:app.Pid)
        $helperRecord = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object {
            [int]$_.ProcessId -in $ownedWta -and $_.ExecutablePath -eq $script:target.WtaPath -and
                $_.CommandLine -match '--connect-master(?:\s|$)'
        })
        $helperRecord | Should -HaveCount 1
        $script:helper = Get-Process -Id $helperRecord[0].ProcessId -ErrorAction Stop
        $null = $script:helper.Handle
        $null = $script:helper.StartTime
        (Get-CimInstance Win32_Process -Filter "ProcessId=$script:fixturePid").ParentProcessId |
            Should -BeIn $ownedWta -Because 'the fixture must be launched by this owned helper/master tree'
        $script:fixtureOwned = $true
        @(Get-AuthenticationRecords | Where-Object authenticated) | Should -HaveCount 0
        @(Get-AuthenticationRecords | Where-Object method -eq 'authenticate') | Should -HaveCount 0
        @{
            package = $script:app.PackageFullName; installLocation = $script:app.InstallLocation
            wtaSha256 = (Get-FileHash -LiteralPath $script:app.WtaPath -Algorithm SHA256).Hash
            appPid = $script:app.Pid; hwnd = $script:app.Hwnd; helperPid = $script:helper.Id
            fixturePid = $script:fixturePid; windowId = $script:app.WindowId; tabIds = $script:tabIds
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:caseDir 'identity.json') -Encoding utf8
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'first-sign-in.png') | Out-Null
    }

    AfterEach {
        if ($script:app) {
            # Stop-Terminal owns creation-proven shutdown and inactive-only byte restoration.
            # Keep local protocol/screenshots as evidence; never shut down package members by name.
            try {
                try {
                    Get-AuthenticationText |
                        Set-Content -LiteralPath (Join-Path $script:caseDir 'final-agent-pane.txt') -Encoding utf8
                }
                catch { Write-Warning "Authentication UI diagnostics unavailable: $($_.Exception.Message)" }
                Stop-Terminal -App $script:app
            }
            finally {
                if ($script:fixtureOwned -and $script:fixtureProcess -and -not $script:fixtureProcess.HasExited) {
                    $exited = Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition { $script:fixtureProcess.HasExited }
                    if (-not $exited) {
                        $current = Get-Process -Id $script:fixturePid -ErrorAction Stop
                        if ($current.StartTime -ne $script:fixtureProcess.StartTime -or
                            $current.Path -ne $script:fixtureProcess.Path) {
                            throw 'Fixture cleanup refuses a changed PID/start-time/path lease.'
                        }
                        Stop-Process -Id $script:fixturePid -Force -ErrorAction Stop
                        Wait-Until -TimeoutSec 5 -Because 'the captured stdio fixture to exit' -Condition {
                            $script:fixtureProcess.HasExited
                        } | Out-Null
                    }
                }
            }
            $script:app = $null
        }
    }

    It 'ACP first sign-in follows the advertised method in the existing agent process' {
        Open-AuthenticationMethods
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'advertised-method-picker.png') | Out-Null
        $wait = Start-PersonalAuthentication
        Wait-Until -TimeoutSec 12 -IntervalSec 0.1 -Because 'authorization to remain pending beyond the ordinary ten-second RPC budget' -Condition {
            $wait.Elapsed.TotalSeconds -ge 10.2
        } | Out-Null
        (Get-AuthenticationText) | Should -Match $script:waitingRegex
        @(Get-AuthenticationRecords | Where-Object { $_.method -eq 'authenticate' -and $_.authenticated }) |
            Should -HaveCount 0 -Because 'the visible wait must precede the delayed fixture completion'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'authorization-after-ten-seconds.png') | Out-Null
        Assert-AuthenticationConnected
        $wait.Elapsed.TotalSeconds | Should -BeGreaterThan 10
        $auth = @(Get-AuthenticationRecords | Where-Object method -eq 'authenticate')
        $auth | Should -HaveCount 2
        $auth[0].methodId | Should -BeExactly 'oauth-personal'
        $auth[1].methodId | Should -BeExactly 'oauth-personal'
        $auth[0].authenticated | Should -BeFalse
        $auth[1].authenticated | Should -BeTrue
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'authenticated.png') | Out-Null
    }

    It 'ACP sign-in can be cancelled without losing the agent selection' {
        Open-AuthenticationMethods
        Start-PersonalAuthentication | Out-Null
        Send-AuthenticationKey -Vk 0x1B
        Assert-AuthenticationSelection -Name 'Personal OAuth'
        (Get-AuthenticationText) | Should -Not -Match $script:waitingRegex
        Assert-AuthenticationOwnedProcesses
        Send-AuthenticationKey -Vk 0x28
        Assert-AuthenticationSelection -Name 'Other method'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'cancelled-other-method-selected.png') | Out-Null

        Wait-Until -TimeoutSec 20 -IntervalSec 0.2 -Because 'the abandoned Personal OAuth response to actually complete' -Condition {
            Get-AuthenticationRecords | Where-Object {
                $_.method -eq 'authenticate' -and $_.methodId -eq 'oauth-personal' -and $_.authenticated
            } | Select-Object -First 1
        } | Out-Null
        # Observe after actual completion, not an arbitrary pre-completion sleep.
        $observation = [pscustomobject]@{ Clock = [Diagnostics.Stopwatch]::StartNew(); Violation = ''; Count = 0 }
        Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Because 'the stale completion to leave the new method selection untouched' -Condition {
            $text = Get-AuthenticationText
            $observation.Count++
            if ($text -match $script:connectedRegex -or $text -match $script:waitingRegex -or
                $text -notmatch '(?m)^[^\r\n]*>\s+[^\r\n]*Other method') {
                $observation.Violation = 'Abandoned authentication changed the visible selection/connection.'
                return $true
            }
            $observation.Clock.Elapsed.TotalSeconds -ge 2
        } | Out-Null
        $observation.Violation | Should -BeNullOrEmpty
        $observation.Count | Should -BeGreaterThan 1
        @(Get-AuthenticationRecords | Where-Object { $_.method -eq 'session/new' -and $_.authenticated }) |
            Should -HaveCount 0 -Because 'late authentication is not consent to create a session'
        @(Get-AuthenticationRecords | Where-Object methodId -eq 'other-method') |
            Should -HaveCount 0 -Because 'highlighting another method must not authenticate'
        Assert-AuthenticationOwnedProcesses
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'late-completion-still-cancelled.png') | Out-Null

        Send-AuthenticationKey -Vk 0x0D
        Assert-AuthenticationConnected
        $auth = @(Get-AuthenticationRecords | Where-Object method -eq 'authenticate')
        $auth | Should -HaveCount 4 -Because 'retry must issue exactly one fresh advertised-method authenticate'
        $auth[2].methodId | Should -BeExactly 'other-method'
        $auth[3].methodId | Should -BeExactly 'other-method'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'retry-authenticated.png') | Out-Null
    }

    It 'ACP authorization displays a usable manual sign-in link' -Tag 'AcpBrowserFallback' {
        Set-Content -LiteralPath $script:browserProgressTrigger -Value 'Synthetic browser progress only.' -Encoding utf8
        Open-AuthenticationMethods
        Start-PersonalAuthentication | Out-Null
        Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'browser progress from the original stdio fixture' -Condition {
            Get-AuthenticationRecords | Where-Object { $_.method -eq 'browser-progress' -and -not $_.authenticated }
        } | Out-Null
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'browser-link-before-oracle.png') | Out-Null
        $url = 'https://accounts.google.com/o/oauth2/v2/auth?client_id=invalid-acp-integration-fixture&redirect_uri=http%3A%2F%2F127.0.0.1%3A43210%2Fcallback&prompt=none&state=fixture-only'
        Wait-Until -TimeoutSec 8 -IntervalSec 0.2 -Because 'the full current sign-in link to cross SDK wire dispatch into the waiting page' -Condition {
            ([regex]::Replace((Get-AuthenticationText), '\s+', '')).Contains($url)
        } | Out-Null
        $hint = Get-WtaLocalizedTextRegex -Key 'auth.browser_link_hint'
        if (-not $hint) { throw 'The deployed fallback instruction resource is unavailable.' }
        (Get-AuthenticationText) | Should -Match $hint
        (Get-AuthenticationText) | Should -Match $script:waitingRegex
        Assert-AuthenticationOwnedProcesses
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'manual-sign-in-link.png') | Out-Null

        Send-AuthenticationKey -Vk 0x1B
        Assert-AuthenticationSelection -Name 'Personal OAuth'
        (Get-AuthenticationText) | Should -Not -Match 'accounts\.google\.com'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'cancelled-link-cleared.png') | Out-Null
        Wait-Until -TimeoutSec 20 -IntervalSec 0.2 -Because 'the abandoned fixture to emit late browser progress' -Condition {
            Get-AuthenticationRecords | Where-Object { $_.method -eq 'browser-progress' -and $_.authenticated }
        } | Out-Null
        $observation = [Diagnostics.Stopwatch]::StartNew()
        Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Because 'late browser progress to remain absent from the cancelled pane' -Condition {
            $text = Get-AuthenticationText
            $text | Should -Not -Match 'accounts\.google\.com'
            $text | Should -Not -Match $script:connectedRegex
            $text | Should -Not -Match $script:waitingRegex
            $observation.Elapsed.TotalSeconds -ge 2
        } | Out-Null
        @(Get-AuthenticationRecords | Where-Object { $_.method -eq 'session/new' -and $_.authenticated }) |
            Should -HaveCount 0 -Because 'late progress and completion are not new consent'
        Assert-AuthenticationOwnedProcesses
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:caseDir 'late-link-remains-cleared.png') | Out-Null
    }
}
