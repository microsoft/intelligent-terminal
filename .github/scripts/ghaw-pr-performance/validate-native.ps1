[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('OriginalListing', 'Focused', 'FullSuite')][string]$Phase,
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
$originalHashes = Get-TrackedHashes
# Decode the validated artifact before Cargo can execute candidate build scripts.
# Only this private snapshot authorizes application; no later helper/file reload.
$replacements = @(
    foreach ($file in $proposal.files) {
        if ($paths -cnotcontains $file.path) { throw 'Proposal replacement is not in the original tracked inventory.' }
        [pscustomobject]@{
            Path = $file.path
            LocalPath = Join-Path $root $file.path.Replace('/', '\')
            Bytes = [Convert]::FromBase64String($file.contents)
        }
    }
)
$appliedHashes = $originalHashes.Clone()
foreach ($replacement in $replacements) {
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $appliedHashes[$replacement.Path] = [BitConverter]::ToString($sha256.ComputeHash($replacement.Bytes)).Replace('-', '')
    } finally {
        $sha256.Dispose()
    }
}
$expectedHashes = $originalHashes
function Get-AncestorConfigHash([string]$ConfigPath) {
    $cargoDirectory = [IO.Path]::GetDirectoryName($ConfigPath)
    foreach ($pathToInspect in @($cargoDirectory, $ConfigPath)) {
        try { $attributes = [IO.File]::GetAttributes($pathToInspect) }
        catch [IO.FileNotFoundException] { return $null }
        catch [IO.DirectoryNotFoundException] { return $null }
        if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Cargo ancestor configuration reparse points are not allowed: $pathToInspect"
        }
        $isDirectory = ($attributes -band [IO.FileAttributes]::Directory) -ne 0
        if ($isDirectory -ne ($pathToInspect -eq $cargoDirectory)) {
            throw "Cargo ancestor configuration has an unexpected file type: $pathToInspect"
        }
    }
    return (Get-FileHash -LiteralPath $ConfigPath -Algorithm SHA256).Hash
}
# Cargo discovers both names above its working directory, independently of CARGO_HOME.
# Capture absent paths too, before original-head build scripts can run.
$ancestorDirectories = @()
$ancestorConfigs = @(
    $ancestor = [IO.Directory]::GetParent($root)
    while ($null -ne $ancestor) {
        $ancestorDirectories += $ancestor.FullName
        if (([IO.File]::GetAttributes($ancestor.FullName) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Cargo ancestor directory reparse points are not allowed: $($ancestor.FullName)"
        }
        foreach ($name in @('config', 'config.toml')) {
            $configPath = Join-Path $ancestor.FullName ('.cargo\' + $name)
            [pscustomobject]@{ Path = $configPath; Hash = Get-AncestorConfigHash $configPath }
        }
        $ancestor = $ancestor.Parent
    }
)
function Assert-AncestorConfigHashes([string]$Stage) {
    foreach ($directory in $ancestorDirectories) {
        if (([IO.File]::GetAttributes($directory) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "${Stage}: Cargo ancestor directory reparse points are not allowed: $directory"
        }
    }
    foreach ($config in $ancestorConfigs) {
        if ($config.Hash -cne (Get-AncestorConfigHash $config.Path)) {
            throw "${Stage}: changed Cargo ancestor configuration: $($config.Path)"
        }
    }
}
function Assert-ExpectedSourceHashes([string]$Stage) {
    $observed = Get-TrackedHashes
    foreach ($path in $paths) {
        if ($expectedHashes[$path] -cne $observed[$path]) { throw "${Stage}: changed tracked workspace bytes: $path" }
    }
}
$filter = $proposal.validationPlan.testFilter
# Each phase runs on its own fresh hosted VM. Actions owns job-end descendant cleanup.
function Invoke-CargoStage([string]$Stage, [string[]]$Arguments, [bool]$RequireTests = $false, [string]$NameCheck = '') {
    Assert-AncestorConfigHashes $Stage
    Assert-SourceOnlyCheckout $Stage
    Assert-ExpectedSourceHashes $Stage
    # Never reuse artifacts or Cargo configuration/registry state from earlier code.
    # Actions owns hosted cleanup; local callers own their isolated temporary parent.
    $targetParent = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { Split-Path $root -Parent }
    $targetDirectory = [IO.Path]::GetFullPath((Join-Path $targetParent ('performance-native-target-' + [guid]::NewGuid().ToString('N'))))
    $cargoHome = [IO.Path]::GetFullPath((Join-Path $targetParent ('performance-native-cargo-home-' + [guid]::NewGuid().ToString('N'))))
    foreach ($directory in @($targetDirectory, $cargoHome)) {
        if ($directory.Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
            $directory.StartsWith($root.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Native Cargo stage directories must be outside the candidate checkout.'
        }
        if (Test-Path -LiteralPath $directory) { throw 'Native Cargo stage directories must be fresh.' }
    }
    $null = [IO.Directory]::CreateDirectory($cargoHome)
    if ($Arguments[0] -eq 'test') { $null = [IO.Directory]::CreateDirectory($targetDirectory) }
    Write-Output "${Stage}: cargo $($Arguments -join ' ')"
    Write-Output "${Stage}: CARGO_HOME=$cargoHome"
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
    $start.Environment['CARGO_HOME'] = $cargoHome
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
        Assert-AncestorConfigHashes $Stage
        Assert-SourceOnlyCheckout $Stage
        Assert-ExpectedSourceHashes $Stage
    }
}
if ($Phase -eq 'OriginalListing') {
    Invoke-CargoStage 'original-test-listing' @('test', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
        'tools\wta\Cargo.toml', '--', '--list') $false 'listing'
    return
}
Assert-SourceOnlyCheckout 'proposal-application'
Assert-ExpectedSourceHashes 'proposal-application'
foreach ($replacement in $replacements) {
    $attributes = [IO.File]::GetAttributes($replacement.LocalPath)
    if (($attributes -band ([IO.FileAttributes]::Directory -bor [IO.FileAttributes]::ReparsePoint)) -ne 0) {
        throw 'Proposal application requires an existing regular source file.'
    }
    [IO.File]::WriteAllBytes($replacement.LocalPath, $replacement.Bytes)
}
$expectedHashes = $appliedHashes
Assert-SourceOnlyCheckout 'proposal-application'
Assert-ExpectedSourceHashes 'proposal-application'
if ($Phase -eq 'Focused') {
    Invoke-CargoStage 'format-check' @('fmt', '--manifest-path', 'tools\wta\Cargo.toml', '--', '--check')
    Invoke-CargoStage 'focused-tests' @('test', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
        'tools\wta\Cargo.toml', $filter, '--', '--exact') $true 'passing'
} else {
    Invoke-CargoStage 'full-suite' @('test', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path',
        'tools\wta\Cargo.toml') $true
}
Write-Output 'Unit-test success is not an end-to-end performance measurement.'
