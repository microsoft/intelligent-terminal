# Deterministic ACP/MCP conformance fixture. This is not an AI agent or autonomy demonstration.
$ErrorActionPreference = 'Stop'
$script:Session = 'conformance-session'
$script:Endpoint = $null
$script:Authorization = $null

function Execute-Work($Invocation) {
    Tool-Permission 'Write work file' @{path = 'executor-turns.txt'} $true
    $turns = Join-Path (Get-Location).Path 'executor-turns.txt'
    [IO.File]::AppendAllText($turns, "$($Invocation.executorInput.turnId):$PID`n")
    $server = Join-Path (Get-Location).Path 'executor-server.json'
    if (-not (Test-Path $server)) {
        $escaped = $server.Replace("'", "''")
        $script = @'
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start()
[IO.File]::WriteAllText('__SERVER__', (@{pid=$PID;port=$listener.LocalEndpoint.Port} | ConvertTo-Json -Compress))
while ($true) {
    $client = $listener.AcceptTcpClient()
    $bytes = [Text.Encoding]::UTF8.GetBytes('alive')
    $client.GetStream().Write($bytes, 0, $bytes.Length)
    $client.Dispose()
}
'@
        $script = $script.Replace('__SERVER__', $escaped)
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
        $null = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList @('-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) -NoNewWindow -PassThru
        $limit = [DateTime]::UtcNow.AddSeconds(15)
        while (-not (Test-Path $server)) {
            if ([DateTime]::UtcNow -gt $limit) { throw 'Executor test server did not start' }
            Start-Sleep -Milliseconds 25
        }
    }
}

function Send-Frame($Frame) {
    [Console]::WriteLine(($Frame | ConvertTo-Json -Depth 100 -Compress))
    [Console]::Out.Flush()
}

function Reference($Record) {
    return @{ kind = $Record.kind; id = $Record.id; version = $Record.version }
}

function Tool-Permission([string]$Title, $Arguments, [bool]$ExpectedAllowed) {
    $requestId = [guid]::NewGuid().ToString()
    Send-Frame @{
        jsonrpc = '2.0'; id = $requestId; method = 'session/request_permission'
        params = @{
            sessionId = $script:Session
            toolCall = @{toolCallId = [guid]::NewGuid().ToString(); title = $Title; kind = 'other'; rawInput = $Arguments}
            options = @(
                @{optionId = 'always'; name = 'Always'; kind = 'allow_always'},
                @{optionId = 'once'; name = 'Once'; kind = 'allow_once'}
            )
        }
    }
    while ($null -ne ($line = [Console]::ReadLine())) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $response = $line | ConvertFrom-Json
        if ($response.method -eq 'session/cancel') { throw 'Controlled permission cancelled' }
        if ($response.id -ne $requestId) {
            throw "Unexpected ACP frame while awaiting permission: method=$($response.method), id=$($response.id), expected=$requestId, error=$($response.error.message), detail=$($response.error.data | ConvertTo-Json -Compress)"
        }
        if ($null -ne $response.error) { throw "ACP permission failed: $($response.error.message)" }
        if ($ExpectedAllowed) {
            if ($response.result.outcome.outcome -ne 'selected' -or $response.result.outcome.optionId -ne 'once') {
                throw "Bound MCP permission was not allowed once: $Title"
            }
        } elseif ($response.result.outcome.outcome -ne 'cancelled') {
            throw 'Unbound pre-acknowledgement permission was not denied'
        }
        return
    }
    throw 'ACP input closed before permission response'
}

