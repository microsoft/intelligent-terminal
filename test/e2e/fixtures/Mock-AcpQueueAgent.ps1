# A file-gated ACP server. All protocol/state work stays on this runspace;
# the StreamReader owns the asynchronous pipe read while gates are polled.
param(
    [Parameter(Mandatory)][string]$ControlDirectory,
    [Parameter(Mandatory)][string]$RecordDirectory,
    [switch]$HoldInitialize,
    [switch]$HoldSession
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$sequence = 0
$sessionCounter = 0
$promptCounter = 0
$sessions = @{}
$heldPrompts = @{}
$initializations = [System.Collections.Generic.List[object]]::new()
$pendingSessions = [System.Collections.Generic.List[object]]::new()

function Write-FixtureRecord {
    param([string]$Kind, [hashtable]$Fields = @{})

    $script:sequence++
    $record = @{ kind = $Kind; sequence = $script:sequence; pid = $PID }
    foreach ($key in $Fields.Keys) { $record[$key] = $Fields[$key] }
    $path = Join-Path $RecordDirectory ('{0:D10}-{1}.json' -f $script:sequence, $PID)
    [System.IO.File]::WriteAllText("$path.pending", ($record | ConvertTo-Json -Depth 10 -Compress))
    [System.IO.File]::Move("$path.pending", $path)
}

function Send-AcpMessage {
    param([hashtable]$Message)

    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 15 -Compress))
    [Console]::Out.Flush()
}

function Send-TextUpdate {
    param([string]$SessionId, [string]$Text)

    Send-AcpMessage @{
        jsonrpc = '2.0'
        method = 'session/update'
        params = @{
            sessionId = $SessionId
            update = @{
                sessionUpdate = 'agent_message_chunk'
                content = @{ type = 'text'; text = $Text }
            }
        }
    }
}

function Complete-FixturePrompt {
    param([hashtable]$Prompt, [string]$Reason, [string]$ErrorMessage = 'QUEUE_FIXTURE_FAILURE')

    $response = @{ jsonrpc = '2.0'; id = $Prompt.requestId }
    switch ($Reason) {
        'end_turn' {
            Send-TextUpdate -SessionId $Prompt.sessionId -Text "ACK_$($Prompt.marker)"
            $response.result = @{ stopReason = 'end_turn' }
        }
        'cancelled' { $response.error = @{ code = -32800; message = 'cancelled by fixture' } }
        'error' { $response.error = @{ code = -32603; message = $ErrorMessage } }
        default { throw "Unknown completion reason: $Reason" }
    }
    Send-AcpMessage $response
    Write-FixtureRecord -Kind completion -Fields @{
        sessionId = $Prompt.sessionId
        requestId = $Prompt.requestId
        marker = $Prompt.marker
        reason = $Reason
    }
}

function Receive-AcpRequest {
    param([hashtable]$Request)

    $hasId = $Request.ContainsKey('id') -and $null -ne $Request.id
    $sessionId = [string]$Request.params.sessionId
    if ($Request.method -eq 'session/cancel') {
        $prompt = $heldPrompts[$sessionId]
        Write-FixtureRecord -Kind cancel -Fields @{
            sessionId = $sessionId
            requestId = $Request.id
            marker = $(if ($prompt) { $prompt.marker } else { $null })
        }
        if ($prompt) { $prompt.cancelled = $true }
        if ($hasId) { Send-AcpMessage @{ jsonrpc = '2.0'; id = $Request.id; result = @{} } }
        return
    }
    if ($Request.method -eq 'session/close') {
        if ($heldPrompts.ContainsKey($sessionId)) {
            Complete-FixturePrompt -Prompt $heldPrompts[$sessionId] -Reason cancelled
            $heldPrompts.Remove($sessionId)
        }
        $sessions.Remove($sessionId)
        if ($hasId) { Send-AcpMessage @{ jsonrpc = '2.0'; id = $Request.id; result = @{} } }
        return
    }
    if (-not $hasId) { return }

    switch ($Request.method) {
        'initialize' {
            Write-FixtureRecord -Kind initialize -Fields @{ requestId = $Request.id }
            $initializations.Add($Request.id)
        }
        'session/new' {
            $script:sessionCounter++
            $sessionId = "queue-$PID-$script:sessionCounter"
            $sessions[$sessionId] = $true
            Write-FixtureRecord -Kind session -Fields @{
                sessionId = $sessionId
                requestId = $Request.id
                cwd = $Request.params.cwd
            }
            $pendingSessions.Add(@{ requestId = $Request.id; sessionId = $sessionId })
        }
        'session/prompt' {
            if (-not $sessions.ContainsKey($sessionId)) {
                Send-AcpMessage @{
                    jsonrpc = '2.0'; id = $Request.id
                    error = @{ code = -32602; message = 'Unknown session' }
                }
                return
            }
            $text = (@($Request.params.prompt) | Where-Object { $_.type -eq 'text' } | ForEach-Object { $_.text }) -join "`n"
            $script:promptCounter++
            $holdMarker = [regex]::Match($text, 'QUEUE_HOLD_[A-Z0-9_]+').Value
            $marker = if ($holdMarker) { $holdMarker } else { [regex]::Match($text, 'QUEUE_[A-Z0-9_]+').Value }
            if (-not $marker) { $marker = "QUEUE_GENERATED_${PID}_$script:promptCounter" }
            $prompt = @{
                sessionId = $sessionId; requestId = $Request.id
                marker = $marker; cancelled = $false
            }
            Write-FixtureRecord -Kind prompt -Fields @{
                sessionId = $sessionId; requestId = $Request.id; marker = $marker; text = $text
            }
            if ($heldPrompts.ContainsKey($sessionId)) {
                Write-FixtureRecord -Kind overlap -Fields @{
                    sessionId = $sessionId; requestId = $Request.id; marker = $marker
                }
                Complete-FixturePrompt -Prompt $prompt -Reason error -ErrorMessage 'QUEUE_FIXTURE_OVERLAP'
            }
            elseif ($holdMarker) {
                $heldPrompts[$sessionId] = $prompt
                Send-TextUpdate -SessionId $sessionId -Text "START_$marker"
            }
            else {
                Complete-FixturePrompt -Prompt $prompt -Reason end_turn
            }
        }
        default {
            Send-AcpMessage @{
                jsonrpc = '2.0'; id = $Request.id
                error = @{ code = -32601; message = 'Method not found' }
            }
        }
    }
}

