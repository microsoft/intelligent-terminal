#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Deterministic human UI/protocol coverage, not a claim of model autonomy.
# Global chat starts without Projects. Later public-API setup is not conversational approval.

Describe 'Feature: Agent Center human work flow' -Tag 'Feature', 'AgentCenterWorkFlow' `
    -Skip:($env:ITE2E_PACKAGE -in @('Store', 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe')) {
    BeforeAll {
        $script:context = @{}
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\AgentCenterDraft.ps1')
        . (Join-Path $PSScriptRoot 'helpers\AgentCenterWorkFlow.ps1')
        Start-WorkFlowTestContext -Context $script:context

        function Read-WorkFlowUi {
            $script:context.Sequence++
            $startedUtc = [datetime]::UtcNow
            $frame = Get-WtCapture -App $script:context.App -SessionId $script:context.Pane.session_id
            if ($frame.Contains('Live facts are stale.')) { $script:context.SawStaleFacts = $true }
            $frame | Set-Content -LiteralPath (Join-Path $script:context.Root ('frame-{0:D4}.txt' -f $script:context.Sequence)) -Encoding utf8NoBOM
            if ($script:context.Runtime.navigationDiagnostics) {
                @{
                    navigationId = $script:context.LastNavigationId; capturePath = $script:context.CapturedActionEvidencePath
                    sequence = $script:context.Sequence; frame = ('frame-{0:D4}.txt' -f $script:context.Sequence)
                    startedUtc = $startedUtc; completedUtc = [datetime]::UtcNow
                    paneSessionId = $script:context.Pane.session_id; uiPid = $script:context.UiIdentity.pid
                } | ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $script:context.Root 'navigation-captures.jsonl') -Encoding utf8NoBOM
            }
            return $frame
        }
        function Send-WorkFlowKey([int]$Vk, [switch]$Ctrl, [switch]$Shift) {
            if ($script:context.Runtime.navigationDiagnostics -and $Vk -in @(0x21, 0x22, 0x7B)) {
                Send-WorkFlowNavigationKey -Context $script:context -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift
                return
            }
            Send-WtWindowKey -App $script:context.App -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift -RequireForeground | Out-Null
            $script:context.NativeInputSent = $true
        }
        function Paste-WorkFlowText([string]$Text) {
            Set-Clipboard -Value $Text
            (Get-Clipboard -Raw) | Should -BeExactly $Text
            Send-WorkFlowKey 0x56 -Ctrl -Shift
        }
        function Assert-WorkFlowDraft([string[]]$Expected) {
            Wait-Until -TimeoutSec 15 -IntervalSec 0.2 -Because 'the selected work restores exact draft rows' -Condition {
                $rows = Get-NativePasteDraftRows -Frame (Read-WorkFlowUi)
                if ($rows.Count -lt $Expected.Count) { return $false }
                for ($i = 0; $i -lt $Expected.Count; $i++) {
                    if ($rows[$i] -cne $Expected[$i]) { return $false }
                }
                for ($i = $Expected.Count; $i -lt $rows.Count; $i++) {
                    if ($rows[$i] -cne '') { return $false }
                }
                return $true
            } | Should -BeTrue
        }
        function Read-WorkFlowWork([int]$Index) {
            (Invoke-WorkFlowRequest -Connection $script:context.Connection -Method 'work.get' `
                -Params @{ workId = $script:context.Fixture.works[$Index].id } `
                -ReceiptPath (Join-Path $script:context.Root 'query-receipts.jsonl')).data
        }
        function Select-WorkFlowAction([string]$Prefix, [switch]$MenuAlreadyOpen, [switch]$Contains) {
            if (-not $MenuAlreadyOpen) { Send-WorkFlowKey 0x73 }
            Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because 'the physical F4 menu is visible' -Condition {
                Get-WorkFlowSelectedMenuLabel -Frame (Read-WorkFlowUi) -IncludeWrapped:$Contains | Out-Null
                return $true
            } | Should -BeTrue
            for ($step = 0; $step -lt 32; $step++) {
                $label = Get-WorkFlowSelectedMenuLabel -Frame (Read-WorkFlowUi) -IncludeWrapped:$Contains
                if (($Contains -and $label.Contains($Prefix, [StringComparison]::Ordinal)) -or
                    (-not $Contains -and $label.StartsWith($Prefix, [StringComparison]::Ordinal))) {
                    Send-WorkFlowKey 0x0D
                    Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'the chosen action replaces or closes its menu' -Condition {
                        try { (Get-WorkFlowSelectedMenuLabel -Frame (Read-WorkFlowUi) -IncludeWrapped:$Contains) -cne $label }
                        catch { $true }
                    } | Should -BeTrue
                    return
                }
                Send-WorkFlowKey 0x28
                Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because "menu selection advances toward '$Prefix'" -Condition {
                    (Get-WorkFlowSelectedMenuLabel -Frame (Read-WorkFlowUi) -IncludeWrapped:$Contains) -cne $label
                } | Should -BeTrue
            }
            throw "No rendered work-flow action matches '$Prefix' (contains=$Contains)."
        }
        function Assert-WorkFlowSelection([int]$Index) {
            $goal = $script:context.Fixture.works[$Index].goal
            Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because "the native header selects '$goal'" -Condition {
                Test-WorkFlowRenderedSelection -Frame (Read-WorkFlowUi) -Goal $goal `
                    -ProjectName $script:context.Fixture.works[$Index].projectName
            } | Should -BeTrue
        }
        function Open-WorkFlowWork([int]$Index) {
            Select-WorkFlowAction 'Global conversation'
            Select-WorkFlowAction ('Open: ' + $script:context.Fixture.works[$Index].goal)
            Assert-WorkFlowSelection $Index
        }
        function Set-WorkFlowLoadedSelection([int]$Index) {
            Send-WorkFlowKey 0x23
            for ($i = 0; $i -lt 6; $i++) { Send-WorkFlowKey 0x25 }
            for ($i = 0; $i -lt 2; $i++) { Send-WorkFlowKey 0x27 -Shift }
        }
        function Assert-WorkFlowLoadedCaret([int]$Index, [string]$Prefix) {
            $work = $script:context.Fixture.works[$Index]
            Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because 'the native global caret and selection survive context-hint navigation' -Condition {
                $actual = Get-WorkFlowNativeCaret -App $script:context.App -ExpectedScope 'Global conversation'
                @{ workId = $work.id; expected = $Prefix; actual = $actual; observedUtc = [datetime]::UtcNow } |
                    ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $script:context.Root 'loaded-caret-observations.jsonl') -Encoding utf8NoBOM
                return $actual -ceq $Prefix
            } | Should -BeTrue
        }
        function Assert-MeaningfulWorkFlowFrame {
            $frame = Read-WorkFlowUi
            foreach ($work in $script:context.Fixture.works) {
                $frame | Should -Not -Match ([regex]::Escape($work.id))
                $frame | Should -Not -Match ([regex]::Escape($work.projectId))
            }
        }
        function Read-WorkFlowCapturedAction([ValidateRange(0, 32)][int]$MaxPages = 32, $ExpectedRequest) {
            $pages = 0
            $frames = [Collections.Generic.List[string]]::new()
            $rows = @()
            $actual = $null
            $captureError = $null
            $script:context.CapturedActionEvidencePath = Join-Path $script:context.Root ('captured-action-' + [guid]::NewGuid().ToString('N') + '.json')
            Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because 'the captured confirmation is rendered before opening its diagnostics' -Condition {
                (Read-WorkFlowUi).Contains('Confirm the captured action')
            } | Should -BeTrue
            Send-WorkFlowKey 0x7B
            try {
                $frame = Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'the frozen diagnostic record is visible' -Condition {
                    $capture = Read-WorkFlowUi
                    if (((Get-WorkFlowBodyRows $capture) -join "`n").StartsWith('{')) { return $capture }
                    return $false
                }
                $frames.Add($frame)
                $rows = Get-WorkFlowBodyRows $frame
                while ($true) {
                    $combined = ($rows | ForEach-Object { "││$_│" }) -join "`n"
                    try {
                        $actual = Get-WorkFlowCapturedRequest -Frame $combined -RequireEncoding
                        $script:context.LastDecodedCommandId = $actual.commandId
                        return $actual
                    }
                    catch [IO.EndOfStreamException] { if ($pages -ge $MaxPages) { throw } }
                    $prior = (Get-WorkFlowBodyRows $frame) -join "`n"
                    Send-WorkFlowKey 0x22
                    $pages++
                    $frame = Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'the next diagnostic page is rendered' -Condition {
                        $capture = Read-WorkFlowUi
                        if (((Get-WorkFlowBodyRows $capture) -join "`n") -cne $prior) { return $capture }
                        return $false
                    }
                    $frames.Add($frame)
                    $rows = Merge-WorkFlowBodyRows -Earlier $rows -Later (Get-WorkFlowBodyRows $frame)
                }
            }
            catch {
                $captureError = $_.Exception.Message
                throw
            }
            finally {
                try {
                    Save-WorkFlowCapturedActionEvidence -Path $script:context.CapturedActionEvidencePath `
                        -Expected $ExpectedRequest -Actual $actual -Frames $frames.ToArray() -Rows $rows -CaptureError $captureError
                }
                finally {
                    for ($i = 0; $i -lt $pages; $i++) { Send-WorkFlowKey 0x21 }
                    Send-WorkFlowKey 0x7B
                    Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Because 'the human-readable preview is restored after diagnostic inspection' -Condition {
                        (Read-WorkFlowUi).Contains('Confirm the captured action')
                    } | Out-Null
                }
            }
        }
        function Read-WorkFlowCommandReceipt([string]$CommandId) {
            $reader = Join-Path $PSScriptRoot '..\fixtures\Read-AgentCenterCommandReceipt.cjs'
            $result = Invoke-Native -FilePath 'node.exe' `
                -Arguments @('--no-warnings', $reader, $script:context.Root, $CommandId) -TimeoutSec 10
            if ($result.ExitCode -ne 0) { throw "Read-only fixture receipt inspection failed: $($result.StdErr)" }
            ($result.StdOut | ConvertFrom-Json -Depth 25).receipt
        }
        function Read-WorkFlowConversation([string]$ConversationId) {
            $reader = Join-Path $PSScriptRoot '..\fixtures\Read-AgentCenterConversation.ps1'
            $result = Invoke-Native -FilePath 'pwsh.exe' -Arguments @('-NoProfile', '-File', $reader,
                '-EvidenceDirectory', $script:context.Root, '-ConversationId', $ConversationId) -TimeoutSec 15
            if ($result.ExitCode -ne 0) { throw "Conversation snapshot query failed: $($result.StdErr)" }
            $result.StdOut | ConvertFrom-Json -Depth 80
        }
        function Assert-WorkFlowField([string]$Label, [string]$Value) {
            Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because "the native '$Label' field has its exact value" -Condition {
                (Get-WorkFlowFormField -Frame (Read-WorkFlowUi) -Label $Label) -ceq $Value
            } | Should -BeTrue
        }
        function Wait-WorkFlowDecisionEvidence([string]$Name, [int]$TimeoutSec = 60, [string[]]$ExcludedFiles = @()) {
            $result = Wait-Until -TimeoutSec $TimeoutSec -Because "the actual bound decision fixture records $Name" -Condition {
                Read-WorkFlowUi | Out-Null
                foreach ($prefix in @('global', 'decision')) {
                    $failure = Read-WorkFlowJsonSnapshot -Path (Join-Path $script:context.Root "$prefix-fixture-error.json")
                    if ($failure) { return @{ fixtureError = ($failure | ConvertTo-Json -Depth 12 -Compress) } }
                }
                if ($Name -ceq 'global-status-*.json') {
                    $value = Read-WorkFlowNewGlobalStatus -Root $script:context.Root -ExcludedFiles $ExcludedFiles
                }
                else {
                    $value = Read-WorkFlowJsonSnapshot -Path (Join-Path $script:context.Root $Name)
                }
                if ($value) { return $value }
                return $false
            }
            if ($result.fixtureError) { throw "The bound global/decision fixture contract failed: $($result.fixtureError)" }
            return $result
        }
        function Select-WorkFlowProposal($Proposal) {
            $Proposal.kind | Should -BeExactly 'HumanActionProposal'
            $Proposal.status | Should -BeExactly 'Open'
            Select-WorkFlowAction -Prefix $Proposal.summary -Contains
            $actual = Read-WorkFlowCapturedAction -ExpectedRequest $Proposal.request
            Test-WorkFlowFrozenRequest -Expected $Proposal.request -Actual $actual |
                Should -BeTrue -Because "all frozen fields must match; inspect $($script:context.CapturedActionEvidencePath)"
        }
        function Assert-WorkFlowGlobalIdentity([switch]$Probe, [string]$RestoreDraft, [switch]$RestoreSelection) {
            if ($Probe) {
                if (-not $PSBoundParameters.ContainsKey('RestoreDraft')) { throw 'The native identity probe must restore its captured test draft.' }
                $previous = @(Get-ChildItem -LiteralPath $script:context.Root -File -Filter 'global-status-*.json' | ForEach-Object Name)
                Send-WorkFlowKey 0x41 -Ctrl
                Paste-WorkFlowText 'How are the harbor and orchard checklists progressing?'
                Send-WorkFlowKey 0x0D
                $current = Wait-WorkFlowDecisionEvidence 'global-status-*.json' -ExcludedFiles $previous
                $current.conversationId | Should -BeExactly $script:context.GlobalConversationId
                $current.source.context.consoleSessionId | Should -BeExactly $script:context.GlobalConsoleId
                $current.source.context.scope | Should -BeExactly 'Global'
                Paste-WorkFlowText $RestoreDraft
                Assert-WorkFlowDraft @($RestoreDraft)
                if ($RestoreSelection) {
                    Set-WorkFlowLoadedSelection 1
                    Assert-WorkFlowLoadedCaret 1 '/workflow-loaded-gl'
                }
            }
            $snapshot = Read-WorkFlowConversation $script:context.GlobalConversationId
            $snapshot.scope | Should -BeExactly 'Global'
            $snapshot.id | Should -BeExactly $script:context.GlobalConversationId
            $snapshot.consoleSessionId | Should -BeExactly $script:context.GlobalConsoleId
            $snapshot.messages.id | Should -Contain $script:context.GlobalHelloMessage
        }
        function Assert-WorkFlowPreviewText([string[]]$Expected) {
            $pages = 0
            $frame = Wait-Until -TimeoutSec 15 -IntervalSec 0.1 -Because 'the asynchronous authority preparation has rendered its captured confirmation' -Condition {
                $capture = Read-WorkFlowUi
                if ($capture.Contains('Confirm the captured action')) { return $capture }
                return $false
            }
            $rows = Get-WorkFlowBodyRows $frame
            try {
                while ($true) {
                    if (@($Expected | Where-Object { -not (Test-WorkFlowPreviewText -Rows $rows -Expected $_) }).Count -eq 0) { return }
                    if ($pages -ge 16) { throw 'The human preview did not expose the required goal, project, scope and authority.' }
                    $previous = (Get-WorkFlowBodyRows (Read-WorkFlowUi)) -join "`n"
                    Send-WorkFlowKey 0x22
                    $pages++
                    $frame = Wait-Until -TimeoutSec 5 -Because 'the human-readable preview advances to its next page' -Condition {
                        $capture = Read-WorkFlowUi
                        if (((Get-WorkFlowBodyRows $capture) -join "`n") -cne $previous) { return $capture }
                        return $false
                    }
                    $rows = Merge-WorkFlowBodyRows -Earlier $rows -Later (Get-WorkFlowBodyRows $frame)
                }
            }
            finally { for ($i = 0; $i -lt $pages; $i++) { Send-WorkFlowKey 0x21 } }
        }
    }

    BeforeEach {
        if ($script:context.LoadProcess -and -not $script:context.LoadProcess.HasExited) {
            throw 'A prior owned load producer is still active; this case will not issue competing mutations.'
        }
    }

    It 'Agent Center chats globally without configuring a project' {
        @(Invoke-WorkFlowRequest $script:context.Connection 'project.list' -Params @{ limit = 100 }).data.items.Count | Should -Be 0
        $frame = Read-WorkFlowUi
        ($frame -split '\r?\n')[0].TrimEnd() | Should -BeExactly 'Global conversation'
        (Get-NativePasteInputBox $frame).hasRail | Should -BeFalse
        $frame | Should -Not -Match 'F6 · Work actions|Next responsibility'
        Paste-WorkFlowText 'Hello.'
        Assert-WorkFlowDraft @('Hello.')
        Send-WorkFlowKey 0x0D
        $hello = Wait-WorkFlowDecisionEvidence 'global-hello.json'
        $hello.scope | Should -BeExactly 'Global'
        $hello.source.text | Should -BeExactly 'Hello.'
        $hello.source.context.projectId | Should -BeNullOrEmpty
        $hello.projects.Count | Should -Be 0
        $hello.works.Count | Should -Be 0
        $script:context.GlobalConversationId = $hello.conversationId
        $script:context.GlobalConsoleId = $hello.consoleSessionId
        $script:context.GlobalHelloMessage = $hello.source.id
        $snapshot = Read-WorkFlowConversation $hello.conversationId
        $reply = @($snapshot.messages | Where-Object id -CEQ $hello.replyMessageId)
        $reply.Count | Should -Be 1
        $reply[0].parts.text | Should -Contain $hello.reply
        Wait-Until -TimeoutSec 15 -Because 'the actual global assistant response is visible without a dashboard' -Condition {
            (Read-WorkFlowUi).Contains($hello.reply)
        } | Should -BeTrue
        @(Invoke-WorkFlowRequest $script:context.Connection 'project.list' -Params @{ limit = 100 }).data.items.Count | Should -Be 0
        Assert-WorkFlowGlobalIdentity
        Save-WorkFlowVisualEvidence -Context $script:context -Name overview -Frame (Read-WorkFlowUi)
    }

    It 'Agent Center creates an execution project only after global conversation approval' {
        if (-not $script:context.GlobalConversationId) { throw 'The zero-project global conversation prerequisite did not complete.' }
        $configuration = $script:context.Fixture.global
        Assert-WorkFlowDraft @('')
        Paste-WorkFlowText $configuration.projectPrompt
        Send-WorkFlowKey 0x0D
        $evidence = Wait-WorkFlowDecisionEvidence 'global-project-proposal.json'
        $proposal = $evidence.proposal
        $proposal.conversationId | Should -BeExactly $script:context.GlobalConversationId
        $proposal.request.method | Should -BeExactly 'project.configure'
        $proposal.request.params.root | Should -BeExactly $configuration.projectRoot
        $proposal.request.params.coordinatorCapabilityId | Should -BeExactly 'ite2e-local-global'
        $proposal.request.params.workerCapabilityId | Should -BeExactly 'ite2e-local-global'
        $proposal.request.params.checkCapabilityId | Should -BeExactly 'native-check'
        $adapters = Get-Content -LiteralPath (Join-Path $script:context.Root 'adapters.json') -Raw | ConvertFrom-Json
        @($proposal.request.params.limits.PSObject.Properties).Count | Should -Be 7
        foreach ($limit in $adapters.conversationLimits.PSObject.Properties) {
            $proposal.request.params.limits.($limit.Name) | Should -Be $limit.Value
        }
        $destination = ($adapters.capabilities | Where-Object id -CEQ 'ite2e-local-global').adapter.approvedModelDestination
        ($proposal.preview | ConvertTo-Json -Depth 60).Contains($destination) | Should -BeTrue
        $evidence.source.text | Should -BeExactly $configuration.projectPrompt
        @(Invoke-WorkFlowRequest $script:context.Connection 'project.list' -Params @{ limit = 100 }).data.items.Count | Should -Be 0
        Select-WorkFlowProposal $proposal
        Assert-WorkFlowPreviewText @($configuration.projectName, $configuration.projectRoot, $destination)
        Send-WorkFlowKey 0x0D
        Read-WorkFlowCommandReceipt $proposal.request.commandId | Should -BeNullOrEmpty
        @(Invoke-WorkFlowRequest $script:context.Connection 'project.list' -Params @{ limit = 100 }).data.items.Count | Should -Be 0
        Send-WorkFlowKey 0x1B
        Read-WorkFlowCommandReceipt $proposal.request.commandId | Should -BeNullOrEmpty
        Select-WorkFlowProposal $proposal
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'only the frozen human project confirmation records the project' -Condition {
            Read-WorkFlowCommandReceipt $proposal.request.commandId
        }
        $receipt.status | Should -BeExactly 'ok'
        $project = (Invoke-WorkFlowRequest $script:context.Connection 'project.get' -Params @{ projectId = $receipt.data.projectId }).data
        $project.name | Should -BeExactly $configuration.projectName
        @(Invoke-WorkFlowRequest $script:context.Connection 'project.list' -Params @{ limit = 100 }).data.items.Count | Should -Be 1
        @(Invoke-WorkFlowRequest $script:context.Connection 'work.list' -Params @{ limit = 100 }).data.items.Count | Should -Be 0
        Wait-WorkFlowDecisionEvidence "global-submission-$($proposal.request.commandId).json" | Out-Null
        $recorded = (Read-WorkFlowConversation $script:context.GlobalConversationId).actionProposals | Where-Object id -CEQ $proposal.id
        $recorded.status | Should -BeExactly 'Submitted'
        Assert-WorkFlowGlobalIdentity
        @{ proposed = $evidence; receipt = $receipt; project = $project; submitted = $recorded } |
            ConvertTo-Json -Depth 80 | Set-Content -LiteralPath (Join-Path $script:context.Root 'global-project-approval.json') -Encoding utf8NoBOM
    }

    It 'Agent Center preserves one global draft across dashboard and work hints' {
        if (-not $script:context.GlobalConversationId) { throw 'The zero-project global conversation prerequisite did not complete.' }
        Send-WorkFlowKey 0x1B
        $script:context.Fixture = Initialize-WorkFlowFixture -Connection $script:context.Connection -EvidenceDirectory $script:context.Root -EnableIntake
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:context.Root 'work-flow.json') -Value $script:context.Fixture
        Select-WorkFlowAction 'Refresh current facts'
        Send-WorkFlowKey 0x1B
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText 'How are the harbor and orchard checklists progressing?'
        Send-WorkFlowKey 0x0D
        $status = Wait-WorkFlowDecisionEvidence 'global-status-*.json'
        $status.conversationId | Should -BeExactly $script:context.GlobalConversationId
        foreach ($work in $script:context.Fixture.works) { $status.works.work.id | Should -Contain $work.id }
        $reply = (Read-WorkFlowConversation $script:context.GlobalConversationId).messages | Where-Object id -CEQ $status.replyMessageId
        $reply.parts.text | Should -Contain $status.reply
        Open-WorkFlowWork 0
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText '/workflow-global-draft'
        Assert-WorkFlowDraft @('/workflow-global-draft')
        foreach ($index in @(1, 0, 1)) {
            Send-WorkFlowKey 0x76
            Wait-Until -TimeoutSec 15 -Because 'F7 explicitly opens the dashboard' -Condition { (Read-WorkFlowUi).Contains('Next responsibility') } | Should -BeTrue
            Send-WorkFlowKey 0x1B
            Open-WorkFlowWork $index
            Assert-WorkFlowDraft @('/workflow-global-draft')
            (Get-NativePasteInputBox (Read-WorkFlowUi)).scope | Should -BeExactly 'Global conversation'
        }
        Assert-WorkFlowGlobalIdentity
        Save-WorkFlowVisualEvidence -Context $script:context -Name current-work -Frame (Read-WorkFlowUi)
        Send-WorkFlowKey 0x74
        Wait-Until -TimeoutSec 15 -Because 'physical F5 exposes the read-only brief rather than changing the composer' -Condition {
            (Read-WorkFlowUi).Contains('F5 close details · PgUp/PgDn read · Esc input')
        } | Should -BeTrue
        Send-WorkFlowKey 0x0D
        Assert-WorkFlowDraft @('/workflow-global-draft')
        Read-WorkFlowUi | Should -Not -Match 'METHOD_UNSUPPORTED'
        Send-WorkFlowKey 0x1B
        Assert-WorkFlowSelection 1
        Send-WorkFlowKey 0x76
        Send-WorkFlowKey 0x75
        Wait-Until -TimeoutSec 15 -Because 'physical F6 focuses visible action cards without submitting the retained draft' -Condition {
            (Read-WorkFlowUi).Contains('Up/Down select · Enter open · Esc input')
        } | Should -BeTrue
        Paste-WorkFlowText '/workflow-focused-paste-must-be-ignored'
        foreach ($sample in 1..3) {
            Start-Sleep -Milliseconds 200
            Assert-WorkFlowDraft @('/workflow-global-draft')
            (Read-WorkFlowUi).Contains('Up/Down select · Enter open · Esc input') | Should -BeTrue
        }
        Send-WorkFlowKey 0x1B
        Send-WorkFlowKey 0x1B
        Assert-WorkFlowDraft @('/workflow-global-draft')
        Assert-WorkFlowSelection 1
        Assert-MeaningfulWorkFlowFrame
        foreach ($index in 0..1) {
            $view = Read-WorkFlowWork $index
            $view.work.lifecycle | Should -BeExactly 'Draft'
            $view.taskSummaries | Should -BeNullOrEmpty
        }
        Assert-WorkFlowGlobalIdentity -Probe -RestoreDraft '/workflow-global-draft'
    }

    It 'Agent Center preserves global input through sustained dashboard navigation' {
        $script:context.SawStaleFacts = $false
        Open-WorkFlowWork 0
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText '/workflow-loaded-global'
        Assert-WorkFlowDraft @('/workflow-loaded-global')
        Set-WorkFlowLoadedSelection 0
        Assert-WorkFlowLoadedCaret 0 '/workflow-loaded-gl'
        Open-WorkFlowWork 1
        Assert-WorkFlowDraft @('/workflow-loaded-global')
        Assert-WorkFlowLoadedCaret 1 '/workflow-loaded-gl'
        $producer = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\fixtures\Invoke-AgentCenterWorkFlowLoad.ps1'))
        $invocation = "& '$($producer.Replace("'", "''"))' -EvidenceDirectory '$($script:context.Root.Replace("'", "''"))' -Sustained"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invocation))
        $monitor = @{
            Clock = [Diagnostics.Stopwatch]::StartNew()
            LastCompleted = 0; LastAdvanceSeconds = 0; LastRecordedSeconds = -1; LastReceiptSpanSeconds = 0
        }
        $proof = @{
            primarySwitchBoundSeconds = 15; minimumReceiptSpanSeconds = 180; minimumSuccessfulUpdates = 784
            producerBudgetSeconds = 240; globalSustainedScenarioExercised = $false
            timings = [Collections.Generic.List[object]]::new()
            checkpoints = [Collections.Generic.List[object]]::new()
            selectionChecks = [Collections.Generic.List[object]]::new()
        }
        $proofPath = Join-Path $script:context.Root 'loaded-switch-timings.json'
        $script:context.LoadProcess = Start-Process -FilePath 'pwsh.exe' -ArgumentList @('-NoProfile', '-EncodedCommand', $encoded) -PassThru -NoNewWindow `
            -RedirectStandardOutput (Join-Path $script:context.Root 'load.stdout.txt') `
            -RedirectStandardError (Join-Path $script:context.Root 'load.stderr.txt')
        try {
            foreach ($stage in (Get-WorkFlowSustainedSwitchPlan)) {
                $before = Wait-WorkFlowLoadCheckpoint -Context $script:context -Monitor $monitor `
                    -TargetSeconds $stage.targetSeconds -MinimumUpdates $stage.minimumUpdates
                $proof.checkpoints.Add(@{ phase = $stage.name; before = $before; observedUtc = [datetime]::UtcNow })
                if ($stage.afterStop) {
                    $proof.stopRequestedUtc = [datetime]::UtcNow
                    Stop-WorkFlowLoadProducer -Context $script:context
                    $proof.stopCompletedUtc = [datetime]::UtcNow
                    $script:context.LoadProcess.ExitCode | Should -Be 0
                    $load = Read-WorkFlowJsonSnapshot -Path (Join-Path $script:context.Root 'load-result.json')
                    $load.failed | Should -BeFalse
                    $load.completed | Should -BeGreaterOrEqual 784
                    $load.receiptSpanSeconds | Should -BeGreaterOrEqual 180
                }
                elseif ($stage.targetSeconds -gt 0) {
                    $before.receiptSpanSeconds | Should -BeLessOrEqual ($stage.targetSeconds + 15) `
                        -Because 'the scheduled physical switches must occur around the requested load checkpoints'
                }
                foreach ($index in $stage.indices) {
                    $clock = [Diagnostics.Stopwatch]::StartNew()
                    $sentUtc = [datetime]::UtcNow
                    $draft = '/workflow-loaded-global'
                    $timing = @{
                        phase = $stage.name; work = $script:context.Fixture.works[$index].goal
                        draft = $draft; sentUtc = $sentUtc; verified = $false
                        afterStop = $stage.afterStop; observerSeconds = $monitor.Clock.Elapsed.TotalSeconds
                        sinceStopSeconds = $(if ($stage.afterStop) { ($sentUtc - $proof.stopCompletedUtc).TotalSeconds } else { $null })
                    }
                    $proof.timings.Add($timing)
                    try {
                        Send-WorkFlowKey 0x76
                        Send-WorkFlowKey 0x71
                        Send-WorkFlowKey 0x1B
                        Assert-WorkFlowSelection $index
                        Assert-WorkFlowDraft @($draft)
                        $prefix = '/workflow-loaded-gl'
                        Assert-WorkFlowLoadedCaret $index $prefix
                        $clock.Stop()
                        $clock.Elapsed.TotalSeconds | Should -BeLessOrEqual 15 -Because 'round11 stalled physical F2 beyond this unchanged primary bound'
                        $timing.verified = $true
                    }
                    finally {
                        $clock.Stop()
                        $timing.seconds = $clock.Elapsed.TotalSeconds
                        Write-WorkFlowJsonSnapshot -Path $proofPath -Value $proof
                    }
                    if ($stage.name -ceq 'around-130s') {
                        $selectionClock = [Diagnostics.Stopwatch]::StartNew()
                        $replacement = '/workflow-loaded-<sel>obal'
                        Paste-WorkFlowText '<sel>'
                        Assert-WorkFlowDraft @($replacement)
                        Read-WorkFlowUi | Should -Not -Match 'METHOD_UNSUPPORTED'
                        Send-WorkFlowKey 0x41 -Ctrl
                        Paste-WorkFlowText $draft
                        Assert-WorkFlowDraft @($draft)
                        Set-WorkFlowLoadedSelection $index
                        Assert-WorkFlowLoadedCaret $index $prefix
                        $selectionClock.Stop()
                        $proof.selectionChecks.Add(@{
                            phase = $stage.name; work = $script:context.Fixture.works[$index].goal
                            replacement = $replacement; restoredDraft = $draft; restoredCaret = $prefix
                            seconds = $selectionClock.Elapsed.TotalSeconds; verified = $true
                        })
                        Write-WorkFlowJsonSnapshot -Path $proofPath -Value $proof
                    }
                }
                if (-not $stage.afterStop) {
                    $after = Wait-WorkFlowLoadCheckpoint -Context $script:context -Monitor $monitor
                    $after.completed | Should -BeGreaterThan $before.completed -Because 'the physical switch group must overlap actual committed updates'
                    $proof.checkpoints.Add(@{ phase = $stage.name; after = $after; observedUtc = [datetime]::UtcNow })
                }
                Write-WorkFlowJsonSnapshot -Path $proofPath -Value $proof
            }
            Wait-Until -TimeoutSec 15 -Because 'any observed stale stream recovers automatically without reopening or a manual refresh' -Condition {
                -not (Read-WorkFlowUi).Contains('Live facts are stale.')
            } | Should -BeTrue
            $proof.postStopFreshFactsSeconds = ([datetime]::UtcNow - $proof.stopCompletedUtc).TotalSeconds
            Assert-WorkFlowSelection 1
            Assert-WorkFlowDraft @('/workflow-loaded-global')
            Assert-WorkFlowGlobalIdentity
            $view = Read-WorkFlowWork 1
            $view.work.version | Should -Be $load.versions.($script:context.Fixture.works[1].id)
            $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($script:context.UiIdentity.pid)"
            $current.CreationDate | Should -Be $script:context.UiIdentity.created -Because 'the same Console process must remain open'
            Read-WorkFlowUi | Should -Not -Match 'Event stream is closed|Live facts are stale|METHOD_UNSUPPORTED'
            $records = @(Get-Content -LiteralPath (Join-Path $script:context.Root 'load-receipts.jsonl') | ConvertFrom-Json)
            $successful = @($records | Where-Object { $_.response.status -ceq 'ok' })
            $firstReceipt = [datetime]$successful[0].receivedUtc
            $lastReceipt = [datetime]$successful[-1].receivedUtc
            $proof.pressure = @{
                successfulReceipts = $successful.Count; firstReceiptUtc = $firstReceipt; lastReceiptUtc = $lastReceipt
                receiptSpanSeconds = ($lastReceipt - $firstReceipt).TotalSeconds; producerResult = $load
            }
            $successful.Count | Should -Be $load.completed
            $successful.Count | Should -BeGreaterOrEqual 784
            $proof.pressure.receiptSpanSeconds | Should -BeGreaterOrEqual 180 -Because 'idle time after producer exit cannot qualify sustained pressure'
            $proof.selectionChecks.Count | Should -Be 2
            Assert-WorkFlowGlobalIdentity -Probe -RestoreDraft '/workflow-loaded-global' -RestoreSelection
            $proof.globalSustainedScenarioExercised = $true
        }
        finally {
            $proof.observedStaleFacts = [bool]$script:context.SawStaleFacts
            try { Write-WorkFlowJsonSnapshot -Path $proofPath -Value $proof }
            finally { Stop-WorkFlowLoadProducer -Context $script:context }
        }
    }

    It 'Agent Center restores a fresh subscription after a slow reader overflows' {
        # This deliberately stalls its OWN pipe consumer, not the Console. A healthy
        # Console is not required to overflow to prove its loaded-switch contract.
        $stalled = Open-WorkFlowConnection -StateRoot $script:context.Runtime.stateRoot
        $fresh = $null
        try {
            $subscription = Invoke-WorkFlowRequest $stalled 'events.subscribe' -Params @{ scope = @{ kind = 'WorkList' } }
            $retiredId = $subscription.data.subscriptionId
            $work = (Read-WorkFlowWork 1).work
            $receipt = $null
            $produce = [Diagnostics.Stopwatch]::StartNew()
            $count = 0
            $receiptBytes = 0L
            while ($count -lt 1024 -and $produce.Elapsed.TotalSeconds -lt 120) {
                $receipt = Invoke-WorkFlowRequest $script:context.Connection 'work.control' -Mutation `
                    -Params @{ workId = $work.id; action = 'Hold' } `
                    -IfMatch @(@{ kind = 'Work'; id = $work.id; version = $work.version }) `
                    -ReceiptPath (Join-Path $script:context.Root 'slow-reader-receipts.jsonl')
                $work.version = $receipt.data.version
                $receiptBytes += $script:context.Connection.LastReceivedBytes
                $count++
            }
            $failure = $null
            $drain = [Diagnostics.Stopwatch]::StartNew()
            $setup = @{
                successfulReceipts = $count; responsePayloadBytes = $receiptBytes
                producerSeconds = $produce.Elapsed.TotalSeconds; producerBudgetSeconds = 120
                consumedEventFrames = 0; faultObserved = $false
            }
            try {
                while ($drain.Elapsed.TotalSeconds -lt 15) {
                    $frame = Read-WorkFlowFrame $stalled.Pipe -TimeoutMs ([Math]::Max(1, 15000 - [int]$drain.ElapsedMilliseconds))
                    $frame.subscriptionId | Should -BeExactly $retiredId
                    if ($frame.type -ceq 'stream_error') { $failure = $frame; break }
                    $frame.type | Should -BeExactly 'event'
                    $setup.consumedEventFrames++
                }
                if (-not $failure) { throw 'No recoverable stream marker was observed.' }
                $failure.failure.code | Should -BeExactly 'RESYNC_REQUIRED'
                $setup.faultObserved = $true
            }
            catch {
                $setup.failure = $_.Exception.Message
                throw "Fault setup did not establish RESYNC_REQUIRED after $count successful receipts ($receiptBytes response bytes); recovery was not exercised. $($_.Exception.Message)"
            }
            finally {
                $setup | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:context.Root 'subscription-fault-setup.json') -Encoding utf8NoBOM
            }
            $recover = [Diagnostics.Stopwatch]::StartNew()
            $stalled.Pipe.Dispose()
            $fresh = Open-WorkFlowConnection -StateRoot $script:context.Runtime.stateRoot
            $fresh.Welcome.storeId | Should -BeExactly $script:context.Fixture.storeId
            $fresh.Welcome.serviceInstanceId | Should -BeExactly $stalled.Welcome.serviceInstanceId
            $resubscribed = Invoke-WorkFlowRequest $fresh 'events.subscribe' -Params @{ scope = @{ kind = 'WorkList' } }
            $resubscribed.data.subscriptionId | Should -Not -Be $retiredId
            $snapshot = @($resubscribed.data.snapshot.items | Where-Object { $_.work.id -eq $work.id })
            $snapshot.Count | Should -Be 1
            $snapshot[0].work.version | Should -Be $receipt.data.version
            $authoritative = (Read-WorkFlowWork 1).work
            $snapshot[0].work.version | Should -Be $authoritative.version
            $authoritative.desiredAdvancement | Should -BeExactly 'Hold'
            $authoritative.lifecycle | Should -BeExactly 'Draft'
            $snapshot[0].work.desiredAdvancement | Should -Be $authoritative.desiredAdvancement
            $snapshot[0].work.lifecycle | Should -Be $authoritative.lifecycle
            $later = Invoke-WorkFlowRequest $script:context.Connection 'work.control' -Mutation `
                -Params @{ workId = $work.id; action = 'Hold' } `
                -IfMatch @(@{ kind = 'Work'; id = $work.id; version = $authoritative.version }) `
                -ReceiptPath (Join-Path $script:context.Root 'slow-reader-receipts.jsonl')
            $event = Read-WorkFlowFrame $fresh.Pipe
            $event.type | Should -BeExactly 'event'
            $event.subscriptionId | Should -BeExactly $resubscribed.data.subscriptionId
            $later.cursor | Should -Not -BeNullOrEmpty
            $event.cursor | Should -BeExactly $later.cursor
            $recover.Elapsed.TotalSeconds | Should -BeLessOrEqual 15 -Because 'the recovery bound starts only after the real fault is observed'
            @{
                boundary = 'owned raw-pipe subscriber; not forced Console overflow'
                retiredSubscription = $retiredId; freshSubscription = $resubscribed.data.subscriptionId
                failure = $failure; snapshot = $resubscribed; subsequentEvent = $event; produced = $count
            } | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath (Join-Path $script:context.Root 'subscription-recovery.json') -Encoding utf8NoBOM
        }
        finally {
            $stalled.Pipe.Dispose()
            if ($fresh) { $fresh.Pipe.Dispose() }
        }
    }

    It 'Agent Center records a typed intake answer through the native form' {
        Open-WorkFlowWork 1
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText '/workflow-intake-background'
        Assert-WorkFlowDraft @('/workflow-intake-background')
        Select-WorkFlowAction 'Choose a configured project'
        Select-WorkFlowAction 'Harbor Reports' -MenuAlreadyOpen
        Assert-WorkFlowDraft @('/workflow-intake-background')
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText 'ITE2E typed intake fixture only'
        Assert-WorkFlowDraft @('ITE2E typed intake fixture only')
        Send-WorkFlowKey 0x0D
        $created = Wait-Until -TimeoutSec 45 -Because 'the local coordinator records an intake through its actual bound MCP invocation' -Condition {
            $file = Join-Path $script:context.Root 'intake-created.json'
            if (Test-Path -LiteralPath $file) { return Get-Content -LiteralPath $file -Raw | ConvertFrom-Json }
            $errorFile = Join-Path $script:context.Root 'global-fixture-error.json'
            if (Test-Path -LiteralPath $errorFile) {
                return @{ fixtureError = (Get-Content -LiteralPath $errorFile -Raw); evidence = $errorFile }
            }
            foreach ($stateFile in Get-ChildItem -LiteralPath (Join-Path $script:context.Runtime.stateRoot 'invocations') -Filter '*.json') {
                $saved = Read-WorkFlowJsonSnapshot -Path $stateFile.FullName
                $failure = $saved.state.terminalObservation.data.errorText
                if ($failure) { return @{ fixtureError = $failure; evidence = $stateFile.FullName } }
            }
            return $false
        }
        if ($created.fixtureError) { throw "The real intake invocation failed: $($created.fixtureError). Evidence: $($created.evidence)" }
        $snapshot = Read-WorkFlowConversation $created.conversationId
        $question = @($snapshot.intakeRequests | Where-Object id -EQ $created.response.inputRequest.id)
        $question.Count | Should -Be 1
        $question = $question[0]
        $question.kind | Should -BeExactly 'IntakeRequest'
        $question.status | Should -BeExactly 'Open'
        Open-WorkFlowWork 1
        Paste-WorkFlowText '/workflow-intake-background'
        Assert-WorkFlowDraft @('/workflow-intake-background')
        $action = 'Choose the fixture report format'
        Select-WorkFlowAction $action -Contains
        Assert-WorkFlowField 'Format' ''
        Assert-WorkFlowField 'Include details' ''
        Save-WorkFlowVisualEvidence -Context $script:context -Name typed-intake -Frame (Read-WorkFlowUi)
        Send-WorkFlowKey 0x1B
        Assert-WorkFlowDraft @('/workflow-intake-background')
        Select-WorkFlowAction $action -Contains
        Paste-WorkFlowText '7'
        Assert-WorkFlowField 'Copies' '7'
        Send-WorkFlowKey 0x0D
        Send-WorkFlowKey 0x27
        Assert-WorkFlowField 'Format' 'plain'
        Send-WorkFlowKey 0x27
        Assert-WorkFlowField 'Format' 'table'
        Send-WorkFlowKey 0x0D
        Assert-WorkFlowField 'Include details' ''
        Send-WorkFlowKey 0x27
        Assert-WorkFlowField 'Include details' 'Yes'
        Send-WorkFlowKey 0x0D
        Paste-WorkFlowText 'harbor α'
        Assert-WorkFlowField 'Note' 'harbor α'
        Send-WorkFlowKey 0x0D -Ctrl
        $abandoned = Read-WorkFlowCapturedAction
        Read-WorkFlowCommandReceipt $abandoned.commandId | Should -BeNullOrEmpty
        Send-WorkFlowKey 0x1B
        Assert-WorkFlowField 'Note' 'harbor α'
        Assert-WorkFlowField 'Format' 'table'
        Send-WorkFlowKey 0x0D -Ctrl
        $captured = Read-WorkFlowCapturedAction
        $captured.commandId | Should -Not -Be $abandoned.commandId
        $captured.method | Should -BeExactly 'conversation.answer_input'
        $captured.params.requestId | Should -BeExactly $question.id
        $captured.params.action | Should -BeExactly 'Answer'
        @($captured.ifMatch).Count | Should -Be 1
        $captured.ifMatch[0].kind | Should -BeExactly 'IntakeRequest'
        $captured.ifMatch[0].id | Should -BeExactly $question.id
        $captured.ifMatch[0].version | Should -Be $question.version
        $captured.params.value.copies | Should -Be 7
        ($captured.params.value.copies -is [long] -or $captured.params.value.copies -is [int]) | Should -BeTrue
        $captured.params.value.format | Should -BeExactly 'table'
        $captured.params.value.includeDetails | Should -BeTrue
        ($captured.params.value.includeDetails -is [bool]) | Should -BeTrue
        $captured.params.value.note | Should -BeExactly 'harbor α'
        Assert-MeaningfulWorkFlowFrame
        (Read-WorkFlowUi) | Should -Not -Match ([regex]::Escape($question.id))
        Send-WorkFlowKey 0x0D
        Read-WorkFlowCommandReceipt $captured.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowCapturedAction).commandId | Should -BeExactly $captured.commandId
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'the native confirmation commits the exact typed intake answer' -Condition {
            Read-WorkFlowCommandReceipt $captured.commandId
        }
        $receipt.status | Should -BeExactly 'ok'
        $receipt.data.requestId | Should -BeExactly $question.id
        $receipt.data.state | Should -BeExactly 'Answered'
        $answered = @((Read-WorkFlowConversation $created.conversationId).intakeRequests | Where-Object id -EQ $question.id)[0]
        $answered.status | Should -BeExactly 'Answered'
        ($answered.answer | ConvertTo-Json -Compress) | Should -BeExactly ($captured.params.value | ConvertTo-Json -Compress)
        Read-WorkFlowCommandReceipt $abandoned.commandId | Should -BeNullOrEmpty
        Assert-WorkFlowSelection 1
        Assert-WorkFlowDraft @('/workflow-intake-background')
        Wait-Until -TimeoutSec 15 -Because 'the answered intake closes its form and returns control to the preserved editor' -Condition {
            $frame = Read-WorkFlowUi
            $body = (Get-WorkFlowBodyRows $frame) -join "`n"
            -not $frame.Contains('Confirm the captured action') -and $body -cnotmatch '(?m)^[> ] Note \*:'
        } | Should -BeTrue
        Wait-Until -TimeoutSec 30 -Because 'the local coordinator acknowledges the actual answer without starting work' -Condition {
            Test-Path -LiteralPath (Join-Path $script:context.Root 'intake-observed.json')
        } | Should -BeTrue
        foreach ($index in 0..1) {
            $view = Read-WorkFlowWork $index
            $view.work.lifecycle | Should -BeExactly 'Draft'
            $view.taskSummaries | Should -BeNullOrEmpty
        }
        @{
            boundary = 'scripted local ACP -> bound MCP -> IntakeRequest -> physical form -> durable human answer'
            excluded = 'No model autonomy, worker results, or post-work DecisionRequest continuation proved'
            captured = $captured; receipt = $receipt; answered = $answered
        } | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $script:context.Root 'typed-intake.json') -Encoding utf8NoBOM
    }

    It 'Agent Center confirms a global proposal without retargeting to another work hint' {
        Open-WorkFlowWork 1
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText 'Please hold Review the orchard checklist in Orchard Notes. Ask me before changing it.'
        Send-WorkFlowKey 0x0D
        $evidence = Wait-WorkFlowDecisionEvidence 'global-hold-proposal.json'
        $proposal = $evidence.proposal
        $beforeA = (Read-WorkFlowWork 0).work
        $beforeB = (Read-WorkFlowWork 1).work
        $proposal.conversationId | Should -BeExactly $script:context.GlobalConversationId
        $proposal.request.method | Should -BeExactly 'work.control'
        $proposal.request.params.workId | Should -BeExactly $beforeB.id
        $proposal.request.params.action | Should -BeExactly 'Hold'
        $proposal.request.ifMatch[0].version | Should -Be $beforeB.version
        $evidence.projects.id | Should -Contain $beforeA.projectId
        $evidence.projects.id | Should -Contain $beforeB.projectId
        Open-WorkFlowWork 0
        Paste-WorkFlowText '/workflow-retained-global-draft'
        Select-WorkFlowProposal $proposal
        Assert-WorkFlowPreviewText @($script:context.Fixture.works[1].goal, $script:context.Fixture.works[1].projectName)
        Assert-MeaningfulWorkFlowFrame
        Send-WorkFlowKey 0x0D
        Read-WorkFlowCommandReceipt $proposal.request.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowWork 0).work.version | Should -Be $beforeA.version
        (Read-WorkFlowWork 1).work.version | Should -Be $beforeB.version
        Send-WorkFlowKey 0x1B
        Assert-WorkFlowDraft @('/workflow-retained-global-draft')
        Read-WorkFlowCommandReceipt $proposal.request.commandId | Should -BeNullOrEmpty
        Select-WorkFlowProposal $proposal
        Paste-WorkFlowText '/must-not-edit-a-frozen-proposal'
        Assert-WorkFlowDraft @('/workflow-retained-global-draft')
        Test-WorkFlowFrozenRequest -Expected $proposal.request -Actual (Read-WorkFlowCapturedAction) | Should -BeTrue
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'only exact global confirmation changes the proposed work B' -Condition {
            Read-WorkFlowCommandReceipt $proposal.request.commandId
        }
        $receipt.status | Should -BeExactly 'ok'
        (Read-WorkFlowWork 1).work.version | Should -Be ($beforeB.version + 1)
        (Read-WorkFlowWork 0).work.version | Should -Be $beforeA.version
        Wait-WorkFlowDecisionEvidence "global-submission-$($proposal.request.commandId).json" | Out-Null
        Assert-WorkFlowGlobalIdentity
        @{ proposed = $evidence; receipt = $receipt; unchangedWork = $beforeA } |
            ConvertTo-Json -Depth 80 | Set-Content -LiteralPath (Join-Path $script:context.Root 'global-cross-work-confirmation.json') -Encoding utf8NoBOM
    }

    It 'Agent Center readable confirmation targets the selected work' {
        Open-WorkFlowWork 0
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText '/workflow-confirmation-draft'
        Assert-WorkFlowDraft @('/workflow-confirmation-draft')
        $beforeA = (Read-WorkFlowWork 0).work
        $beforeB = (Read-WorkFlowWork 1).work
        $action = 'Cancel work · ' + $script:context.Fixture.works[0].goal
        Select-WorkFlowAction $action
        Wait-Until -TimeoutSec 15 -Because 'the confirmation describes the captured target without system identifiers' -Condition {
            $frame = Read-WorkFlowUi
            $body = (Get-WorkFlowBodyRows $frame) -join "`n"
            $frame.Contains('Confirm the captured action') -and
                $body.Contains($script:context.Fixture.works[0].goal) -and $body.Contains('Harbor Reports')
        } | Should -BeTrue
        Assert-MeaningfulWorkFlowFrame
        $cancelledPreview = Read-WorkFlowCapturedAction
        $cancelledPreview.method | Should -BeExactly 'work.control'
        $cancelledPreview.params.workId | Should -BeExactly $beforeA.id
        $cancelledPreview.params.action | Should -BeExactly 'Cancel'
        @($cancelledPreview.ifMatch).Count | Should -Be 1
        $cancelledPreview.ifMatch[0].kind | Should -BeExactly 'Work'
        $cancelledPreview.ifMatch[0].id | Should -BeExactly $beforeA.id
        $cancelledPreview.ifMatch[0].version | Should -Be $beforeA.version
        Read-WorkFlowCommandReceipt $cancelledPreview.commandId | Should -BeNullOrEmpty

        Send-WorkFlowKey 0x0D
        (Read-WorkFlowCapturedAction).commandId | Should -BeExactly $cancelledPreview.commandId
        Read-WorkFlowCommandReceipt $cancelledPreview.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowWork 0).work.version | Should -Be $beforeA.version
        Send-WorkFlowKey 0x1B
        Wait-Until -TimeoutSec 15 -Because 'Escape dismisses the preview without executing it' -Condition {
            -not (Read-WorkFlowUi).Contains('Confirm the captured action')
        } | Should -BeTrue
        Assert-WorkFlowDraft @('/workflow-confirmation-draft')
        Read-WorkFlowCommandReceipt $cancelledPreview.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowWork 0).work.version | Should -Be $beforeA.version
        (Read-WorkFlowWork 1).work.version | Should -Be $beforeB.version

        Select-WorkFlowAction $action
        $confirmedPreview = Read-WorkFlowCapturedAction
        $confirmedPreview.commandId | Should -Not -Be $cancelledPreview.commandId
        $confirmedPreview.method | Should -BeExactly 'work.control'
        $confirmedPreview.params.workId | Should -BeExactly $beforeA.id
        $confirmedPreview.params.action | Should -BeExactly 'Cancel'
        $confirmedPreview.ifMatch[0].version | Should -Be $beforeA.version
        Assert-MeaningfulWorkFlowFrame
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'only explicit modified Enter commits the captured command identity' -Condition {
            Read-WorkFlowCommandReceipt $confirmedPreview.commandId
        }
        $receipt.status | Should -BeExactly 'ok'
        $receipt.data.version | Should -Be ($beforeA.version + 1)
        $receipt.data.desiredAdvancement | Should -BeExactly 'Cancel'
        $receipt.data.settlementOperationIds | Should -BeNullOrEmpty
        $afterA = Wait-Until -TimeoutSec 15 -Because 'the never-started work settles as cancelled without worker execution' -Condition {
            $work = (Read-WorkFlowWork 0).work
            if ($work.lifecycle -ceq 'Cancelled') { return $work }
            return $false
        }
        $afterA.version | Should -BeGreaterOrEqual $receipt.data.version
        (Read-WorkFlowWork 1).work.version | Should -Be $beforeB.version
        Read-WorkFlowCommandReceipt $cancelledPreview.commandId | Should -BeNullOrEmpty
        @{
            suppressedPreview = $cancelledPreview; committedPreview = $confirmedPreview
            receipt = $receipt; otherWorkBefore = $beforeB; cancelledWork = $afterA
            suppressionOracle = 'No durable human command receipt and unchanged authoritative Work versions; not a network packet capture'
        } | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $script:context.Root 'readable-confirmation.json') -Encoding utf8NoBOM
    }

    It 'Agent Center inspects and accepts the current captured report' {
        $root = Join-Path $script:context.Root 'beacon'
        New-Item -ItemType Directory -Path $root -Force | Out-Null
        $receipts = Join-Path $script:context.Root 'report-setup-receipts.jsonl'
        $project = Invoke-WorkFlowRequest $script:context.Connection 'project.configure' -Mutation -ReceiptPath $receipts -Params @{
            name = 'Beacon Reports'; root = $root
            coordinatorCapabilityId = 'ite2e-local-report'; workerCapabilityId = 'ite2e-local-report'; checkCapabilityId = 'native-check'
            limits = @{ concurrency = 1; executionAttempts = 1; evaluationAttempts = 1; coordinationTurns = 3
                contextRounds = 1; executionSeconds = 90; coordinationSeconds = 90 }
        }
        $draft = Invoke-WorkFlowRequest $script:context.Connection 'work.create_draft' -Mutation -ReceiptPath $receipts -Params @{
            projectId = $project.data.projectId; goal = 'Produce the fixture report'
            scope = @('report.txt'); exclusions = @(); context = @(); sourceMessageIds = @()
            criteria = @(@{ id = 'report'; description = 'The required captured report is present'; evidenceRule = 'artifact:report' })
            delivery = @{ kind = 'Report' }
        }
        $meta = [pscustomobject]@{ id = $draft.data.workId; projectId = $project.data.projectId
            projectName = 'Beacon Reports'; goal = 'Produce the fixture report' }
        $script:context.Fixture.works += $meta
        $index = $script:context.Fixture.works.Count - 1
        $view = Read-WorkFlowWork $index
        @{ id = $meta.id; projectId = $meta.projectId; workspaceId = $view.work.workspaceId } |
            ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:context.Root 'report-work.json') -Encoding utf8NoBOM
        $guard = @(@{ kind = 'Work'; id = $meta.id; version = $view.work.version })
        $grant = Invoke-WorkFlowRequest $script:context.Connection 'grant.preview' -Mutation -ReceiptPath $receipts `
            -IfMatch $guard -Params @{ workId = $meta.id; specRevision = 1; policyRevision = 1 }
        $started = Invoke-WorkFlowRequest $script:context.Connection 'work.start' -Mutation -ReceiptPath $receipts -IfMatch $guard `
            -Params @{ workId = $meta.id; specRevision = 1; projectPolicyRevision = 1; grantProposalId = $grant.data.grantProposalId }
        if ($started.status -ceq 'pending') {
            $operation = Wait-Until -TimeoutSec 30 -Because 'the recorded start operation actually provisions its workspace' -Condition {
                $response = Invoke-WorkFlowRequest $script:context.Connection 'operation.get' -Params @{ operationId = $started.operationId }
                $observed = Get-WorkFlowObservedOperation -Response $response -OperationId $started.operationId
                if ($observed.status -notin @('Pending', 'Running')) { return $observed }
                return $false
            }
            $operation.status | Should -BeExactly 'Succeeded' -Because ($operation | ConvertTo-Json -Depth 20 -Compress)
        }
        $ready = Wait-Until -TimeoutSec 120 -Because 'real planning, an acknowledged worker, capture, submission and settlement produce one report candidate' -Condition {
            $errorFile = Join-Path $script:context.Root 'report-fixture-error.json'
            if (Test-Path -LiteralPath $errorFile) {
                return @{ fixtureError = (Get-Content -LiteralPath $errorFile -Raw); evidence = $errorFile }
            }
            $view = Read-WorkFlowWork $index
            $blocked = @($view.obligations | Where-Object {
                $_.reason -in @('PROTOCOL_INCOMPLETE', 'EffectFailed', 'MissingSubmission', 'CoordinationAllowanceExhausted')
            })
            if ($blocked.Count) { return @{ fixtureError = ($blocked | ConvertTo-Json -Depth 20 -Compress); evidence = $receipts } }
            $file = Join-Path $script:context.Root 'report-submitted.json'
            if (-not $view.candidate -or -not (Test-Path -LiteralPath $file)) { return $false }
            $produced = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json -Depth 30
            $task = Invoke-WorkFlowRequest $script:context.Connection 'task.get' -Params @{ taskId = $produced.dispatch.taskId }
            if ($task.data.attempt.reservationHeld -eq $false) { return $view }
            return $false
        }
        if ($ready.fixtureError) { throw "The legitimate report execution contract failed: $($ready.fixtureError). Evidence: $($ready.evidence)" }
        $ready.taskSummaries.Count | Should -Be 1
        $ready.work.lifecycle | Should -BeExactly 'Active'
        $ready.work.currentAcceptanceId | Should -BeNullOrEmpty
        $candidate = $ready.candidate
        $candidate.status | Should -BeExactly 'Proposed'
        $candidate.destination.kind | Should -BeExactly 'Report'
        $candidate.artifacts.Count | Should -Be 1
        $result = (Invoke-WorkFlowRequest $script:context.Connection 'result.get' -Params @{ resultId = $candidate.integrationResultId }).data
        $result.disposition | Should -BeExactly 'Accepted'
        $result.body.outputs.Count | Should -Be 1
        $result.body.outputs[0].artifact.artifactId | Should -BeExactly $candidate.artifacts[0].artifactId
        $result.gates | Should -BeNullOrEmpty -Because 'artifact presence is not a code or OS check'
        $workspace = (Invoke-WorkFlowRequest $script:context.Connection 'workspace.get' -Params @{ workspaceId = $ready.work.workspaceId }).data
        $workspace.writer | Should -BeExactly 'None'
        Open-WorkFlowWork $index
        Select-WorkFlowAction 'Inspect fixed delivery and check evidence (read-only)'
        Wait-Until -TimeoutSec 15 -Because 'inspection displays the actual result and its evidence claim' -Condition {
            (Read-WorkFlowUi).Contains('The required immutable report artifact is present.')
        } | Should -BeTrue
        (Read-WorkFlowUi) | Should -Not -Match 'ITE2E CAPTURED REPORT 2026'
        Select-WorkFlowAction 'Read evidence: Produce the fixture report'
        Wait-Until -TimeoutSec 15 -Because 'the native evidence reader displays the captured report bytes before acceptance' -Condition {
            $frame = Read-WorkFlowUi
            $frame.Contains('ITE2E CAPTURED REPORT 2026') -and $frame.Contains('This fixed report proves artifact presence only.')
        } | Should -BeTrue
        Save-WorkFlowVisualEvidence -Context $script:context -Name fixed-report -Frame (Read-WorkFlowUi)
        Send-WorkFlowKey 0x1B
        Wait-Until -TimeoutSec 15 -Because 'Esc returns from read-only delivery content without discarding its inspection' -Condition {
            -not (Read-WorkFlowUi).Contains('ITE2E CAPTURED REPORT 2026')
        } | Should -BeTrue
        Assert-WorkFlowSelection $index
        $inspected = Read-WorkFlowWork $index
        $inspected.work.version | Should -Be $ready.work.version
        $inspected.work.currentAcceptanceId | Should -BeNullOrEmpty
        $inspected.candidate.id | Should -BeExactly $candidate.id
        $inspected.candidate.status | Should -BeExactly 'Proposed'
        $afterRead = (Invoke-WorkFlowRequest $script:context.Connection 'workspace.get' -Params @{ workspaceId = $ready.work.workspaceId }).data
        $afterRead.writer | Should -BeExactly $workspace.writer
        $afterRead.version | Should -Be $workspace.version
        Select-WorkFlowAction 'Accept the inspected delivery'
        $captured = Read-WorkFlowCapturedAction -MaxPages 32
        $captured.method | Should -BeExactly 'delivery.accept'
        $captured.params.candidateId | Should -BeExactly $candidate.id
        @($captured.ifMatch).Count | Should -Be 2
        $workGuard = @($captured.ifMatch | Where-Object kind -EQ 'Work')
        $candidateGuard = @($captured.ifMatch | Where-Object kind -EQ 'DeliveryCandidate')
        $workGuard.Count | Should -Be 1
        $candidateGuard.Count | Should -Be 1
        $workGuard[0].id | Should -BeExactly $ready.work.id
        $workGuard[0].version | Should -Be $ready.work.version
        $candidateGuard[0].id | Should -BeExactly $candidate.id
        $candidateGuard[0].version | Should -Be $candidate.version
        Assert-MeaningfulWorkFlowFrame
        (Read-WorkFlowUi) | Should -Not -Match ([regex]::Escape($candidate.id))
        Send-WorkFlowKey 0x0D
        (Read-WorkFlowCapturedAction -MaxPages 32).commandId | Should -BeExactly $captured.commandId
        Read-WorkFlowCommandReceipt $captured.commandId | Should -BeNullOrEmpty
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'explicit native acceptance commits the inspected candidate and its captured guards' -Condition {
            Read-WorkFlowCommandReceipt $captured.commandId
        }
        $receipt.status | Should -BeExactly 'ok'
        $completed = Wait-Until -TimeoutSec 15 -Because 'the real accepted report settles its work as completed' -Condition {
            $view = Read-WorkFlowWork $index
            if ($view.work.lifecycle -ceq 'Completed') { return $view }
            return $false
        }
        $completed.candidate.status | Should -BeExactly 'Accepted'
        $completed.candidate.id | Should -BeExactly $candidate.id
        $completed.work.currentAcceptanceId | Should -BeExactly $receipt.data.acceptanceId
        @{
            boundary = 'real bound planning/worker/capture/result/candidate followed by native inspection and explicit acceptance'
            qualification = 'One artifact-presence Report; not natural-model autonomy or code/OS qualification'
            before = $ready; inspected = $inspected; workspaceBefore = $workspace; workspaceAfterRead = $afterRead
            captured = $captured; receipt = $receipt; completed = $completed
        } | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath (Join-Path $script:context.Root 'report-acceptance.json') -Encoding utf8NoBOM
    }

    It 'Agent Center creates an initial brief and explicitly starts the captured work' {
        $project = $script:context.Fixture.decisionProject
        $project.projectId | Should -Not -BeNullOrEmpty
        Send-WorkFlowKey 0x1B
        $brief = $project.prompt
        Send-WorkFlowKey 0x41 -Ctrl
        Send-WorkFlowKey 0x08
        Assert-WorkFlowDraft @('')
        Paste-WorkFlowText $brief
        Send-WorkFlowKey 0x0D
        $drafted = Wait-WorkFlowDecisionEvidence 'decision-drafted.json'
        $drafted.params.projectId | Should -BeExactly $project.projectId
        $drafted.params.scope | Should -Be @('report.txt')
        $drafted.params.exclusions | Should -BeNullOrEmpty
        $drafted.params.sourceMessageIds | Should -Be @($drafted.sourceMessageId)
        $source = @((Read-WorkFlowConversation $drafted.conversationId).messages | Where-Object id -EQ $drafted.sourceMessageId)
        $source.Count | Should -Be 1
        $source[0].text | Should -BeExactly $brief
        $source[0].context.scope | Should -BeExactly 'Global'
        $source[0].text.Contains($project.root) | Should -BeTrue
        $drafted.conversationId | Should -BeExactly $script:context.GlobalConversationId
        $meta = [pscustomobject]@{ id = $drafted.created.data.workId; projectId = $project.projectId
            projectName = $project.projectName; goal = $project.goal }
        $script:context.Fixture.works += $meta
        $index = $script:context.Fixture.works.Count - 1
        $script:context.DecisionWorkIndex = $index
        $before = Read-WorkFlowWork $index
        $before.work.lifecycle | Should -BeExactly 'Draft'
        $before.work.desiredAdvancement | Should -BeExactly 'Hold'
        $before.taskSummaries | Should -BeNullOrEmpty
        $before.spec.goal | Should -BeExactly $project.goal
        $before.spec.scope | Should -Be @('report.txt')
        $before.spec.delivery.kind | Should -BeExactly 'Report'
        Open-WorkFlowWork $index
        $startProposal = Wait-WorkFlowDecisionEvidence 'decision-start-proposal.json'
        Select-WorkFlowProposal $startProposal
        Assert-WorkFlowPreviewText @($project.goal, $project.projectName, 'report.txt', 'Requested authority and execution limits')
        $abandoned = Read-WorkFlowCapturedAction
        $abandoned.method | Should -BeExactly 'work.start'
        Send-WorkFlowKey 0x0D
        Read-WorkFlowCommandReceipt $abandoned.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowWork $index).work.lifecycle | Should -BeExactly 'Draft'
        (Read-WorkFlowCapturedAction).commandId | Should -BeExactly $abandoned.commandId
        Send-WorkFlowKey 0x1B
        Read-WorkFlowCommandReceipt $abandoned.commandId | Should -BeNullOrEmpty
        Select-WorkFlowProposal $startProposal
        $captured = Read-WorkFlowCapturedAction
        $captured.method | Should -BeExactly 'work.start'
        $captured.commandId | Should -BeExactly $abandoned.commandId
        $captured.params.workId | Should -BeExactly $meta.id
        $captured.params.specRevision | Should -Be $before.work.currentSpecRevision
        $policy = (Invoke-WorkFlowRequest $script:context.Connection 'project.get' -Params @{ projectId = $project.projectId }).data
        $captured.params.projectPolicyRevision | Should -Be $policy.policyRevision
        $captured.params.grantProposalId | Should -Not -BeNullOrEmpty
        @($captured.ifMatch).Count | Should -Be 1
        $captured.ifMatch[0].kind | Should -BeExactly 'Work'
        $captured.ifMatch[0].id | Should -BeExactly $meta.id
        $captured.ifMatch[0].version | Should -Be $before.work.version
        Send-WorkFlowKey 0x0D
        Read-WorkFlowCommandReceipt $captured.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowCapturedAction).commandId | Should -BeExactly $captured.commandId
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'the explicit native Start records the exact frozen nonce' -Condition {
            Read-WorkFlowCommandReceipt $captured.commandId
        }
        $receipt.status | Should -BeIn @('ok', 'pending')
        $operation = $null
        if ($receipt.status -ceq 'pending') {
            $receipt.operationId | Should -Not -BeNullOrEmpty
            $operation = Wait-Until -TimeoutSec 30 -Because 'recorded Start is not proof of workspace provisioning completion' -Condition {
                $response = Invoke-WorkFlowRequest $script:context.Connection 'operation.get' -Params @{ operationId = $receipt.operationId }
                $observed = Get-WorkFlowObservedOperation -Response $response -OperationId $receipt.operationId
                if ($observed.status -notin @('Pending', 'Running')) { return $observed }
                return $false
            }
            $operation.status | Should -BeExactly 'Succeeded'
        }
        $requested = Wait-WorkFlowDecisionEvidence 'decision-context-requested.json'
        $after = Read-WorkFlowWork $index
        $after.work.lifecycle | Should -BeExactly 'Active'
        $after.work.currentGrantId | Should -Not -BeNullOrEmpty
        $requested.dispatch.workId | Should -BeExactly $meta.id
        $requested.dispatch.workspaceId | Should -BeExactly $after.work.workspaceId
        $task = (Invoke-WorkFlowRequest $script:context.Connection 'task.get' -Params @{ taskId = $requested.dispatch.taskId }).data
        $task.attempt.invocationId | Should -BeExactly $requested.invocationId
        $task.attempt.state | Should -BeExactly 'WaitingForContext'
        $task.contextRequests.Count | Should -Be 1
        $task.contextRequests[0].id | Should -BeExactly $requested.requested.inputRequest.id
        $task.contextRequests[0].status | Should -BeExactly 'Open'
        Test-WorkFlowFrozenRequest -Expected $startProposal.request -Actual $captured | Should -BeTrue
        Assert-WorkFlowGlobalIdentity
        @{
            boundary = 'native Home brief -> real bound intake draft -> readable authority preview -> frozen native Start -> actual waiting worker'
            drafted = $drafted; before = $before; captured = $captured; receipt = $receipt; operation = $operation; after = $after; waitingTask = $task
            qualification = 'Recorded Start, provisioning and context-waiting execution are distinct states; no running/completion claim from a receipt alone'
        } | ConvertTo-Json -Depth 60 | Set-Content -LiteralPath (Join-Path $script:context.Root 'initial-brief-start.json') -Encoding utf8NoBOM
    }

    It 'Agent Center applies a post-start decision through the bound worker continuation' {
        if (-not $script:context.ContainsKey('DecisionWorkIndex')) { throw 'The native initial-brief/Start prerequisite did not create its actual work.' }
        $index = $script:context.DecisionWorkIndex
        $meta = $script:context.Fixture.works[$index]
        $created = Wait-WorkFlowDecisionEvidence 'decision-created.json'
        $created.workId | Should -BeExactly $meta.id
        $question = (Invoke-WorkFlowRequest $script:context.Connection 'decision.get' -Params @{ decisionId = $created.requested.inputRequest.id }).data
        $question.kind | Should -BeExactly 'DecisionRequest'
        $question.purpose | Should -BeExactly 'TaskInput'
        $question.status | Should -BeExactly 'Open'
        $question.workId | Should -BeExactly $meta.id
        $question.contextRequestId | Should -BeExactly $created.context.id
        $question.application.requestId | Should -BeExactly $created.context.id
        Open-WorkFlowWork 1
        Send-WorkFlowKey 0x41 -Ctrl
        Paste-WorkFlowText '/workflow-decision-background'
        Assert-WorkFlowDraft @('/workflow-decision-background')
        Select-WorkFlowAction ('Respond: ' + $meta.goal)
        Assert-WorkFlowField 'Format' ''
        Assert-WorkFlowField 'Approved' ''
        Save-WorkFlowVisualEvidence -Context $script:context -Name post-start-question -Frame (Read-WorkFlowUi)
        Paste-WorkFlowText '2'
        Assert-WorkFlowField 'Copies' '2'
        Send-WorkFlowKey 0x0D
        Send-WorkFlowKey 0x27
        Assert-WorkFlowField 'Format' 'brief'
        Send-WorkFlowKey 0x27
        Assert-WorkFlowField 'Format' 'detailed'
        Send-WorkFlowKey 0x0D
        Send-WorkFlowKey 0x27
        Assert-WorkFlowField 'Approved' 'Yes'
        Send-WorkFlowKey 0x0D
        Paste-WorkFlowText 'harbor α'
        Assert-WorkFlowField 'Note' 'harbor α'
        Send-WorkFlowKey 0x0D -Ctrl
        $captured = Read-WorkFlowCapturedAction
        $captured.method | Should -BeExactly 'decision.answer'
        $captured.params.decisionId | Should -BeExactly $question.id
        @($captured.ifMatch).Count | Should -Be 1
        $captured.ifMatch[0].kind | Should -BeExactly 'DecisionRequest'
        $captured.ifMatch[0].id | Should -BeExactly $question.id
        $captured.ifMatch[0].version | Should -Be $question.version
        $captured.params.value.copies | Should -Be 2
        ($captured.params.value.copies -is [long] -or $captured.params.value.copies -is [int]) | Should -BeTrue
        $captured.params.value.format | Should -BeExactly 'detailed'
        $captured.params.value.approved | Should -BeTrue
        ($captured.params.value.approved -is [bool]) | Should -BeTrue
        $captured.params.value.note | Should -BeExactly 'harbor α'
        Send-WorkFlowKey 0x0D
        Read-WorkFlowCommandReceipt $captured.commandId | Should -BeNullOrEmpty
        (Read-WorkFlowCapturedAction).commandId | Should -BeExactly $captured.commandId
        (Invoke-WorkFlowRequest $script:context.Connection 'decision.get' -Params @{ decisionId = $question.id }).data.status | Should -BeExactly 'Open'
        Send-WorkFlowKey 0x0D -Ctrl
        $receipt = Wait-Until -TimeoutSec 15 -Because 'the native answer records the exact decision rather than an IntakeRequest' -Condition {
            Read-WorkFlowCommandReceipt $captured.commandId
        }
        $receipt.status | Should -BeExactly 'ok'
        $receipt.data.state | Should -BeExactly 'Recorded'
        $continued = Wait-WorkFlowDecisionEvidence 'decision-continuation.json'
        $continued.continuation.requestId | Should -BeExactly $question.contextRequestId
        $continued.dispatch.workId | Should -BeExactly $meta.id
        ($continued.answer | ConvertTo-Json -Compress) | Should -BeExactly ($captured.params.value | ConvertTo-Json -Compress)
        $produced = Wait-WorkFlowDecisionEvidence 'decision-report-submitted.json'
        $produced.dispatch.id | Should -BeExactly $continued.dispatch.id
        $settled = Wait-Until -TimeoutSec 30 -Because 'the continued report is accepted as a real candidate and releases its original reservation' -Condition {
            $view = Read-WorkFlowWork $index
            $observed = (Invoke-WorkFlowRequest $script:context.Connection 'task.get' -Params @{ taskId = $created.taskId }).data
            if ($view.candidate.status -ceq 'Proposed' -and $observed.attempt.reservationHeld -eq $false) {
                return @{ view = $view; task = $observed }
            }
            return $false
        }
        $settled.view.work.lifecycle | Should -BeExactly 'Active'
        $settled.view.work.currentAcceptanceId | Should -BeNullOrEmpty
        $resolved = (Invoke-WorkFlowRequest $script:context.Connection 'decision.get' -Params @{ decisionId = $question.id }).data
        $resolved.status | Should -BeExactly 'Resolved'
        $resolved.answerId | Should -BeExactly $receipt.data.answerId
        $resolved.applicationId | Should -BeExactly $receipt.data.applicationId
        $task = $settled.task
        $task.attempt.id | Should -BeExactly $continued.dispatch.attemptId
        $task.attempt.invocationId | Should -BeExactly $continued.invocationId
        $context = @($task.contextRequests | Where-Object id -EQ $question.contextRequestId)
        $context.Count | Should -Be 1
        $context[0].status | Should -BeExactly 'Applied'
        $context[0].continuationId | Should -BeExactly $continued.continuation.id
        $artifact = $produced.captured.data.artifacts[0]
        $read = Invoke-WorkFlowRequest $script:context.Connection 'artifact.read' -Params @{
            artifactId = $artifact.artifactId; relativePath = 'report.txt'; limit = 16384
        }
        $expected = "ITE2E DECISION CONTINUATION REPORT`n" + ($continued.answer | ConvertTo-Json -Compress) + "`n"
        $read.data.kind | Should -BeExactly 'Text'
        $read.data.encoding | Should -BeExactly 'utf-8'
        $read.data.eof | Should -BeTrue
        $read.data.text | Should -BeExactly $expected
        Assert-WorkFlowSelection 1
        Assert-WorkFlowDraft @('/workflow-decision-background')
        @{
            boundary = 'actual ContextRequest -> bound TaskInput DecisionRequest -> native typed answer -> original continuation acknowledgement -> applied context and captured report'
            captured = $captured; receipt = $receipt; resolved = $resolved; continued = $continued; settled = $settled; produced = $produced; artifactRead = $read
        } | ConvertTo-Json -Depth 70 | Set-Content -LiteralPath (Join-Path $script:context.Root 'post-start-decision.json') -Encoding utf8NoBOM
    }

    AfterAll {
        if ($script:context) { Stop-WorkFlowTestContext -Context $script:context }
    }
}
