[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [ValidateRange(1, 4096)][int]$Count = 1024,
    [switch]$Sustained
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterWorkFlow.ps1')
$runtime = Get-Content -LiteralPath (Join-Path $EvidenceDirectory 'runtime.json') -Raw | ConvertFrom-Json
$fixture = Get-Content -LiteralPath (Join-Path $EvidenceDirectory 'work-flow.json') -Raw | ConvertFrom-Json
$connection = $null
$budget = [Diagnostics.Stopwatch]::StartNew()
$receiptClock = $null
$completed = 0
$receiptSpanSeconds = 0.0
try {
    $connection = Open-WorkFlowConnection -StateRoot $runtime.stateRoot
    if ($connection.Welcome.storeId -cne $fixture.storeId) { throw 'Load producer refused a different authority.' }
    $versions = @{}
    foreach ($work in $fixture.works) {
        $view = Invoke-WorkFlowRequest $connection 'work.get' -Params @{ workId = $work.id }
        if ($view.data.work.lifecycle -cne 'Draft') { throw 'Load producer operates only on never-started draft works.' }
        $versions[$work.id] = $view.data.work.version
    }
    while (Test-WorkFlowLoadContinues -Sustained ([bool]$Sustained) -Completed $completed -Count $Count `
        -ElapsedSeconds $budget.Elapsed.TotalSeconds -StopRequested (Test-Path -LiteralPath (Join-Path $EvidenceDirectory 'stop-load'))) {
        $work = $fixture.works[$completed % $fixture.works.Count]
        $response = Invoke-WorkFlowRequest $connection 'work.control' -Mutation `
            -Params @{ workId = $work.id; action = 'Hold' } `
            -IfMatch @(@{ kind = 'Work'; id = $work.id; version = $versions[$work.id] }) `
            -ReceiptPath (Join-Path $EvidenceDirectory 'load-receipts.jsonl')
        $versions[$work.id] = $response.data.version
        if (-not $receiptClock) { $receiptClock = [Diagnostics.Stopwatch]::StartNew() }
        $receiptSpanSeconds = $receiptClock.Elapsed.TotalSeconds
        $completed++
        Write-WorkFlowJsonSnapshot -Path (Join-Path $EvidenceDirectory 'load-progress.json') -Value @{
            completed = $completed; requested = $Count; workId = $work.id
            version = $response.data.version; storeId = $fixture.storeId
            sustained = [bool]$Sustained; receiptSpanSeconds = $receiptSpanSeconds
        }
        Start-Sleep -Milliseconds $(if ($Sustained) { 150 } else { 10 })
    }
    Write-WorkFlowJsonSnapshot -Path (Join-Path $EvidenceDirectory 'load-result.json') -Value @{
        completed = $completed; failed = $false; versions = $versions
        sustained = [bool]$Sustained; receiptSpanSeconds = $receiptSpanSeconds
    }
}
catch {
    Write-WorkFlowJsonSnapshot -Path (Join-Path $EvidenceDirectory 'load-result.json') -Value @{
        completed = $completed; failed = $true; error = $_.Exception.Message
        sustained = [bool]$Sustained; receiptSpanSeconds = $receiptSpanSeconds
    }
    throw
}
finally { if ($connection) { $connection.Pipe.Dispose() } }
