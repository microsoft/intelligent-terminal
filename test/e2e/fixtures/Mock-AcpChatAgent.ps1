param(
    [Parameter(Mandatory)][string]$LogPath,
    [string]$ReleasePromptPath,
    [string]$HistoryPath,
    [string]$HistoryControlPath,
    [string]$StartupControlPath,
    [switch]$SupportsImages
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

function Test-HistoryHeld {
    if (-not $HistoryControlPath -or -not (Test-Path -LiteralPath $HistoryControlPath)) { return $false }
    $control = Get-Content -LiteralPath $HistoryControlPath -Raw | ConvertFrom-Json -AsHashtable
    return $control.mode -eq 'hold' -and [int]$control.pid -eq $PID
}

function Complete-HistoryRequest {
    param([Parameter(Mandatory)]$Request)
    $history = Get-Content -LiteralPath $HistoryPath -Raw | ConvertFrom-Json -AsHashtable
    $result = @{ sessions = @($history.sessions) }
    if ($history.pageSize) {
        $offset = if ($Request.params.cursor) { [int]$Request.params.cursor } else { 0 }
        $pageSize = [int]$history.pageSize
        if ($pageSize -le 0 -or $offset -lt 0) { throw 'invalid controlled history page' }
        $result.sessions = @($history.sessions | Select-Object -Skip $offset -First $pageSize)
        if ($offset + $result.sessions.Count -lt @($history.sessions).Count) {
            $result.nextCursor = [string]($offset + $result.sessions.Count)
        }
        Write-FixtureLog -Message "list-page|$([DateTimeOffset]::UtcNow.ToString('o'))|$offset|$($result.sessions.Count)"
    }
    Send-AcpMessage @{ jsonrpc = '2.0'; id = $Request.id; result = $result }
    Write-FixtureLog -Message "list-complete|$([DateTimeOffset]::UtcNow.ToString('o'))|$($Request.id)"
}

if ($StartupControlPath) {
    Write-FixtureLog -Message "start|$([DateTimeOffset]::UtcNow.ToString('o'))"
}

$pendingPrompt = $null
$pendingHistory = $null
if ($ReleasePromptPath -or $HistoryControlPath) {
    $reader = [System.IO.StreamReader]::new([Console]::OpenStandardInput(), [Text.Encoding]::UTF8)
    $readTask = $reader.ReadLineAsync()
}
while ($true) {
    if ($pendingHistory -and -not (Test-HistoryHeld)) {
        Complete-HistoryRequest $pendingHistory
        $pendingHistory = $null
    }
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
    if ($ReleasePromptPath -or $HistoryControlPath) {
        if (-not $readTask.Wait(50)) { continue }
        $line = $readTask.GetAwaiter().GetResult()
        $readTask = $reader.ReadLineAsync()
    }
    else {
        $line = [Console]::In.ReadLine()
    }
    if ($null -eq $line) { break }
    $request = $line | ConvertFrom-Json
    if ($StartupControlPath) { Write-FixtureLog -Message "method|$($request.method)" }
    switch ($request.method) {
        'initialize' {
            if ($HistoryPath -or $StartupControlPath) {
                Write-FixtureLog -Message "initialize|$([DateTimeOffset]::UtcNow.ToString('o'))"
            }
            if ($StartupControlPath) {
                $startup = Get-Content -LiteralPath $StartupControlPath -Raw |
                    ConvertFrom-Json -AsHashtable
                if ($startup.mode -notin @('ready', 'fail')) {
                    throw 'startup control must select ready or fail'
                }
                if ($startup.mode -eq 'fail') {
                    if ($startup['failureStderr']) {
                        [Console]::Error.WriteLine([string]$startup['failureStderr'])
                        [Console]::Error.Flush()
                    }
                    if ($startup.spawnChild) {
                        $program = [Diagnostics.Process]::GetCurrentProcess().MainModule.FileName
                        $child = Start-Process -FilePath $program -ArgumentList @(
                            '-NoLogo', '-NoProfile', '-NonInteractive', '-Command', 'Start-Sleep -Seconds 120'
                        ) -WindowStyle Hidden -PassThru
                        Write-FixtureLog -Message "child|$($child.Id)|$($child.StartTime.ToUniversalTime().Ticks)|$($child.Path)"
                    }
                    Send-AcpMessage @{
                        jsonrpc = '2.0'; id = $request.id
                        error = @{ code = -32603; message = 'Controlled ACP initialization failure' }
                    }
                    exit 23
                }
            }
            $capabilities = if ($SupportsImages) {
                @{ promptCapabilities = @{ image = $true } }
            } else { @{} }
            if ($HistoryPath) {
                $capabilities.sessionCapabilities = @{ list = @{}; close = @{} }
            }
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = @{
                    protocolVersion = 1
                    agentCapabilities = $capabilities
                    agentInfo = @{
                        name = 'Chat Fixture'
                        version = '1.0.0'
                    }
                }
            }
        }
        'session/new' {
            $sessionCounter++
            if ($HistoryPath -or $StartupControlPath) { Write-FixtureLog -Message "new|$([DateTimeOffset]::UtcNow.ToString('o'))|chat-fixture-$PID-$sessionCounter" }
            $result = @{ sessionId = "chat-fixture-$PID-$sessionCounter" }
            if ($StartupControlPath) {
                $result.configOptions = @(
                    @{
                        id = 'model'; name = 'Model'; category = 'model'; type = 'select'
                        currentValue = 'lifecycle-fixture-model'
                        options = @(@{ value = 'lifecycle-fixture-model'; name = 'Lifecycle Fixture Model' })
                    }
                )
            }
            Send-AcpMessage @{
                jsonrpc = '2.0'
                id = $request.id
                result = $result
            }
        }
        'session/list' {
            if (-not $HistoryPath) {
                Send-AcpMessage @{ jsonrpc = '2.0'; id = $request.id; error = @{ code = -32601; message = 'Method not found' } }
                break
            }
            Write-FixtureLog -Message "list|$([DateTimeOffset]::UtcNow.ToString('o'))|$($request.id)"
            if (Test-HistoryHeld) {
                if ($pendingHistory) {
                    Write-FixtureLog -Message "duplicate-list|$([DateTimeOffset]::UtcNow.ToString('o'))"
                    throw 'another history request arrived while the previous request was held'
                }
                $pendingHistory = $request
                Write-FixtureLog -Message "history-held|$([DateTimeOffset]::UtcNow.ToString('o'))|$($request.id)"
            }
            else {
                Complete-HistoryRequest $request
            }
        }
        'session/close' {
            if (-not $HistoryPath) {
                Send-AcpMessage @{ jsonrpc = '2.0'; id = $request.id; error = @{ code = -32601; message = 'Method not found' } }
                break
            }
            Write-FixtureLog -Message "close|$([DateTimeOffset]::UtcNow.ToString('o'))|$($request.params.sessionId)"
            Send-AcpMessage @{ jsonrpc = '2.0'; id = $request.id; result = @{} }
        }
        'session/prompt' {
            $sessionId = [string]$request.params.sessionId
            $promptText = (@($request.params.prompt) | Where-Object type -eq 'text' | ForEach-Object text) -join "`n"
            $marker = [regex]::Match($promptText, 'SCROLL_TURN_\d{2}_[a-f0-9]{32}').Value
            if (-not $marker) {
                throw 'prompt did not contain a completed-turn scroll marker'
            }
            $reply = "ACK_$marker"
            Write-FixtureLog -Message "prompt|$marker"
            if ($SupportsImages) {
                foreach ($image in @($request.params.prompt) | Where-Object type -eq 'image') {
                    $bytes = [Convert]::FromBase64String([string]$image.data)
                    $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes))
                    Write-FixtureLog -Message "image|$marker|$($image.mimeType)|$($bytes.Length)|$hash"
                }
            }

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