function Work-Tool([string]$Name, $Params, $Subjects = @(), [switch]$AllowFailure) {
    $arguments = @{ params = $Params }
    $script:LastCommand = $null
    if (-not ($Name.EndsWith('_get') -or $Name.EndsWith('_list') -or $Name -eq 'artifact_read')) {
        $script:LastCommand = [guid]::NewGuid().ToString()
        $arguments.commandId = $script:LastCommand
    }
    if ($Subjects.Count -gt 0) { $arguments.ifMatch = @($Subjects) }
    $title = if ($Name -eq 'task_acknowledge') { $Name } else { "agent-center-work-$Name" }
    Tool-Permission $title $arguments $true
    $body = @{
        jsonrpc = '2.0'; id = [guid]::NewGuid().ToString(); method = 'tools/call'
        params = @{ name = $Name; arguments = $arguments }
    } | ConvertTo-Json -Depth 100 -Compress
    $rpc = Invoke-RestMethod -Uri $script:Endpoint -Method Post -Headers @{
        Authorization = $script:Authorization; Accept = 'application/json'; 'MCP-Protocol-Version' = $script:ProtocolVersion
    } -ContentType 'application/json' -Body ([Text.Encoding]::UTF8.GetBytes($body)) -TimeoutSec 90
    $response = $rpc.result.content[0].text | ConvertFrom-Json
    if (-not $AllowFailure -and $response.status -notin @('ok', 'pending', 'needs_input')) {
        throw "$Name failed: $($response | ConvertTo-Json -Depth 50 -Compress)"
    }
    return $response
}

function Invoke-ManagedGit([string[]]$Arguments) {
    $output = & git -c core.hooksPath=NUL -c commit.gpgsign=false @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Controlled Git operation failed: $output" }
    return ($output -join "`n").Trim()
}

function Coordinate-Report($Invocation, $View) {
    $workId = $View.work.id
    if ($null -ne $View.plan) {
        $tasks = (Work-Tool 'task_list' @{workId = $workId; limit = 100}).data.items
        $pending = $tasks | Where-Object { $_.currentResult.disposition -ne 'Accepted' } | Select-Object -First 1
        if ($null -eq $pending -or $pending.currentResult.disposition -eq 'ChangesRequested') {
            throw "Report conformance did not advance its declared checks: $($tasks | ConvertTo-Json -Depth 40 -Compress)"
        }
        $null = Work-Tool 'coordination_finish' @{
            turnId = $Invocation.coordinationInput.turnId; outcome = 'WaitingOnRecordedSubject'
            commandIds = @(); operationIds = @(); waitingSubject = (Reference $pending.task)
            explanation = 'Waiting for the admitted report task and its actual command check'
        }
        return
    }
    $project = (Work-Tool 'project_get' @{projectId = $View.work.projectId}).data
    $pinned = $View.spec.goal -like 'PINNED_REPORT_CONFORMANCE*'
    $check = @'
if ((Get-Content -Raw .\report.txt) -cne 'fixed report') { exit 41 }
if ((Get-Content -Raw .\evidence.json | ConvertFrom-Json).count -ne 1) { exit 42 }
'@
    $check += if ($pinned) {
        "`nif ((Get-Content -Raw .\value.txt) -cne '1') { exit 43 }"
    } else {
        "`nif (Test-Path .\value.txt) { exit 44 }"
    }
    $check += "`n[Console]::Out.Write('file-report-check-passed'); exit 0"
    $contract = @{
        clientKey = 'report'; role = 'Integration'; objective = $View.spec.goal
        scope = @($View.spec.scope); exclusions = @(); inputSlots = @()
        outputs = @(@{slot = 'report'; kind = 'Report'; required = $true}, @{slot = 'evidence'; kind = 'Evidence'; required = $true})
        criteria = @(@{
            id = 'report'; description = $View.spec.criteria[0].description
            evidenceRule = $View.spec.criteria[0].evidenceRule; requiredEvidence = @('report-check')
        })
        gateDefinitions = @(@{
            id = 'report-check'; revision = 1; criterionIds = @('report'); kind = 'Command'; required = $true
            recipe = @{
                executable = 'powershell.exe'; args = @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command', $check)
                cwdRelative = '.'; environmentRef = 'local-default'; timeoutSeconds = 30; evidenceParserId = 'process-exit-v1'
            }
        })
        reviewPolicy = @{revision = 1; required = $false; rule = 'AllRequiredGatesThenReview'}
        capabilityId = $project.workerCapabilityId; requiredForDelivery = $true
        resourceRequirements = @{workspaceId = $View.work.workspaceId; mode = $(if ($pinned) {'ReadOnly'} else {'ExclusiveWrite'})}
    }
    $tasks = @($contract)
    $edges = @()
    if ($pinned) {
        $contract.inputSlots = @(
            @{slot = 'code'; source = @{kind = 'Dependency'; sourceTaskKey = 'source'; outputSlot = 'code'}},
            @{slot = 'same-code'; source = @{kind = 'Dependency'; sourceTaskKey = 'source'; outputSlot = 'code'}}
        )
        $source = @{
            clientKey = 'source'; role = 'Contribution'; objective = 'SNAPSHOT_SOURCE_CONFORMANCE'
            scope = @('value.txt'); exclusions = @(); inputSlots = @()
            outputs = @(@{slot = 'code'; kind = 'GitCommit'; required = $true})
            criteria = @(@{id = 'code'; description = 'Value equals one'; requiredEvidence = @('code-check')})
            gateDefinitions = @(@{
                id = 'code-check'; revision = 1; criterionIds = @('code'); kind = 'Command'; required = $true
                recipe = @{
                    executable = 'powershell.exe'
                    args = @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command', 'if ((Get-Content -Raw .\value.txt) -cne ''1'') { exit 45 }; exit 0')
                    cwdRelative = '.'; environmentRef = 'local-default'; timeoutSeconds = 30; evidenceParserId = 'process-exit-v1'
                }
            })
            reviewPolicy = @{revision = 1; required = $false; rule = 'AllRequiredGatesThenReview'}
            capabilityId = $project.workerCapabilityId; requiredForDelivery = $true
            resourceRequirements = @{workspaceId = $View.work.workspaceId; mode = 'ExclusiveWrite'}
        }
        $tasks = @($source, $contract)
        $edges = @(@{sourceTaskKey = 'source'; outputSlot = 'code'; consumerTaskKey = 'report'; condition = 'GatePassed'; requiredGateIds = @('code-check')})
    }
    $proposal = Work-Tool 'plan_propose' @{
        workId = $workId; tasks = $tasks; edges = $edges; integrationTaskKey = 'report'; reason = 'Check immutable File outputs with exact pinned dependencies'
    } @((Reference $View.work))
    $fresh = (Work-Tool 'work_get' @{workId = $workId}).data
    $null = Work-Tool 'plan_apply' @{proposalId = $proposal.data.proposalId} @(
        (Reference $fresh.work), @{kind = 'PlanProposal'; id = $proposal.data.proposalId; version = $proposal.data.version}
    )
    $null = Work-Tool 'coordination_finish' @{
        turnId = $Invocation.coordinationInput.turnId; outcome = 'ActionsRecorded'
        commandIds = @($script:LastCommand); operationIds = @(); explanation = 'Admitted the report and immutable-input checks'
    }
}

