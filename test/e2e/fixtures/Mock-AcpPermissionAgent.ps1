param(
    [Parameter(Mandatory)][string]$LogPath
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$sessionCounter = 0
$permissionCounter = 1000
$pending = @{}

function Send-AcpMessage {
    param([Parameter(Mandatory)][hashtable]$Message)

    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 20 -Compress))
    [Console]::Out.Flush()
}

function Write-FixtureLog {
    param([Parameter(Mandatory)][string]$Message)

    Add-Content -LiteralPath $LogPath -Value "$PID|$Message" -Encoding utf8
}

while ($null -ne ($line = [Console]::In.ReadLine())) {
    $message = $line | ConvertFrom-Json
    if ($message.method) {
        switch ($message.method) {
            'initialize' {
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $message.id
                    result = @{
                        protocolVersion = 1
                        agentCapabilities = @{}
                        agentInfo = @{
                            name = 'Permission Fixture'
                            version = '1.0.0'
                        }
                    }
                }
            }
            'session/new' {
                $sessionCounter++
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $message.id
                    result = @{ sessionId = "permission-fixture-$PID-$sessionCounter" }
                }
            }
            'session/prompt' {
                $promptText = (@($message.params.prompt) | ForEach-Object text) -join "`n"
                $marker = [regex]::Match($promptText, 'PERM[a-f0-9]{12}').Value
                if (-not $marker) {
                    Send-AcpMessage @{
                        jsonrpc = '2.0'
                        id = $message.id
                        error = @{ code = -32602; message = 'permission marker missing' }
                    }
                }
                else {
                    $permissionCounter++
                    $permissionId = $permissionCounter
                    $pending[[string]$permissionId] = @{
                        PromptId = $message.id
                        SessionId = [string]$message.params.sessionId
                        Marker = $marker
                    }
                    Write-FixtureLog -Message "permission-requested|$marker"
                    Send-AcpMessage @{
                        jsonrpc = '2.0'
                        id = $permissionId
                        method = 'session/request_permission'
                        params = @{
                            sessionId = [string]$message.params.sessionId
                            toolCall = @{
                                toolCallId = "permission-tool-$marker"
                                title = "Run safe fixture command $marker"
                                kind = 'execute'
                                rawInput = @{ command = "echo $marker" }
                            }
                            options = @(
                                @{ optionId = 'allow-once'; name = 'Allow once'; kind = 'allow_once' }
                                @{ optionId = 'allow-always'; name = 'Allow always'; kind = 'allow_always' }
                                @{ optionId = 'reject-once'; name = 'Reject'; kind = 'reject_once' }
                            )
                        }
                    }
                }
            }
            default {
                if ($null -ne $message.id) {
                    Send-AcpMessage @{
                        jsonrpc = '2.0'
                        id = $message.id
                        error = @{ code = -32601; message = 'Method not found' }
                    }
                }
            }
        }
    }
    else {
        $request = $pending[[string]$message.id]
        if ($request) {
            $outcome = $message.result.outcome
            $optionId = if ($outcome.outcome -eq 'selected') {
                [string]$outcome.optionId
            } else {
                'cancelled'
            }
            Write-FixtureLog -Message "permission-resolved|$optionId|$($request.Marker)"
            Send-AcpMessage @{
                jsonrpc = '2.0'
                method = 'session/update'
                params = @{
                    sessionId = $request.SessionId
                    update = @{
                        sessionUpdate = 'agent_message_chunk'
                        content = @{
                            type = 'text'
                            text = "PERMISSION_RESULT_$($request.Marker)_$optionId"
                        }
                    }
                }
            }
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.PromptId
                result = @{ stopReason = 'end_turn' }
            }
            $pending.Remove([string]$message.id)
        }
    }
}