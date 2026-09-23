[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(30, 900)][int]$TimeoutSeconds = 600
)

# This is the entire elevated operation: three providers, a unique session,
# bounded duration, and cleanup of that session only. No app or settings writes.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $OutputDirectory).Path
$session = 'ItE2E-Telemetry-' + [guid]::NewGuid().ToString('N')
$etl = Join-Path $root 'telemetry.etl'
$started = $false
try {
    $providers = @(
        '{24a1622f-7da7-5c77-3303-d850bd1ab2ed} 0xffffffffffffffff 5'
        '{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b} 0xffffffffffffffff 5'
        '{be579944-4d33-5202-e5d6-a7a57f1935cb} 0xffffffffffffffff 5'
    )
    $providerFile = Join-Path $root 'providers.txt'
    $providers | Set-Content -LiteralPath $providerFile -Encoding ascii
    & logman.exe create trace $session -pf $providerFile -o $etl -bs 64 -nb 16 128 -ets 2>&1 |
        Set-Content -LiteralPath (Join-Path $root 'logman-start.txt')
    if ($LASTEXITCODE) { throw "logman start failed: $LASTEXITCODE" }
    $started = $true
    @{ session = $session; pid = $PID; startedUtc = [DateTime]::UtcNow.ToString('o') } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'ready.json')
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not (Test-Path -LiteralPath (Join-Path $root 'stop'))) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Telemetry scenario exceeded the bounded capture duration.' }
        Start-Sleep -Milliseconds 200
    }
}
catch {
    $_ | Out-String | Set-Content -LiteralPath (Join-Path $root 'error.txt')
}
finally {
    if ($started) {
        & logman.exe stop $session -ets 2>&1 |
            Set-Content -LiteralPath (Join-Path $root 'logman-stop.txt')
        $stopExit = $LASTEXITCODE
        if ($stopExit) { "logman stop failed: $stopExit" | Add-Content -LiteralPath (Join-Path $root 'error.txt') }
        else {
            & tracerpt.exe $etl -o (Join-Path $root 'events.xml') -of XML `
                -export (Join-Path $root 'schema.xml') -summary (Join-Path $root 'summary.txt') -y 2>&1 |
                Set-Content -LiteralPath (Join-Path $root 'tracerpt.txt')
            if ($LASTEXITCODE) { "tracerpt failed: $LASTEXITCODE" | Add-Content -LiteralPath (Join-Path $root 'error.txt') }
        }
    }
    [DateTime]::UtcNow.ToString('o') | Set-Content -LiteralPath (Join-Path $root 'done')
}