function Coordinate($Invocation) {
    $workId = $Invocation.coordinationInput.scope.workId
    $view = (Work-Tool 'work_get' @{ workId = $workId }).data
    if ($view.spec.goal -like '*REPORT_CONFORMANCE*') {
        Coordinate-Report $Invocation $view
        return
    }
    $commands = @()
    $operations = @()
    if (@($Invocation.coordinationInput.snapshot.declinedAttempts).Count -gt 0 -and
        $null -ne $Invocation.coordinationInput.snapshot.declinedAttempts[0]) {
        $declined = $Invocation.coordinationInput.snapshot.declinedAttempts[0]
        if ($declined.state -ne 'Failed' -or $declined.declineReason -ne 'Required diagnostic evidence is unavailable in this assignment') {
            throw 'Declined contract lost its reason or terminal classification before coordination'
        }
        $attention = $view.obligations | Where-Object reason -eq 'ContractDeclined' | Select-Object -First 1
        if ($null -eq $attention) { throw 'Decline has no recorded repair owner' }
        $null = Work-Tool 'coordination_finish' @{
            turnId = $Invocation.coordinationInput.turnId; outcome = 'WaitingOnRecordedSubject'
            commandIds = @(); operationIds = @(); explanation = "Observed declined contract: $($declined.declineReason)"
            waitingSubject = (Reference $attention)
        }
        return
    }
    if ($null -eq $view.plan) {
        $project = (Work-Tool 'project_get' @{projectId = $view.work.projectId}).data
        $check = '$value = [int](Get-Content -Raw .\value.txt); if ($value -ne 1) { [Console]::Error.Write("expected 1, actual " + $value); exit 7 }; [Console]::Out.Write("actual check passed"); exit 0'
        $contract = @{
            clientKey = 'code'; role = 'Integration'; objective = 'Deliver checked value'
            scope = @('value.txt'); exclusions = @(); inputSlots = @()
            outputs = @(@{ slot = 'code'; kind = 'GitCommit'; required = $true })
            criteria = @(@{
                id = 'value'; description = 'Value equals one'
                evidenceRule = $view.spec.criteria[0].evidenceRule; requiredEvidence = @('value-check')
            })
            gateDefinitions = @(@{
                id = 'value-check'; revision = 1; criterionIds = @('value'); kind = 'Command'; required = $true
                recipe = @{
                    executable = 'powershell.exe'; args = @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command', $check)
                    cwdRelative = '.'; environmentRef = 'local-default'; timeoutSeconds = 30; evidenceParserId = 'process-exit-v1'
                }
            })
            reviewPolicy = @{ revision = 1; required = $false; rule = 'AllRequiredGatesThenReview' }
            capabilityId = $project.workerCapabilityId; requiredForDelivery = $true
            resourceRequirements = @{ workspaceId = $view.work.workspaceId; mode = 'ExclusiveWrite' }
        }
        if ($view.spec.goal -like 'DECLINE_CONFORMANCE*') { $contract.objective = 'DECLINE_CONFORMANCE' }
        $plan = @{
            workId = $workId; tasks = @($contract); edges = @(); integrationTaskKey = 'code'; reason = 'Conformance plan with an actual executable check'
        }
        $contract.outputs[0].kind = 'InvalidConformanceOutput'
        $invalid = Work-Tool 'plan_propose' $plan @((Reference $view.work)) -AllowFailure
        if ($invalid.status -ne 'error' -or $invalid.failure.code -ne 'INVALID_ARGUMENT' -or
            $invalid.failure.fieldErrors[0].path -ne 'params.tasks[0].outputs[0].kind' -or
            $invalid.failure.fieldErrors[0].message -notlike '*GitCommit*') {
            throw 'Invalid output kind did not return actionable plan-field feedback'
        }
        $unchanged = (Work-Tool 'work_get' @{workId = $workId}).data
        if ($unchanged.work.version -ne $view.work.version -or $null -ne $unchanged.plan) {
            throw 'Rejected output kind mutated the work or admitted a plan'
        }
        $contract.outputs[0].kind = $script:CommitOutputKind
        $proposal = Work-Tool 'plan_propose' @{
            workId = $workId; tasks = @($contract); edges = @(); integrationTaskKey = 'code'; reason = 'Conformance plan with an actual executable check'
        } @((Reference $view.work))
        $fresh = (Work-Tool 'work_get' @{ workId = $workId }).data
        $null = Work-Tool 'plan_apply' @{ proposalId = $proposal.data.proposalId } @(
            (Reference $fresh.work), @{kind = 'PlanProposal'; id = $proposal.data.proposalId; version = $proposal.data.version}
        )
        $commands += $script:LastCommand
    } else {
        $tasks = (Work-Tool 'task_list' @{ workId = $workId; limit = 100 }).data.items
        foreach ($taskView in $tasks) {
            foreach ($question in $taskView.contextRequests) {
                if ($question.status -eq 'Open') {
                    $reportId = ($question.question -split 'reportId=')[-1]
                    $report = (Work-Tool 'progress_get' @{reportId = $reportId}).data
                    if ($report.findings[0] -ne 'The approved value is one; confirm without changing inputs.') {
                        throw 'Coordinator did not receive the actual worker report body'
                    }
                    $null = Work-Tool 'task_answer_context' @{
                        requestId = $question.id; answer = 'The approved value is one.'; evidence = @(); compatibility = 'ExistingInputs'
                    } @((Reference $question))
                    $commands += $script:LastCommand
                }
            }
            if ($taskView.currentResult.disposition -eq 'ChangesRequested') {
                $resultView = (Work-Tool 'result_get' @{ resultId = $taskView.currentResult.id }).data
                $evidence = $resultView.gates[0].evidence[0]
                $manifest = (Work-Tool 'artifact_get' @{artifactId = $evidence.artifactId}).data
                if ($manifest.digest -ne $evidence.digest) { throw 'Diagnostic artifact reference changed' }
                $offset = 0
                $diagnostic = ''
                do {
                    $page = (Work-Tool 'artifact_read' @{
                        artifactId = $evidence.artifactId; relativePath = 'process.json'; offset = $offset; limit = 127
                    }).data
                    if ($page.kind -ne 'Text') { throw 'Captured check diagnostic is not readable text' }
                    $diagnostic += $page.text
                    $offset = $page.nextOffset
                } while (-not $page.eof)
                $diagnostic = $diagnostic | ConvertFrom-Json
                if ($diagnostic.outcome.exitCode -ne 7 -or $diagnostic.outcome.stderr -ne 'expected 1, actual 0') {
                    throw 'Rework requires the actual failing captured diagnostic, not metadata'
                }
                $response = Work-Tool 'task_rework' @{
                    taskId = $taskView.task.id; resultId = $resultView.result.id
                    reworkId = $resultView.result.reworkId; action = 'ReviseOutput'
                } @((Reference $taskView.task), (Reference $resultView.result))
                $commands += $script:LastCommand
                $operations += $response.data.operationId
            }
        }
    }
    if ($commands.Count -eq 0) { throw 'Conformance coordinator received an unexpected nonactionable trigger' }
    $null = Work-Tool 'coordination_finish' @{
        turnId = $Invocation.coordinationInput.turnId; outcome = 'ActionsRecorded'
        commandIds = @($commands); operationIds = @($operations); explanation = 'Deterministic conformance commands actually recorded'
    }
}

