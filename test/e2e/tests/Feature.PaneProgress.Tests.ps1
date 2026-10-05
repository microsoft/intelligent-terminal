#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #1043. Published cases use real shell OSC and native hook transport, never model quota.
# Protection: SidebarTabKeyboard, SidebarTelemetry, SessionHookRouting and HookBridgeCli.
BeforeDiscovery {
    $script:Ready = [bool]((Get-Command winapp -ErrorAction SilentlyContinue) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-AppxPackage | Where-Object Name -eq 'IntelligentTerminal'))
}

Describe 'Feature: pane progress user routes' -Tag @('Feature', 'PaneProgress') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
        if (-not ('ItE2E.ItPaneProgressDpi' -as [type])) {
            Add-Type -Name ItPaneProgressDpi -Namespace ItE2E -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern uint GetDpiForWindow(System.IntPtr hwnd);
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern System.IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern bool CloseDesktop(System.IntPtr desktop);
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet=System.Runtime.InteropServices.CharSet.Unicode)]
public static extern bool GetUserObjectInformation(System.IntPtr handle, int index, System.Text.StringBuilder value, int length, out int needed);
'@
        }
        $script:app = $null
        $script:cursor = $null
        (Get-ItTestPackage) | Should -Be Dev -Because 'this regression targets the intended feature-branch Dev package'
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply exact-source build receipt hashes for TerminalApp.dll and wta.exe.'
        }
        $target = Resolve-ItApp -Package Dev
        @(Get-WtProcessesForApp -App $target -IncludePackageExecutables) | Should -HaveCount 0
        foreach ($path in @($target.SettingsPath, $target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) { throw "Recover existing backup first: $path" }
        }
        (Get-FileHash (Join-Path $target.InstallLocation 'TerminalApp.dll')).Hash | Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) ('pane-progress-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:evidence | Out-Null
        $profile = '{' + [guid]::NewGuid().ToString() + '}'
        $script:profile = $profile
        $pwsh = (Get-Command pwsh).Source
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1')).Path
        $fixtureCode = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:evidence.Replace("'", "''"))\acp.log'"
        $fixtureCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureCode))
        try {
            $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
                language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
                firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
                'warning.confirmOnClose' = 'never'; autoErrorDetectionEnabled = $false
                acpAgent = 'custom:pane-progress-fixture'
                acpCustomCommand = "`"$pwsh`" -NoProfile -EncodedCommand $fixtureCommand"
                defaultProfile = $profile
                profiles = @{ list = @(@{
                    guid = $profile; name = 'ItE2E progress PowerShell'
                    commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                    icon = 'ms-appx:///ProfileIcons/pwsh.png'; shellIntegrationEnabled = $false
                }) }
            }
        }
        catch {
            if (@(Get-WtProcessesForApp -App $target -IncludePackageExecutables).Count) {
                throw "Launch failed with package processes remaining; backups retained: $_"
            }
            Restore-WtConfig -App $target
            throw
        }
        $script:app.Launched | Should -BeTrue
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        $script:cursor = [ItE2E.ItWtWin32Input]::GetCursorPosition()
        $dpi = [ItE2E.ItPaneProgressDpi]::GetDpiForWindow([IntPtr][long]$script:app.Hwnd)
        $dpi | Should -BeGreaterThan 0
        $script:scale = $dpi / 96
        $loaded = @((Get-Process -Id $script:app.Pid).Modules | Where-Object ModuleName -eq 'TerminalApp.dll')
        $loaded | Should -HaveCount 1
        $loaded[0].FileName | Should -Be (Join-Path $target.InstallLocation 'TerminalApp.dll')
        @{
            pid = $script:app.Pid; profile = $profile; source = $env:ITE2E_EXPECTED_SOURCE_REVISION
            appHash = $env:ITE2E_EXPECTED_APP_SHA256; wtaHash = $env:ITE2E_EXPECTED_WTA_SHA256
        } | ConvertTo-Json | Set-Content (Join-Path $script:evidence 'package.json')

        function Get-OwnedElements {
            param([string]$Property, [string]$Value, $Parent)
            if (-not $Parent) { $Parent = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd) }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::"${Property}Property", $Value)
            @($Parent.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) |
                Where-Object { -not $_.Current.IsOffscreen -and $_.Current.ProcessId -eq $script:app.Pid })
        }
        function Get-PaneRow {
            param([string]$Title)
            $rows = @(Get-OwnedElements AutomationId PaneActivateButton | Where-Object { $_.Current.Name.StartsWith($Title) })
            $rows | Should -HaveCount 1 -Because 'unique pane title must identify one visible owned row'
            $rows[0]
        }
        function Open-TabMenu {
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground -Repeat 2 | Out-Null
            $tree = Get-UiTree -App $script:app -Depth 12
            $pattern = '(?m)^(?<indent>[ \t]*)(?<selector>lbl-textview-\S+|TextView) Text "' + [regex]::Escape($script:titleA) + '"'
            $matches = @([regex]::Matches($tree, $pattern))
            $depth = ($matches | ForEach-Object { $_.Groups['indent'].Length } | Measure-Object -Minimum).Minimum
            $matches = @($matches | Where-Object { $_.Groups['indent'].Length -eq $depth })
            $matches | Should -HaveCount 1 -Because 'right-click must target the current parent tab title, not its child pane or a prefix-matching guard'
            Invoke-UiClick -App $script:app -Selector $matches[0].Groups['selector'].Value -Right | Out-Null
            Wait-UiElement -App $script:app -Selector 'Move tab' | Out-Null
            Save-CompositorFrame "context-$([guid]::NewGuid().ToString('N'))"
        }
        function Switch-ContextLayout {
            param([ValidateSet('vertical', 'horizontal')][string]$Layout)
            Open-TabMenu
            $label = if ($Layout -eq 'vertical') { 'Switch to sidebar' } else { 'Switch to horizontal tabs' }
            Invoke-UiElement -App $script:app -Selector $label | Out-Null
            Wait-Until -TimeoutSec 10 -Because 'context layout switch persists' -Condition {
                (Get-WtSetting -App $script:app -Key tabLayout) -eq $Layout
            } | Out-Null
        }
        function Move-ContextTab {
            param([string]$Direction, [int]$Index)
            Open-TabMenu
            Invoke-UiElement -App $script:app -Selector 'Move tab' | Out-Null
            Invoke-UiElement -App $script:app -Selector "Move $Direction" | Out-Null
            Wait-Until -TimeoutSec 10 -Because "context move $Direction reaches index $Index" -Condition {
                $active = Get-ActivePane -App $script:app
                $active.session_id -eq $script:a.session_id -and [int]$active.tab_id -eq $Index
            } | Out-Null
            $script:tab.tab_id = (Get-ActivePane -App $script:app).tab_id
            Set-WtPaneFocus -App $script:app -SessionId $script:a.session_id
            @((Get-WtPanes -App $script:app -TabId $script:tab.tab_id).session_id | Sort-Object) |
                Should -Be @(@($script:a.session_id, $script:b.session_id) | Sort-Object)
        }
        function Get-VisualDigest {
            param($Bounds, [string]$Name, [switch]$Icon)
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd).Current.BoundingRectangle
            [ItE2E.ItWtWin32Input]::SetCursorPos([int]($window.Right - 30), [int]($window.Bottom - 30)) | Should -BeTrue
            Start-Sleep -Milliseconds 350
            $Bounds.Width | Should -BeGreaterThan 0
            $Bounds.Height | Should -BeGreaterThan 0
            $bitmap = [Drawing.Bitmap]::new([int][math]::Ceiling($Bounds.Width), [int][math]::Ceiling($Bounds.Height))
            $graphics = [Drawing.Graphics]::FromImage($bitmap)
            try {
                $graphics.CopyFromScreen([int]$Bounds.X, [int]$Bounds.Y, 0, 0, $bitmap.Size)
                $path = Join-Path $script:evidence "$Name.png"
                $bitmap.Save($path, [Drawing.Imaging.ImageFormat]::Png)
                @{
                    backend = 'Desktop compositor CopyFromScreen (not PrintWindow)'
                    hwnd = $script:app.Hwnd; pid = $script:app.Pid
                    bounds = $Bounds.ToString(); utc = [DateTime]::UtcNow
                } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "$Name.json")
                if ($Icon) {
                    $colors = [Collections.Generic.HashSet[int]]::new()
                    for ($x = 0; $x -lt $bitmap.Width; $x++) {
                        for ($y = 0; $y -lt $bitmap.Height; $y++) { [void]$colors.Add($bitmap.GetPixel($x, $y).ToArgb()) }
                    }
                    $colors.Count | Should -BeGreaterThan 1 -Because 'a blank icon slot is not profile restoration'
                }
                (Get-FileHash -LiteralPath $path).Hash
            }
            finally { $graphics.Dispose(); $bitmap.Dispose() }
        }
        function Save-CompositorFrame {
            param([string]$Name)
            $bounds = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd).Current.BoundingRectangle
            Get-VisualDigest -Bounds $bounds -Name $Name | Out-Null
        }
        function Assert-ProgressFrames {
            param([string]$Phase, [switch]$Horizontal)
            Set-WtWindowForeground -App $script:app | Should -BeTrue
            $ringId = if ($Horizontal) { 'HeaderIndeterminateProgressRing' } else { 'PaneIndeterminateProgressRing' }
            Wait-Until -TimeoutSec 10 -Because "$Phase layout has realized its owned progress widget" -Condition {
                @(Get-OwnedElements AutomationId $ringId).Count -eq 1
            } | Out-Null
            foreach ($frame in 1..6) {
                $rings = @(Get-OwnedElements AutomationId $ringId)
                $rings | Should -HaveCount 1 -Because 'only pane A emitted progress'
                Set-WtPaneFocus -App $script:app -SessionId $script:a.session_id
                @((Get-WtPanes -App $script:app -TabId $script:tab.tab_id).session_id | Sort-Object) |
                    Should -Be @(@($script:a.session_id, $script:b.session_id) | Sort-Object)
                $bounds = $rings[0].Current.BoundingRectangle
                if (-not $Horizontal) {
                    $row = Get-PaneRow $script:titleA
                    $row.Current.Name | Should -Match 'Indeterminate progress'
                    $rings[0].Current.Name | Should -Be $row.Current.Name -Because 'the rendered ring belongs to the emitting pane'
                    (Get-PaneRow $script:titleB).Current.Name | Should -Not -Match 'Indeterminate progress'
                    $row.Current.BoundingRectangle.Contains($bounds) | Should -BeTrue -Because 'ring must not be clipped or attached to pane B'
                }
                Save-CompositorFrame "$Phase-$frame-window"
                Get-VisualDigest -Bounds $bounds -Name "$Phase-$frame-ring" | Out-Null
                # Avoid sampling the one-second rotation at an accidentally matching cadence.
                Start-Sleep -Milliseconds @(41, 97, 173, 263, 389, 521)[$frame - 1]
            }
            $digests = @(foreach ($frame in 1..6) {
                (Get-FileHash -LiteralPath (Join-Path $script:evidence "$Phase-$frame-ring.png")).Hash
            })
            $distinct = @($digests | Select-Object -Unique).Count
            @{ phase = $Phase; hashes = $digests; frames = 6; distinct = $distinct; animated = ($distinct -gt 1); pane = $script:a.session_id } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "$Phase-frames.json")
            $digests | Should -HaveCount 6
            $distinct | Should -BeGreaterThan 1 -Because 'six actual ring frames must animate, not merely expose active bound state'
        }
        function Get-ProfileIconDigest {
            param([string]$Name, [string]$Title = $script:titleA)
            $bounds = (Get-PaneRow $Title).Current.BoundingRectangle
            # PaneActivateButton's template: 3-DIP indicator, 28-DIP icon column, centered 16-DIP icon.
            $size = 16 * $script:scale
            $icon = [Windows.Rect]::new($bounds.X + 9 * $script:scale, $bounds.Y + ($bounds.Height - $size) / 2, $size, $size)
            $bounds.Contains($icon) | Should -BeTrue -Because 'the entire profile icon must remain inside its owned row'
            Get-VisualDigest -Bounds $icon -Name $Name -Icon
        }
        function Send-ShellScript {
            param([string]$Pane, [string]$Code, [string]$ExitPath)
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($Code))
            $command = "pwsh -NoProfile -EncodedCommand $encoded"
            if ($ExitPath) { $command += "; [IO.File]::WriteAllText('$ExitPath', [string]`$LASTEXITCODE)" }
            Send-WtInput -App $script:app -SessionId $Pane -Text $command
            Send-WtKeys -App $script:app -SessionId $Pane -Keys Enter
        }
        function Assert-NativeStatus {
            param([string]$Session, [string]$Status)
            $pipe = (Get-Content -Raw -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt')).Trim()
            $expectedPane = if ($Status -eq 'Ended') { $null } else { $script:a.session_id }
            $row = Wait-Until -TimeoutSec 30 -Because "owned native session reaches $Status" -Condition {
                $result = Invoke-Wta -App $script:app -Arguments @('sessions', 'list', '--master', $pipe, '--json', '--include-status') -Raw
                $result.ExitCode | Should -Be 0 -Because $result.StdErr
                $snapshot = $result.StdOut | ConvertFrom-Json
                $rows = @($snapshot.sessions | Where-Object {
                    $_.session_id -eq $Session -and $_.provider_id -eq 'copilot' -and
                    $_.pane_session_id -eq $expectedPane -and $_.status -eq $Status
                })
                if ($rows.Count -eq 1) { $rows[0] }
            }
            $row | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:evidence "$Session-$Status.json")
        }
        $script:tabTitle = 'IT-progress-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
        $script:titleA = "$script:tabTitle-A"; $script:titleB = "$script:tabTitle-B"
        New-WtTab -App $script:app -Title "$script:tabTitle-guard" | Out-Null
        $script:tab = New-WtTab -App $script:app -Title $script:tabTitle
        $script:a = $script:tab
        $script:b = Split-WtPane -App $script:app -SessionId $script:a.session_id -Direction right -Size 0.5
        $panes = @(Get-WtPanes -App $script:app -TabId $script:tab.tab_id)
        $panes | Should -HaveCount 2
        foreach ($pane in $panes) {
            $pane.pid | Should -BeGreaterThan 0
            (Get-CimInstance Win32_Process -Filter "ProcessId=$($pane.pid)").ExecutablePath | Should -Be $pwsh
        }
        foreach ($pane in @(@{ id = $script:a.session_id; title = $script:titleA; titleOsc = 0 }, @{ id = $script:b.session_id; title = $script:titleB; titleOsc = 2 })) {
            Send-WtInput -App $script:app -SessionId $pane.id -Text "[Console]::Write([char]27+']$($pane.titleOsc);$($pane.title)'+[char]7); [Console]::Write([char]27+']9;4;0'+[char]7)"
            Send-WtKeys -App $script:app -SessionId $pane.id -Keys Enter
        }
        Set-WtPaneFocus -App $script:app -SessionId $script:a.session_id
        Wait-Until -TimeoutSec 15 -Because 'both explicitly profiled owned pane titles appear' -Condition {
            $rows = @(Get-OwnedElements AutomationId PaneActivateButton)
            @($rows | ForEach-Object { @{ name = $_.Current.Name; bounds = $_.Current.BoundingRectangle.ToString() } }) |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'setup-rows.json')
            @($rows | Where-Object {
                $_.Current.Name.StartsWith($script:titleA) -or $_.Current.Name.StartsWith($script:titleB)
            }).Count -eq 2
        } | Out-Null
        @{
            tab = $script:tab.tab_id; a = $script:a.session_id; b = $script:b.session_id
            titleA = $script:titleA; titleB = $script:titleB
            profileA = $profile; profileB = $profile
            iconA = 'ms-appx:///ProfileIcons/pwsh.png'; iconB = 'ms-appx:///ProfileIcons/pwsh.png'
            titleOscA = 0; titleOscB = 2
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'pane-owners.json')
    }
    BeforeEach {
        $desktop = [ItE2E.ItPaneProgressDpi]::OpenInputDesktop(0, $false, 1)
        $name = [Text.StringBuilder]::new(128)
        $needed = 0
        $available = $false
        if ($desktop -ne [IntPtr]::Zero) {
            try {
                $available = [ItE2E.ItPaneProgressDpi]::GetUserObjectInformation($desktop, 2, $name, 256, [ref]$needed) -and $name.ToString() -eq 'Default'
            }
            finally { [void][ItE2E.ItPaneProgressDpi]::CloseDesktop($desktop) }
        }
        @{ available = $available; name = $name.ToString(); backend = 'Desktop compositor CopyFromScreen' } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'desktop.json')
        if (-not $available) {
            Set-ItResult -Skipped -Because 'the Default interactive desktop is unavailable; PrintWindow cannot substitute for compositor proof'
            return
        }
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            Set-ItResult -Skipped -Because 'the interactive desktop cannot grant the owned window foreground'
            return
        }
        Set-WtPaneFocus -App $script:app -SessionId $script:a.session_id
        $script:tab.tab_id = (Get-ActivePane -App $script:app).tab_id
        if ((Get-WtSetting -App $script:app -Key tabLayout) -ne 'vertical') { Switch-ContextLayout vertical }
        for ($reset = 0; [int]$script:tab.tab_id -ne 2 -and $reset -lt 3; $reset++) {
            $direction = if ([int]$script:tab.tab_id -lt 2) { 'down' } else { 'up' }
            $next = [int]$script:tab.tab_id + $(if ($direction -eq 'down') { 1 } else { -1 })
            Move-ContextTab $direction $next
        }
        [int]$script:tab.tab_id | Should -Be 2
        Send-ShellScript $script:a.session_id "[Console]::Write([char]27+']9;4;0'+[char]7)"
        Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing -Gone | Out-Null
        $script:profileIcon = Get-ProfileIconDigest 'baseline-profile'
        $script:siblingIcon = Get-ProfileIconDigest 'baseline-sibling' $script:titleB
    }
    AfterAll {
        if ($script:cursor) { [void][ItE2E.ItWtWin32Input]::SetCursorPos($script:cursor[0], $script:cursor[1]) }
        if ($script:app) {
            Stop-Terminal -App $script:app
            @{ fixtureProcessClosed = ($null -eq (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue))
                settingsRestored = (-not (Test-Path "$($script:app.SettingsPath).e2ebak"))
                stateRestored = (-not (Test-Path "$($script:app.StatePath).e2ebak")) } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'cleanup.json')
        }
    }

    It 'Literal one-shot OSC3 at an idle PowerShell prompt survives the exact context route' -Tag 'PaneProgressLiteral' {
        $command = '$ESC=[char]27;$BEL=[char]7;[Console]::Write("$ESC]9;4;3;0$BEL")'
        Send-WtInput -App $script:app -SessionId $script:a.session_id -Text $command
        Send-WtKeys -App $script:app -SessionId $script:a.session_id -Keys Enter
        try {
            Wait-Until -TimeoutSec 10 -Because 'the literal command returns to an ordinary idle prompt before any move' -Condition {
                $controls = @(Get-OwnedElements ClassName TermControl)
                foreach ($control in $controls) {
                    $pattern = $null
                    if ($control.TryGetCurrentPattern([Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
                        $text = $pattern.DocumentRange.GetText(-1)
                        if ($text -match [regex]::Escape($command) -and $text.TrimEnd() -match '(?:\A|\r?\n)PS [^\r\n]*>\s*\z') { return $true }
                    }
                }
                $false
            } | Out-Null
            $settled = Test-Until -TimeoutSec 5 -Condition {
                @(Get-OwnedElements AutomationId PaneIndeterminateProgressRing).Count -eq 1
            }
            $rings = @(Get-OwnedElements AutomationId PaneIndeterminateProgressRing)
            @{
                command = $command; emissions = 1; idlePrompt = $true; preMoveRings = $rings.Count
                pane = $script:a.session_id; shellIntegrationEnabled = $false
                coreTaskbarQuerySupported = $false
                modelName = (Get-PaneRow $script:titleA).Current.Name
                note = 'Missing pre-action progress is an input/state contract failure, not evidence of a move/render regression.'
            } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'literal-one-shot.json')
            $settled | Should -BeTrue -Because 'a missing pre-action state is not a reproduced move/render regression'
            $rings | Should -HaveCount 1 -Because 'prove progress before moving; do not replace the literal command with a sleeping fixture'
            Assert-ProgressFrames literal-initial
            foreach ($move in @(@{ d = 'up'; i = 1 }, @{ d = 'up'; i = 0 }, @{ d = 'down'; i = 1 }, @{ d = 'down'; i = 2 })) {
                Move-ContextTab $move.d $move.i
                Assert-ProgressFrames "literal-$($move.d)-$($move.i)"
            }
            Switch-ContextLayout horizontal
            Assert-ProgressFrames literal-horizontal -Horizontal
            Switch-ContextLayout vertical
            Assert-ProgressFrames literal-return
        }
        finally {
            Send-ShellScript $script:a.session_id "[Console]::Write([char]27+']9;4;0'+[char]7)"
            Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing -Gone | Out-Null
        }
    }

    It 'Sidebar group chevrons share the tab icon slot and title alignment' -Tag 'PaneProgressGroup' {
        # Boundary: group projection -> realized Sidebar template -> compositor/UIA bounds.
        # Negative: pane rows retain their identity, and compact rail has no group chevron.
        foreach ($collapsed in @($false, $true, $false)) {
            if ($collapsed) {
                Invoke-UiElement -App $script:app -Selector 'Collapse tab group' | Out-Null
                Wait-UiElement -App $script:app -Selector PaneActivateButton -Gone | Out-Null
            }
            elseif (@(Get-OwnedElements AutomationId PaneActivateButton).Count -eq 0) {
                Invoke-UiElement -App $script:app -Selector 'Expand tab group' | Out-Null
                Wait-UiElement -App $script:app -Selector PaneActivateButton | Out-Null
            }
            $label = if ($collapsed) { 'collapsed' } else { 'expanded' }
            $group = @(Get-OwnedElements Name $script:titleA | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text
            } | Sort-Object { $_.Current.BoundingRectangle.Y }) | Select-Object -First 1
            $single = @(Get-OwnedElements Name "$script:tabTitle-guard" | Where-Object {
                $_.Current.ControlType -eq [Windows.Automation.ControlType]::Text
            } | Sort-Object { $_.Current.BoundingRectangle.Y }) | Select-Object -First 1
            $group | Should -Not -BeNullOrEmpty
            $single | Should -Not -BeNullOrEmpty
            $toggle = @(Get-OwnedElements AutomationId TabGroupToggleButton)
            $toggle | Should -HaveCount 1
            $bounds = $group.Current.BoundingRectangle
            $offset = [math]::Abs($bounds.X - $single.Current.BoundingRectangle.X)
            $offset | Should -BeLessOrEqual $script:scale -Because 'top-level group and singleton titles share the same leading slot'
            ($bounds.X - $toggle[0].Current.BoundingRectangle.Right) |
                Should -BeLessThan (20 * $script:scale) -Because 'no extra profile-icon column may appear after the group chevron'
            Save-CompositorFrame "group-slot-$label"
            @{ collapsed = $collapsed; groupBounds = $bounds.ToString()
                singleBounds = $single.Current.BoundingRectangle.ToString()
                toggleBounds = $toggle[0].Current.BoundingRectangle.ToString(); titleOffset = $offset } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "group-slot-$label.json")
            if (-not $collapsed) {
                Get-ProfileIconDigest "group-slot-$label-pane" | Should -Be $script:profileIcon
                Get-ProfileIconDigest "group-slot-$label-sibling" $script:titleB | Should -Be $script:siblingIcon
            }
        }
        Invoke-UiElement -App $script:app -Selector 'Collapse sidebar' | Out-Null
        try {
            @(Get-OwnedElements AutomationId TabGroupToggleButton) | Should -HaveCount 0
            Save-CompositorFrame group-slot-compact
            $list = @(Get-OwnedElements AutomationId ItemsList)
            $list | Should -HaveCount 1
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            $items = @($list[0].FindAll([Windows.Automation.TreeScope]::Children, $condition) |
                Where-Object { -not $_.Current.IsOffscreen })
            $items | Should -HaveCount 3
            $active = Get-ActivePane -App $script:app
            $active.session_id | Should -Be $script:a.session_id
            $bounds = $items[[int]$active.tab_id].Current.BoundingRectangle
            $size = 16 * $script:scale
            $icon = [Windows.Rect]::new($bounds.X + 6 * $script:scale, $bounds.Y + ($bounds.Height - $size) / 2, $size, $size)
            $bounds.Contains($icon) | Should -BeTrue
            Get-VisualDigest -Bounds $icon -Name compact-group-profile -Icon | Out-Null
        }
        finally { Invoke-UiElement -App $script:app -Selector 'Expand sidebar' | Out-Null }
        Wait-UiElement -App $script:app -Selector PaneActivateButton | Out-Null
        Get-ProfileIconDigest group-slot-restored-pane | Should -Be $script:profileIcon
    }

    It 'OSC progress survives context-menu moves and layout switches' -Tag 'PaneProgressLifecycle' {
        # Contract/title: above. Trigger: one OSC3, Up Up Down Down, context layout round trip.
        # Boundary: owned ConPTY -> Core -> real tab menu -> rendered row/header.
        # Oracle: unchanged pane IDs, six animated ring crops per phase, intact sibling profile icon.
        # Negative: B OSC0 has no ring; A OSC0 removes its ring. Protection: SidebarTabKeyboard.
        $release = Join-Path $script:evidence 'osc-release'
        Send-ShellScript $script:a.session_id "[Console]::Write([char]27+']9;4;3;0'+[char]7); while (-not (Test-Path '$release')) { Start-Sleep -Milliseconds 100 }; [Console]::Write([char]27+']9;4;0'+[char]7)"
        try {
            Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing | Out-Null
            Assert-ProgressFrames initial
            foreach ($move in @(@{ d = 'up'; i = 1 }, @{ d = 'up'; i = 0 }, @{ d = 'down'; i = 1 }, @{ d = 'down'; i = 2 })) {
                Move-ContextTab $move.d $move.i
                Assert-ProgressFrames "$($move.d)-$($move.i)"
                Get-ProfileIconDigest "$($move.d)-$($move.i)-profile" | Should -Be $script:profileIcon
                Get-ProfileIconDigest "$($move.d)-$($move.i)-sibling" $script:titleB | Should -Be $script:siblingIcon
            }
            foreach ($cycle in 1..2) {
                Switch-ContextLayout horizontal
                Assert-ProgressFrames "horizontal-$cycle" -Horizontal
                Switch-ContextLayout vertical
                Assert-ProgressFrames "vertical-$cycle"
                Get-ProfileIconDigest "vertical-$cycle-profile" | Should -Be $script:profileIcon
                Get-ProfileIconDigest "vertical-$cycle-sibling" $script:titleB | Should -Be $script:siblingIcon
            }
            Invoke-UiElement -App $script:app -Selector 'Collapse tab group' | Out-Null
            Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing -Gone | Out-Null
            Save-CompositorFrame group-collapsed
            @(Get-OwnedElements AutomationId HeaderIndeterminateProgressRing) | Should -HaveCount 1
            Invoke-UiElement -App $script:app -Selector 'Expand tab group' | Out-Null
            Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing | Out-Null
            Assert-ProgressFrames group-expanded
            Invoke-UiElement -App $script:app -Selector 'Collapse sidebar' | Out-Null
            Save-CompositorFrame rail-collapsed
            Invoke-UiElement -App $script:app -Selector 'Expand sidebar' | Out-Null
            Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing | Out-Null
            Assert-ProgressFrames rail-expanded
            Get-ProfileIconDigest rail-profile | Should -Be $script:profileIcon
            Get-ProfileIconDigest rail-sibling $script:titleB | Should -Be $script:siblingIcon
        }
        finally { [IO.File]::WriteAllText($release, 'clear') }
        Wait-UiElement -App $script:app -Selector PaneIndeterminateProgressRing -Gone | Out-Null
        (Get-PaneRow $script:titleA).Current.Name | Should -Not -Match 'Indeterminate progress'
    }

    It 'Move menu order follows tab layout' -Tag 'PaneProgressMenu' {
        # Contract/title: above. Trigger/boundary: physical tab right-click -> actual Move submenu.
        # Oracle: visible menu Y positions and the resulting owned tab indices in both layouts.
        # Negative: inverse direction returns the same tab, not a sibling. Protection: SidebarTelemetry.
        foreach ($layout in @('vertical', 'horizontal', 'vertical', 'horizontal', 'vertical')) {
            if ((Get-WtSetting -App $script:app -Key tabLayout) -ne $layout) { Switch-ContextLayout $layout }
            Open-TabMenu
            Invoke-UiElement -App $script:app -Selector 'Move tab' | Out-Null
            $first = if ($layout -eq 'vertical') { 'Move up' } else { 'Move right' }
            $second = if ($layout -eq 'vertical') { 'Move down' } else { 'Move left' }
            Wait-Until -TimeoutSec 10 -Because 'the current layout-specific submenu is realized, not a stale flyout peer' -Condition {
                @(Get-OwnedElements Name $first | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem }).Count -eq 1 -and
                @(Get-OwnedElements Name $second | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem }).Count -eq 1
            } | Out-Null
            $top = @(Get-OwnedElements Name $first | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem })
            $bottom = @(Get-OwnedElements Name $second | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem })
            $top | Should -HaveCount 1; $bottom | Should -HaveCount 1
            $top[0].Current.BoundingRectangle.Y | Should -BeLessThan $bottom[0].Current.BoundingRectangle.Y
            $capture = "move-menu-$layout-$([guid]::NewGuid().ToString('N'))"
            Save-CompositorFrame $capture
            @{ layout = $layout; first = $first; second = $second
                firstBounds = $top[0].Current.BoundingRectangle.ToString()
                secondBounds = $bottom[0].Current.BoundingRectangle.ToString() } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence "$capture-order.json")
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
            $back = if ($layout -eq 'vertical') { 'up' } else { 'left' }
            $forward = if ($layout -eq 'vertical') { 'down' } else { 'right' }
            Move-ContextTab $back 1
            Move-ContextTab $forward 2
        }
        Open-TabMenu
        Invoke-UiElement -App $script:app -Selector PinTabMenuItem | Out-Null
        try {
            Wait-Until -TimeoutSec 10 -Because 'only the owned tab enters the pinned group' -Condition {
                $active = Get-ActivePane -App $script:app
                $active.session_id -eq $script:a.session_id -and [int]$active.tab_id -eq 0
            } | Out-Null
            $script:tab.tab_id = 0
            Open-TabMenu
            Invoke-UiElement -App $script:app -Selector 'Move tab' | Out-Null
            Wait-UiElement -App $script:app -Selector 'Move up' | Out-Null
            foreach ($direction in @('Move up', 'Move down')) {
                $item = @(Get-OwnedElements Name $direction | Where-Object { $_.Current.ControlType -eq [Windows.Automation.ControlType]::MenuItem })
                $item | Should -HaveCount 1
                $item[0].Current.IsEnabled | Should -BeFalse -Because 'one pinned tab cannot cross its group boundaries'
            }
            Save-CompositorFrame pinned-move-boundaries
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x1B -RequireForeground | Out-Null
        }
        finally {
            Open-TabMenu
            Invoke-UiElement -App $script:app -Selector PinTabMenuItem | Out-Null
            $script:tab.tab_id = (Get-ActivePane -App $script:app).tab_id
            Move-ContextTab down 1
            Move-ContextTab down 2
        }
    }

    It 'Native prompt-mode agent identity restores the profile icon' -Tag 'PaneProgressNative' {
        # Contract/title: above; deterministic native CLI fixture, NOT a real Copilot/model claim.
        # Trigger: child shell calls shipped agent-hook start/prompt/end with inherited WT_SESSION.
        # Boundary: real native hook -> COM -> registry -> owned pane icon, with OSC3 and OSC0.
        # Oracle: pane/session-scoped Working/Ended events and profile raster restored after CLI exit.
        # Negative: sibling B stays unbranded. Protection: SessionHookRouting and HookBridgeCli.
        foreach ($state in @(3, 0)) {
            $sid = [guid]::NewGuid().ToString()
            $release = Join-Path $script:evidence "hook-release-$state"
            $done = Join-Path $script:evidence "hook-exit-$state"
            $payload = @{ session_id = $sid; cwd = $script:evidence; reason = 'user_exit' } | ConvertTo-Json -Compress
            $hook = "'$payload' | & '$($script:app.WtcliPath)' agent-hook --cli-source copilot --event"
            $listener = Start-WtEventListener -App $script:app -WaitForReady
            try {
                Send-ShellScript $script:a.session_id -ExitPath $done -Code @"
[Console]::Write([char]27+']9;4;$state'+[char]7)
$hook agent.session.start
$hook agent.prompt.submit
while (-not (Test-Path '$release')) { Start-Sleep -Milliseconds 100 }
$hook agent.session.end
"@
                foreach ($event in @('agent.session.start', 'agent.prompt.submit')) {
                    Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate {
                        $_.method -eq 'agent_event' -and $_.params.event -eq $event -and
                        $_.params.pane_id -eq $script:a.session_id -and $_.params.agent_session_id -eq $sid
                    } | Should -Not -BeNullOrEmpty
                }
                Assert-NativeStatus $sid Working
                @(Get-OwnedElements AutomationId PaneIndeterminateProgressRing).Count | Should -Be $(if ($state -eq 3) { 1 } else { 0 })
                Wait-Until -TimeoutSec 20 -Because 'Working native identity replaces the profile raster' -Condition {
                    (Get-ProfileIconDigest "working-$state") -ne $script:profileIcon
                } | Out-Null
                Get-ProfileIconDigest "working-$state-sibling" $script:titleB | Should -Be $script:siblingIcon
                [IO.File]::WriteAllText($release, 'end')
                Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.event -eq 'agent.session.end' -and
                    $_.params.pane_id -eq $script:a.session_id -and $_.params.agent_session_id -eq $sid
                } | Should -Not -BeNullOrEmpty
                Assert-NativeStatus $sid Ended
                @(Get-OwnedElements AutomationId PaneIndeterminateProgressRing).Count | Should -Be $(if ($state -eq 3) { 1 } else { 0 })
                Wait-Until -TimeoutSec 20 -Because 'fixture exit and Ended restore the complete profile icon' -Condition {
                    (Test-Path $done) -and (Get-Content -Raw $done) -eq '0' -and
                    (Get-ProfileIconDigest "ended-$state") -eq $script:profileIcon
                } | Out-Null
                Get-ProfileIconDigest "ended-$state-sibling" $script:titleB | Should -Be $script:siblingIcon
            }
            finally {
                [IO.File]::WriteAllText($release, 'end')
                @($listener.Events | Where-Object {
                    $_.method -eq 'agent_event' -and $_.params.pane_id -eq $script:a.session_id -and
                    $_.params.agent_session_id -eq $sid
                }) | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $script:evidence "native-$state-events.json")
                Stop-WtEventListener -Listener $listener
            }
        }
        @{
            fixtureTransport = $true; realLlm = $false; hookedOwner = $script:a.session_id
            provider = 'copilot'; providerIconUri = 'ms-appx:///AgentIcons/copilot.svg'
            providerIconPackaged = (Test-Path (Join-Path $script:app.InstallLocation 'AgentIcons\copilot.svg'))
            workingAndEndedObserved = $true; cliExitZero = $true; profileRestored = $true
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'native-proof.json')
    }
}
