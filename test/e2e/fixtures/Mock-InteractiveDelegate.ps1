param(
    [Parameter(Mandatory)][string]$LogPath,
    [Parameter(Mandatory)][string]$RunId,
    [switch]$Canonical,
    [string]$SessionId,
    [string]$WtcliPath
)

$ErrorActionPreference = 'Stop'
$session = [guid]::NewGuid().ToString()
if ($Canonical) {
    $parsed = [guid]::Empty
    if (-not [guid]::TryParse($SessionId, [ref]$parsed) -or $parsed -eq [guid]::Empty -or
        -not $env:ITE2E_SHIM_PID -or -not $env:WT_SESSION -or -not $WtcliPath) {
        throw 'Canonical fixture requires a fresh explicit UUID, owned native shim and Terminal pane.'
    }
    $session = $SessionId
    @{ session_id = $session; cwd = [IO.Directory]::GetCurrentDirectory() } |
        ConvertTo-Json -Compress |
        & $WtcliPath agent-hook --cli-source copilot --event agent.session.start
    if ($LASTEXITCODE -ne 0) { throw "Canonical session-start hook failed: $LASTEXITCODE" }
}
$record = @{
    run_id = $RunId
    session_id = $session
    pid = $PID
    native_pid = if ($Canonical) { [int]$env:ITE2E_SHIM_PID } else { $null }
    native_command_line = if ($Canonical) { $env:ITE2E_SHIM_ARGS } else { $null }
    provider = if ($Canonical) { 'copilot' } else { 'custom:agents-actions-cli' }
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
