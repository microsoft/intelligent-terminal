param(
    [Parameter(Mandatory)][string]$LogPath,
    [Parameter(Mandatory)][string]$RunId,
    [Parameter(Mandatory)][Alias('i')][string]$Task
)

$ErrorActionPreference = 'Stop'
# Custom delegate startup uses -i. The shared interactive fixture cannot accept
# it (ambiguous InformationAction/InformationVariable), so this narrow native
# fixture records the task without ever assigning a conversation or firing hooks.
@{
    run_id = $RunId
    pid = $PID
    native_pid = $null
    mode = 'fresh'
    source = 'host'
    task = $Task
    args = @($args)
    cwd = [IO.Directory]::GetCurrentDirectory()
    command_line = [Environment]::CommandLine
    pane_session_id = $env:WT_SESSION
    at = [DateTimeOffset]::UtcNow.ToString('o')
} | ConvertTo-Json -Compress | Add-Content -LiteralPath $LogPath -Encoding utf8
[Console]::WriteLine("ITE2E-MCP-NATIVE-DELEGATE $RunId $Task PID=$PID")
while ($null -ne ($line = [Console]::ReadLine())) {
    if ($line -eq 'exit') { break }
    [Console]::WriteLine("ITE2E-MCP-DELEGATE-ALIVE $RunId $line")
}