function Produce($Invocation, $Continuation) {
    $dispatch = $Invocation.dispatch
    Tool-Permission 'Run PowerShell' @{command = 'echo must-not-execute'} $false
    Tool-Permission 'agent-center-work-task_acknowledge' @{
        commandId = [guid]::NewGuid().ToString()
        params = @{dispatchId = $dispatch.id; taskRevision = ($dispatch.taskRevision + 1); disposition = 'Accepted'}
    } $false
    if ($dispatch.objective -eq 'DECLINE_CONFORMANCE') {
        $null = Work-Tool 'task_acknowledge' @{
            dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision; disposition = 'Declined'
            reason = 'Required diagnostic evidence is unavailable in this assignment'
        }
        return
    }
    $ack = @{ dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision; disposition = 'Accepted' }
    if ($null -ne $Continuation) { $ack.continuationId = $Continuation.id }
    $null = Work-Tool 'task_acknowledge' $ack
    $null = Work-Tool 'task_get' @{ taskId = $dispatch.taskId }
    if ($dispatch.objective -like '*REPORT_CONFORMANCE*') {
        if ($dispatch.objective -like 'PINNED_REPORT_CONFORMANCE*') {
            $code = $script:Inputs | Where-Object slot -eq 'code' | Select-Object -First 1
            $alias = $script:Inputs | Where-Object slot -eq 'same-code' | Select-Object -First 1
            if ($code.localPath -ne $alias.localPath -or (Get-Content -Raw (Join-Path $code.localPath 'value.txt')) -cne '1') {
                throw 'Report worker did not receive the exact accepted snapshot through both input aliases'
            }
        }
        [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'report.txt'), 'fixed report')
        [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'evidence.json'), '{"count":1}')
        $captured = Work-Tool 'artifact_capture' @{
            workspaceId = $dispatch.workspaceId
            sources = @(@{kind = 'File'; relativePath = 'report.txt'}, @{kind = 'File'; relativePath = 'evidence.json'})
            purpose = 'Output'
        }
        $body = @{
            dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision; inputManifestDigest = $dispatch.inputManifestDigest
            outputs = @(@{slot = 'report'; artifact = $captured.data.artifacts[0]}, @{slot = 'evidence'; artifact = $captured.data.artifacts[1]})
            criterionEvidence = @(@{criterionId = 'report'; evidence = @($captured.data.artifacts); claim = 'Native process must verify the fixed files and inputs'})
            summary = 'Only two immutable File outputs'; knownGaps = @()
        }
        $null = Work-Tool 'result_submit' $body
        # Deliberate synthetic workspace drift must not become the evaluator's input.
        [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'report.txt'), 'uncaptured changed report')
        [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'value.txt'), 'uncaptured changed code')
        return
    }
    if ($dispatch.objective -eq 'SNAPSHOT_SOURCE_CONFORMANCE') {
        [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'value.txt'), '1')
        $null = Invoke-ManagedGit @('add', '--all')
        $null = Invoke-ManagedGit @('-c', 'user.name=Conformance', '-c', 'user.email=conformance@localhost', 'commit', '-m', 'Pinned report input')
        $commit = Invoke-ManagedGit @('rev-parse', 'HEAD')
        $captured = Work-Tool 'artifact_capture' @{
            workspaceId = $dispatch.workspaceId; sources = @(@{kind = 'GitCommit'; commitId = $commit}); purpose = 'Output'
        }
        $null = Work-Tool 'result_submit' @{
            dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision; inputManifestDigest = $dispatch.inputManifestDigest
            outputs = @(@{slot = 'code'; artifact = $captured.data.artifacts[0]})
            criterionEvidence = @(@{criterionId = 'code'; evidence = @($captured.data.artifacts); claim = 'Native process verifies the committed value'})
            summary = 'Fixed upstream code for report integration'; knownGaps = @()
        }
        return
    }
    if ($null -eq $dispatch.rework -and $null -eq $Continuation) {
        $progress = Work-Tool 'task_report_progress' @{
            dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision
            activity = 'Verify existing approved context'; findings = @('The approved value is one; confirm without changing inputs.')
            artifacts = @(); nextStep = 'Ask coordinator to confirm the reported context'
        }
        $null = Work-Tool 'task_request_context' @{
            dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision
            question = "Confirm the already approved value; reportId=$($progress.data.reportId)"; target = @{kind = 'Coordinator'}
            inputs = @(); blocking = $true
        }
        return
    }
    # Deliberately seeded incorrect content is checked by a real process, never a fabricated GateResult.
    $value = if ($null -eq $dispatch.rework) { '0' } else { '1' }
    [IO.File]::WriteAllText((Join-Path (Get-Location).Path 'value.txt'), $value)
    $null = Invoke-ManagedGit @('add', '--all')
    $null = Invoke-ManagedGit @('-c', 'user.name=Conformance', '-c', 'user.email=conformance@localhost', 'commit', '-m', "Conformance value $value")
    $commit = Invoke-ManagedGit @('rev-parse', 'HEAD')
    $capture = Work-Tool 'artifact_capture' @{
        workspaceId = $dispatch.workspaceId; sources = @(@{kind = 'GitCommit'; commitId = $commit}); purpose = 'Output'
    }
    if ($capture.status -ne 'ok') { throw 'Artifact capture did not await its actual terminal operation' }
    $artifact = $capture.data.artifacts[0]
    $null = Work-Tool 'artifact_get' @{ artifactId = $artifact.artifactId }
    $null = Work-Tool 'task_report_progress' @{
        dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision
        activity = 'Captured real committed output'; findings = @("Wrote value $value")
        artifacts = @($artifact); nextStep = 'Submit for actual native command evaluation'
    }
    $body = @{
        dispatchId = $dispatch.id; taskRevision = $dispatch.taskRevision; inputManifestDigest = $dispatch.inputManifestDigest
        outputs = @(@{slot = 'code'; artifact = $artifact})
        criterionEvidence = @(@{criterionId = 'value'; evidence = @($artifact); claim = 'Captured content; command gate determines correctness'})
        summary = "Conformance committed value $value"; knownGaps = @()
    }
    if ($null -ne $dispatch.rework) { $body.supersedesResultId = $dispatch.rework.resultId }
    $null = Work-Tool 'result_submit' $body
}

