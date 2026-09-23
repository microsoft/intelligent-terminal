# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

#Requires -Version 7.4
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SetupJson,
    [Parameter(Mandatory)][string]$ArtifactDirectory,
    [Parameter(Mandatory)][switch]$RunModel
)
$ErrorActionPreference = 'Stop'
if (-not $RunModel -or $env:ITE2E_PACKAGE -cne 'Dev') {
    throw 'This local-only validation consumes model credits. Explicitly set ITE2E_PACKAGE=Dev and pass -RunModel.'
}
. (Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterWorkFlow.ps1')
$setup = Get-Content -LiteralPath $SetupJson -Raw | ConvertFrom-Json
if ((Get-FileHash -LiteralPath $setup.wtaPath).Hash -ne $setup.wtaSha256) { throw 'WTA build changed since setup.' }
$root = $setup.stateDirectory
$adapter = Get-Content -LiteralPath (Join-Path $root 'adapters.json') -Raw | ConvertFrom-Json
$provider = @($adapter.capabilities | Where-Object id -eq $adapter.conversationCapabilityId)
if ($provider.Count -ne 1 -or [IO.Path]::GetFileName($provider[0].adapter.executable) -ine 'copilot.exe') {
    throw 'This real-provider qualification requires the explicitly configured Copilot executable, not a scripted adapter.'
}
if (Test-Path -LiteralPath $ArtifactDirectory) { throw 'Use a new artifact directory for each validation.' }
New-Item -ItemType Directory -Path $ArtifactDirectory | Out-Null
$receipts = Join-Path $ArtifactDirectory 'requests.jsonl'
$connection = Open-WorkFlowConnection -StateRoot $root
function Request([string]$Method, [hashtable]$Params = @{}, [switch]$Mutation) {
    Invoke-WorkFlowRequest -Connection $connection -Method $Method -Params $Params -Mutation:$Mutation -ReceiptPath $receipts
}
function Snapshot([string]$Kind, [string]$Id) {
    $reader = Open-WorkFlowConnection -StateRoot $root
    try {
        $scope = @{ kind = $Kind }
        if ($Id) { $scope.id = $Id }
        (Invoke-WorkFlowRequest -Connection $reader -Method events.subscribe -Params @{ scope = $scope }).data.snapshot
    }
    finally { $reader.Pipe.Dispose() }
}
function Save([string]$Name, $Value) {
    $Value | ConvertTo-Json -Depth 80 | Set-Content -LiteralPath (Join-Path $ArtifactDirectory "$Name.json") -Encoding utf8NoBOM
}
function Wait-Condition([string]$Description, [scriptblock]$Condition, [int]$Seconds = 240) {
    $watch = [Diagnostics.Stopwatch]::StartNew()
    do {
        $value = & $Condition
        if ($value) { return $value }
        Start-Sleep -Milliseconds 1500
    } while ($watch.Elapsed.TotalSeconds -lt $Seconds)
    throw "Timed out: $Description. Inspect saved real-provider state; no canned response or repair is substituted."
}
function Submit-Global([string]$Text) {
    Request conversation.submit @{
        conversationId = $conversation; clientMessageId = [guid]::NewGuid().ToString()
        text = $Text; attachments = @()
        context = @{ scope = 'Global'; consoleSessionId = $console; contextVersion = 1; projectId = $projectId }
    } -Mutation
}
function Wait-GlobalReply([string]$MessageId, [string]$Name) {
    Wait-Condition 'real coordinator response' {
        $snapshot = Snapshot Conversation $conversation
        Save $Name $snapshot
        $messages = @($snapshot.messages)
        $inputIndex = [array]::FindIndex([object[]]$messages, [Predicate[object]]{ param($m) $m.id -eq $MessageId })
        if ($inputIndex -lt 0) { throw 'Submitted input is missing from its conversation.' }
        $answers = @($messages | Select-Object -Skip ($inputIndex + 1) | Where-Object role -eq 'assistant')
        if (@($answers | Where-Object status -eq 'Interrupted').Count) {
            throw "Coordinator response was interrupted; inspect $Name.json."
        }
        if (@($answers | Where-Object status -eq 'Complete').Count) { return $snapshot }
    }
}
try {
    if (@((Request project.list @{ limit = 100 }).data.items).Count) { throw 'Validation requires a new isolated authority store.' }
    $projectRoot = Join-Path $root 'sample-project'
    New-Item -ItemType Directory -Path $projectRoot | Out-Null
    Copy-Item -Path (Join-Path $PSScriptRoot '..\fixtures\agent-center-real\*') -Destination $projectRoot
    & git -C $projectRoot init --quiet
    if ($LASTEXITCODE) { throw 'Fixture Git initialization failed.' }
    & git -C $projectRoot add .
    if ($LASTEXITCODE) { throw 'Fixture Git staging failed.' }
    & git -C $projectRoot -c user.name='Agent Center validation' -c user.email='validation@example.invalid' commit --quiet -m 'Unimplemented real-agent qualification fixture' -m "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`nCopilot-Session: 5c4d5766-493a-4890-8c5f-78903d9b1409"
    if ($LASTEXITCODE) { throw 'Fixture baseline commit failed.' }
    & node --test (Join-Path $projectRoot 'release.test.cjs') *> (Join-Path $ArtifactDirectory 'before.tap')
    if ($LASTEXITCODE -eq 0) { throw 'Negative control unexpectedly passed before the agent wrote code.' }
    $policy = (Request console.get).data
    $projectId = (Request project.configure @{
        name = 'Real release qualification'; root = $projectRoot
        coordinatorCapabilityId = $policy.capabilityId; workerCapabilityId = $policy.workerCapabilityId
        checkCapabilityId = $policy.checkCapabilityId
        limits = @{
            concurrency = 2; executionAttempts = 4; evaluationAttempts = 4
            coordinationTurns = 8; contextRounds = 2; executionSeconds = 300; coordinationSeconds = 120
        }
    } -Mutation).data.projectId
    $console = [guid]::NewGuid().ToString()
    $conversation = [guid]::NewGuid().ToString()
    Save 'context' @{ projectId = $projectId; consoleId = $console; conversationId = $conversation; setup = $setup }
    $input = Submit-Global @"
In the existing Real release qualification project at "$projectRoot", prepare TWO independent Work items toward a release. Work "Release checks": implement summarizeChecks in release.cjs according to SPEC.txt and run node --test release.test.cjs. Only change release.cjs; do not change tests or the specification. Work "Release review": independently inspect SPEC.txt, release.test.cjs and pipeline.json and write REVIEW.md explaining the required checks, what the tests prove and missing coverage; do not change code or tests. Both use a persistent WorkExecutor, each in its own workspace. Do not execute anything yourself. Create both drafts and propose starting both for my explicit approval in this turn; do not wait for either to finish. I will review the resulting files and independently run tests, not treat a response as acceptance.
"@
    $plan = Wait-GlobalReply $input.data.messageId 'coordinator-plan'
    $works = @((Request work.list @{ limit = 100 }).data.items)
    if ($works.Count -ne 2 -or @($works | Where-Object { $_.work.executionMode -ne 'WorkExecutor' -or $_.work.projectId -ne $projectId }).Count) {
        throw 'The real coordinator did not create exactly two independent executor Works in the approved project.'
    }
    $proposals = @($plan.actionProposals | Where-Object { $_.status -eq 'Open' -and $_.request.method -eq 'work.start' })
    if ($proposals.Count -ne 2) { throw 'Expected two real model-generated start proposals for explicit human-driver approval.' }
    Save 'approved-drafts' $works
    foreach ($proposal in $proposals) {
        if ($proposal.request.params.workId -notin @($works.work.id)) { throw 'Refusing an out-of-scope model proposal.' }
        $frozen = $proposal.request | ConvertTo-Json -Depth 60 | ConvertFrom-Json -AsHashtable
        $frozen.type = 'request'
        $frozen.requestId = [guid]::NewGuid().ToString()
        Write-WorkFlowFrame $connection.Pipe $frozen
        $response = Read-WorkFlowFrame $connection.Pipe
        Save "approval-$($proposal.id)" @{ request = $frozen; response = $response }
        if ($response.requestId -ne $frozen.requestId -or $response.status -notin @('ok', 'pending')) {
            throw 'Explicit approval of the frozen model proposal failed.'
        }
    }
    $running = Wait-Condition 'two real provider session bindings' {
        $current = @((Request work.list @{ limit = 100 }).data.items)
        Save 'starting' $current
        if (@($current | Where-Object { $_.continuation.state -in @('Unavailable', 'NeedsRecovery') }).Count) {
            throw 'A real executor could not start; inspect starting.json.'
        }
        if (@($current | Where-Object { $_.work.primarySession.providerSessionId }).Count -eq 2) { return ,$current }
    }
    $sessions = @($running.work.primarySession.providerSessionId)
    if (@($sessions | Select-Object -Unique).Count -ne 2) { throw 'Independent Works shared an executor session.' }
    if (@($running | Where-Object { $_.continuation.activity -eq 'Running' }).Count -ne 2) {
        throw 'Both executor sessions started, but this run did not observe actual concurrent execution.'
    }
    $turnCounts = @{}
    foreach ($work in $running) { $turnCounts[$work.work.id] = $work.executionSummary.turnCount }
    $statusInput = Submit-Global 'What is happening across my two Works, and what still needs my review? Only read actual Work records; do not send either executor another message or create more work.'
    $null = Wait-GlobalReply $statusInput.data.messageId 'coordinator-progress'
    $settled = Wait-Condition 'both real executors to become idle after actual work' {
        $current = @((Request work.list @{ limit = 100 }).data.items)
        Save 'executing' $current
        if (@($current | Where-Object { $_.continuation.state -in @('Unavailable', 'NeedsRecovery', 'WaitingForInput') }).Count) {
            throw 'Execution is blocked; inspect executing.json rather than claiming success.'
        }
        if (@($current | Where-Object { $_.continuation.activity -eq 'Idle' }).Count -eq 2) { return ,$current }
    } -Seconds 360
    foreach ($work in $settled) {
        if ($work.executionSummary.turnCount -ne $turnCounts[$work.work.id] -or $work.work.lifecycle -ne 'Active') {
            throw 'Coordination inquiry changed executor input, or a reply was mistaken for Work acceptance.'
        }
    }
    $checked = @()
    foreach ($work in $settled) {
        $workspace = (Request workspace.get @{ workspaceId = $work.work.workspaceId }).data
        $path = $workspace.localRoot
        if (-not $path) { throw 'Work workspace has no real local root.' }
        foreach ($file in @('release.test.cjs', 'SPEC.txt', 'pipeline.json')) {
            if ((Get-FileHash (Join-Path $path $file)).Hash -ne (Get-FileHash (Join-Path $projectRoot $file)).Hash) {
                throw "Agent changed protected qualification input: $file"
            }
        }
        $changes = @(& git -C $path status --porcelain)
        if ($LASTEXITCODE -ne 0 -or $changes.Count -ne 1) { throw 'Expected exactly one changed path per approved Work.' }
        $report = Join-Path $path 'REVIEW.md'
        if (Test-Path -LiteralPath $report) {
            if ($changes[0].Trim() -cne '?? REVIEW.md' -or
                (Get-FileHash (Join-Path $path 'release.cjs')).Hash -ne (Get-FileHash (Join-Path $projectRoot 'release.cjs')).Hash) {
                throw 'Report Work changed implementation rather than remaining independent.'
            }
            $text = Get-Content -LiteralPath $report -Raw
            if ($text.Length -lt 150 -or $text -notmatch 'integration' -or $text -notmatch 'release.test') {
                throw 'Real review report does not reference the actual test and pipeline inputs.'
            }
            Copy-Item -LiteralPath $report -Destination (Join-Path $ArtifactDirectory 'REVIEW.md')
            $checked += @{ workId = $work.work.id; kind = 'Report'; workspace = $path }
        }
        else {
            if ($changes[0].Trim() -cne 'M release.cjs') { throw 'Code Work changed an unapproved path.' }
            & node --test (Join-Path $path 'release.test.cjs') *> (Join-Path $ArtifactDirectory 'after.tap')
            if ($LASTEXITCODE -ne 0) { throw 'Independent test run of real agent changes failed.' }
            if ((Get-FileHash (Join-Path $path 'release.cjs')).Hash -eq (Get-FileHash (Join-Path $projectRoot 'release.cjs')).Hash) {
                throw 'Implementation was not changed by the real executor.'
            }
            Copy-Item -LiteralPath (Join-Path $path 'release.cjs') -Destination (Join-Path $ArtifactDirectory 'agent-written-release.cjs')
            $checked += @{ workId = $work.work.id; kind = 'Code'; workspace = $path }
        }
    }
    if (@($checked.kind | Select-Object -Unique).Count -ne 2) { throw 'Expected one code Work and one independent report Work.' }
    $codeId = ($checked | Where-Object kind -eq 'Code').workId
    $beforeContinue = $settled | Where-Object { $_.work.id -eq $codeId }
    $opened = (Request work.open @{ workId = $codeId } -Mutation).data
    $workContext = $opened.context | ConvertTo-Json | ConvertFrom-Json -AsHashtable
    $workContext.Remove('conversationId')
    $null = Request conversation.submit @{
        conversationId = $opened.conversation.id; clientMessageId = [guid]::NewGuid().ToString()
        text = 'Run node --test release.test.cjs again in this same Work and briefly report the outcome. Do not change any files.'
        attachments = @(); context = $workContext
    } -Mutation
    $continued = Wait-Condition 'follow-up in the same persistent executor session' {
        $current = (Request work.get @{ workId = $codeId }).data
        Save 'continuing' $current
        if ($current.continuation.state -in @('Unavailable', 'NeedsRecovery', 'WaitingForInput')) {
            throw 'The existing real executor could not process its next input.'
        }
        if ($current.continuation.activity -eq 'Idle' -and
            $current.executionSummary.turnCount -eq ($beforeContinue.executionSummary.turnCount + 1)) { return $current }
    }
    if ($continued.work.primarySession.providerSessionId -ne $beforeContinue.work.primarySession.providerSessionId -or
        $continued.work.primarySession.invocationId -ne $beforeContinue.work.primarySession.invocationId) {
        throw 'Continuation replaced the executor instead of using its persistent session.'
    }
    $codePath = Join-Path ($checked | Where-Object kind -eq 'Code').workspace 'release.cjs'
    if ((Get-FileHash $codePath).Hash -ne (Get-FileHash (Join-Path $ArtifactDirectory 'agent-written-release.cjs')).Hash) {
        throw 'The read-only follow-up changed the independently verified implementation.'
    }
    $finalWorks = @((Request work.list @{ limit = 100 }).data.items)
    if ($finalWorks.Count -ne 2) { throw 'Continuation created additional Work.' }
    foreach ($work in $finalWorks | Where-Object { $_.work.id -ne $codeId }) {
        if ($work.executionSummary.turnCount -ne $turnCounts[$work.work.id] -or
            $work.work.primarySession.providerSessionId -notin $sessions) {
            throw 'Continuing one Work changed the other executor.'
        }
    }
    Save 'continuation' @{ before = $beforeContinue; after = $continued }
    Save 'verification' @{
        status = 'passed'; realProvider = $provider[0].adapter.executable; build = $setup.wtaSha256
        distinctExecutorSessions = $sessions; afterCoordinationInputCounts = $turnCounts
        continuedInSameExecutor = $continued.work.primarySession
        independentlyChecked = $checked; finalWorks = $finalWorks
        acceptance = 'Not implemented: replies do not complete or accept Work.'
    }
    Write-Host "Real ACP validation passed. Evidence: $ArtifactDirectory"
}
catch {
    Save 'failure' @{ message = $_.Exception.Message; time = [datetime]::UtcNow }
    throw
}
finally { $connection.Pipe.Dispose() }
