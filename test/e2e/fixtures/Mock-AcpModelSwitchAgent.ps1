param(
    [Parameter(Mandatory)][string]$LogPath
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

function Send-AcpMessage {
    param([Parameter(Mandatory)][hashtable]$Message)

    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 20 -Compress))
    [Console]::Out.Flush()
}

function Write-RequestLog {
    param([Parameter(Mandatory)]$Request)

    $params = if ($null -eq $Request.params) { '{}' } else { $Request.params | ConvertTo-Json -Depth 20 -Compress }
    Add-Content -LiteralPath $LogPath -Value "$PID|$($Request.method)|$params" -Encoding utf8
}

$models = @(
    @{ value = 'initial-model'; name = 'Initial Model' }
    @{ value = 'effective-model'; name = 'Effective Model' }
)
$currentModel = 'initial-model'

while ($null -ne ($line = [Console]::In.ReadLine())) {
    $request = $line | ConvertFrom-Json
    Write-RequestLog -Request $request

    switch ($request.method) {
        'initialize' {
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    protocolVersion = 1
                    agentCapabilities = @{}
                    agentInfo = @{
                        name = 'Model Switch Fixture'
                        version = '1.0.0'
                    }
                }
            }
        }
        'session/new' {
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    sessionId = "model-switch-$PID"
                    configOptions = @(
                        @{
                            id = 'model'
                            name = 'Model'
                            category = 'model'
                            type = 'select'
                            currentValue = $currentModel
                            options = $models
                        }
                    )
                }
            }
        }
        { $_ -in @('session/set_config_option', 'session/set_model') } {
            $currentModel = if ($request.params.value) {
                [string]$request.params.value
            }
            else {
                [string]$request.params.modelId
            }
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    configOptions = @(
                        @{
                            id = 'model'
                            name = 'Model'
                            category = 'model'
                            type = 'select'
                            currentValue = $currentModel
                            options = $models
                        }
                    )
                }
            }
        }
    }
}
