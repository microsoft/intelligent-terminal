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
$clock = [Diagnostics.Stopwatch]::StartNew()
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
# Actions must impose a 32-minute step deadline and owns cleanup of descendants.
function Invoke-CargoStage([string]$Stage, [string[]]$Arguments, [bool]$RequireTests = $false) {
    Write-Output "${Stage}: cargo $($Arguments -join ' ')"
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
    $start.WorkingDirectory = $root
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    foreach ($key in @($start.Environment.Keys)) {
        if ($key -match '(?i)(TOKEN|SECRET|PASSWORD|CREDENTIAL|OTLP.*HEADERS|^GITHUB_(ENV|OUTPUT)$)') {
            $null = $start.Environment.Remove($key)
        }
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $started = $false
    try {
        $remaining = [int][Math]::Max(0, 1800000 - $clock.ElapsedMilliseconds)
        if ($remaining -le 0) { throw "${Stage}: exceeded the total 30-minute deadline." }
        $null = $process.Start()
        $started = $true
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($remaining)) { throw "${Stage}: exceeded the total 30-minute deadline." }
        $remaining = [int][Math]::Max(0, 1800000 - $clock.ElapsedMilliseconds)
        if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($stdout, $stderr), $remaining)) {
            throw "${Stage}: output capture exceeded the total 30-minute deadline."
        }
        $output = $stdout.GetAwaiter().GetResult()
        Write-Output $output
        Write-Output $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "${Stage}: native validation failed with exit code $($process.ExitCode)." }
        if ($RequireTests) {
            $passed = 0L
            foreach ($match in [regex]::Matches($output, '(?m)^test result: ok\. ([0-9]+) passed; 0 failed;')) {
                $passed += [long]$match.Groups[1].Value
            }
            if ($passed -le 0) { throw "${Stage}: native validation did not execute any passing tests." }
            Write-Output "${Stage}: $passed native test(s) passed."
        }
    } finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            $process.WaitForExit(10000) | Out-Null
        }
        $process.Dispose()
        $after = Get-TrackedHashes
        foreach ($path in $paths) {
            if ($before[$path] -cne $after[$path]) { throw "${Stage}: changed tracked workspace bytes: $path" }
        }
    }
}
Invoke-CargoStage 'format-check' @('fmt', '--manifest-path', 'tools\wta\Cargo.toml', '--', '--check')
Invoke-CargoStage 'focused-tests' @('test', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
    'tools\wta\Cargo.toml', $filter, '--', '--nocapture') $true
Invoke-CargoStage 'full-suite' @('test', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
    'tools\wta\Cargo.toml') $true
Write-Output 'Unit-test success is not an end-to-end performance measurement.'
