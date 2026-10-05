[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(30, 1800)][int]$TimeoutSeconds = 1200,
    [string]$ExpectedUserSid,
    [switch]$PolicyApproved
)

# This is the entire elevated operation: four providers, a unique session,
# bounded duration, and optionally the three explicitly approved HKCU policies.
# Application launch, UI/COM operations, and settings writes stay unelevated.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $OutputDirectory).Path
$session = 'ItE2E-Telemetry-' + [guid]::NewGuid().ToString('N')
$etl = Join-Path $root 'telemetry.etl'
$started = $false
$policy = $null
try {
    . (Join-Path $PSScriptRoot '..\tests\helpers\TelemetryPolicy.ps1')
    if ($PolicyApproved) {
        Assert-TelemetryPolicyBrokerIdentity -ExpectedSid $ExpectedUserSid -Approved $PolicyApproved.IsPresent
        $env:ITE2E_TELEMETRY_POLICY_APPROVED = '1'
        $policy = Initialize-TelemetryPolicyTransaction -Directory $root
        Save-TelemetryPolicyJournal -Transaction $policy
    }
    $providers = @(
        '{56c06166-2e2e-5f4d-7ff3-74f4b78c87d6} 0xffffffffffffffff 5'
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
    @{
        session = $session; pid = $PID; startedUtc = [DateTime]::UtcNow.ToString('o')
        policyBroker = [bool]$policy; userSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'ready.pending')
    Move-Item -LiteralPath (Join-Path $root 'ready.pending') -Destination (Join-Path $root 'ready.json')
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not (Test-Path -LiteralPath (Join-Path $root 'stop'))) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Telemetry scenario exceeded the bounded capture duration.' }
        $requestPath = Join-Path $root 'policy-request.json'
        if (Test-Path -LiteralPath $requestPath) {
            $request = $null
            $response = @{ Id = $null; Success = $false; Error = $null }
            try {
                if (-not $policy) { throw 'Policy operations were not approved for this collector.' }
                if ((Get-Item -LiteralPath $requestPath).Length -gt 16384) { throw 'Policy request exceeds 16 KiB.' }
                $request = Get-Content -LiteralPath $requestPath -Raw | ConvertFrom-Json -AsHashtable
                Assert-TelemetryPolicyRequest -Request $request
                $response.Id = $request.Id
                Invoke-TelemetryPolicyBrokerRequest -Transaction $policy -Request $request
                $response.Success = $true
            }
            catch { $response.Error = $_.ToString() }
            finally {
                Remove-Item -LiteralPath $requestPath -Force
                $responseName = if ($response.Id) { "policy-response-$($response.Id)" } else { 'policy-invalid-request' }
                $response | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $root "$responseName.pending")
                Move-Item -LiteralPath (Join-Path $root "$responseName.pending") -Destination (Join-Path $root "$responseName.json") -Force
            }
        }
        Start-Sleep -Milliseconds 200
    }
}
catch {
    $_ | Out-String | Set-Content -LiteralPath (Join-Path $root 'error.txt')
}
finally {
    if ($policy) {
        try { Restore-TelemetryPolicy -Transaction $policy }
        catch { "Policy restoration failed: $_" | Add-Content -LiteralPath (Join-Path $root 'error.txt') }
    }
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