while ($null -ne ($line = [Console]::ReadLine())) {
    $message = $line | ConvertFrom-Json
    if ($null -eq $message.id) { continue }
    try {
        switch ($message.method) {
            'initialize' {
                $result = @{ protocolVersion = 1; agentCapabilities = @{loadSession = $true; mcpCapabilities = @{http = $true}} }
            }
            { $_ -in @('session/new', 'session/load') } {
                if ($message.method -eq 'session/load' -and $message.params.sessionId -cne $script:Session) {
                    throw 'Controlled ACP session is unavailable'
                }
                $server = $message.params.mcpServers | Where-Object name -eq 'agent-center-work' | Select-Object -First 1
                $script:Endpoint = $server.url
                $script:Authorization = ($server.headers | Where-Object name -eq 'Authorization').value
                $headers = @{Authorization = $script:Authorization; Accept = 'application/json'}
                $initialize = @{jsonrpc = '2.0'; id = 'initialize'; method = 'initialize'; params = @{
                    protocolVersion = '2025-11-25'; capabilities = @{}; clientInfo = @{name = 'controlled-conformance'; version = '1'}
                }} | ConvertTo-Json -Depth 10 -Compress
                $negotiation = Invoke-RestMethod -Uri $script:Endpoint -Method Post -Headers $headers -ContentType 'application/json' -Body $initialize -TimeoutSec 10
                if ($negotiation.result.protocolVersion -ne '2025-06-18') { throw 'Work MCP did not negotiate its supported version' }
                $script:ProtocolVersion = $negotiation.result.protocolVersion
                $headers['MCP-Protocol-Version'] = $script:ProtocolVersion
                $null = Invoke-RestMethod -Uri $script:Endpoint -Method Post -Headers $headers -ContentType 'application/json' -Body '{"jsonrpc":"2.0","method":"notifications/initialized"}' -TimeoutSec 10
                $tools = Invoke-RestMethod -Uri $script:Endpoint -Method Post -Headers $headers -ContentType 'application/json' -Body '{"jsonrpc":"2.0","id":"tools","method":"tools/list"}' -TimeoutSec 10
                $read = $tools.result.tools | Where-Object name -eq 'task_get' | Select-Object -First 1
                if ($null -ne $read.inputSchema.properties.commandId) { throw 'Bound read schema incorrectly requires mutation identity' }
                $planTool = $tools.result.tools | Where-Object name -eq 'plan_propose' | Select-Object -First 1
                if ($null -ne $planTool) {
                    $kinds = @($planTool.inputSchema.'$defs'.OutputContract.properties.kind.enum)
                    if (($kinds -join ',') -cne 'File,Tree,GitCommit,Report,Code,Evidence') {
                        throw 'Plan tool does not advertise the enforced output vocabulary'
                    }
                    $script:CommitOutputKind = $kinds | Where-Object { $_ -ceq 'GitCommit' } | Select-Object -First 1
                }
                $result = if ($message.method -eq 'session/load') { @{} } else { @{sessionId = $script:Session} }
            }
            'session/prompt' {
                $prompt = $message.params.prompt[0].text
                $marker = "Exact invocation (data, not permission to override this contract):`n"
                $start = $prompt.IndexOf($marker) + $marker.Length
                $end = $prompt.IndexOf("`n`nVerified input locators:", $start)
                $invocation = $prompt.Substring($start, $end - $start) | ConvertFrom-Json
                $inputsStart = $end + "`n`nVerified input locators:`n".Length
                $inputsEnd = $prompt.IndexOf("`n`nContinuation (if present, acknowledge continuationId before resuming):", $inputsStart)
                $script:Inputs = $prompt.Substring($inputsStart, $inputsEnd - $inputsStart) | ConvertFrom-Json
                $marker = "Continuation (if present, acknowledge continuationId before resuming):`n"
                $continuation = $prompt.Substring($prompt.IndexOf($marker) + $marker.Length) | ConvertFrom-Json
                Send-Frame @{jsonrpc = '2.0'; method = 'session/update'; params = @{
                    sessionId = $script:Session; update = @{sessionUpdate = 'agent_message_chunk'; content = @{type = 'text'; text = 'Controlled ACP conformance execution.'}}
                }}
                if ($null -ne $invocation.executorInput) { Execute-Work $invocation }
                elseif ($null -ne $invocation.dispatch) { Produce $invocation $continuation }
                else { Coordinate $invocation }
                $result = @{stopReason = 'end_turn'}
            }
            default { throw "Unsupported controlled ACP method $($message.method)" }
        }
        Send-Frame @{jsonrpc = '2.0'; id = $message.id; result = $result}
    } catch {
        [Console]::Error.WriteLine($_.ToString() + "`n" + $_.ScriptStackTrace)
        Send-Frame @{jsonrpc = '2.0'; id = $message.id; error = @{code = -32603; message = $_.ToString()}}
    }
}
