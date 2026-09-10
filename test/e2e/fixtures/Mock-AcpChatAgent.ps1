param(
    [Parameter(Mandatory)][string]$LogPath,
    [string]$ReleasePromptPath
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$sessionCounter = 0

function Send-AcpMessage {
    param([Parameter(Mandatory)][hashtable]$Message)

    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 20 -Compress))
    [Console]::Out.Flush()
}

function Write-FixtureLog {
    param([Parameter(Mandatory)][string]$Message)

    Add-Content -LiteralPath $LogPath -Value "$PID|$Message" -Encoding utf8
}

$pendingPrompt = $null
if ($ReleasePromptPath) {
    $reader = [System.IO.StreamReader]::new([Console]::OpenStandardInput(), [Text.Encoding]::UTF8)
    $readTask = $reader.ReadLineAsync()
}
while ($true) {
    # An opt-in held turn keeps reading ACP, including cancellation, until the test
    # releases it. Ordinary fixture prompts retain their immediate response.
    if ($pendingPrompt -and (Test-Path -LiteralPath $ReleasePromptPath)) {
        Write-FixtureLog -Message "released|$($pendingPrompt.Marker)"
        Send-AcpMessage @{
            jsonrpc = '2.0'
            method = 'session/update'
            params = @{
                sessionId = $pendingPrompt.SessionId
                update = @{
                    sessionUpdate = 'agent_message_chunk'
                    content = @{ type = 'text'; text = "`nACK_$($pendingPrompt.Marker)" }
                }
            }
        }
        Send-AcpMessage @{
            jsonrpc = '2.0'
            id = $pendingPrompt.Id
            result = @{ stopReason = 'end_turn' }
        }
        $pendingPrompt = $null
    }
    if ($ReleasePromptPath) {
        if (-not $readTask.Wait(50)) { continue }
        $line = $readTask.GetAwaiter().GetResult()
        $readTask = $reader.ReadLineAsync()
    }
    else {
        $line = [Console]::In.ReadLine()
    }
    if ($null -eq $line) { break }
    $request = $line | ConvertFrom-Json
    switch ($request.method) {
        'initialize' {
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    protocolVersion = 1
                    agentCapabilities = @{}
                    agentInfo = @{
                        name = 'Chat Fixture'
                        version = '1.0.0'
                    }
                }
            }
        }
        'session/new' {
            $sessionCounter++
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{ sessionId = "chat-fixture-$PID-$sessionCounter" }
            }
        }
        'session/prompt' {
            $sessionId = [string]$request.params.sessionId
            $promptText = (@($request.params.prompt) | ForEach-Object text) -join "`n"
            $marker = [regex]::Match($promptText, 'SCROLL_TURN_\d{2}_[a-f0-9]{32}').Value
            if (-not $marker) {
                throw 'prompt did not contain a completed-turn scroll marker'
            }
            $reply = "ACK_$marker"
            Write-FixtureLog -Message "prompt|$marker"

            $hold = $ReleasePromptPath -and $promptText -match '\bHOLD_FOR_RELEASE\b'
            if ($hold) {
                if ($pendingPrompt) { throw 'only one held fixture prompt is supported' }
                $pendingPrompt = @{ Id = $request.id; SessionId = $sessionId; Marker = $marker }
                $reply = "PENDING_$marker"
                Write-FixtureLog -Message "held|$marker"
            }
            Send-AcpMessage @{
                jsonrpc = '2.0'
                method = 'session/update'
                params = @{
                    sessionId = $sessionId
                    update = @{
                        sessionUpdate = 'agent_message_chunk'
                        content = @{
                            type = 'text'
                            text = $reply
                        }
                    }
                }
            }
            if (-not $hold) {
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{ stopReason = 'end_turn' }
                }
            }
        }
        'session/cancel' {
            Write-FixtureLog -Message "cancel|$($request.params.sessionId)"
            if ($pendingPrompt -and $pendingPrompt.SessionId -eq $request.params.sessionId) {
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $pendingPrompt.Id
                    result = @{ stopReason = 'cancelled' }
                }
                $pendingPrompt = $null
            }
        }
        default {
            if ($null -ne $request.id) {
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    error = @{
                        code = -32601
                        message = 'Method not found'
                    }
                }
            }
        }
    }
}
