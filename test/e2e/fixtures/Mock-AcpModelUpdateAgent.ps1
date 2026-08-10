$ErrorActionPreference = 'Stop'

[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

function Send-AcpMessage {
    param([Parameter(Mandatory)][hashtable]$Message)

    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 20 -Compress))
    [Console]::Out.Flush()
}

$models = @(
    @{ value = 'initial-model'; name = 'Initial Model' }
    @{ value = 'effective-model'; name = 'Effective Model' }
)

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
                        name = 'Model Update Fixture'
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
                    sessionId = 'model-update-session'
                    models = @{
                        currentModelId = 'initial-model'
                        availableModels = @(
                            @{ modelId = 'initial-model'; name = 'Initial Model' }
                            @{ modelId = 'effective-model'; name = 'Effective Model' }
                        )
                    }
                    configOptions = @(
                        @{
                            id = 'model'
                            name = 'Model'
                            category = 'model'
                            type = 'select'
                            currentValue = 'initial-model'
                            options = $models
                        }
                    )
                }
            }

            Start-Sleep -Milliseconds 500
            Send-AcpMessage @{
                jsonrpc = '2.0'
                method = 'session/update'
                params = @{
                    sessionId = 'model-update-session'
                    update = @{
                        sessionUpdate = 'config_option_update'
                        configOptions = @(
                            @{
                                id = 'model'
                                name = 'Model'
                                category = 'model'
                                type = 'select'
                                currentValue = 'effective-model'
                                options = $models
                            }
                        )
                    }
                }
            }
        }
    }
}