# Console.In is a SyncTextReader: its ReadLineAsync can block the caller.
$reader = [System.IO.StreamReader]::new([Console]::OpenStandardInput(), [System.Text.UTF8Encoding]::new($false))
try {
    $readTask = $reader.ReadLineAsync()
    while ($true) {
        if ($initializations.Count -gt 0 -and
            (-not $HoldInitialize -or [System.IO.File]::Exists((Join-Path $ControlDirectory 'initialize.release')))) {
            foreach ($requestId in $initializations) {
                Send-AcpMessage @{
                    jsonrpc = '2.0'; id = $requestId
                    result = @{
                        protocolVersion = 1
                        agentInfo = @{ name = 'Queue Fixture'; version = '1.0.0' }
                        agentCapabilities = @{
                            mcpCapabilities = @{ http = $true }
                            sessionCapabilities = @{ close = @{} }
                        }
                    }
                }
                Write-FixtureRecord -Kind initialized -Fields @{ requestId = $requestId }
            }
            $initializations.Clear()
        }
        if ($pendingSessions.Count -gt 0 -and
            (-not $HoldSession -or [System.IO.File]::Exists((Join-Path $ControlDirectory 'session.release')))) {
            foreach ($session in $pendingSessions) {
                Send-AcpMessage @{ jsonrpc = '2.0'; id = $session.requestId; result = @{ sessionId = $session.sessionId } }
                Write-FixtureRecord -Kind session_ready -Fields $session
            }
            $pendingSessions.Clear()
        }
        foreach ($sessionId in @($heldPrompts.Keys)) {
            $prompt = $heldPrompts[$sessionId]
            $reason = $null
            if ($prompt.cancelled) {
                if (-not [System.IO.File]::Exists((Join-Path $ControlDirectory 'hold-cancel')) -or
                    [System.IO.File]::Exists((Join-Path $ControlDirectory 'release-cancel'))) {
                    $reason = 'cancelled'
                }
            }
            elseif ([System.IO.File]::Exists((Join-Path $ControlDirectory "$($prompt.marker).fail"))) {
                $reason = 'error'
            }
            elseif ([System.IO.File]::Exists((Join-Path $ControlDirectory "$($prompt.marker).release"))) {
                $reason = 'end_turn'
            }
            if ($reason) {
                Complete-FixturePrompt -Prompt $prompt -Reason $reason
                $heldPrompts.Remove($sessionId)
            }
        }
        if ($readTask.IsCompleted) {
            $line = $readTask.GetAwaiter().GetResult()
            if ($null -eq $line) { break }
            Receive-AcpRequest -Request (ConvertFrom-Json -InputObject $line -AsHashtable)
            $readTask = $reader.ReadLineAsync()
        }
        else {
            Start-Sleep -Milliseconds 10
        }
    }
}
finally {
    $reader.Dispose()
}
