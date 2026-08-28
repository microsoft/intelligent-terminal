param(
    [Parameter(Mandatory)][string]$LogPath
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$sessionCounter = 0
$currentMode = 'ask'
$sessionMcpServers = @{}

function Send-AcpMessage {
    param([Parameter(Mandatory)][hashtable]$Message)

    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 30 -Compress))
    [Console]::Out.Flush()
}

function Write-FixtureLog {
    param([Parameter(Mandatory)][string]$Message)

    Add-Content -LiteralPath $LogPath -Value "$PID|$Message" -Encoding utf8
}

function Get-SessionConfigOptions {
    @(
        @{
            id = 'mode'
            name = 'Mode'
            category = 'mode'
            type = 'select'
            currentValue = $currentMode
            options = @(
                @{ value = 'ask'; name = 'Ask'; description = 'Ask before editing' }
                @{ value = 'code'; name = 'Code'; description = 'Edit files directly' }
            )
        }
        @{
            id = 'reasoning'
            name = 'Reasoning'
            category = 'thought_level'
            type = 'select'
            currentValue = 'medium'
            options = @(
                @{ value = 'medium'; name = 'Medium' }
                @{ value = 'high'; name = 'High' }
            )
        }
        @{
            id = 'model'
            name = 'Model'
            category = 'model'
            type = 'select'
            currentValue = 'fixture-model'
            options = @(
                @{ value = 'fixture-model'; name = 'Fixture Model' }
            )
        }
    )
}

