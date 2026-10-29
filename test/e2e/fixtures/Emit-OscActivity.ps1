param(
    [Parameter(Mandatory)][string]$Inbox,
    [Parameter(Mandatory)][string]$Ready,
    [Parameter(Mandatory)][string]$Title,
    [int]$LifetimeSec = 900
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::Write([char]27 + ']0;' + $Title + [char]27 + '\')
[Console]::Out.Flush()
@{
    pid = $PID
    pane = $env:WT_SESSION
    clsid = $env:WT_COM_CLSID
    wtcli = (Get-Command wtcli.exe -ErrorAction Stop | Select-Object -First 1).Source
    wta = (Get-Command wta.exe -ErrorAction Stop | Select-Object -First 1).Source
} | ConvertTo-Json | Set-Content -LiteralPath $Ready

$clock = [Diagnostics.Stopwatch]::StartNew()
while ($clock.Elapsed.TotalSeconds -lt $LifetimeSec) {
    foreach ($file in @(Get-ChildItem -LiteralPath $Inbox -Filter '*.request.json' -File | Sort-Object Name)) {
        $request = Get-Content -LiteralPath $file.FullName -Raw | ConvertFrom-Json
        if ($request.kind -eq 'close') { exit 0 }
        if ($request.kind -eq 'progress') {
            if ([int]$request.state -notin 0, 1, 2, 3, 4) { throw 'Invalid fixture progress state.' }
            [Console]::Write([char]27 + ']9;4;' + $request.state + ';35' + [char]27 + '\')
            [Console]::Out.Flush()
        }
        elseif ($request.kind -ne 'noop') { throw 'Invalid fixture request.' }
        @{
            token = $request.token
            pane = $env:WT_SESSION
            kind = $request.kind
            state = $request.state
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Inbox "$($request.token).reply.json")
        Remove-Item -LiteralPath $file.FullName
    }
    Start-Sleep -Milliseconds 40
}
throw 'The bounded OSC fixture timed out.'
