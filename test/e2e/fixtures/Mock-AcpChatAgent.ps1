param(
    [Parameter(Mandatory)][string]$LogPath
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

while ($null -ne ($line = [Console]::In.ReadLine())) {
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
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{ stopReason = 'end_turn' }
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
