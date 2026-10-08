[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Analysis', 'OriginalListing', 'Focused', 'FullSuite')][string]$Phase,
    [string]$ProposalPath,
    [Parameter(Mandatory)][string]$RepositoryRoot,
    [Parameter(Mandatory)][string]$TrustedRuntimePath,
    [string]$ScopePath,
    [ValidateSet('BASE', 'HEAD')][string]$Revision,
    [string]$TrustedRepositoryRoot,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or $PSVersionTable.PSVersion.Major -lt 7) {
    throw 'Native validation requires PowerShell 7 on Windows.'
}
$clock = [Diagnostics.Stopwatch]::StartNew()
$root = [IO.Path]::GetFullPath($RepositoryRoot)
if ($Phase -eq 'Analysis') {
    $metadata = [ordered]@{
        version = 2; revision = $Revision; status = 'incomplete'; identity = $null; analyzedSha = $null
        authoringSha = $null; plan = $null; commands = @(); tools = [ordered]@{}; missingPrerequisites = @()
        checks = @(); authority = 'Source diagnostics only, not repair validation or publication authority.'
        analyzedScope = @{ wtaRustCrate = $false; cppProjects = @() }
        analyzedCppTranslationUnits = @(); manualScope = @()
    }
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    $null = [IO.Directory]::CreateDirectory($output)
    $utf8 = [Text.UTF8Encoding]::new($false)
    function Invoke-AnalysisCommand([string]$Name, [string]$Executable, [string[]]$Arguments, [string]$WorkingDirectory = $root) {
        $metadata.commands += [pscustomobject]@{ name = $Name; executable = $Executable; arguments = $Arguments; workingDirectory = $WorkingDirectory }
        $start = [Diagnostics.ProcessStartInfo]::new()
        $start.FileName = $Executable
        $start.WorkingDirectory = $WorkingDirectory
        $start.UseShellExecute = $false
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
        foreach ($key in @($start.Environment.Keys)) {
            if ($key -match '(?i)(TOKEN|SECRET|PASSWORD|CREDENTIAL|OTLP.*HEADERS|^GITHUB_(ENV|OUTPUT)$)') {
                $null = $start.Environment.Remove($key)
            }
        }
        if ($metadata.plan.rust.required) {
            $start.Environment['CARGO_HOME'] = Join-Path $output 'cargo-home'
            $start.Environment['CARGO_TARGET_DIR'] = Join-Path $output 'cargo-target'
        }
        $process = [Diagnostics.Process]::new()
        $process.StartInfo = $start
        $started = $false
        try {
            $remaining = [int][Math]::Max(0, 600000 - $clock.ElapsedMilliseconds)
            if ($remaining -le 0) { throw 'Source analysis exceeded the total ten-minute deadline.' }
            $null = $process.Start()
            $started = $true
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            if (-not $process.WaitForExit($remaining)) { throw 'Source analysis exceeded the total ten-minute deadline.' }
            $remaining = [int][Math]::Max(0, 600000 - $clock.ElapsedMilliseconds)
            if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@($stdout, $stderr), $remaining)) {
                throw 'Source analysis output capture exceeded the total ten-minute deadline.'
            }
            $text = $stdout.GetAwaiter().GetResult()
            [IO.File]::WriteAllText((Join-Path $output "$Name.stdout.log"), $text, $utf8)
            [IO.File]::WriteAllText((Join-Path $output "$Name.stderr.log"), $stderr.GetAwaiter().GetResult(), $utf8)
            $metadata.checks += [pscustomobject]@{ name = $Name; exitCode = $process.ExitCode; status = if ($process.ExitCode -eq 0) { 'completed' } else { 'failed' } }
            if ($process.ExitCode -ne 0) { throw "$Name exited $($process.ExitCode); inspect raw logs. Missing SDK, generated headers or failed compilation is not clean coverage." }
            return $text
        } finally {
            if ($started -and -not $process.HasExited) {
                $process.Kill($true)
                $null = $process.WaitForExit(10000)
            }
            if ($started) {
                $capturedOutput = if ($stdout.IsCompleted) { $stdout.GetAwaiter().GetResult() } else { 'Output capture incomplete after deadline; inspect GitHub job failure.' }
                $capturedError = if ($stderr.IsCompleted) { $stderr.GetAwaiter().GetResult() } else { 'Error capture incomplete after deadline; inspect GitHub job failure.' }
                [IO.File]::WriteAllText((Join-Path $output "$Name.stdout.log"), $capturedOutput, $utf8)
                [IO.File]::WriteAllText((Join-Path $output "$Name.stderr.log"), $capturedError, $utf8)
            }
            $process.Dispose()
        }
    }
    try {
        $scope = Get-Content -LiteralPath $ScopePath -Raw | ConvertFrom-Json
        $metadata.identity = $scope.identity
        $metadata.plan = $scope.analysisPlan
        $metadata.manualScope = @($scope.analysisPlan.manualScope)
        $sha = if ($Revision -eq 'BASE') { $scope.identity.baseSha } else { $scope.identity.headSha }
        $actual = & git.exe -C $root rev-parse HEAD
        if ($LASTEXITCODE -ne 0 -or $actual -cne $sha) { throw 'Analysis checkout does not match the immutable comparison revision.' }
        $metadata.analyzedSha = $actual
        $metadata.authoringSha = & git.exe -C $TrustedRepositoryRoot rev-parse HEAD
        if ($LASTEXITCODE -ne 0) { throw 'Missing trusted authoring revision.' }
        $metadata.tools.node = (& node.exe --version) -join "`n"
        if ($scope.analysisPlan.rust.required) {
            $trustedConfig = Join-Path $TrustedRepositoryRoot '.cargo\config.toml'
            $trustedConfigHash = (Get-FileHash -LiteralPath $trustedConfig).Hash
            foreach ($relative in @('.cargo\config', 'tools\.cargo\config', 'tools\.cargo\config.toml',
                'tools\wta\.cargo\config', 'tools\wta\.cargo\config.toml')) {
                if (Test-Path -LiteralPath (Join-Path $root $relative)) { throw "Unsupported additional Cargo configuration: $relative" }
            }
            $ancestor = [IO.Directory]::GetParent($root)
            while ($null -ne $ancestor) {
                foreach ($name in @('config', 'config.toml')) {
                    $ancestorConfig = Join-Path $ancestor.FullName ".cargo\$name"
                    if (Test-Path -LiteralPath $ancestorConfig) {
                        throw 'Inherited Cargo configuration prevents isolated profiles: identical array aliases would concatenate after freezing.'
                    }
                }
                $ancestor = $ancestor.Parent
            }
            $null = [IO.Directory]::CreateDirectory((Join-Path $root '.cargo'))
            Copy-Item -LiteralPath $trustedConfig -Destination (Join-Path $root '.cargo\config.toml') -Force
            $metadata.tools.cargoConfigurationSha256 = $trustedConfigHash
            $metadata.tools.cargoConfigurationSource = 'Trusted authoring revision; replaced only in disposable analysis checkout. Immutable PR configuration still requires source review.'
            $metadata.tools.rustc = Invoke-AnalysisCommand 'rust-version' 'rustc.exe' @('+1.93.0', '--version', '--verbose')
            $metadata.tools.clippy = Invoke-AnalysisCommand 'clippy-version' 'cargo.exe' @('+1.93.0', 'clippy', '--version')
            $null = Invoke-AnalysisCommand 'cargo-fetch' 'cargo.exe' @('+1.93.0', 'fetch', '--locked', '--target', 'x86_64-pc-windows-msvc', '--manifest-path', 'tools\wta\Cargo.toml')
            $null = Invoke-AnalysisCommand 'rust-analysis' 'cargo.exe' @('+1.93.0', 'wta-perf-extended')
            $metadata.analyzedScope.wtaRustCrate = $true
        }
        if ($scope.analysisPlan.cpp.required) {
            $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
            $msbuild = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
            if (-not $msbuild) { throw 'Missing native Visual Studio MSBuild prerequisite.' }
            $metadata.tools.msbuild = Invoke-AnalysisCommand 'msbuild-version' $msbuild @('-version', '-nologo')
            $profileProject = Join-Path $TrustedRepositoryRoot $scope.analysisPlan.cpp.profileProject.Replace('/', '\')
            $checks = (Invoke-AnalysisCommand 'cpp-profile' $msbuild @($profileProject, '-nologo',
                '/p:PerformanceAnalysis=Extended', "/p:SolutionDir=$($TrustedRepositoryRoot.TrimEnd('\'))\",
                '/p:Configuration=AuditMode', '/p:Platform=x64', '/getProperty:ClangTidyChecks') $TrustedRepositoryRoot).Trim()
            if (-not $checks -or $checks -match '[\r\n]') { throw 'Trusted MSBuild profile query did not produce a single checks value.' }
            $metadata.tools.clangTidyChecks = $checks
            $clang = & $vswhere -latest -products '*' -find 'VC\Tools\Llvm\x64\bin\clang-tidy.exe' | Select-Object -First 1
            if (-not $clang) { throw 'Missing native Visual Studio clang-tidy prerequisite.' }
            $metadata.tools.clangTidy = Invoke-AnalysisCommand 'clang-tidy-version' $clang @('--version')
            $escapedChecks = $checks.Replace(',', '%2C')
            $headerFilter = [regex]::Escape((Join-Path $root 'src')).Replace('\\', '[/\\]') + '[/\\]'
            $index = 0
            foreach ($project in $scope.analysisPlan.cpp.projects) {
                $projectPath = Join-Path $root $project.Replace('/', '\')
                if (-not (Test-Path -LiteralPath $projectPath)) { throw "Selected owning project is unavailable at ${Revision}: $project" }
                $common = @($projectPath, '-nologo', "/p:SolutionDir=$($root.TrimEnd('\'))\", '/p:Configuration=AuditMode',
                    '/p:Platform=x64', "/p:ClangTidyChecks=$escapedChecks", "/p:ClangTidyHeaderFilter=$headerFilter")
                $roundTrip = (Invoke-AnalysisCommand "cpp-checks-$index" $msbuild ($common + '/getProperty:ClangTidyChecks')).Trim()
                if ($roundTrip -cne $checks) { throw 'MSBuild did not preserve the trusted comma-separated global checks property.' }
                $null = Invoke-AnalysisCommand "cpp-restore-$index" $msbuild ($common + @('/t:Restore', '/p:RestorePackagesConfig=true'))
                $items = (Invoke-AnalysisCommand "cpp-items-$index" $msbuild ($common + '/getItem:ClCompile')) | ConvertFrom-Json
                $translationUnits = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
                foreach ($item in $items.Items.ClCompile) {
                    $excluded = $item.PSObject.Properties['ExcludedFromBuild']
                    if ($null -ne $excluded -and [string]$excluded.Value -ieq 'true') { continue }
                    $fullPath = [IO.Path]::GetFullPath([string]$item.FullPath)
                    if (Test-Path -LiteralPath $fullPath -PathType Leaf) { $null = $translationUnits.Add($fullPath) }
                }
                $verifiedPaths = @()
                foreach ($candidate in $scope.analysisPlan.cpp.candidatePaths |
                    Where-Object { $_.project -ceq $project -and $_.path -match '\.(c|cc|cpp|cxx|ixx)$' }) {
                    $candidatePath = [IO.Path]::GetFullPath((Join-Path $root $candidate.path.Replace('/', '\')))
                    if ($translationUnits.Contains($candidatePath)) {
                        $verifiedPaths += [pscustomobject]@{ path = $candidate.path; project = $project; membership = 'MSBuild.ClCompile' }
                    } else {
                        $reason = if (-not (Test-Path -LiteralPath $candidatePath -PathType Leaf)) {
                            "Source is absent at ${Revision}; added/removed files are not claimed analyzed in both revisions."
                        } else {
                            'Not an active evaluated ClCompile translation unit in the selected project/configuration; actual owner requires manual review.'
                        }
                        $metadata.manualScope += [pscustomobject]@{ path = $candidate.path; reason = $reason; project = $project }
                    }
                }
                $null = Invoke-AnalysisCommand "cpp-analysis-$index" $msbuild ($common + '/t:Build;ClangTidy')
                $metadata.analyzedScope.cppProjects += $project
                $metadata.analyzedCppTranslationUnits += $verifiedPaths
                $index++
            }
        }
        $metadata.status = if (-not $scope.analysisPlan.rust.required -and -not $scope.analysisPlan.cpp.required) {
            if ($metadata.manualScope.Count) { 'partial' } else { 'not_applicable' }
        } elseif ($metadata.manualScope.Count) { 'partial' } else { 'completed' }
    } catch {
        $metadata.missingPrerequisites += $_.Exception.Message
        $metadata.status = 'incomplete'
        Write-Warning $_.Exception.Message
    } finally {
        $metadata.elapsedSeconds = $clock.Elapsed.TotalSeconds
        [IO.File]::WriteAllText((Join-Path $output 'analysis-metadata.json'), ($metadata | ConvertTo-Json -Depth 30), $utf8)
    }
    if ($metadata.status -eq 'incomplete') { throw 'Source analysis incomplete; fixed metadata and available logs have been captured.' }
    return
}
if (-not $ProposalPath) { throw 'Native repair phases require ProposalPath.' }
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