function Send-TextUpdate {
    param(
        [Parameter(Mandatory)][string]$SessionId,
        [Parameter(Mandatory)][string]$Text
    )

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

function Invoke-UserInputTool {
    param(
        [Parameter(Mandatory)]$Server
    )

    $headers = @{ 'mcp-protocol-version' = '2025-06-18' }
    foreach ($header in @($Server.headers)) {
        if ($header.name -and $header.value) {
            $headers[[string]$header.name] = [string]$header.value
        }
    }
    $body = @{
        jsonrpc = '2.0'
        id = 1
        method = 'tools/call'
        params = @{
            name = 'request_user_input'
            arguments = @{
                question = 'Choose the deterministic answer'
                choices = @('Alpha', 'Beta')
                allow_freeform = $true
            }
        }
    } | ConvertTo-Json -Depth 12 -Compress
    Invoke-RestMethod -Method Post -Uri $Server.url -Headers $headers -ContentType 'application/json' -Body $body
}

function Invoke-TerminalActionTool {
    param(
        [Parameter(Mandatory)]$Server,
        [Parameter(Mandatory)][string]$Marker
    )

    $headers = @{ 'mcp-protocol-version' = '2025-06-18' }
    foreach ($header in @($Server.headers)) {
        if ($header.name -and $header.value) {
            $headers[[string]$header.name] = [string]$header.value
        }
    }
    $body = @{
        jsonrpc = '2.0'
        id = 2
        method = 'tools/call'
        params = @{
            name = 'run_command_in_workspace'
            arguments = @{
                summary = "Direction $Marker"
                command = "echo $Marker"
                placement = 'new_tab'
                split_direction = 'auto'
            }
        }
    } | ConvertTo-Json -Depth 12 -Compress
    Invoke-RestMethod -Method Post -Uri $Server.url -Headers $headers -ContentType 'application/json' -Body $body
}

while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    switch ($request.method) {
        'initialize' {
            Write-FixtureLog -Message 'initialize'
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    protocolVersion = 1
                    agentCapabilities = @{
                        mcpCapabilities = @{ http = $true; sse = $false }
                        sessionCapabilities = @{ close = @{} }
                    }
                    agentInfo = @{
                        name = 'Interaction Fixture'
                        version = '1.0.0'
                    }
                }
            }
        }
        'session/new' {
            $sessionCounter++
            $sessionId = "interaction-$PID-$sessionCounter"
            $sessionMcpServers[$sessionId] = @($request.params.mcpServers) | Select-Object -First 1
            Write-FixtureLog -Message "session/new|$sessionId"
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    sessionId = $sessionId
                    configOptions = @(Get-SessionConfigOptions)
                }
            }
        }
        'session/close' {
            Write-FixtureLog -Message "session/close|$($request.params.sessionId)"
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{ configOptions = @(Get-SessionConfigOptions) }
            }
        }
        'session/set_config_option' {
            $currentMode = [string]$request.params.value
            Write-FixtureLog -Message "session/set_config_option|$($request.params.configId)|$currentMode"
            Send-AcpMessage @{
                jsonrpc = '2.0'
                method = 'session/update'
                params = @{
                    sessionId = [string]$request.params.sessionId
                    update = @{
                        sessionUpdate = 'config_option_update'
                        configOptions = @(Get-SessionConfigOptions)
                    }
                }
            }
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{}
            }
        }
        'session/prompt' {
            $sessionId = [string]$request.params.sessionId
            $promptText = (@($request.params.prompt) | ForEach-Object text) -join "`n"
            Write-FixtureLog -Message "session/prompt|$sessionId|$promptText"

            if ($promptText -match 'TOOL_FLOW') {
                Send-TextUpdate -SessionId $sessionId -Text 'BEFORE_TOOL_MARKER'
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    method = 'session/update'
                    params = @{
                        sessionId = $sessionId
                        update = @{
                            sessionUpdate = 'tool_call'
                            toolCallId = 'ite2e-tool'
                            title = 'Run integration command'
                            kind = 'execute'
                            status = 'in_progress'
                            rawInput = @{
                                command = 'echo TOOL_DETAIL_MARKER'
                                cwd = 'C:\ite2e-work'
                            }
                            content = @()
                            locations = @()
                        }
                    }
                }
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    method = 'session/update'
                    params = @{
                        sessionId = $sessionId
                        update = @{
                            sessionUpdate = 'tool_call_update'
                            toolCallId = 'ite2e-tool'
                            status = 'completed'
                            rawOutput = @{
                                stdout = 'TOOL_OUTPUT_MARKER'
                                exitCode = 7
                            }
                        }
                    }
                }
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    method = 'session/update'
                    params = @{
                        sessionId = $sessionId
                        update = @{
                            sessionUpdate = 'plan'
                            entries = @(
                                @{ content = 'PLAN_MARKER'; priority = 'medium'; status = 'completed' }
                            )
                        }
                    }
                }
                Send-TextUpdate -SessionId $sessionId -Text 'AFTER_TOOL_MARKER'
                Start-Sleep -Seconds 10
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{ stopReason = 'end_turn' }
                }
                Write-FixtureLog -Message 'tool-flow-complete'
            }
            elseif ($promptText -match 'ASK_INPUT') {
                $server = $sessionMcpServers[$sessionId]
                if (-not $server) {
                    throw 'session/new did not provide a Session MCP server'
                }
                $response = Invoke-UserInputTool -Server $server
                $result = $response.result.structuredContent | ConvertTo-Json -Depth 20 -Compress
                Write-FixtureLog -Message "user-input-result|$result"
                Send-TextUpdate -SessionId $sessionId -Text "INPUT_RESULT:$result"
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{ stopReason = 'end_turn' }
                }
            }
            elseif ($promptText -match 'TAB_DIRECTION_(?<marker>[A-F0-9]+)') {
                $server = $sessionMcpServers[$sessionId]
                if (-not $server) {
                    throw 'session/new did not provide a Session MCP server'
                }
                $marker = $Matches.marker
                $response = Invoke-TerminalActionTool -Server $server -Marker $marker
                $result = $response.result.structuredContent | ConvertTo-Json -Depth 20 -Compress
                Write-FixtureLog -Message "tab-direction-result|$result"
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{ stopReason = 'end_turn' }
                }
            }
            else {
                Send-TextUpdate -SessionId $sessionId -Text "ACK:$promptText"
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{ stopReason = 'end_turn' }
                }
            }
        }
        default {
            if ($null -ne $request.id) {
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    error = @{ code = -32601; message = 'Method not found' }
                }
            }
        }
    }
}
