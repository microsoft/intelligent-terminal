# Runs only inside the test-owned ordinary TermControl, not the dedicated Console.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$WtaPath,
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [Parameter(Mandatory)][string]$ExpectedSha256,
    [ValidateSet('NativePaste', 'WorkFlow')][string]$Scenario = 'NativePaste',
    [switch]$NavigationDiagnostics
)
$ErrorActionPreference = 'Stop'
if ($NavigationDiagnostics -and $Scenario -ne 'WorkFlow') { throw 'Navigation diagnostics are scoped to the owned WorkFlow UI.' }
if ((Get-FileHash -LiteralPath $WtaPath -Algorithm SHA256).Hash -ne $ExpectedSha256) {
    throw 'The deployed WTA changed before the native fixture started.'
}
$env:LOCALAPPDATA = Join-Path $EvidenceDirectory 'local'
$env:APPDATA = Join-Path $EvidenceDirectory 'roaming'
New-Item -ItemType Directory -Path $env:LOCALAPPDATA, $env:APPDATA -Force | Out-Null
$configuration = Join-Path $EvidenceDirectory 'adapters.json'
$adapters = @{ capabilities = @() }
if ($Scenario -eq 'WorkFlow') {
    . (Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterWorkFlow.ps1')
    $adapters = Get-WorkFlowAdapterConfiguration -IntakeScriptPath (Join-Path $PSScriptRoot 'AgentCenterIntakeCoordinator.cjs') `
        -EvidenceDirectory $EvidenceDirectory
}
$adapters | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $configuration -Encoding utf8NoBOM
& $WtaPath center configure --input-json $configuration
if ($LASTEXITCODE) { throw 'Isolated Agent Center adapter configuration failed.' }
$service = $null
try {
    $service = Start-Process -FilePath $WtaPath -ArgumentList 'center', 'serve' -PassThru `
        -RedirectStandardOutput (Join-Path $EvidenceDirectory 'service.stdout.txt') `
        -RedirectStandardError (Join-Path $EvidenceDirectory 'service.stderr.txt')
    # A read-only request proves the separately owned authority is responsive before ui
    # can auto-start a detached authority that would escape fixture ownership.
    $ready = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        if ($service.HasExited) { throw 'The isolated Agent Center service exited during startup.' }
        if (@(Get-ChildItem -LiteralPath $env:LOCALAPPDATA -Filter work.db -Recurse).Count) {
            $ready = $true
            break
        }
        Start-Sleep -Milliseconds 200
    }
    if (-not $ready) { throw 'The isolated Agent Center database did not initialize.' }
    $databases = @(Get-ChildItem -LiteralPath $env:LOCALAPPDATA -Filter work.db -Recurse)
    if ($databases.Count -ne 1) { throw 'Expected exactly one test-owned Agent Center store.' }
    $stateRoot = $databases[0].DirectoryName
    if ($Scenario -eq 'WorkFlow') {
        $connection = Open-WorkFlowConnection -StateRoot $stateRoot
        try {
            Initialize-WorkFlowGlobalFixture -Connection $connection -EvidenceDirectory $EvidenceDirectory |
                ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'work-flow.json') -Encoding utf8NoBOM
        }
        finally { $connection.Pipe.Dispose() }
    }
    & $WtaPath work list --json | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'work-list-before.json') -Encoding utf8NoBOM
    if ($LASTEXITCODE -or $service.HasExited) { throw 'The isolated authority did not answer work.list.' }
    @{
        shellPid = $PID; servicePid = $service.Id
        wtaPath = $WtaPath; sha256 = $ExpectedSha256
        localAppData = $env:LOCALAPPDATA; appData = $env:APPDATA
        stateRoot = $stateRoot; scenario = $Scenario
        navigationDiagnostics = [bool]$NavigationDiagnostics
        uiLogFilter = $(if ($NavigationDiagnostics) { Get-WorkFlowNavigationLogFilter } else { $null })
        providerCapabilities = @($adapters.capabilities | ForEach-Object { $_.id })
        boundary = 'ordinary TermControl -> ConPTY -> deployed wta ui'
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'runtime.json') -Encoding utf8NoBOM
    if ($Scenario -eq 'WorkFlow') { Invoke-WorkFlowUi -WtaPath $WtaPath -NavigationDiagnostics:$NavigationDiagnostics }
    else { & $WtaPath ui }
    if ($LASTEXITCODE) { throw "Agent Center UI exited with code $LASTEXITCODE." }
    & $WtaPath work list --json | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'work-list-after.json') -Encoding utf8NoBOM
    if ($LASTEXITCODE) { throw 'The isolated authority did not answer the final work.list.' }
}
finally {
    if ($service -and -not $service.HasExited) { Stop-Process -Id $service.Id -Force }
}
