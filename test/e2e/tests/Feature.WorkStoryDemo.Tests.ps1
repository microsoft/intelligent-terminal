#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# C334-C344 now qualify the integrated native Console, not the retired shell storyboard.
# Boundary: isolated per-window environment -> native wta ui -> shared State/renderer
# -> native composer/navigation -> persisted Scenario and HWND-local UIA projection.
# No shell tab, pane GUID, COM send-keys or production work-service/provider fallback.
# Matrix: four interactive legacy histories; natural resume/branch and F1/F2/F5/F6
# routes; explicit cap selection and autonomous attention/idle budget progression;
# rejected inputs, replay, restart and isolated reset. PNGs are diagnostic, not pixel proof.
# A disposable child attaches ONLY to the new demo
# wta ui console and submits INPUT_RECORDs; this is not physical-keyboard proof.

Describe 'Feature: integrated native work story demo' -Tag 'Feature', 'WorkStoryDemo', 'NativeWorkStoryDemo' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\NativeWorkStoryDemo.ps1')
        if ($env:ITE2E_PACKAGE -cne 'Dev') { throw 'Native work-story qualification requires explicit ITE2E_PACKAGE=Dev.' }
        if ($env:ITE2E_EXPECTED_WTA_SHA256 -notmatch '^[a-fA-F0-9]{64}$') {
            throw 'Pin ITE2E_EXPECTED_WTA_SHA256 to the independently built native-integration binary.'
        }
        $base = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:root = [IO.Path]::GetFullPath((Join-Path $base ('native-work-story-' + [guid]::NewGuid().ToString('N'))))
        $script:stateDirectory = Join-Path $script:root 'state'
        $script:demo = @{}
        $script:lastPrompt = ''
        $script:lastKey = $null
        $script:clipboardCaptured = $false
        Start-NativeWorkStoryDemo -Context $script:demo -Package Dev -StateDirectory $script:stateDirectory `
            -ArtifactDirectory $script:root -ExpectedWtaSha256 $env:ITE2E_EXPECTED_WTA_SHA256 | Out-Null
        $script:consoleInput = $true
        $script:demo.InputRoute = 'ConsoleInput'
        @{
            inputQualification = $(if ($script:consoleInput) { 'Owned native wta ui ConsoleInput records; physical keyboard NOT EXERCISED' } else { 'Native HWND input will be verified by each action' })
            physicalScreenRepaint = 'UNQUALIFIED; PNGs are diagnostics until independently reviewed'
            boundary = 'Dedicated native Console with zero shell tabs; shared Work views'
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:root 'qualification.json') -Encoding utf8NoBOM
        if (-not $script:consoleInput) {
            $script:clipboard = Get-ClipboardSnapshot
            $script:clipboardCaptured = $true
        }

        function Read-NativeDemo { Read-NativeWorkStoryState -Context $script:demo }
        function Read-NativeFrame { Get-NativeWorkStoryText -Context $script:demo }
        function Fingerprint($Value) { $Value | ConvertTo-Json -Depth 64 -Compress }
        function Get-NativeDemoWork($State, [string]$Id) {
            $work = @($State.works | Where-Object id -CEQ $Id)
            $work.Count | Should -Be 1
            $work[0]
        }
        function Send-NativeDemoKey([int]$Vk, [switch]$Ctrl, [switch]$Shift) {
            $script:lastKey = @{ vk = $Vk; ctrl = [bool]$Ctrl; shift = [bool]$Shift }
            try {
                Send-NativeWorkStoryKey -Context $script:demo -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift
            }
            catch {
                $failure = $_
                Save-NativeDemoFailure -Reason $failure.Exception.Message -Expected @{ key = $script:lastKey }
                throw $failure
            }
        }
        function Save-NativeDemoFailure([string]$Reason, $Expected) {
            $stem = 'native-failure-' + [guid]::NewGuid().ToString('N')
            try { Read-NativeFrame | Set-Content -LiteralPath (Join-Path $script:root "$stem.txt") -Encoding utf8NoBOM }
            catch { Write-Warning "Native UIA failure capture unavailable: $($_.Exception.Message)" }
            try { Read-NativeDemo | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath (Join-Path $script:root "$stem-state.json") }
            catch { Write-Warning "Native state failure capture unavailable: $($_.Exception.Message)" }
            try { Save-UiScreenshot -App $script:demo.App -Path (Join-Path $script:root "$stem.png") -RequireSuccess | Out-Null }
            catch { Write-Warning "Native diagnostic screenshot unavailable: $($_.Exception.Message)" }
            $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
            $documentFocus = try {
                $document = Get-NativeWorkStoryDocument -Context $script:demo
                @{
                    hasKeyboardFocus = $document.Current.HasKeyboardFocus
                    isKeyboardFocusable = $document.Current.IsKeyboardFocusable
                    processId = $document.Current.ProcessId
                    controlType = $document.Current.ControlType.ProgrammaticName
                }
            }
            catch { @{ error = $_.Exception.Message } }
            @{
                reason = $Reason; expected = $Expected; lastPrompt = $script:lastPrompt; lastKey = $script:lastKey
                ownedHwnd = $script:demo.App.Hwnd; ownedUiPid = $script:demo.UiPid
                foregroundHwnd = $foreground.ToInt64()
                foregroundPid = [ItE2E.ItWtWin32Input]::GetWindowProcessId($foreground)
                consoleInput = $script:consoleInput
                nativeDocumentFocus = $documentFocus
            } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $script:root "$stem-diagnostic.json")
        }
        function Assert-NativeDraft([string]$Text) {
            try {
                Wait-Until -TimeoutSec 10 -Because 'the exact text is in the shared native composer, not merely old conversation history' -Condition {
                    (Get-NativeWorkStoryDraftText -Frame (Read-NativeFrame)) -ceq $Text
                } | Should -BeTrue
            }
            catch {
                $failure = $_
                Save-NativeDemoFailure -Reason $failure.Exception.Message -Expected @{ draft = $Text }
                throw $failure
            }
        }
        function Send-NativeDemoPrompt([string]$Text, [switch]$Global) {
            $script:lastPrompt = $Text
            try {
                Send-NativeWorkStoryPrompt -Context $script:demo -Text $Text -Global:$Global
            }
            catch {
                $failure = $_
                Save-NativeDemoFailure -Reason $failure.Exception.Message -Expected @{ draft = $Text }
                throw $failure
            }
        }
        function Wait-NativeDemoScene([int]$Scene, [switch]$AtLeast, [int]$DisplayScene = 0) {
            try {
                Wait-Until -TimeoutSec 20 -Because "native composer commits scene $Scene" -Condition {
                    $state = Read-NativeDemo
                    $sceneMatches = if ($AtLeast) { $state.scene -ge $Scene } else { $state.scene -eq $Scene }
                    $visibleScene = if ($DisplayScene) { $DisplayScene } else { $state.scene }
                    if ($sceneMatches -and (Read-NativeFrame).Contains("Scene $visibleScene/8")) { return $state }
                    return $false
                }
            }
            catch {
                $failure = $_
                Save-NativeDemoFailure -Reason $failure.Exception.Message -Expected @{ scene = $Scene }
                throw $failure
            }
        }
        function Save-NativeDemoProof([string]$Name, [int]$Scene, [string[]]$Text, [ValidateSet('Up', 'Down')][string]$Scroll = 'Up', [switch]$AtLeast, [int]$DisplayScene = 0) {
            $state = Wait-NativeDemoScene $Scene -AtLeast:$AtLeast -DisplayScene $DisplayScene
            @(Get-WtTabs -App $script:demo.App -WindowId $script:demo.OwnedWindowId).Count | Should -Be 0
            $state | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath (Join-Path $script:root "$Name-state.json") -Encoding utf8NoBOM
            $frames = [Collections.Generic.List[string]]::new()
            for ($page = 0; $page -lt 32; $page++) {
                $frame = Read-NativeFrame
                $frames.Add($frame)
                $stem = '{0}-page-{1:D2}' -f $Name, ($page + 1)
                $frame | Set-Content -LiteralPath (Join-Path $script:root "$stem.txt") -Encoding utf8NoBOM
                $image = Join-Path $script:root "$stem.png"
                Save-UiScreenshot -App $script:demo.App -Path $image -RequireSuccess | Out-Null
                (Get-Item -LiteralPath $image).Length | Should -BeGreaterThan 0
                $combined = $frames -join "`n--- native page ---`n"
                $missing = @($Text | Where-Object { -not $combined.Contains($_, [StringComparison]::Ordinal) })
                if (-not $missing.Count) {
                    $combined | Set-Content -LiteralPath (Join-Path $script:root "$Name.txt") -Encoding utf8NoBOM
                    return $state
                }
                Send-NativeDemoKey $(if ($Scroll -eq 'Up') { 0x21 } else { 0x22 })
                Wait-Until -TimeoutSec 5 -Because "native $Scroll paging reveals missing facts: $($missing -join ', ')" -Condition {
                    (Read-NativeFrame) -cne $frame
                } | Should -BeTrue
            }
            throw "Native evidence exceeded 32 pages for $Name."
        }
    }

    AfterAll {
        try { if ($script:demo) { Stop-NativeWorkStoryDemo -Context $script:demo } }
        finally {
            if ($script:clipboardCaptured) { Restore-ClipboardSnapshot -Snapshot $script:clipboard }
        }
    }

    It 'Native demo scene 1 exposes four distinct interactive legacy session tabs' {
        $state = Read-NativeDemo
        @($state.works).Count | Should -Be 1
        $state.clock | Should -Not -BeNullOrEmpty
        $state.capConfigured | Should -BeFalse
        $state.resumed | Should -BeFalse
        @($state.events | Where-Object kind -CEQ 'systemStarted').Count | Should -Be 1
        @($state.events | Where-Object kind -CEQ 'capSetupEnabled').Count | Should -Be 1
        $script:initial = $state
        $tabs = @(Get-NativeWorkStoryLegacyTabs)
        for ($i = 0; $i -lt $tabs.Count; $i++) {
            if ($i) { Send-NativeDemoKey 0x27 }
            $tab = $tabs[$i]
            Save-NativeDemoProof "native-scene-01-legacy-$i" 1 @('Work story demo | Simulated data', "[$($tab.Title)]",
                $tab.Marker) | Out-Null
            $frame = Read-NativeFrame
            foreach ($other in $tabs | Where-Object Title -CNE $tab.Title) {
                $frame.Contains($other.Marker, [StringComparison]::Ordinal) | Should -BeFalse
            }
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $state)
        }
        Send-NativeDemoKey 0x25
        Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) -not $s.resumed } `
            -Text @('[Migration]', 'Keep the investigation separate') | Out-Null
        Send-NativeDemoKey 0x70
        Save-NativeDemoProof 'native-scene-01-global' 1 @('Global conversation', 'One existing Work') | Out-Null
    }

    Context 'Native composer and shared Work routes' {
        It 'Native demo scene 2 resumes the existing Work from the composer' {
            Send-NativeDemoKey 0x70
            Send-NativeDemoPrompt 'Continue fixing issue #4821'
            $state = Save-NativeDemoProof 'native-scene-02' 2 @('Resumed Fix Issue #4821.', 'Completed: Reproduce issue',
                'Blocked: Compatibility test', 'Pending: Documentation')
            $work = Get-NativeDemoWork $state 'fix-4821'
            $work.id | Should -BeExactly $script:initial.works[0].id
            $work.repository | Should -BeExactly 'microsoft/foo'
            $work.branch | Should -BeExactly 'fix-4821'
            $work.nextStep | Should -BeExactly 'Investigate shell integration behavior'
            @($work.steps | Where-Object status -CEQ 'completed').Count | Should -Be 3
        }

        It 'Native demo scene 3 shares Work identity across list chat and details' {
            Send-NativeDemoKey 0x74
            $state = Wait-NativeDemoScene 2 -DisplayScene 3
            $before = Fingerprint $state
            Send-NativeDemoKey 0x71
            Save-NativeDemoProof 'native-scene-03-list' 2 @('Work Overview', 'Fix Issue #4821', 'Status:') -Scroll Down | Out-Null
            Send-NativeDemoKey 0x0D
            Save-NativeDemoProof 'native-scene-03-chat' 2 @('Fix Issue #4821', 'Blocked: Compatibility test') | Out-Null
            Send-NativeDemoKey 0x74
            Save-NativeDemoProof 'native-scene-03-details' 2 @('Work ID: fix-4821', 'microsoft/foo', 'fix-4821',
                'sample-fix-4821-compatibility', 'exitCode: 1') -Scroll Down -DisplayScene 3 | Out-Null
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly $before
            $work = Get-NativeDemoWork $state 'fix-4821'
            @($work.steps | Where-Object { $_.id -ceq 'compatibility' -and $_.status -ceq 'blocked' }).Count | Should -Be 1
            @($work.steps | Where-Object status -CEQ 'pending').Count | Should -Be 2
            $script:failedEvidence = Fingerprint $work.evidence
        }

        It 'Native demo scene 4 confirms related Work without polluting its parent' {
            Send-NativeDemoKey 0x74
            Send-NativeDemoPrompt 'Should we migrate to API v2?'
            $preview = Save-NativeDemoProof 'native-scene-04-confirmation' 4 @('Create Related Work: Investigate API v2?',
                'The bug fix keeps its original goal.', 'F4: confirm or cancel.')
            @($preview.works).Count | Should -Be 1
            $preview.branchConfirmationPending | Should -BeTrue
            Send-NativeDemoKey 0x73
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) $s.branchConfirmationPending } `
                -Text @('Create related work', 'Cancel related work') | Out-Null
            Send-NativeDemoKey 0x0D
            Wait-Until -TimeoutSec 15 -Because 'confirmation creates the projected related Work' -Condition { @((Read-NativeDemo).works).Count -eq 2 } | Out-Null
            $state = Save-NativeDemoProof 'native-scene-04-created' 5 @('Related Work created: Investigate API v2', 'Schema 2.3 | 3 breaking changes') -AtLeast
            $parent = Get-NativeDemoWork $state 'fix-4821'
            $related = Get-NativeDemoWork $state 'api-v2'
            $related.parentWorkId | Should -BeExactly $parent.id
            (Fingerprint $related.context) | Should -BeExactly (Fingerprint $parent.context)
            $related.executorSessionId | Should -Not -BeExactly $parent.executorSessionId
            (Fingerprint $parent.evidence) | Should -BeExactly $script:failedEvidence
            $script:branchFindings = Fingerprint $related.findings
            $script:branchExecutor = $related.executorSessionId
            ($null -ne $state.clock.attentionDueMs -or $state.clock.attentionRaised) | Should -BeTrue
            @($state.events | Where-Object { $_.input -clike 'Show *' }).Count | Should -Be 0
            $script:migrationDraft = 'Keep migration findings independent'
            Set-NativeWorkStoryDraft -Context $script:demo -Text $script:migrationDraft
        }

        It 'Native demo scene 5 automatically exchanges structured schema in related Work details' {
            $state = Wait-NativeDemoScene 5 -AtLeast
            Send-NativeDemoKey 0x74
            Save-NativeDemoProof 'native-scene-05' 5 @('Work ID: api-v2',
                'Schema exchange | Request owner: api-v2 | Result owner: fix-4821', 'Need: API v2 schema',
                'Blocking: false', 'Schema 2.3 | Breaking changes: 3 | Source: scripted-fixture') -Scroll Down -AtLeast | Out-Null
            $state.exchange.request.blocking | Should -BeFalse
            $state.exchange.request.consumer | Should -BeExactly 'api-v2'
            $state.exchange.response.version | Should -BeExactly '2.3'
            $state.exchange.response.breakingChanges | Should -Be 3
            $state.exchange.response.source | Should -BeExactly 'scripted-fixture'
            foreach ($kind in @('schemaRequest', 'schemaResponse')) {
                $event = @($state.events | Where-Object kind -CEQ $kind)
                $event.Count | Should -Be 1
                $event[0].input | Should -BeNullOrEmpty
            }
        }

        It 'Native demo scene 6 receives autonomous owner-scoped attention and preserves failed evidence' {
            $attention = Wait-NativeWorkStoryCondition -Context $script:demo -TimeoutSec 25 -StateCondition {
                param($s) $s.clock.attentionRaised -and $null -eq $s.decision
            } -Text @('Work ID: api-v2', 'Fix Issue #4821 | Compatibility failed - human decision needed', 'F6: decide')
            $raised = @($attention.State.events | Where-Object kind -CEQ 'attentionRaised')
            $raised.Count | Should -Be 1
            $raised[0].workId | Should -BeExactly 'fix-4821'
            $raised[0].input | Should -BeNullOrEmpty
            $attention.State.selectedWorkId | Should -BeExactly 'api-v2'
            $attention.State.clock.elapsedMs | Should -BeGreaterOrEqual 10000
            $beforeDecisionMenu = Fingerprint (Read-NativeDemo)
            Send-NativeDemoKey 0x75
            $decisionMenu = Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition {
                param($s) $null -eq $s.decision
            } -Text @('Fix Issue #4821 | Decision', 'Up/Down Select | Enter Confirm | Esc Cancel')
            foreach ($stale in @('Schema exchange', 'F5 Back', 'Ask anything, / for commands..')) {
                $decisionMenu.Text.Contains($stale, [StringComparison]::Ordinal) | Should -BeFalse
            }
            Send-NativeDemoKey 0x1B
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition {
                param($s) $null -eq $s.decision
            } -Text @('Investigate API v2 | Evidence', 'Work ID: api-v2', 'Schema exchange') | Out-Null
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly $beforeDecisionMenu
            Send-NativeDemoKey 0x74
            Assert-NativeDraft $script:migrationDraft
            Save-NativeDemoProof 'native-scene-06-attention' 6 @('Fix Issue #4821 | Compatibility failed - human decision needed',
                'B: Fix compatibility, then recheck (recommended)', 'F6: decide') | Out-Null
            Send-NativeDemoKey 0x71
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) $null -eq $s.decision } `
                -Text @('Work Overview', 'Fix Issue #4821 | Compatibility failed - human decision needed') | Out-Null
            Send-NativeDemoKey 0x75
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) $null -eq $s.decision } `
                -Text @('B. Fix compatibility (recommended) - Fix Issue #4821') | Out-Null
            Send-NativeDemoKey 0x0D
            Wait-Until -TimeoutSec 15 -Because 'the explicit native decision is persisted' -Condition { (Read-NativeDemo).decision.resolved } | Out-Null
            $state = Read-NativeDemo
            $state.decision.choice | Should -BeExactly 'B'
            $state.decision.workId | Should -BeExactly 'fix-4821'
            (Fingerprint (Get-NativeDemoWork $state 'fix-4821').evidence) | Should -BeExactly $script:failedEvidence
            (Get-NativeDemoWork $state 'fix-4821').acceptance | Should -Match '^Pending;'
            (Get-NativeDemoWork $state 'documentation').title | Should -BeExactly 'Prepare Documentation'
        }

        It 'Native demo scene 7 sets a token cap before automatic budget progression' {
            $before = Read-NativeDemo
            $before.capConfigured | Should -BeFalse
            $before.budget.consumed | Should -Be 0
            $before.clock.budgetDueMs | Should -BeNullOrEmpty
            Send-NativeDemoKey 0x71
            Send-NativeDemoKey 0x26
            Send-NativeDemoKey 0x26
            Send-NativeDemoKey 0x28
            Send-NativeDemoKey 0x0D
            Send-NativeDemoKey 0x74
            Send-NativeDemoKey 0x75
            $capModal = Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition {
                param($s) $s.budget.consumed -eq 0
            } -Text @('Investigate API v2 | Token cap', 'Migration cap: 10K',
                '> Migration cap: 20K tokens (default)', 'Migration cap: 30K', 'Cancel',
                'Up/Down Select | Enter Confirm | Esc Cancel')
            foreach ($stale in @('Schema exchange', 'F5 Back', 'Ask anything, / for commands..')) {
                $capModal.Text.Contains($stale, [StringComparison]::Ordinal) | Should -BeFalse
            }
            Wait-NativeWorkStoryPace -Seconds 4
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            Send-NativeDemoKey 0x1B
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition {
                param($s) -not $s.capConfigured
            } -Text @('Investigate API v2 | Evidence', 'Work ID: api-v2', 'Schema exchange') | Out-Null
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            Send-NativeDemoKey 0x75
            Send-NativeDemoKey 0x26
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) -not $s.capConfigured } `
                -Text @('> Migration cap: 10K tokens') | Out-Null
            Send-NativeDemoKey 0x28
            Send-NativeDemoKey 0x28
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) -not $s.capConfigured } `
                -Text @('> Migration cap: 30K tokens') | Out-Null
            Send-NativeDemoKey 0x28
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) -not $s.capConfigured } -Text @('> Cancel') | Out-Null
            Send-NativeDemoKey 0x0D
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            Send-NativeDemoKey 0x75
            Save-NativeDemoProof 'native-scene-07-cap-picker' 7 @('10K', '> Migration cap: 20K tokens (default)', '30K', 'Cancel') | Out-Null
            Send-NativeDemoKey 0x0D
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition {
                param($s) $s.capConfigured -eq $true -and $s.budget.limit -eq 20000 -and $null -ne $s.clock.budgetDueMs
            } -Text @('Token cap set: 20K', 'Investigate API v2') | Out-Null
            try { $progress = Wait-NativeWorkStoryBudget -Context $script:demo }
            catch {
                $failure = $_
                Save-NativeDemoFailure -Reason $failure.Exception.Message -Expected @{ automaticBudget = 20000 }
                throw $failure
            }
            @($progress.Samples | Where-Object { $_.Consumed -gt 0 -and $_.Consumed -lt 20000 }).Count | Should -BeGreaterOrEqual 2
            $state = Save-NativeDemoProof 'native-scene-07' 7 @('Tokens: 20000 / 20000', 'Paused at cap',
                'Token cap reached: 20K.', 'Findings saved; performance validation remains.')
            $state.budget.limit | Should -Be 20000
            $state.budget.consumed | Should -Be 20000
            $state.budget.paused | Should -BeTrue
            $state.budget.maxConcurrency | Should -Be 1
            $state.budget.priority | Should -BeExactly 'Low'
            foreach ($kind in @('budgetConsumed', 'budgetReached', 'systemAdvanced')) {
                $events = @($state.events | Where-Object kind -CEQ $kind)
                $events.Count | Should -BeGreaterThan 0
                foreach ($event in $events) {
                    $event.input | Should -BeNullOrEmpty
                    $event.workId | Should -BeExactly 'api-v2'
                }
            }
            @($state.events | Where-Object kind -CEQ 'budgetConsumed').Count | Should -Be 3
            @($state.events | Where-Object kind -CEQ 'budgetReached').Count | Should -Be 1
            @($state.events | Where-Object { $_.kind -ceq 'userRequest' -and $_.input -ceq 'Set token cap to 20K' }).Count | Should -Be 1
            $capEvent = @($state.events | Where-Object kind -CEQ 'tokenCapSet')
            $capEvent.Count | Should -Be 1
            $capEvent[0].input | Should -BeNullOrEmpty
            $capEvent[0].workId | Should -BeExactly 'api-v2'
            $migration = Get-NativeDemoWork $state 'api-v2'
            (Fingerprint $migration.findings) | Should -BeExactly $script:branchFindings
            $migration.executorSessionId | Should -BeExactly $script:branchExecutor
            $script:findings = Fingerprint $migration.findings
            $script:migrationSession = $migration.executorSessionId
            Send-NativeDemoKey 0x71
            Send-NativeDemoKey 0x26
            Send-NativeDemoKey 0x26
            Send-NativeDemoKey 0x28
            Send-NativeDemoKey 0x0D
            Send-NativeDemoKey 0x74
            Save-NativeDemoProof 'native-scene-07-budget-details' 7 @('Work ID: api-v2', 'Tokens: 20000 / 20000',
                'Priority: Low', 'Max concurrency: 1', 'Performance Validation') -Scroll Down | Out-Null
            Send-NativeDemoKey 0x74
            Assert-NativeDraft $script:migrationDraft
        }

        It 'Native demo scene 8 lists and opens three real projected Works' {
            Send-NativeDemoKey 0x71
            $state = Save-NativeDemoProof 'native-scene-08-list' 7 @('Work Overview', 'Fix Issue #4821', 'Investigate API v2', 'Prepare Documentation',
                'Status:', 'Progress:', 'Blockers:', 'Decisions:', 'Deliverables:', 'Acceptance:') -Scroll Down -DisplayScene 8
            @($state.works).Count | Should -Be 3
            @($state.works.id | Sort-Object -Unique).Count | Should -Be 3
            $overview = Read-NativeFrame
            foreach ($field in @('Status', 'Progress', 'Blockers', 'Decisions', 'Deliverables', 'Acceptance')) {
                [regex]::Matches($overview, [regex]::Escape("$($field):")).Count | Should -Be 3
            }
            Send-NativeDemoKey 0x26
            Send-NativeDemoKey 0x26
            Send-NativeDemoKey 0x28
            Send-NativeDemoKey 0x28
            Send-NativeDemoKey 0x0D
            Send-NativeDemoKey 0x74
            Save-NativeDemoProof 'native-scene-08-documentation' 7 @('Work ID: documentation', 'Prepare Documentation') -Scroll Down | Out-Null
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $state)
            @($state.events | Where-Object { $_.input -clike 'Show *' }).Count | Should -Be 0
        }

        It 'Native demo rejects unrelated prompts and duplicate Work creation' {
            foreach ($prompt in @('Continue fixing issue #9999', 'Please order a pizza')) {
                $before = Read-NativeDemo
                Send-NativeDemoPrompt $prompt -Global
                Wait-Until -TimeoutSec 10 -Because 'native input visibly rejects the unsupported phrase' -Condition {
                    (Read-NativeFrame).Contains('Unsupported scripted demo input')
                } | Out-Null
                (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            }
            Send-NativeDemoPrompt 'Should we migrate to API v2?' -Global
            $before = Wait-NativeDemoScene 4
            Send-NativeDemoPrompt 'Create related work'
            Assert-NativeDraft ''
            $after = Read-NativeDemo
            (Fingerprint $after.works) | Should -BeExactly (Fingerprint $before.works)
            @($after.events | Where-Object kind -CEQ 'workCreated').Count | Should -Be 2
        }

        It 'Native demo restores the same Work history and budget continuation' {
            Send-NativeDemoKey 0x71
            $before = Read-NativeDemo
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) $s.budget.paused } -Text @('Scene 8/8') | Out-Null
            $automaticBefore = Fingerprint @($before.events | Where-Object { $null -eq $_.input })
            Send-NativeDemoPrompt '/quit' -Global
            Wait-Until -TimeoutSec 15 -Because 'the native demo exits rather than leaving another live writer' -Condition {
                -not (Get-Process -Id $script:demo.UiPid -ErrorAction SilentlyContinue)
            } | Out-Null
            Stop-NativeWorkStoryDemo -Context $script:demo
            $script:demo = @{}
            Start-NativeWorkStoryDemo -Context $script:demo -Package Dev -StateDirectory $script:stateDirectory `
                -ArtifactDirectory (Join-Path $script:root 'reopened') -ExpectedWtaSha256 $env:ITE2E_EXPECTED_WTA_SHA256 | Out-Null
            $script:demo.InputRoute = 'ConsoleInput'
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            (Fingerprint @((Read-NativeDemo).events | Where-Object { $null -eq $_.input })) | Should -BeExactly $automaticBefore
            Send-NativeDemoKey 0x71
            Save-NativeDemoProof 'native-reopened' ([int]$before.scene) @('Work Overview', 'Prepare Documentation') -Scroll Down -DisplayScene 8 | Out-Null
            Send-NativeDemoKey 0x75
            Wait-NativeWorkStoryCondition -Context $script:demo -StateCondition { param($s) $s.budget.paused } -Text @('Add 10K budget') | Out-Null
            Send-NativeDemoKey 0x0D
            $resumed = Save-NativeDemoProof 'native-resumed' 7 @('Tokens: 20000 / 30000',
                'Cap increased to 30K. Migration resumed.', 'Same Work, findings and history.')
            $resumed.budget.paused | Should -BeFalse
            $resumed.budget.limit | Should -Be 30000
            (Get-NativeDemoWork $resumed 'api-v2').executorSessionId | Should -BeExactly $script:migrationSession
            (Fingerprint (Get-NativeDemoWork $resumed 'api-v2').findings) | Should -BeExactly $script:findings
        }

        It 'Native demo inspection and reset remain isolated from production work' {
            $before = Read-NativeDemo
            $database = Join-Path $script:stateDirectory 'work-story.sqlite3'
            $written = (Get-Item -LiteralPath $database).LastWriteTimeUtc.Ticks
            1..3 | ForEach-Object { (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before) }
            (Get-Item -LiteralPath $database).LastWriteTimeUtc.Ticks | Should -Be $written
            $arguments = @('--language', 'en-US', 'demo', '--state-dir', $script:stateDirectory)
            $conflict = Invoke-Native -FilePath $script:demo.App.WtaPath -Arguments ($arguments + @('--inspect', '--reset'))
            $conflict.ExitCode | Should -Not -Be 0
            $conflict.TimedOut | Should -BeFalse
            $writer = Invoke-Native -FilePath $script:demo.App.WtaPath -Arguments ($arguments + '--reset')
            $writer.ExitCode | Should -Not -Be 0
            $writer.TimedOut | Should -BeFalse
            $writer.StdErr | Should -Match 'lock|already.*open'
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            Send-NativeDemoPrompt '/reset' -Global
            Wait-Until -TimeoutSec 10 -Because 'native reset asks for its explicit confirmation' -Condition { (Read-NativeFrame).Contains('/reset confirm') } | Out-Null
            (Fingerprint (Read-NativeDemo)) | Should -BeExactly (Fingerprint $before)
            Send-NativeDemoPrompt '/reset confirm'
            $reset = Save-NativeDemoProof 'native-reset' 1 @('[Fix bug]', 'Work story demo | Simulated data')
            (Fingerprint $reset) | Should -BeExactly (Fingerprint $script:initial)
        }
    }
}
