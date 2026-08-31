param(
    [Parameter(Mandatory)][string]$LogPath
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$proposalServers = @{}
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

function Invoke-ProposalTool {
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
    $request = @{
        jsonrpc = '2.0'
        id = 1
        method = 'tools/call'
        params = @{
            name = 'run_command_in_current_shell'
            arguments = @{
                summary = "Run echo $Marker"
                command = "echo $Marker"
            }
        }
    } | ConvertTo-Json -Depth 12 -Compress
    $response = Invoke-RestMethod -Method Post -Uri $Server.url -Headers $headers -ContentType 'application/json' -Body $request
    Write-FixtureLog -Message "proposal|$($response.result.structuredContent.status)|$Marker"
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
                    agentCapabilities = @{
                        mcpCapabilities = @{ http = $true; sse = $false }
                    }
                    agentInfo = @{
                        name = 'Proposal Fixture'
                        version = '1.0.0'
                    }
                }
            }
        }
        'session/new' {
            $sessionCounter++
            $sessionId = "proposal-fixture-$PID-$sessionCounter"
            $proposalServers[$sessionId] = @($request.params.mcpServers) | Select-Object -First 1
            Write-FixtureLog -Message "session/new|$($request.params | ConvertTo-Json -Depth 20 -Compress)"
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{ sessionId = $sessionId }
            }
        }
        'session/prompt' {
            try {
                $promptText = (@($request.params.prompt) | ForEach-Object text) -join "`n"
                $marker = [regex]::Match($promptText, 'COMPACT[a-f0-9]{12}').Value
                if (-not $marker) {
                    throw 'prompt did not contain the compact-layout marker'
                }
                $proposalServer = $proposalServers[[string]$request.params.sessionId]
                if (-not $proposalServer) {
                    throw 'the prompt session did not provide a proposal MCP server'
                }
                Invoke-ProposalTool -Server $proposalServer -Marker $marker
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{ stopReason = 'end_turn' }
                }
            }
            catch {
                Write-FixtureLog -Message "error|$($_.Exception.Message)"
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    error = @{ code = -32603; message = $_.Exception.Message }
                }
            }
        }
    }
}
