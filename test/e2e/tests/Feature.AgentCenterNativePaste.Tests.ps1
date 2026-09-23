#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Round9: OS clipboard -> foreground Ctrl+Shift+V -> ordinary TermControl ->
# ConPTY bracketed paste -> deployed wta ui native input -> exact rendered draft.
# This deliberately does NOT claim the dedicated Console's shell-key routing.
# Empty capabilities and unknown slash commands prohibit model/work execution.

Describe 'Feature: Agent Center native paste' -Tag 'Feature', 'AgentCenterNativePaste' `
    -Skip:($env:ITE2E_PACKAGE -in @('Store', 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe')) {
    BeforeAll {
        $script:app = $null
        $script:pane = $null
        $script:root = $null
        $script:settingsHashes = $null
        $script:ownedProcesses = @()
        $script:nativeInputSent = $false
        $script:clipboardSaved = $false
        . (Join-Path $PSScriptRoot 'helpers\AgentCenterDraft.ps1')
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'Native paste proof requires ITE2E_PACKAGE=Dev.' }
        if ($env:ITE2E_EXPECTED_WTA_SHA256 -notmatch '^[0-9a-fA-F]{64}$') {
            throw 'Set ITE2E_EXPECTED_WTA_SHA256 to the independently verified feature-build hash.'
        }
        $script:target = Resolve-ItApp -Package Dev
        $hash = (Get-FileHash -LiteralPath $script:target.WtaPath -Algorithm SHA256).Hash
        $hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        # Start-Terminal always cold-starts. Refuse its destructive preflight rather
        # than treating an existing user's host/backup as a stale test instance.
        if (@(Get-WtProcessesForApp -App $script:target).Count) {
            throw 'Close Dev hosts yourself before this suite; no existing host will be stopped.'
        }
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "A previous test owns a configuration backup: $path"
            }
        }
        $script:root = [IO.Path]::GetFullPath((Join-Path $(if ($env:ITE2E_ARTIFACT_ROOT) {
            $env:ITE2E_ARTIFACT_ROOT
        } else { Join-Path $PSScriptRoot '..\artifacts' }) "agent-center-native-paste-$([guid]::NewGuid().ToString('N'))"))
        New-Item -ItemType Directory -Path $script:root -Force | Out-Null
        $script:settingsHashes = @{}
        foreach ($path in @($script:target.SettingsPath, $script:target.StatePath)) {
            $script:settingsHashes[$path] = if (Test-Path -LiteralPath $path) {
                (Get-FileHash -LiteralPath $path).Hash
            } else { 'missing' }
        }
        $script:settingsHashes | ConvertTo-Json |
            Set-Content -LiteralPath (Join-Path $script:root 'settings-before.json') -Encoding utf8NoBOM
        @{
            package = $script:target.Package; version = $script:target.Version
            binary = $script:target.WtaPath; sha256 = $hash
            boundary = 'ordinary TermControl; dedicated Console routing NOT exercised'
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'package.json') -Encoding utf8NoBOM
        $script:originalClipboard = Get-ClipboardSnapshot
        $script:clipboardSaved = $true
        $fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Start-AgentCenterNativePaste.ps1')).Path
        $invocation = "& '$($fixture.Replace("'", "''"))' -WtaPath '$($script:target.WtaPath.Replace("'", "''"))' -EvidenceDirectory '$($script:root.Replace("'", "''"))' -ExpectedSha256 '$hash'"
        $command = "pwsh.exe -NoLogo -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
        $profile = '{e205fd51-cb24-4147-94b5-bd487857c70c}'
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            defaultProfile = $profile
            profiles = @{ defaults = @{}; list = @(@{
                guid = $profile; name = 'ItE2E native paste'; commandline = $command
                startingDirectory = $script:root; closeOnExit = 'never'
            }) }
            startupActions = ''
            firstWindowPreference = 'defaultProfile'
            initialCols = 120
            initialRows = 40
            acpAgent = 'custom:native-paste-no-provider'
            acpCustomCommand = 'cmd.exe /d /c exit 0'
            acpModel = ''
            autoErrorDetectionEnabled = $false
            autoFixEnabled = $false
            agentSessionManagementEnabled = $false
            'warning.confirmOnClose' = 'never'
            multiLinePasteWarning = $false
            actions = @(@{ command = 'paste'; keys = 'ctrl+shift+v' })
        }
        $script:ownedProcesses = @(
            (@($script:app.Pid) + @(Get-DescendantWtaIds -RootPid $script:app.Pid)) |
                Select-Object -Unique | ForEach-Object {
                    $process = Get-Process -Id $_ -ErrorAction SilentlyContinue
                    if ($process) { @{ pid = $process.Id; started = $process.StartTime.ToUniversalTime().ToString('o') } }
                }
        )
        $script:pane = Get-ActivePane -App $script:app
        @{
            pid = $script:app.Pid; hwnd = $script:app.Hwnd
            paneSessionId = $script:pane.session_id; windowId = $script:app.WindowId
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'window.json') -Encoding utf8NoBOM
        $script:sequence = 0
        $script:explicitEnterSent = $false
        $script:prematureDispatch = $false
        function Read-NativePasteFrame {
            $text = Get-WtCapture -App $script:app -SessionId $script:pane.session_id
            if (-not $script:explicitEnterSent -and $text -match 'METHOD_UNSUPPORTED') {
                $script:prematureDispatch = $true
            }
            $script:sequence++
            Set-Content -LiteralPath (Join-Path $script:root ('frame-{0:D4}.txt' -f $script:sequence)) `
                -Value $text -NoNewline -Encoding utf8NoBOM
            return $text
        }
        function Send-NativePasteKey([int]$Vk, [switch]$Ctrl, [switch]$Shift) {
            Send-WtWindowKey -App $script:app -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift -RequireForeground | Out-Null
            $script:nativeInputSent = $true
        }
        function Assert-NativePasteDraft([string[]]$Expected, [switch]$AllowError) {
            $observed = Wait-Until -TimeoutSec 10 -IntervalSec 0.2 -Because 'exact Agent Center draft rows' -Condition {
                $frame = Read-NativePasteFrame
                if ($script:prematureDispatch -or (-not $AllowError -and $frame -match 'METHOD_UNSUPPORTED')) {
                    throw 'Paste dispatched an unknown slash command before physical Enter.'
                }
                $rows = Get-NativePasteDraftRows $frame
                if ($rows.Count -lt $Expected.Count) { return $false }
                for ($i = 0; $i -lt $Expected.Count; $i++) {
                    if ($rows[$i] -cne $Expected[$i]) { return $false }
                }
                for ($i = $Expected.Count; $i -lt $rows.Count; $i++) {
                    if ($rows[$i] -cne '') { return $false }
                }
                return $true
            }
            $observed | Should -BeTrue
        }
        function Invoke-NativeClipboardPaste([string]$Text) {
            Set-Clipboard -Value $Text
            (Get-Clipboard -Raw) | Should -BeExactly $Text
            Send-NativePasteKey 0x56 -Ctrl -Shift
        }
        Wait-Until -TimeoutSec 40 -Because 'live isolated Agent Center input and service identity' -Condition {
            if (-not (Test-Path -LiteralPath (Join-Path $script:root 'runtime.json'))) { return $false }
            Get-NativePasteDraftRows -Frame (Read-NativePasteFrame) | Out-Null
            return $true
        } | Should -BeTrue
        $runtime = Get-Content -LiteralPath (Join-Path $script:root 'runtime.json') -Raw | ConvertFrom-Json
        foreach ($ownedId in @($runtime.shellPid, $runtime.servicePid)) {
            $process = Get-Process -Id $ownedId -ErrorAction Stop
            $script:ownedProcesses += @{ pid = $process.Id; started = $process.StartTime.ToUniversalTime().ToString('o') }
        }
        $runtime.sha256 | Should -Be $hash
        $runtime.providerCapabilities | Should -BeNullOrEmpty
        $before = Get-Content -LiteralPath (Join-Path $script:root 'work-list-before.json') -Raw | ConvertFrom-Json
        $before.status | Should -Be 'ok'
        $before.data.items | Should -BeNullOrEmpty
        $service = Get-CimInstance Win32_Process -Filter "ProcessId=$($runtime.servicePid)"
        $service.ExecutablePath | Should -Be $script:target.WtaPath
        $service.CommandLine | Should -Match 'center\s+serve'
        Set-WtPaneFocus -App $script:app -SessionId $script:pane.session_id
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) {
            throw 'An unlocked foreground desktop is required; no native input was sent.'
        }
        Assert-NativePasteDraft @('')
    }

    BeforeEach {
        Send-NativePasteKey 0x41 -Ctrl
        Send-NativePasteKey 0x08
        Assert-NativePasteDraft @('')
    }

    It 'Agent Center native paste preserves Unicode and selection replacement' {
        # ASCII/BMP negative control, exact round9 surrogate repro, then joined emoji.
        foreach ($text in @('/round9-ascii-123', "/round9-$([char]0x4E2D)$([char]0x00E9)",
            ('a' + [char]::ConvertFromUtf32(0x1F469) + 'z'),
            ('/round9-' + [char]::ConvertFromUtf32(0x1F469) + [char]0x200D + [char]::ConvertFromUtf32(0x1F4BB)))) {
            Invoke-NativeClipboardPaste $text
            Assert-NativePasteDraft @($text)
            Send-NativePasteKey 0x41 -Ctrl
            Send-NativePasteKey 0x08
            Assert-NativePasteDraft @('')
        }
        Invoke-NativeClipboardPaste '/round9-ab'
        Assert-NativePasteDraft @('/round9-ab')
        Send-NativePasteKey 0x25 -Shift
        Send-NativePasteKey 0x25 -Shift
        $replacement = [char]::ConvertFromUtf32(0x1F469) + [char]0x4E2D
        Invoke-NativeClipboardPaste $replacement
        Assert-NativePasteDraft @(('/round9-' + $replacement))
        Send-NativePasteKey 0x41 -Ctrl
        Invoke-NativeClipboardPaste '/round9-replaced'
        Assert-NativePasteDraft @('/round9-replaced')
    }

    It 'Agent Center multiline native paste waits for explicit Enter' {
        $first = '/round9-invalid-one'
        $second = '/round9-invalid-two'
        Invoke-NativeClipboardPaste ($first + "`r`n" + $second)
        Assert-NativePasteDraft @($first, $second)
        # Positive delivery first, then sustained negative observation (redraw/replay).
        foreach ($sample in 1..8) {
            Start-Sleep -Milliseconds 250
            Assert-NativePasteDraft @($first, $second)
        }
        Send-NativePasteKey 0x41 -Ctrl
        Invoke-NativeClipboardPaste '/round10-shift-first'
        Assert-NativePasteDraft @('/round10-shift-first')
        Send-NativePasteKey 0x0D -Shift
        Assert-NativePasteDraft @('/round10-shift-first', '')
        Invoke-NativeClipboardPaste '/round10-shift-second'
        Assert-NativePasteDraft @('/round10-shift-first', '/round10-shift-second')
        Send-NativePasteKey 0x41 -Ctrl
        Invoke-NativeClipboardPaste '/round9-explicit-enter'
        Assert-NativePasteDraft @('/round9-explicit-enter')
        $script:explicitEnterSent = $true
        Send-NativePasteKey 0x0D
        Wait-Until -TimeoutSec 10 -Because 'physical Enter dispatches exactly the unknown-command control' -Condition {
            (Read-NativePasteFrame) -match 'METHOD_UNSUPPORTED'
        } | Should -BeTrue
        Assert-NativePasteDraft @('/round9-explicit-enter') -AllowError
    }

    AfterAll {
        $cleanupErrors = [Collections.Generic.List[string]]::new()
        $cleanup = @{
            nativeInputSent = [bool]$script:nativeInputSent
            keyboardExitAttempted = $false
            terminalStopped = $false
            clipboardRestored = $false
            settingsRestored = $false
            ownedProcessesStopped = $false
        }
        try {
            if ($script:app) {
                # Pre-input failures must never synthesize a cleanup key. Lost
                # foreground after a test likewise falls back to owned-process teardown.
                if ($script:nativeInputSent -and $script:pane -and (Test-WtWindowKeyFocusable -App $script:app)) {
                    Save-UiScreenshot -App $script:app -Path (Join-Path $script:root 'final.png') | Out-Null
                    $cleanup.keyboardExitAttempted = $true
                    Send-NativePasteKey 0x43 -Ctrl
                    Wait-Until -TimeoutSec 10 -Because 'fixture records final read-only work list' -Condition {
                        Test-Path -LiteralPath (Join-Path $script:root 'work-list-after.json')
                    } | Should -BeTrue
                    $after = Get-Content -LiteralPath (Join-Path $script:root 'work-list-after.json') -Raw | ConvertFrom-Json
                    $after.status | Should -Be 'ok'
                    $after.data.items | Should -BeNullOrEmpty -Because 'clipboard editing must create no work'
                }
            }
        }
        catch { $cleanupErrors.Add($_.Exception.Message) }
        finally {
            try {
                if ($script:app) {
                    Stop-Terminal -App $script:app
                    $cleanup.terminalStopped = $true
                }
            }
            catch { $cleanupErrors.Add($_.Exception.Message) }
            finally {
                try {
                    if ($script:clipboardSaved) {
                        Restore-ClipboardSnapshot -Snapshot $script:originalClipboard
                        $cleanup.clipboardRestored = $true
                    }
                }
                catch { $cleanupErrors.Add($_.Exception.Message) }
            }
        }
        if ($script:settingsHashes) {
            $cleanup.settingsRestored = $true
            foreach ($path in $script:settingsHashes.Keys) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { 'missing' }
                if ($actual -ne $script:settingsHashes[$path]) {
                    $cleanup.settingsRestored = $false
                    $cleanupErrors.Add("Settings/state were not restored byte-for-byte: $path")
                }
            }
        }
        $alive = @($script:ownedProcesses | Where-Object {
            $current = Get-Process -Id $_.pid -ErrorAction SilentlyContinue
            $current -and $current.StartTime.ToUniversalTime().ToString('o') -eq $_.started
        } | ForEach-Object { $_.pid } | Select-Object -Unique)
        $cleanup.ownedProcessesStopped = $alive.Count -eq 0
        if ($alive.Count) { $cleanupErrors.Add("Owned fixture process(es) survived teardown: $($alive -join ', ')") }
        $cleanup.errors = @($cleanupErrors.ToArray())
        $cleanup.ownedProcesses = $script:ownedProcesses
        if ($script:root) {
            $cleanup | ConvertTo-Json -Depth 5 |
                Set-Content -LiteralPath (Join-Path $script:root 'cleanup.json') -Encoding utf8NoBOM
        }
        if ($cleanupErrors.Count) { throw ($cleanupErrors -join '; ') }
    }
}
