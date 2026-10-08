param(
    [Parameter(Mandatory)][string]$InputPath,
    [Parameter(Mandatory)][string]$ReceiptPath,
    [Parameter(Mandatory)][string]$WtcliPath
)

$ErrorActionPreference = 'Stop'
if (-not $env:WT_SESSION -or -not $env:WT_COM_CLSID) {
    throw 'Sidebar hook fixtures must execute inside the selected Terminal shell pane.'
}

$events = @(Get-Content -LiteralPath $InputPath -Raw | ConvertFrom-Json)
foreach ($entry in $events) {
    $entry.payload | ConvertTo-Json -Depth 8 -Compress |
        & $WtcliPath agent-hook --cli-source copilot --event $entry.event
    if ($LASTEXITCODE -ne 0) {
        throw "The $($entry.event) hook failed with exit code $LASTEXITCODE."
    }
}
@{ events = $events.Count; pane_session_id = $env:WT_SESSION } |
    ConvertTo-Json -Compress | Set-Content -LiteralPath $ReceiptPath
