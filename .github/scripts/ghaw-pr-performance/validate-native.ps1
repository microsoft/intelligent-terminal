[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ProposalPath,
    [Parameter(Mandatory)][string]$RepositoryRoot,
    [Parameter(Mandatory)][string]$TrustedRuntimePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or $PSVersionTable.PSVersion.Major -lt 7) {
    throw 'Native validation requires PowerShell 7 on Windows.'
}
$root = [IO.Path]::GetFullPath($RepositoryRoot)
$proposalPath = [IO.Path]::GetFullPath($ProposalPath)
$runtimePath = [IO.Path]::GetFullPath($TrustedRuntimePath)
$proposal = Get-Content -LiteralPath $proposalPath -Raw | ConvertFrom-Json
$tracked = & git.exe -C $root ls-files -z
if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate original tracked paths.' }
$paths = (($tracked -join "`n").Split([char]0) | Where-Object { $_ })

Push-Location $root
try {
    & node.exe $runtimePath apply-proposal --input $proposalPath --pr $proposal.identity.prNumber `
        --base $proposal.identity.baseSha --head $proposal.identity.headSha
    if ($LASTEXITCODE -ne 0) { throw 'Trusted proposal validation/application failed.' }
} finally {
    Pop-Location
}

function Get-TrackedHashes {
    $hashes = @{}
    foreach ($path in $paths) {
        $localPath = Join-Path $root $path.Replace('/', '\')
        $hashes[$path] = (Get-FileHash -LiteralPath $localPath -Algorithm SHA256).Hash
    }
    return $hashes
}
$before = Get-TrackedHashes
$filter = $proposal.validationPlan.testFilter
$command = "cargo test --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml $filter -- --nocapture"
Write-Output $command
$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
$start.WorkingDirectory = $root
$start.UseShellExecute = $false
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
foreach ($argument in @('test', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
    'tools\wta\Cargo.toml', $filter, '--', '--nocapture')) {
    $start.ArgumentList.Add($argument)
}
foreach ($key in @($start.Environment.Keys)) {
    if ($key -match '(?i)(TOKEN|SECRET|PASSWORD|CREDENTIAL|OTLP.*HEADERS|^GITHUB_(ENV|OUTPUT)$)') {
        $null = $start.Environment.Remove($key)
    }
}

# Actions must impose a 32-minute step deadline and owns cleanup of descendants.
$process = [Diagnostics.Process]::new()
$process.StartInfo = $start
$started = $false
$clock = [Diagnostics.Stopwatch]::StartNew()
try {
    $null = $process.Start()
    $started = $true
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(1800000)) { throw 'Cargo exceeded its 30-minute deadline.' }
    $remaining = [int][Math]::Max(0, 1800000 - $clock.ElapsedMilliseconds)
    if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($stdout, $stderr), $remaining)) {
        throw 'Cargo output capture exceeded its 30-minute deadline.'
    }
    $output = $stdout.GetAwaiter().GetResult()
    Write-Output $output
    Write-Output $stderr.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0) { throw "Native validation failed with exit code $($process.ExitCode)." }
    $passed = 0L
    foreach ($match in [regex]::Matches($output, '(?m)^test result: ok\. ([0-9]+) passed; 0 failed;')) {
        $passed += [long]$match.Groups[1].Value
    }
    if ($passed -le 0) { throw 'Native validation did not execute any passing tests.' }
    $after = Get-TrackedHashes
    foreach ($path in $paths) {
        if ($before[$path] -cne $after[$path]) { throw "Native test changed tracked workspace bytes: $path" }
    }
    Write-Output "$passed native test(s) passed. Unit-test success is not an end-to-end performance measurement."
} finally {
    if ($started -and -not $process.HasExited) {
        $process.Kill($true)
        $process.WaitForExit(10000) | Out-Null
    }
    $process.Dispose()
}
