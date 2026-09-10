#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Physical keys -> TerminalControl/ConPTY -> editable WTA draft -> exact OS clipboard text.
# Mock ACP supplies readiness and submission/history boundaries without model requests.
# AgentSelectAll and AgentInputNavigation retain the broader focus, selection and row protections.

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-Command Get-AppxPackage -ErrorAction SilentlyContinue) -and
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command pwsh -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent input undo and redo' -Tag 'Feature', 'AgentInputUndoRedo' -Skip:(-not $script:Ready) {
    BeforeAll {
        . (Join-Path $PSScriptRoot 'helpers\TestWindowKeyboardLayout.ps1')
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = $null
        $script:keyboardLayout = $null
        $script:clipboardSaved = $false
        $script:fixtureDir = $null
        $script:configBefore = @{}
        $script:cleanup = [ordered]@{
            KeyboardLayoutRestored = $false
            TerminalStopped = $false
            ConfigRestored = $false
            ClipboardRestored = $false
            FixtureRemoved = $false
        }
        $artifactRoot = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT }
        else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:evidenceDir = Join-Path ([IO.Path]::GetFullPath($artifactRoot)) "agent-input-undo-redo\$([guid]::NewGuid().ToString('N'))"
        $script:fixtureDir = Join-Path $script:evidenceDir 'fixture'
        New-Item -ItemType Directory -Force -Path $script:fixtureDir | Out-Null
        $script:fixtureLog = Join-Path $script:evidenceDir 'fixture.log'
        $script:evidenceIndex = 0
        $package = Get-ItTestPackage
        $script:targetApp = Resolve-ItApp -Package $package
        $binaryHash = (Get-FileHash -LiteralPath $script:targetApp.WtaPath -Algorithm SHA256).Hash
        # PR validation supplies the intended source-built hash. Intentional production/baseline
        # runs may omit it; their results describe the recorded package, not the current checkout.
        if ($env:ITE2E_EXPECTED_WTA_SHA256) {
            $binaryHash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256 -Because 'the deployed WTA must match the explicitly selected revision'
        }
        @{
            Package = $script:targetApp.Package
            PackageFullName = $script:targetApp.PackageFullName
            InstallLocation = $script:targetApp.InstallLocation
            WtaPath = $script:targetApp.WtaPath
            WtaSha256 = $binaryHash
            StartedUtc = [DateTime]::UtcNow.ToString('o')
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'package.json') -Encoding utf8NoBOM

        # Require an unused target so a failed Start-Terminal can recover only this run's process/config.
        @(Get-WtProcessesForApp -App $script:targetApp).Count |
            Should -Be 0 -Because 'the selected package must have no user-owned window before this physical suite'
        foreach ($path in @($script:targetApp.SettingsPath, $script:targetApp.StatePath)) {
            if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                throw "Recover the prior run's configuration backup before starting: $path.e2ebak"
            }
            $script:configBefore[$path] = if (Test-Path -LiteralPath $path) {
                (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
            }
            else { $null }
        }
        foreach ($hive in @('HKLM:', 'HKCU:')) {
            $policyPath = "$hive\SOFTWARE\Policies\Microsoft\IntelligentTerminal"
            $allowCustom = if (Test-Path -LiteralPath $policyPath) {
                $property = (Get-ItemProperty -LiteralPath $policyPath -ErrorAction Stop).PSObject.Properties['AllowCustomAgents']
                if ($property) { $property.Value }
            }
            else { $null }
            if ($null -ne $allowCustom) {
                if ($allowCustom -eq 0) { throw "Prerequisite blocked: $policyPath denies custom agents; policy is not changed by this suite." }
                break
            }
        }
        $fixture = Join-Path $script:fixtureDir 'Mock ACP Chat Agent.ps1'
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot '..\fixtures\Mock-AcpChatAgent.ps1') -Destination $fixture
        $invocation = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:fixtureLog.Replace("'", "''"))'"
        $command = "pwsh -NoProfile -EncodedCommand $([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation)))"
        $script:originalClipboard = Get-ClipboardSnapshot
        $script:clipboardSaved = $true
        $launchStarted = Get-Date
        try {
            $script:app = Start-Terminal -Package $package -PassFre $true -Settings @{
                acpAgent = 'custom:input-undo-redo-fixture'
                acpCustomCommand = $command
                rightClickContextMenu = $false
                'warning.confirmOnClose' = 'never'
            }
        }
        catch {
            $startupFailure = $_
            $candidates = @(Get-WtProcessesForApp -App $script:targetApp)
            foreach ($process in $candidates) {
                if ($process.Path -ine $script:targetApp.WindowsTerminal -or $process.StartTime -lt $launchStarted) {
                    throw 'Startup recovery cannot establish a test-owned PID; configuration backups are preserved.'
                }
                $recovery = $script:targetApp.PSObject.Copy()
                $recovery.Pid = $process.Id
                $recovery | Add-Member -NotePropertyName Launched -NotePropertyValue $true -Force
                Stop-Terminal -App $recovery -RestoreSettings $false
            }
            if (@(Get-WtProcessesForApp -App $script:targetApp).Count) {
                throw 'Startup recovery did not stop the exact test process; configuration backups are preserved.'
            }
            Restore-WtConfig -App $script:targetApp
            throw $startupFailure
        }
        $shell = Get-ActivePane -App $script:app
        $script:ownerTabId = Resolve-AgentOwnerTabId -App $script:app -OwnerPaneSessionId $shell.session_id
        Open-AgentPane -App $script:app | Out-Null
        $script:agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shell.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $script:agentPane -TimeoutSec 60 |
            Should -BeTrue -Because 'the deterministic ACP fixture must connect before undo/redo assertions'
        $script:keyboardLayout = Enable-TestWindowEnglishKeyboardLayout -App $script:app
        $script:readyPattern = Get-WtaLocalizedTextRegex -Key 'input.placeholder.connected'
        if (-not $script:readyPattern) { $script:readyPattern = '(?i)Ask anything.*for commands' }
        @{
            Pid = $script:app.Pid
            Hwnd = $script:app.Hwnd
            WindowId = $script:app.WindowId
            PaneSessionId = $script:agentPane
            OwnerTabId = $script:ownerTabId
            PreviousKeyboardLayout = ('0x{0:X}' -f $script:keyboardLayout.PreviousLayout.ToInt64())
            TestKeyboardLayout = ('0x{0:X}' -f $script:keyboardLayout.TestLayout.ToInt64())
            KeyboardLayoutChanged = $script:keyboardLayout.Changed
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidenceDir 'target.json') -Encoding utf8NoBOM

        $script:sendKey = {
            param([int]$Vk, [switch]$Ctrl, [int]$Repeat = 1)
            [uint32]$windowPid = 0
            [void][ItE2E.TestWindowKeyboardLayout]::GetWindowThreadProcessId([IntPtr][int64]$script:app.Hwnd, [ref]$windowPid)
            if ($windowPid -ne $script:app.Pid) { throw 'Physical-key target no longer belongs to the test-owned process.' }
            Send-WtWindowKey -App $script:app -Vk $Vk -Ctrl:$Ctrl -Repeat $Repeat -RequireForeground | Out-Null
        }
        $script:capture = {
            Get-AgentPaneText -App $script:app -PaneSessionId $script:agentPane -MaxLines 500
        }
        $script:saveEvidence = {
            param([string]$Name)
            $script:evidenceIndex++
            $prefix = Join-Path $script:evidenceDir ('{0:D3}-{1}' -f $script:evidenceIndex, $Name)
            Set-Content -LiteralPath "$prefix.txt" -Value (& $script:capture) -NoNewline -Encoding utf8NoBOM
            Save-UiScreenshot -App $script:app -Path "$prefix.png" | Out-Null
        }
        $script:assertEmpty = {
            Test-Until -TimeoutSec 5 -IntervalSec 0.2 -Condition {
                (& $script:capture) -match ('(?m)^\s*[│║|]\s*>\s*' + $script:readyPattern + '[.…]*\s*[│║|]\s*$')
            } | Should -BeTrue -Because 'the editable draft must show the empty connected placeholder'
        }
        $script:clearDraft = {
            & $script:sendKey -Vk 0x1B
            & $script:sendKey -Vk 0x1B
            & $script:assertEmpty
        }
        $script:pasteText = {
            param([string]$Text)
            Set-Clipboard -Value $Text
            $listener = Start-WtEventListener -App $script:app -WaitForReady
            try {
                & $script:sendKey -Vk 0x56 -Ctrl
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 5 -Predicate {
                    $_.method -eq 'agent_paste_text' -and
                    "$($_.params.tab_id)".Trim('{}') -eq "$($script:ownerTabId)".Trim('{}') -and
                    "$($_.params.pane_id)".Trim('{}') -eq "$($script:agentPane)".Trim('{}') -and
                    "$($_.params.window_id)" -eq "$($script:app.WindowId)"
                }
                $event | Should -Not -BeNullOrEmpty -Because 'physical Ctrl+V must reach the subscribed owner-scoped paste path'
                $tail = @($Text -split "`r?`n" | Where-Object Length)[-1]
                $script:pasteTail = $tail.Substring([Math]::Max(0, $tail.Length - 8))
                Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                    (& $script:capture) -match [regex]::Escape($script:pasteTail)
                } | Should -BeTrue -Because 'pasted source must render before the next edit'
            }
            finally { Stop-WtEventListener -Listener $listener }
        }
        $script:copyDraft = {
            param([string]$Name, [switch]$SelectionOnly)
            $script:copySentinel = "UNDO_CLIPBOARD_$([guid]::NewGuid().ToString('N'))"
            Set-Clipboard -Value $script:copySentinel
            if (-not $SelectionOnly) { & $script:sendKey -Vk 0x41 -Ctrl }
            & $script:sendKey -Vk 0x43 -Ctrl
            $copied = Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Quiet -Condition {
                $text = Get-Clipboard -Raw
                if ($text -cne $script:copySentinel) { $text }
            }
            Set-Content -LiteralPath (Join-Path $script:evidenceDir "$Name-clipboard.txt") -Value $copied -NoNewline -Encoding utf8NoBOM
            & $script:saveEvidence -Name $Name
            $copied | Should -Not -BeNullOrEmpty -Because 'physical copy must replace the sentinel with actual selected source'
            $copied
        }
        $script:assertText = {
            param([AllowEmptyString()][string]$Expected, [string]$Name)
            if ($Expected.Length) {
                (& $script:copyDraft -Name $Name) | Should -BeExactly $Expected
            }
            else {
                & $script:assertEmpty
                & $script:saveEvidence -Name $Name
            }
        }
        $script:promptCount = {
            @(Get-Content -LiteralPath $script:fixtureLog -ErrorAction SilentlyContinue |
                Where-Object { $_ -match '\|prompt\|' }).Count
        }
    }

    BeforeEach {
        Invoke-WtCli -App $script:app -Arguments @('focus-pane', '-t', $script:agentPane) | Out-Null
        Test-WtWindowKeyFocusable -App $script:app |
            Should -BeTrue -Because 'physical input requires an unlocked foreground desktop'
        # Clearing may itself be undoable: each case seeds identifiable new transactions
        # and never assumes the preceding case left an empty undo stack.
        & $script:clearDraft
    }

    AfterEach {
        if ($script:app -and $script:saveEvidence) { & $script:saveEvidence -Name 'case-final' }
    }

    AfterAll {
        try {
            try {
                if ($script:keyboardLayout) {
                    Restore-TestWindowKeyboardLayout -App $script:app -Context $script:keyboardLayout
                    $script:cleanup.KeyboardLayoutRestored = $true
                }
            }
            finally {
                if ($script:app) {
                    Stop-Terminal -App $script:app
                    $script:cleanup.TerminalStopped = $null -eq (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue)
                    $script:cleanup.TerminalStopped | Should -BeTrue
                }
                foreach ($path in $script:configBefore.Keys) {
                    $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
                    else { $null }
                    $actual | Should -Be $script:configBefore[$path] -Because 'settings/state must be restored byte-for-byte'
                    (Test-Path "$path.e2ebak") | Should -BeFalse
                    (Test-Path "$path.e2ebak.missing") | Should -BeFalse
                }
                $script:cleanup.ConfigRestored = $script:configBefore.Count -eq 2
            }
        }
        finally {
            try {
                if ($script:clipboardSaved) {
                    Restore-ClipboardSnapshot -Snapshot $script:originalClipboard
                    $script:cleanup.ClipboardRestored = $true
                }
            }
            finally {
                try {
                    if ($script:fixtureDir -and (Test-Path -LiteralPath $script:fixtureDir)) {
                        Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
                        $script:cleanup.FixtureRemoved = $true
                    }
                }
                finally {
                    $script:cleanup | ConvertTo-Json |
                        Set-Content -LiteralPath (Join-Path $script:evidenceDir 'cleanup.json') -Encoding utf8NoBOM
                }
            }
        }
    }

    It 'Agent draft undo and redo groups contiguous typing' -Tag 'AgentInputUndoRedoTyping' {
        $countBefore = & $script:promptCount
        $prefix = "GROUP_$([guid]::NewGuid().ToString('N').Substring(0, 8))_"
        $typed = 'q' + 'w' + 'e'
        & $script:pasteText -Text $prefix
        & $script:sendKey -Vk 0x51
        & $script:sendKey -Vk 0x57
        & $script:sendKey -Vk 0x45
        (& $script:copyDraft -Name 'grouped-before-undo') |
            Should -BeExactly "$prefix$typed" -Because 'setup must prove the physical suffix reached the editable draft'
        & $script:sendKey -Vk 0x27
        & $script:sendKey -Vk 0x5A -Ctrl
        (& $script:copyDraft -Name 'grouped-after-undo') |
            Should -BeExactly $prefix -Because 'one physical Ctrl+Z must remove the entire contiguous typed suffix while retaining the separately pasted prefix'
        & $script:sendKey -Vk 0x59 -Ctrl
        (& $script:copyDraft -Name 'grouped-after-redo') |
            Should -BeExactly "$prefix$typed" -Because 'physical Ctrl+Y must restore the same complete typing group'
        (& $script:promptCount) | Should -Be $countBefore
    }

    It 'Agent draft undo and redo restores atomic editing transactions' -Tag 'AgentInputUndoRedoTransactions' {
        foreach ($action in @('typing', 'paste', 'cut', 'backspace', 'delete')) {
            & $script:clearDraft
            $draft = "ATOMIC_${action}_$([guid]::NewGuid().ToString('N').Substring(0, 8))"
            & $script:pasteText -Text $draft
            & $script:sendKey -Vk 0x41 -Ctrl
            & $script:saveEvidence -Name "$action-selected"
            $replacement = ''
            switch ($action) {
                'typing' { & $script:sendKey -Vk 0x51; $replacement = 'q' }
                'paste' { $replacement = "REPLACED_$action"; & $script:pasteText -Text $replacement }
                'cut' {
                    Set-Clipboard -Value 'CUT_SENTINEL'
                    & $script:sendKey -Vk 0x58 -Ctrl
                    $cut = Wait-Until -TimeoutSec 5 -IntervalSec 0.2 -Quiet -Condition {
                        $text = Get-Clipboard -Raw
                        if ($text -cne 'CUT_SENTINEL') { $text }
                    }
                    $cut | Should -BeExactly $draft
                }
                'backspace' { & $script:sendKey -Vk 0x08 }
                'delete' { & $script:sendKey -Vk 0x2E }
            }
            & $script:assertText -Expected $replacement -Name "$action-edited"
            & $script:sendKey -Vk 0x5A -Ctrl
            (& $script:copyDraft -Name "$action-undone" -SelectionOnly) |
                Should -BeExactly $draft -Because 'undo must restore both the complete source and its pre-edit whole-draft selection'
            & $script:sendKey -Vk 0x59 -Ctrl
            & $script:assertText -Expected $replacement -Name "$action-redone"
        }
        foreach ($action in @('escape', 'ctrl-c')) {
            & $script:clearDraft
            $draft = "IDLE_CLEAR_$action"
            & $script:pasteText -Text $draft
            if ($action -eq 'escape') { & $script:sendKey -Vk 0x1B }
            else { & $script:sendKey -Vk 0x43 -Ctrl }
            & $script:assertEmpty
            & $script:sendKey -Vk 0x5A -Ctrl
            & $script:assertText -Expected $draft -Name "$action-undone"
            & $script:sendKey -Vk 0x59 -Ctrl
            & $script:assertEmpty
        }
    }

    It 'Agent draft undo and redo preserves multiline Unicode source' -Tag 'AgentInputUndoRedoUnicode' {
        $prefix = "UNICODE_$([guid]::NewGuid().ToString('N').Substring(0, 8))_"
        $addition = @('é中😀', 'naïve café', '尾行終') -join "`n"
        $draft = $prefix + $addition
        & $script:pasteText -Text $prefix
        & $script:pasteText -Text $addition
        & $script:assertText -Expected $draft -Name 'unicode-before-undo'
        & $script:sendKey -Vk 0x5A -Ctrl
        & $script:assertText -Expected $prefix -Name 'unicode-paste-undone'
        & $script:sendKey -Vk 0x59 -Ctrl
        & $script:saveEvidence -Name 'unicode-paste-redone'
        & $script:sendKey -Vk 0x25
        & $script:sendKey -Vk 0x51
        $edited = $draft.Insert($draft.Length - 1, 'q')
        & $script:assertText -Expected $edited -Name 'unicode-caret-edited'
        & $script:sendKey -Vk 0x5A -Ctrl
        & $script:assertText -Expected $draft -Name 'unicode-edit-undone'
        & $script:sendKey -Vk 0x59 -Ctrl
        & $script:assertText -Expected $edited -Name 'unicode-edit-redone'
    }

    It 'Agent draft edits invalidate redo but copy and caret movement do not' -Tag 'AgentInputUndoRedoBranches' {
        foreach ($separator in @('copy', 'caret')) {
            & $script:clearDraft
            $prefix = "BRANCH_${separator}_"
            $firstGroup = 'q' + 'w'
            $secondGroup = 'e' + 'r'
            $branched = $firstGroup + 't'
            & $script:pasteText -Text $prefix
            & $script:sendKey -Vk 0x51
            & $script:sendKey -Vk 0x57
            if ($separator -eq 'copy') {
                & $script:assertText -Expected "$prefix$firstGroup" -Name "$separator-splits-typing"
                & $script:sendKey -Vk 0x27
            }
            else {
                & $script:sendKey -Vk 0x25
                & $script:sendKey -Vk 0x27
            }
            & $script:sendKey -Vk 0x45
            & $script:sendKey -Vk 0x52
            & $script:sendKey -Vk 0x5A -Ctrl
            & $script:assertText -Expected "$prefix$firstGroup" -Name "$separator-group-undone"
            & $script:sendKey -Vk 0x25
            & $script:sendKey -Vk 0x27
            & $script:sendKey -Vk 0x59 -Ctrl
            & $script:assertText -Expected "$prefix$firstGroup$secondGroup" -Name "$separator-redo-preserved"
            & $script:sendKey -Vk 0x5A -Ctrl
            & $script:sendKey -Vk 0x54
            & $script:assertText -Expected "$prefix$branched" -Name "$separator-new-branch"
            & $script:sendKey -Vk 0x59 -Ctrl
            & $script:assertText -Expected "$prefix$branched" -Name "$separator-redo-invalidated"
            & $script:sendKey -Vk 0x5A -Ctrl
            & $script:assertText -Expected "$prefix$firstGroup" -Name "$separator-branch-undone"
            & $script:sendKey -Vk 0x5A -Ctrl
            & $script:assertText -Expected $prefix -Name "$separator-original-group-undone"
        }
    }

    It 'Agent draft undo respects submission and prompt history boundaries' -Tag 'AgentInputUndoRedoHistory' {
        $marker = "SCROLL_TURN_00_$([guid]::NewGuid().ToString('N'))"
        $countBefore = & $script:promptCount
        & $script:pasteText -Text $marker
        & $script:sendKey -Vk 0x51
        & $script:assertText -Expected "${marker}q" -Name 'history-before-submit'
        & $script:sendKey -Vk 0x27
        & $script:sendKey -Vk 0x0D
        $script:ackMarker = "ACK_$marker"
        Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
            (& $script:capture) -match [regex]::Escape($script:ackMarker)
        } | Should -BeTrue -Because 'physical Enter must complete a real deterministic ACP turn'
        & $script:assertEmpty
        (& $script:promptCount) | Should -Be ($countBefore + 1)
        & $script:sendKey -Vk 0x5A -Ctrl
        & $script:assertEmpty
        & $script:sendKey -Vk 0x59 -Ctrl
        & $script:assertEmpty

        $prefix = 'HISTORY_DRAFT_'
        & $script:pasteText -Text $prefix
        & $script:sendKey -Vk 0x57
        & $script:sendKey -Vk 0x45
        & $script:sendKey -Vk 0x26
        & $script:assertText -Expected "${marker}q" -Name 'history-recalled'
        & $script:sendKey -Vk 0x27
        & $script:sendKey -Vk 0x28
        & $script:assertText -Expected "${prefix}we" -Name 'history-original-draft'
        & $script:sendKey -Vk 0x5A -Ctrl
        & $script:assertText -Expected $prefix -Name 'history-original-undo-chain'
        & $script:sendKey -Vk 0x59 -Ctrl
        & $script:assertText -Expected "${prefix}we" -Name 'history-original-redo-chain'

        & $script:sendKey -Vk 0x27
        & $script:sendKey -Vk 0x26
        & $script:sendKey -Vk 0x52
        & $script:assertText -Expected "${marker}qr" -Name 'history-recall-edited'
        & $script:sendKey -Vk 0x5A -Ctrl
        & $script:assertText -Expected "${marker}q" -Name 'history-recall-edit-undone'
        & $script:sendKey -Vk 0x5A -Ctrl
        & $script:assertText -Expected "${marker}q" -Name 'history-recall-root'
        & $script:sendKey -Vk 0x59 -Ctrl
        & $script:assertText -Expected "${marker}qr" -Name 'history-recall-redone'
        (& $script:promptCount) | Should -Be ($countBefore + 1) -Because 'undo/redo and history editing must not replay submitted agent actions'
    }
}
