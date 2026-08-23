$ErrorActionPreference = 'Stop'
$pendingPromptId = $null
$sessionNumber = 0

function Write-MockLog([string] $Message)
{
    Add-Content -LiteralPath $env:WTA_MOCK_ACP_LOG -Value $Message -Encoding utf8
}

function Send-Message($Message)
{
    $json = ConvertTo-Json -InputObject $Message -Compress -Depth 20
    [Console]::Out.WriteLine($json)
    [Console]::Out.Flush()
}

while (($line = [Console]::In.ReadLine()) -ne $null)
{
    $message = ConvertFrom-Json -InputObject $line
    switch ($message.method)
    {
        'initialize'
        {
            Write-MockLog 'initialize'
            Send-Message @{
                jsonrpc = '2.0'
                id = $message.id
                result = @{
                    protocolVersion = $message.params.protocolVersion
                    agentCapabilities = @{}
                    authMethods = @()
                }
            }
        }
        'session/new'
        {
            $sessionNumber++
            $sessionId = "mock-session-$sessionNumber"
            Write-MockLog "new:$sessionId"
            Send-Message @{
                jsonrpc = '2.0'
                id = $message.id
                result = @{ sessionId = $sessionId }
            }
        }
        'session/prompt'
        {
            $sessionId = $message.params.sessionId
            $text = $message.params.prompt[0].text
            Write-MockLog "prompt:$sessionId`:$text"
            if ($text -eq 'CRASH')
            {
                exit 17
            }
            if ($text -eq 'WAIT')
            {
                $pendingPromptId = $message.id
                continue
            }

            foreach ($chunk in @('mock ', 'reply'))
            {
                Send-Message @{
                    jsonrpc = '2.0'
                    method = 'session/update'
                    params = @{
                        sessionId = $sessionId
                        update = @{
                            sessionUpdate = 'agent_message_chunk'
                            content = @{
                                type = 'text'
                                text = $chunk
                            }
                        }
                    }
                }
            }
            Send-Message @{
                jsonrpc = '2.0'
                id = $message.id
                result = @{ stopReason = 'end_turn' }
            }
        }
        'session/cancel'
        {
            $sessionId = $message.params.sessionId
            Write-MockLog "cancel:$sessionId"
            if ($null -ne $pendingPromptId)
            {
                Send-Message @{
                    jsonrpc = '2.0'
                    id = $pendingPromptId
                    result = @{ stopReason = 'cancelled' }
                }
                $pendingPromptId = $null
            }
        }
        'session/close'
        {
            $sessionId = $message.params.sessionId
            Write-MockLog "close:$sessionId"
            Send-Message @{
                jsonrpc = '2.0'
                id = $message.id
                result = @{}
            }
        }
        default
        {
            if ($null -ne $message.id)
            {
                Send-Message @{
                    jsonrpc = '2.0'
                    id = $message.id
                    error = @{
                        code = -32601
                        message = "Method not found: $($message.method)"
                    }
                }
            }
        }
    }
}
