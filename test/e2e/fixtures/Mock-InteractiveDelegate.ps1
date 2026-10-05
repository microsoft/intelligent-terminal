param(
    [Parameter(Mandatory)][string]$LogPath,
    [Parameter(Mandatory)][string]$RunId,
    [switch]$Canonical,
    [switch]$External,
    [switch]$Resume,
    [string]$SessionId,
    [string]$WtcliPath,
    [string]$SessionStartGate
)

$ErrorActionPreference = 'Stop'
$session = [guid]::NewGuid().ToString()
if ($Resume -and -not $Canonical) { throw 'Resume fixture must run in a real Terminal pane.' }
if ($External) {
    $parsed = [guid]::Empty
    if ($Canonical -or $env:WT_SESSION -or -not $env:ITE2E_SHIM_PID -or
        -not [guid]::TryParse($SessionId, [ref]$parsed) -or $parsed -eq [guid]::Empty) {
        throw 'External fixture requires an owned native process, explicit UUID and no Terminal pane.'
    }
    $session = $SessionId
}
if ($Canonical) {
    $parsed = [guid]::Empty
    if (-not [guid]::TryParse($SessionId, [ref]$parsed) -or $parsed -eq [guid]::Empty -or
        -not $env:ITE2E_SHIM_PID -or -not $env:WT_SESSION -or -not $WtcliPath) {
        throw 'Canonical fixture requires a fresh explicit UUID, owned native shim and Terminal pane.'
    }
    $session = $SessionId
    if ($SessionStartGate) {
        $title = "ITE2E prehook $session"
        [Console]::Write("$([char]27)]0;$title$([char]7)")
        @{ pane_session_id = $env:WT_SESSION; session_id = $session; title = $title; phase = 'before-hook' } |
            ConvertTo-Json -Compress |
            Set-Content -LiteralPath "$SessionStartGate.waiting-$session.json"
        $deadline = [DateTimeOffset]::UtcNow.AddSeconds(60)
        while (-not (Test-Path -LiteralPath $SessionStartGate)) {
            if ([DateTimeOffset]::UtcNow -ge $deadline) { throw 'Session-start gate was not released.' }
            Start-Sleep -Milliseconds 100
        }
    }
    @{ session_id = $session; cwd = [IO.Directory]::GetCurrentDirectory() } |
        ConvertTo-Json -Compress |
        & $WtcliPath agent-hook --cli-source copilot --event agent.session.start
    if ($LASTEXITCODE -ne 0) { throw "Canonical session-start hook failed: $LASTEXITCODE" }
    if ($SessionStartGate) {
        @{ pane_session_id = $env:WT_SESSION; session_id = $session; provider_id = 'copilot'; phase = 'hook-emitted' } |
            ConvertTo-Json -Compress |
            Set-Content -LiteralPath "$SessionStartGate.emitted-$session.json"
    }
}
$record = @{
    run_id = $RunId
    session_id = $session
    pid = $PID
    native_pid = if ($Canonical -or $External) { [int]$env:ITE2E_SHIM_PID } else { $null }
    native_command_line = if ($Canonical -or $External) { $env:ITE2E_SHIM_ARGS } else { $null }
    provider = if ($Canonical -or $External) { 'copilot' } else { 'custom:agents-actions-cli' }
    mode = if ($External) { 'external' } elseif ($Resume) { 'resume' } else { 'fresh' }
    cwd = [IO.Directory]::GetCurrentDirectory()
    source = 'host'
    args = @($args | Where-Object { $null -ne $_ })
    command_line = [Environment]::CommandLine
    pane_session_id = $env:WT_SESSION
    at = [DateTimeOffset]::UtcNow.ToString('o')
}
$record | ConvertTo-Json -Compress | Add-Content -LiteralPath $LogPath -Encoding utf8
[Console]::WriteLine("ITE2E-INTERACTIVE-DELEGATE $RunId $session PID=$PID")
while ($null -ne ($line = [Console]::ReadLine())) {
    if ($line -eq 'exit') { break }
    [Console]::WriteLine("ITE2E-DELEGATE-ALIVE $session $line")
}
