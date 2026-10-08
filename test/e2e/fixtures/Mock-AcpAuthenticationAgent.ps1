param(
    [Parameter(Mandatory)][string]$LogPath,
    [string]$BrowserProgressTriggerPath = ''
)

# Legacy Agent auth methods intentionally have no browser/device-code metadata.
# session/new is AuthRequired until an explicitly advertised method completes.
# Personal OAuth takes twelve seconds (beyond the ordinary ten-second RPC budget).
# Continue reading while it waits: a cancelled UI must survive its late response.
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$authenticated = $false
$sessionCounter = 0
$clock = [Diagnostics.Stopwatch]::StartNew()
$pending = [Collections.Generic.List[object]]::new()
$authMethods = @(
    @{ id = 'oauth-personal'; name = 'Personal OAuth' }
    @{ id = 'other-method'; name = 'Other method' }
)

function Send-AcpMessage {
    param([Parameter(Mandatory)][hashtable]$Message)
    [Console]::Out.WriteLine(($Message | ConvertTo-Json -Depth 20 -Compress))
    [Console]::Out.Flush()
}

function Write-AuthenticationRecord {
    param(
        [Parameter(Mandatory)][string]$Method,
        [string]$MethodId = ''
    )
    # Only these four allowlisted fields are persisted; never log request params.
    [ordered]@{
        pid = $PID
        method = $Method
        methodId = $MethodId
        authenticated = $script:authenticated
    } | ConvertTo-Json -Compress | Add-Content -LiteralPath $LogPath -Encoding utf8
}

function Send-FixtureBrowserProgress {
    param([string]$State)
    # Invalid client ID and synthetic state: no real sign-in or provider tokens.
    Write-AuthenticationRecord -Method 'browser-progress' -MethodId 'oauth-personal'
    [Console]::Error.WriteLine("Open https://accounts.google.com/o/oauth2/v2/auth?client_id=invalid-acp-integration-fixture&redirect_uri=http%3A%2F%2F127.0.0.1%3A43210%2Fcallback&prompt=none&state=$State")
    [Console]::Error.Flush()
}

$reader = [IO.StreamReader]::new([Console]::OpenStandardInput(), [Text.Encoding]::UTF8)
try {
    $readTask = $reader.ReadLineAsync()
    while ($true) {
        foreach ($authentication in @($pending.ToArray())) {
            if ($clock.Elapsed.TotalSeconds -lt $authentication.Due) { continue }
            $authenticated = $true
            Write-AuthenticationRecord -Method 'authenticate' -MethodId $authentication.MethodId
            if ($authentication.BrowserProgress) {
                Send-FixtureBrowserProgress -State 'late-fixture-only'
            }
            Send-AcpMessage @{ jsonrpc = '2.0'; id = $authentication.Id; result = @{} }
            [void]$pending.Remove($authentication)
        }
        if (-not $readTask.Wait(50)) { continue }
        $line = $readTask.GetAwaiter().GetResult()
        if ($null -eq $line) { break }
        $readTask = $reader.ReadLineAsync()
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $request = $line | ConvertFrom-Json
        switch ($request.method) {
            'initialize' {
                Write-AuthenticationRecord -Method 'initialize'
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{
                        protocolVersion = 1
                        agentInfo = @{ name = 'Authentication Fixture'; version = '1.0.0' }
                        agentCapabilities = @{}
                        authMethods = $authMethods
                    }
                }
            }
            'session/new' {
                Write-AuthenticationRecord -Method 'session/new'
                if (-not $authenticated) {
                    Send-AcpMessage @{
                        jsonrpc = '2.0'; id = $request.id
                        error = @{ code = -32000; message = 'Authentication required' }
                    }
                    continue
                }
                $sessionCounter++
                Send-AcpMessage @{
                    jsonrpc = '2.0'
                    id = $request.id
                    result = @{
                        sessionId = "authentication-fixture-$PID-$sessionCounter"
                        # ACP 1.1 model discovery uses configOptions, not the removed models field.
                        configOptions = @(
                            @{
                                id = 'model'
                                name = 'Model'
                                category = 'model'
                                type = 'select'
                                currentValue = 'authentication-model'
                                options = @(
                                    @{ value = 'authentication-model'; name = 'Authentication Fixture Model' }
                                )
                            }
                        )
                    }
                }
            }
            'authenticate' {
                $methodId = [string]$request.params.methodId
                if ($methodId -notin @('oauth-personal', 'other-method')) {
                    Write-AuthenticationRecord -Method 'authenticate' -MethodId '<unadvertised>'
                    Send-AcpMessage @{
                        jsonrpc = '2.0'; id = $request.id
                        error = @{ code = -32602; message = 'Select an advertised authentication method' }
                    }
                    continue
                }
                Write-AuthenticationRecord -Method 'authenticate' -MethodId $methodId
                $delay = if ($methodId -eq 'oauth-personal') { 12 } else { 0 }
                $browserProgress = $methodId -eq 'oauth-personal' -and
                    $BrowserProgressTriggerPath -and (Test-Path -LiteralPath $BrowserProgressTriggerPath)
                if ($browserProgress) {
                    Send-FixtureBrowserProgress -State 'fixture-only'
                }
                $pending.Add([pscustomobject]@{
                    Id = $request.id; MethodId = $methodId
                    Due = $clock.Elapsed.TotalSeconds + $delay
                    BrowserProgress = $browserProgress
                })
            }
            { $_ -in @('session/cancel', 'session/close') } {
                Write-AuthenticationRecord -Method $request.method
                if ($null -ne $request.id) {
                    Send-AcpMessage @{ jsonrpc = '2.0'; id = $request.id; result = @{} }
                }
            }
            default {
                Write-AuthenticationRecord -Method 'unknown'
                if ($null -ne $request.id) {
                    Send-AcpMessage @{
                        jsonrpc = '2.0'; id = $request.id
                        error = @{ code = -32601; message = 'Method not found' }
                    }
                }
            }
        }
    }
}
finally {
    $reader.Dispose()
}
