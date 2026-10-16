param(
    [Parameter(Mandatory)][string]$LogPath,
    [Parameter(Mandatory)][string]$RunId,
    [switch]$Canonical,
    [switch]$External,
    [switch]$Resume,
    [string]$SessionId,
    [string]$WtcliPath
)

$ErrorActionPreference = 'Stop'
if (-not $Canonical -or -not $Resume -or $External -or
    $SessionId -ne $RunId -or -not $env:WT_SESSION -or -not $env:ITE2E_SHIM_PID) {
    throw 'Watcher fixture only accepts its owned native resume in a Terminal pane.'
}
$directory = Split-Path $LogPath -Parent
$testHome = Join-Path (Split-Path $directory -Parent) 'home'
$transcript = Join-Path $testHome ".copilot\session-state\$SessionId\events.jsonl"
$unrelatedId = [guid]::NewGuid().ToString()
$unrelatedTranscript = Join-Path $testHome ".copilot\session-state\$unrelatedId\events.jsonl"
$record = @{
    session_id = $SessionId; pane_session_id = $env:WT_SESSION
    native_pid = [int]$env:ITE2E_SHIM_PID; fixture_pid = $PID
    userprofile = $env:USERPROFILE; transcript = $transcript
    unrelated_session_id = $unrelatedId; native_command_line = $env:ITE2E_SHIM_ARGS
}
$record | ConvertTo-Json -Compress | Add-Content -LiteralPath $LogPath -Encoding utf8
[Console]::Write("`e]0;$([IO.Path]::GetFileName($directory))`a")
[Console]::WriteLine("WATCHER-RESUME $SessionId")
$sequence = 0
while ($null -ne ($line = [Console]::ReadLine())) {
    if ($line -eq 'exit') { break }
    $sequence++
    $path = $transcript
    $event = switch ($line) {
        'working' { @{ type = 'assistant.turn_start'; data = @{ turnId = "$sequence" } } }
        'attention' { @{ type = 'tool.execution_start'; data = @{ toolName = 'ask_user' } } }
        'idle' { @{ type = 'assistant.turn_end'; data = @{ turnId = "$sequence" } } }
        'unrelated' {
            $path = $unrelatedTranscript
            @{ type = 'assistant.turn_start'; data = @{ turnId = "$sequence" } }
        }
        default { throw "Unknown watcher fixture command: $line" }
    }
    New-Item -ItemType Directory -Path (Split-Path $path -Parent) -Force | Out-Null
    $event.timestamp = [DateTimeOffset]::UtcNow.ToString('o')
    $event | ConvertTo-Json -Compress | Add-Content -LiteralPath $path -Encoding utf8
    @{
        command = $line; sequence = $sequence; transcript = $path
        native_pid = [int]$env:ITE2E_SHIM_PID; timestamp = $event.timestamp
    } | ConvertTo-Json -Compress |
        Set-Content -LiteralPath (Join-Path $directory "stage-$env:ITE2E_SHIM_PID-$sequence.json")
    [Console]::WriteLine("WATCHER-EVENT $line $sequence")
}
