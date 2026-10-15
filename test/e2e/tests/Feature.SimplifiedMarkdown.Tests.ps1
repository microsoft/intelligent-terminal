#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Deterministic ACP text crosses the deployed Terminal/ConPTY/helper/master boundary.
# No provider prompts or authenticated history are used. The selection case requires
# an accessible clipboard; failure to preserve it is a setup failure, never a pass.

Describe 'Feature: simplified agent Markdown' -Tag 'Feature', 'SimplifiedMarkdown' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'Select the intended Dev package explicitly.' }
        $script:target = Resolve-ItApp -Package Dev
        foreach ($pair in @(@('WtaPath', 'ITE2E_EXPECTED_WTA_SHA256'), @('App', 'ITE2E_EXPECTED_APP_SHA256'))) {
            $expected = [Environment]::GetEnvironmentVariable($pair[1])
            if (-not $expected) { throw "An exact-build receipt is required: $($pair[1])" }
            $path = if ($pair[0] -eq 'App') { Join-Path $script:target.InstallLocation 'TerminalApp.dll' } else { $script:target.WtaPath }
            (Get-FileHash -LiteralPath $path).Hash | Should -Be $expected
        }
        $head = @(& git -C (Join-Path $PSScriptRoot '..\..\..') rev-parse --verify HEAD)
        if ($LASTEXITCODE -ne 0 -or $head.Count -ne 1 -or $head[0] -notmatch '\A[0-9a-fA-F]{40}\z') {
            throw 'Cannot resolve the tested source revision.'
        }
        if ($env:ITE2E_SOURCE_COMMIT -cne $head[0]) {
            throw 'ITE2E_SOURCE_COMMIT must match HEAD and the exact-build receipt.'
        }
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpQueueAgent.ps1')).Path
        $script:reply = @('# MD_HEADING', '', '**MD_BOLD**', '', '```rust', 'let value = 1;', '```') -join "`n"

        function Get-MarkdownRecords {
            param([string]$Kind, [string]$Marker)
            @(Get-ChildItem -LiteralPath $script:recordDirectory -Filter '*.json' -File |
                ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json -Depth 20 } |
                Where-Object { (-not $Kind -or $_.kind -eq $Kind) -and (-not $Marker -or $_.marker -eq $Marker) } |
                Sort-Object sequence)
        }

        function Wait-MarkdownRecord {
            param([Parameter(Mandatory)][string]$Kind, [string]$Marker)
            Wait-Until -TimeoutSec 25 -IntervalSec 0.2 -Because "ACP $Kind/$Marker; $script:evidenceDirectory" -Condition {
                Get-MarkdownRecords -Kind $Kind -Marker $Marker | Select-Object -Last 1
            }
        }

        function Get-MarkdownPaneText {
            Get-WtCapture -App $script:app -SessionId $script:agent.PaneSessionId -MaxLines 120
        }

        function Wait-MarkdownText {
            param([Parameter(Mandatory)][string]$Literal, [switch]$Formatted)
            Wait-Until -TimeoutSec 25 -IntervalSec 0.2 -Because "the expected reply presentation; $script:evidenceDirectory" -Condition {
                $text = Get-MarkdownPaneText
                if ($Formatted) {
                    $text.Contains($Literal) -and -not $text.Contains("**$Literal**")
                }
                else { $text.Contains($Literal) }
            } | Out-Null
        }

        function Save-MarkdownEvidence {
            param([Parameter(Mandatory)][string]$Name)
            Get-MarkdownPaneText |
                Set-Content -LiteralPath (Join-Path $script:evidenceDirectory "$Name.txt") -Encoding utf8NoBOM
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDirectory "$Name.png") | Out-Null
        }

        function Bind-MarkdownPane {
            param([switch]$SavedHistory)
            Open-AgentPane -App $script:app | Out-Null
            if ($SavedHistory) {
                $session = Wait-Until -TimeoutSec 45 -Because 'the restored conversation to bind to its live pane' -Condition {
                    $matches = @(Get-WtSessions -App $script:app -Origin agent-pane |
                        Where-Object { $_.session_id -ceq 'markdown-saved-session' -and $_.pane_session_id })
                    if ($matches.Count -eq 1) { $matches[0] }
                    elseif ($matches.Count -gt 1) { throw 'The restored conversation has ambiguous pane ownership.' }
                }
                $status = Get-WtPaneStatus -App $script:app -SessionId $session.pane_session_id
                $status.state | Should -Match 'run'
                $script:agent = [pscustomobject]@{
                    PaneSessionId = $session.pane_session_id
                    AcpSessionId = $session.session_id
                    HelperProcessId = $status.pid
                }
            }
            else {
                Wait-AgentReady -App $script:app -TimeoutSec 45 |
                    Should -BeTrue -Because 'the synthetic ACP agent must connect'
                $script:agent = Get-AgentPaneSession -App $script:app
            }
            $script:agent | Should -Not -BeNullOrEmpty
            $helpers = @(Get-DescendantWtaIds -RootPid $script:app.Pid -AsProcess `
                -RootProcess $script:app.OwnedProcess -RootStartTime $script:app.OwnedProcess.StartTime |
                Where-Object Id -eq $script:agent.HelperProcessId)
            $helpers | Should -HaveCount 1
            $helpers[0].Path | Should -Be $script:target.WtaPath
            $ready = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
            $ready | Should -Not -BeNullOrEmpty
            Wait-Until -TimeoutSec 45 -Because 'the owning pane to show its connected input' -Condition {
                (Get-MarkdownPaneText) -match $ready
            } | Out-Null
            $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($script:agent.HelperProcessId)"
            $owner = [regex]::Match($process.CommandLine, '(?:^|\s)--owner-tab-id\s+(?:"([^"]+)"|(\S+))')
            $owner.Success | Should -BeTrue -Because 'the helper must have an explicit owning tab'
            $script:ownerTab = if ($owner.Groups[1].Success) { $owner.Groups[1].Value } else { $owner.Groups[2].Value }
        }

        function Get-MarkdownIdentity {
            $pane = Get-AgentPaneSession -App $script:app -PaneSessionId $script:agent.PaneSessionId
            $masters = @(Get-DescendantWtaIds -RootPid $script:app.Pid | ForEach-Object {
                $process = Get-CimInstance Win32_Process -Filter "ProcessId=$_"
                if ($process.CommandLine -match '(?:^|\s)--master(?:\s|=)') { [int]$process.ProcessId }
            })
            $masters | Should -HaveCount 1
            [ordered]@{
                TerminalPid = $script:app.Pid
                MasterPid = $masters[0]
                HelperPid = $pane.HelperProcessId
                PaneSessionId = $pane.PaneSessionId
                AcpSessionId = $pane.AcpSessionId
            } | ConvertTo-Json -Compress
        }

        function Start-MarkdownScenario {
            param([bool]$Enabled = $true, [switch]$Fre, [switch]$SavedHistory)
            $invocation = "& '{0}' -ControlDirectory '{1}' -RecordDirectory '{2}'" -f
                $script:fixture.Replace("'", "''"),
                $script:controlDirectory.Replace("'", "''"),
                $script:recordDirectory.Replace("'", "''")
            $command = 'pwsh -NoLogo -NoProfile -EncodedCommand ' +
                [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
            $profileId = '{' + [guid]::NewGuid().ToString() + '}'
            $settings = @{
                acpAgent = 'custom:pwsh'
                acpCustomCommand = $command
                acpCustomCommands = @($command)
                acpModel = ''
                renderAgentMarkdown = $Enabled
                language = 'en-US'
                autoErrorDetectionEnabled = $false
                autoFixEnabled = $false
                agentSessionManagementEnabled = $false
                'agentPane.yoloMode' = $false
                copyOnSelect = $false
                showTokenUsageAndCost = $false
                agentPanePosition = 'right'
                tabLayout = 'horizontal'
                firstWindowPreference = 'defaultProfile'
                startupActions = ''
                initialCols = 110
                initialRows = 45
                'warning.confirmOnClose' = 'never'
                defaultProfile = $profileId
                disabledProfileSources = @(
                    'Windows.Terminal.PowershellCore', 'Windows.Terminal.Wsl',
                    'Windows.Terminal.Azure', 'Windows.Terminal.VisualStudio',
                    'Windows.Terminal.SSH', 'Microsoft.WSL'
                )
                profiles = @{
                    defaults = @{
                        environment = @{
                            PATH = "$($script:target.InstallLocation);$env:PATH"
                            WT_WTCLI_PATH = $script:target.WtcliPath
                            WTCLI_PATH = $script:target.WtcliPath
                        }
                    }
                    list = @(
                        @{ guid = $profileId; name = 'Markdown fixture'; commandline = 'cmd.exe'; startingDirectory = $script:evidenceDirectory }
                        @{ guid = '{61c54bbd-c2c6-5271-96e7-009a87ff44bf}'; commandline = 'cmd.exe'; hidden = $true }
                    )
                }
            }
            $state = @{
                sidebarLayoutMigrationCompleted = $true
                sidebarIntroductionShown = $true
            }
            if ($SavedHistory) {
                $settings.firstWindowPreference = 'persistedLayoutAndContent'
                $restoreCommand = '"wta.exe" --initial-load-session-id "markdown-saved-session" --initial-view chat --agent-backend host:custom:pwsh --agent-custom-command "{0}"' -f $command
                $state.persistedWindowLayouts = @(
                    @{
                        tabLayout = @(
                            @{ action = 'newTab'; profile = $profileId }
                            @{
                                action = 'splitPane'
                                split = 'right'
                                size = 0.5
                                type = 'agent'
                                commandline = $restoreCommand
                                startingDirectory = $script:evidenceDirectory
                            }
                        )
                    }
                )
            }
            $script:app = Start-Terminal -Package Dev -Settings $settings -ShowFre:$Fre -State $state
            $script:app | Add-Member -NotePropertyName RequireOwnedForeground -NotePropertyValue $true -Force
            if (-not $Fre) { Bind-MarkdownPane -SavedHistory:$SavedHistory }
        }

        function Open-MarkdownSettings {
            if (Test-UiElementExists -App $script:app -Selector SettingsNav -TimeoutSec 1) { return }
            $script:app.OwnedProcess.HasExited | Should -BeFalse
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]$script:app.Hwnd)
            $root.Current.ProcessId | Should -Be $script:app.Pid
            $button = $root.FindFirst(
                [Windows.Automation.TreeScope]::Descendants,
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, 'NewTabButton'))
            $button | Should -Not -BeNullOrEmpty
            $button.Current.ProcessId | Should -Be $script:app.Pid
            $button.Current.IsOffscreen | Should -BeFalse
            $pattern = $button.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
            $pattern.Expand()
            Invoke-UiElement -App $script:app -Selector Settings | Out-Null
            Wait-UiElement -App $script:app -Selector SettingsNav | Should -Not -BeNullOrEmpty
        }

        function Start-MarkdownReply {
            param([Parameter(Mandatory)][string]$Marker, [string]$Text = $script:reply)
            [IO.File]::WriteAllText(
                (Join-Path $script:controlDirectory "$Marker.reply.txt"), $Text, [Text.UTF8Encoding]::new($false))
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agent.PaneSessionId `
                -Text "**USER_LITERAL** $Marker" | Out-Null
            Wait-MarkdownRecord -Kind prompt -Marker $Marker
        }

        function Open-MarkdownGate {
            param([Parameter(Mandatory)][string]$Name)
            [IO.File]::WriteAllText((Join-Path $script:controlDirectory $Name), '')
        }

        function Assert-MarkdownInterruptedReply {
            param([ValidateSet('cancel', 'error', 'disconnect')][string]$Kind)
            Start-MarkdownScenario
            $marker = 'QUEUE_HOLD_MD_' + $Kind.ToUpperInvariant() + '_' + $script:runId.Substring(0, 6).ToUpperInvariant()
            $null = Start-MarkdownReply -Marker $marker
            Wait-MarkdownText -Literal '**MD_BOLD**'
            if ($Kind -eq 'cancel') {
                Send-AgentKey -App $script:app -PaneSessionId $script:agent.PaneSessionId -Key C-c | Out-Null
                $null = Wait-MarkdownRecord -Kind cancel -Marker $marker
                (Wait-MarkdownRecord -Kind completion -Marker $marker).reason | Should -Be 'cancelled'
            }
            elseif ($Kind -eq 'error') {
                Open-MarkdownGate "$marker.fail"
                (Wait-MarkdownRecord -Kind completion -Marker $marker).reason | Should -Be 'error'
            }
            else {
                Open-MarkdownGate "$marker.disconnect"
                $null = Wait-MarkdownRecord -Kind disconnect -Marker $marker
            }
            Wait-MarkdownText -Literal MD_BOLD -Formatted
            (Get-MarkdownPaneText).Contains('**USER_LITERAL**') | Should -BeTrue
            Save-MarkdownEvidence "partial-$Kind"
        }

        function Get-MarkdownTextPosition {
            param([Parameter(Mandatory)][string]$Text)
            $lines = (Get-MarkdownPaneText) -split '\r?\n'
            $hits = @(
                for ($row = 0; $row -lt $lines.Count; $row++) {
                    $column = $lines[$row].IndexOf($Text, [StringComparison]::Ordinal)
                    if ($column -ge 0) {
                        [pscustomobject]@{ Row = $row; Column = $Host.UI.RawUI.LengthInBufferCells($lines[$row].Substring(0, $column)) }
                    }
                }
            )
            $hits | Should -HaveCount 1 -Because 'input coordinates must come from the actual displayed reply'
            $hits[0]
        }
    }

    BeforeEach {
        $script:app = $null
        $script:agent = $null
        $script:runtimeBackedUp = $false
        @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables) |
            Should -HaveCount 0 -Because 'the selected package must be inactive; user-owned windows are never adopted'
        $script:originalHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path -LiteralPath "$path.e2ebak") -or (Test-Path -LiteralPath "$path.e2ebak.missing")) {
                throw "An earlier configuration backup requires recovery: $path"
            }
            $script:originalHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { $null }
        }
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:runId = [guid]::NewGuid().ToString('N')
        $script:evidenceDirectory = Join-Path ([IO.Path]::GetFullPath($root)) "simplified-markdown-$script:runId"
        $script:controlDirectory = Join-Path $script:evidenceDirectory 'control'
        $script:recordDirectory = Join-Path $script:evidenceDirectory 'records'
        New-Item -ItemType Directory -Path $script:controlDirectory, $script:recordDirectory | Out-Null
        $script:runtimePath = Join-Path $script:target.LocalStateDir 'IntelligentTerminal'
        $script:runtimeBackup = Join-Path $script:evidenceDirectory 'original-runtime'
        $script:runtimeHashes = @{}
        $script:runtimeExisted = Test-Path -LiteralPath $script:runtimePath -ErrorAction Stop
        if ($script:runtimeExisted) {
            foreach ($file in Get-ChildItem -LiteralPath $script:runtimePath -File -Recurse -Force -ErrorAction Stop) {
                $relative = [IO.Path]::GetRelativePath($script:runtimePath, $file.FullName)
                $script:runtimeHashes[$relative] = (Get-FileHash -LiteralPath $file.FullName -ErrorAction Stop).Hash
            }
            Move-Item -LiteralPath $script:runtimePath -Destination $script:runtimeBackup -ErrorAction Stop
        }
        $script:runtimeBackedUp = $true
        @{
            Settings = $script:originalHashes
            RuntimeExisted = $script:runtimeExisted
            Runtime = $script:runtimeHashes
        } | ConvertTo-Json -Depth 8 |
            Set-Content -LiteralPath (Join-Path $script:evidenceDirectory 'original-hashes.json') -Encoding utf8NoBOM
        $script:historyPath = Join-Path $script:controlDirectory 'history.json'
        @{
            'markdown-saved-session' = @{ prompt = '**SAVED_USER**'; reply = "# SAVED_HEADING`n`n**SAVED_REPLY**" }
        } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $script:historyPath -Encoding utf8NoBOM
        @{
            SourceCommit = $env:ITE2E_SOURCE_COMMIT
            PackageFullName = $script:target.PackageFullName
            InstallLocation = $script:target.InstallLocation
            AppSha256 = $env:ITE2E_EXPECTED_APP_SHA256
            WtaSha256 = $env:ITE2E_EXPECTED_WTA_SHA256
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDirectory 'identity.json') -Encoding utf8NoBOM
    }

    AfterEach {
        try {
            if ($script:app -and $script:app.Launched) {
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDirectory 'final.png') | Out-Null
            }
        }
        finally {
            if ($script:app) { Stop-Terminal -App $script:app | Out-Null }
            if ($script:runtimeBackedUp) {
                @(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables) |
                    Should -HaveCount 0 -Because "runtime restoration requires package inactivity; backup: $script:runtimeBackup"
                if (Test-Path -LiteralPath $script:runtimePath -ErrorAction Stop) {
                    Move-Item -LiteralPath $script:runtimePath -Destination (Join-Path $script:evidenceDirectory 'test-runtime') -ErrorAction Stop
                }
                if ($script:runtimeExisted) {
                    Move-Item -LiteralPath $script:runtimeBackup -Destination $script:runtimePath -ErrorAction Stop
                    $restored = @(Get-ChildItem -LiteralPath $script:runtimePath -File -Recurse -Force -ErrorAction Stop)
                    $restored | Should -HaveCount $script:runtimeHashes.Count
                    foreach ($file in $restored) {
                        $relative = [IO.Path]::GetRelativePath($script:runtimePath, $file.FullName)
                        $script:runtimeHashes.ContainsKey($relative) | Should -BeTrue
                        (Get-FileHash -LiteralPath $file.FullName -ErrorAction Stop).Hash |
                            Should -Be $script:runtimeHashes[$relative]
                    }
                }
                else {
                    Test-Path -LiteralPath $script:runtimePath | Should -BeFalse
                }
            }
            foreach ($path in $script:originalHashes.Keys) {
                if ($null -eq $script:originalHashes[$path]) {
                    Test-Path -LiteralPath $path | Should -BeFalse -Because 'a previously missing configuration file must remain absent'
                }
                else {
                    (Get-FileHash -LiteralPath $path).Hash |
                        Should -Be $script:originalHashes[$path] -Because 'the original configuration must be restored byte for byte'
                }
            }
        }
    }

    It 'Completed agent replies switch from raw text to Markdown' {
        Start-MarkdownScenario
        $marker = 'QUEUE_HOLD_MD_DONE_' + $script:runId.Substring(0, 6).ToUpperInvariant()
        $null = Start-MarkdownReply -Marker $marker
        Wait-MarkdownText -Literal '**MD_BOLD**'
        (Get-MarkdownPaneText).Contains('# MD_HEADING') | Should -BeTrue
        @(Get-MarkdownRecords -Kind completion -Marker $marker) | Should -HaveCount 0
        Save-MarkdownEvidence raw-streaming
        Open-MarkdownGate "$marker.release"
        $null = Wait-MarkdownRecord -Kind completion -Marker $marker
        Wait-MarkdownText -Literal MD_BOLD -Formatted
        $text = Get-MarkdownPaneText
        $text.Contains('# MD_HEADING') | Should -BeFalse
        $text.Contains('MD_HEADING') | Should -BeTrue
        $text.Contains('**USER_LITERAL**') | Should -BeTrue
        $text.Contains('```rust') | Should -BeTrue
        $text.Contains('let value = 1;') | Should -BeTrue
        ([regex]::Matches($text, '```')).Count | Should -Be 2
        Save-MarkdownEvidence completed-markdown
    }

    It 'Cancelled agent replies format their received text' {
        Assert-MarkdownInterruptedReply -Kind cancel
    }

    It 'Failed agent replies format their received text' {
        Assert-MarkdownInterruptedReply -Kind error
    }

    It 'Disconnected agent replies format their received text' {
        Assert-MarkdownInterruptedReply -Kind disconnect
    }

    It 'Raw Markdown preference bypasses formatting from helper startup' {
        Start-MarkdownScenario -Enabled $false
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$($script:agent.HelperProcessId)"
        ($process.CommandLine -match '(?:^|\s)--no-agent-markdown(?:\s|$)') | Should -BeTrue
        $marker = 'QUEUE_HOLD_MD_RAW_' + $script:runId.Substring(0, 6).ToUpperInvariant()
        $null = Start-MarkdownReply -Marker $marker
        Wait-MarkdownText -Literal '**MD_BOLD**'
        Open-MarkdownGate "$marker.release"
        $null = Wait-MarkdownRecord -Kind completion -Marker $marker
        Wait-MarkdownText -Literal "ACK_$marker"
        (Get-MarkdownPaneText).Contains('**MD_BOLD**') | Should -BeTrue
        (Get-MarkdownPaneText).Contains('# MD_HEADING') | Should -BeTrue
        Save-MarkdownEvidence raw-completed
    }

    It 'Selected raw replies survive completion without blocking queued requests' {
        $clipboardCaptured = $false
        try {
            Start-MarkdownScenario
            $selected = '**SELECTED_RAW**'
            $source = ("Earlier line`n" * 35) + "$selected`n" + ("tail`n" * 4)
            $marker = 'QUEUE_HOLD_MD_SELECT_' + $script:runId.Substring(0, 6).ToUpperInvariant()
            $first = Start-MarkdownReply -Marker $marker -Text $source
            Wait-MarkdownText -Literal $selected
            $nextMarker = 'QUEUE_NEXT_' + $script:runId.Substring(0, 6).ToUpperInvariant()
            Send-AgentPrompt -App $script:app -PaneSessionId $script:agent.PaneSessionId -Text $nextMarker | Out-Null
            Wait-MarkdownText -Literal '1 message queued'
            @(Get-MarkdownRecords -Kind prompt -Marker $nextMarker) | Should -HaveCount 0
            $position = Get-MarkdownTextPosition -Text $selected
            Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agent.PaneSessionId -Kind Down -Row $position.Row -Column $position.Column | Out-Null
            Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agent.PaneSessionId -Kind Drag -Row $position.Row -Column ($position.Column + $selected.Length - 1) | Out-Null
            Send-AgentMouseEvent -App $script:app -PaneSessionId $script:agent.PaneSessionId -Kind Up -Row $position.Row -Column ($position.Column + $selected.Length - 1) | Out-Null
            Open-MarkdownGate "$marker.release"
            $completed = Wait-MarkdownRecord -Kind completion -Marker $marker
            $next = Wait-MarkdownRecord -Kind prompt -Marker $nextMarker
            $next.sessionId | Should -Be $first.sessionId
            $next.pid | Should -Be $first.pid
            $next.sequence | Should -BeGreaterThan $completed.sequence
            (Get-MarkdownPaneText).Contains($selected) | Should -BeTrue
            Save-MarkdownEvidence selected-raw-after-next-prompt
            $clipboard = Get-ClipboardSnapshot
            $clipboardCaptured = $true
            Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agent.PaneSessionId) | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x43 -Ctrl -RequireForeground | Out-Null
            Wait-Until -TimeoutSec 5 -Because 'copy must return exactly the selected raw reply' -Condition {
                $copied = Invoke-ClipboardSta {
                    Add-Type -AssemblyName System.Windows.Forms
                    [System.Windows.Forms.Clipboard]::GetText()
                }
                [string]::Equals([string]$copied, $selected, [StringComparison]::Ordinal)
            } | Out-Null
            $lines = (Get-MarkdownPaneText) -split '\r?\n'
            $inputRows = @(for ($row = 0; $row -lt $lines.Count; $row++) {
                if ($lines[$row] -match '^\s*[│║|]\s*>') { $row }
            })
            $inputRows | Should -HaveCount 1
            Send-AgentMouseClick -App $script:app -PaneSessionId $script:agent.PaneSessionId `
                -Row $inputRows[0] -Column ($lines[$inputRows[0]].IndexOf('>') + 2) | Out-Null
            Wait-MarkdownText -Literal SELECTED_RAW -Formatted
            Save-MarkdownEvidence selection-cleared-markdown
        }
        finally {
            if ($clipboardCaptured) { Restore-ClipboardSnapshot -Snapshot $clipboard }
        }
    }

    It 'Saved agent replies use Markdown without changing raw history' {
        $historyHash = (Get-FileHash -LiteralPath $script:historyPath).Hash
        Start-MarkdownScenario -SavedHistory
        (Wait-MarkdownRecord -Kind load).sessionId | Should -Be 'markdown-saved-session'
        $script:agent.AcpSessionId | Should -Be 'markdown-saved-session'
        Wait-MarkdownText -Literal SAVED_REPLY -Formatted
        (Get-MarkdownPaneText).Contains('**SAVED_USER**') | Should -BeTrue
        Save-MarkdownEvidence loaded-history
        Set-WtSetting -App $script:app -Key renderAgentMarkdown -Value $false | Out-Null
        Wait-MarkdownText -Literal '**SAVED_REPLY**'
        (Get-FileHash -LiteralPath $script:historyPath).Hash | Should -Be $historyHash
    }

    It 'FRE and Settings share a live Markdown preference without restarting the session' {
        Start-MarkdownScenario -Enabled $false -Fre
        Invoke-UiElement -App $script:app -Selector NextButton -TimeoutSec 15 | Out-Null
        $toggle = Get-UiElement -App $script:app -Selector RenderAgentMarkdownToggle
        $toggle | Should -Not -BeNullOrEmpty
        $toggle.name | Should -Be 'Markdown formatting'
        $toggle.toggleState | Should -Be 'off' -Because 'FRE must load the saved preference'
        Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDirectory 'fre.png') | Out-Null
        Invoke-UiElement -App $script:app -Selector RenderAgentMarkdownToggle | Out-Null
        Invoke-UiElement -App $script:app -Selector SaveButton | Out-Null
        Wait-Until -TimeoutSec 60 -Because 'FRE to save the preference and finish setup' -Condition {
            -not (Test-FreShowing -App $script:app) -and (Get-FreCompleted -App $script:app)
        } | Out-Null
        Assert-Setting -App $script:app -Key renderAgentMarkdown -Value $true
        Bind-MarkdownPane
        $marker = 'QUEUE_HOLD_MD_SETTINGS_' + $script:runId.Substring(0, 6).ToUpperInvariant()
        $null = Start-MarkdownReply -Marker $marker
        Wait-MarkdownText -Literal '**MD_BOLD**'
        Open-MarkdownGate "$marker.release"
        $null = Wait-MarkdownRecord -Kind completion -Marker $marker
        Wait-MarkdownText -Literal MD_BOLD -Formatted
        $identity = Get-MarkdownIdentity
        foreach ($enabled in @($false, $true)) {
            Open-MarkdownSettings
            Invoke-SettingsNav -App $script:app -NavItem AIAgentsNavItem | Out-Null
            $toggle = Get-UiElement -App $script:app -Selector RenderAgentMarkdownToggle
            $toggle.name | Should -Be 'Markdown formatting'
            $toggle.toggleState | Should -Be $(if ($enabled) { 'off' } else { 'on' })
            Invoke-UiElement -App $script:app -Selector RenderAgentMarkdownToggle | Out-Null
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidenceDirectory "settings-$enabled.png") | Out-Null
            Invoke-UiElement -App $script:app -Selector SaveButton | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'Settings to persist the shared Markdown preference' -Condition {
                (Get-WtSetting -App $script:app -Key renderAgentMarkdown) -eq $enabled
            } | Out-Null
            Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agent.PaneSessionId) | Out-Null
            if ($enabled) { Wait-MarkdownText -Literal MD_BOLD -Formatted }
            else { Wait-MarkdownText -Literal '**MD_BOLD**' }
            [string]::Equals((Get-MarkdownIdentity), $identity, [StringComparison]::Ordinal) |
                Should -BeTrue -Because 'display preferences must not replace the host, master, helper, pane, or ACP session'
        }
    }
}
