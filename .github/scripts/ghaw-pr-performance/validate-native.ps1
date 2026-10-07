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
Push-Location $root
try {
    & node.exe $runtimePath validate-proposal --input $proposalPath --pr $proposal.identity.prNumber `
        --base $proposal.identity.baseSha --head $proposal.identity.headSha
    if ($LASTEXITCODE -ne 0) { throw 'Trusted proposal validation failed.' }
} finally {
    Pop-Location
}
$head = & git.exe -C $root rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $head -cne $proposal.identity.headSha) {
    throw 'Native checkout must equal the immutable reviewed head.'
}
$flags = & git.exe -C $root ls-files -v
if ($LASTEXITCODE -ne 0 -or @($flags | Where-Object { $_ -cmatch '^[a-zS] ' }).Count -gt 0) {
    throw 'Native checkout must have clean tracked HEAD files without hidden index flags.'
}
& git.exe -C $root diff --quiet HEAD --
if ($LASTEXITCODE -ne 0) { throw 'Native checkout must have clean tracked HEAD files before original test listing.' }
$tracked = & git.exe -C $root ls-files -z
if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate original tracked paths.' }
$trackedSnapshot = $tracked -join "`n"
$paths = ($trackedSnapshot.Split([char]0) | Where-Object { $_ })
$sourcePaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($path in $paths) { $null = $sourcePaths.Add($path) }

function Assert-SourceOnlyCheckout([string]$Stage) {
    # Inspect the actual root, not Git's candidate-mutable core.worktree.
    if (([IO.File]::GetAttributes($root) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "${Stage}: source checkout reparse points are not allowed: $root"
    }
    $directories = [Collections.Generic.Stack[string]]::new()
    $directories.Push($root)
    $metadataPath = Join-Path $root '.git'
    while ($directories.Count -gt 0) {
        foreach ($item in Get-ChildItem -LiteralPath $directories.Pop() -Force) {
            if ($item.FullName.Equals($metadataPath, [StringComparison]::OrdinalIgnoreCase)) { continue }
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "${Stage}: source checkout reparse points are not allowed: $($item.FullName)"
            }
            if ($item.PSIsContainer) {
                $directories.Push($item.FullName)
            } else {
                $relativePath = [IO.Path]::GetRelativePath($root, $item.FullName).Replace('\', '/')
                if (-not $sourcePaths.Contains($relativePath)) {
                    throw "${Stage}: untracked workspace paths (including ignored files) are not allowed: $relativePath"
                }
            }
        }
    }
    $currentTracked = & git.exe -C $root ls-files -z
    if ($LASTEXITCODE -ne 0) { throw "${Stage}: could not enumerate tracked workspace paths." }
    if (-not [string]::Equals(($currentTracked -join "`n"), $trackedSnapshot, [StringComparison]::Ordinal)) {
        throw "${Stage}: changed tracked workspace path membership."
    }
    $untracked = & git.exe -C $root ls-files --others -z
    if ($LASTEXITCODE -ne 0) { throw "${Stage}: could not enumerate untracked workspace paths." }
    if (($untracked -join "`n").Length -gt 0) {
        throw "${Stage}: untracked workspace paths (including ignored files) are not allowed."
    }
}
Assert-SourceOnlyCheckout 'initial-checkout'
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
function Invoke-CargoStage([string]$Stage, [string[]]$Arguments, [bool]$RequireTests = $false, [string]$NameCheck = '') {
    Assert-SourceOnlyCheckout $Stage
    # Never reuse artifacts that an earlier test process could have modified.
    # Actions owns hosted cleanup; local callers own their isolated temporary parent.
    $targetParent = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
    $targetDirectory = [IO.Path]::GetFullPath((Join-Path $targetParent ('performance-native-target-' + [guid]::NewGuid().ToString('N'))))
    if ($targetDirectory.Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
        $targetDirectory.StartsWith($root.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Native Cargo target directory must be outside the candidate checkout.'
    }
    if ($Arguments[0] -eq 'test') { $null = [IO.Directory]::CreateDirectory($targetDirectory) }
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
    $start.Environment['CARGO_TARGET_DIR'] = $targetDirectory
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
        if ($NameCheck -eq 'listing') {
            $named = [regex]::Matches($output, '(?m)^' + [regex]::Escape($filter) + ': test\r?$')
            if ($named.Count -ne 1) { throw "${Stage}: selector must name exactly one existing test in compiled original HEAD." }
            Write-Output "${Stage}: original HEAD contains ${filter}: test (listing only, not a passing test claim)."
        }
        if ($RequireTests) {
            $passed = 0L
            foreach ($match in [regex]::Matches($output, '(?m)^test result: ok\. ([0-9]+) passed; 0 failed;')) {
                $passed += [long]$match.Groups[1].Value
            }
            if ($passed -le 0) { throw "${Stage}: native validation did not execute any passing tests." }
            if ($NameCheck -eq 'passing' -and
                [regex]::Matches($output, '(?m)^test ' + [regex]::Escape($filter) + '(?: - should panic)? \.\.\. ok\r?$').Count -ne 1) {
                throw "${Stage}: native validation did not report the selected test passing."
            }
            Write-Output "${Stage}: $passed native test(s) passed."
        }
    } finally {
        if ($started -and -not $process.HasExited) {
            $process.Kill($true)
            $process.WaitForExit(10000) | Out-Null
        }
        $process.Dispose()
        Assert-SourceOnlyCheckout $Stage
        $after = Get-TrackedHashes
        foreach ($path in $paths) {
            if ($before[$path] -cne $after[$path]) { throw "${Stage}: changed tracked workspace bytes: $path" }
        }
    }
}
Invoke-CargoStage 'original-test-listing' @('test', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
    'tools\wta\Cargo.toml', '--', '--list') $false 'listing'
Push-Location $root
try {
    & node.exe $runtimePath apply-proposal --input $proposalPath --pr $proposal.identity.prNumber `
        --base $proposal.identity.baseSha --head $proposal.identity.headSha
    if ($LASTEXITCODE -ne 0) { throw 'Trusted proposal application failed.' }
} finally {
    Pop-Location
}
Assert-SourceOnlyCheckout 'proposal-application'
$before = Get-TrackedHashes
Invoke-CargoStage 'format-check' @('fmt', '--manifest-path', 'tools\wta\Cargo.toml', '--', '--check')
Invoke-CargoStage 'focused-tests' @('test', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
    'tools\wta\Cargo.toml', $filter, '--', '--exact') $true 'passing'
Invoke-CargoStage 'full-suite' @('test', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
    'tools\wta\Cargo.toml') $true
Write-Output 'Unit-test success is not an end-to-end performance measurement.'
