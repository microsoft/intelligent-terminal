[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ScopePath,
    [Parameter(Mandatory)][string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$scope = Get-Content -LiteralPath $ScopePath -Raw | ConvertFrom-Json
foreach ($sha in @($scope.identity.baseSha, $scope.identity.headSha)) {
    if ($sha -cnotmatch '^[0-9a-f]{40}$') { throw 'Native analysis requires immutable base/head commits.' }
}
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $output | Out-Null
$result = [ordered]@{
    baseSha = $scope.identity.baseSha
    headSha = $scope.identity.headSha
    status = 'incomplete'
    reason = $null
    runId = $env:GITHUB_RUN_ID
    runAttempt = $env:GITHUB_RUN_ATTEMPT
    workflowSha = $env:GITHUB_SHA
    phases = @()
    failures = @()
}

function Invoke-RecordedCommand {
    param([string]$File, [string[]]$Arguments, [string]$Log)
    & $File @Arguments 2>&1 | Out-File -LiteralPath (Join-Path $output $Log) -Encoding utf8
    $code = $LASTEXITCODE
    $result.phases += [ordered]@{ command = @($File) + $Arguments; log = $Log; exitCode = $code }
    return $code
}

try {
    if (-not $scope.nativeAnalysis.enabled) {
        $result.status = 'skipped'
        $result.reason = $scope.nativeAnalysis.reason
        return
    }
    if (-not $IsWindows) { throw 'Native performance analysis requires Windows x64.' }
    if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
        throw 'Native performance analysis requires Windows x64.'
    }
    & git merge-base --is-ancestor $scope.identity.baseSha $scope.identity.headSha
    if ($LASTEXITCODE -ne 0) { throw 'Comparison base is not an ancestor of the reviewed head.' }
    $changedConfiguration = @(& git diff --name-only $scope.identity.baseSha $scope.identity.headSha -- `
        .cargo tools/wta/.cargo src/common.build.pre.props .clang-tidy)
    if ($LASTEXITCODE -ne 0) { throw 'Could not verify comparable analyzer configuration.' }
    if ($changedConfiguration.Count) { throw 'Analyzer configuration changed; comparable native analysis requires manual review.' }
    $worktrees = @{}
    foreach ($phase in @('base', 'head')) {
        $directory = Join-Path $output "source-$phase"
        $sha = if ($phase -eq 'base') { $scope.identity.baseSha } else { $scope.identity.headSha }
        if (Test-Path -LiteralPath $directory) {
            $existing = & git -C $directory rev-parse HEAD
            if ($LASTEXITCODE -ne 0 -or $existing -cne $sha) { throw "Existing $phase worktree is not the reviewed snapshot." }
            & git -C $directory diff --quiet $sha --
            if ($LASTEXITCODE -ne 0) { throw "Existing $phase source changed after analysis." }
            $untracked = @(& git -C $directory ls-files --others --exclude-standard)
            if ($LASTEXITCODE -ne 0 -or $untracked.Count) { throw "Existing $phase worktree contains unexpected files." }
        }
        else {
            & git worktree add --detach $directory $sha
            if ($LASTEXITCODE -ne 0) { throw "Could not materialize immutable $phase source." }
        }
        $worktrees[$phase] = $directory
    }
    $projects = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $mapped = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $headRoot = $worktrees.head
    foreach ($project in Get-ChildItem -LiteralPath (Join-Path $headRoot 'src') -Filter *.vcxproj -Recurse) {
        [xml]$xml = Get-Content -LiteralPath $project.FullName -Raw
        foreach ($item in $xml.SelectNodes("//*[local-name()='ClCompile' or local-name()='ClInclude'][@Include]")) {
            $include = $item.GetAttribute('Include')
            if ($include -match '[$*?;]') { continue }
            $full = [IO.Path]::GetFullPath((Join-Path $project.DirectoryName $include))
            $relative = [IO.Path]::GetRelativePath($headRoot, $full).Replace('\', '/')
            if ($scope.nativeAnalysis.cpp -contains $relative) {
                [void]$projects.Add([IO.Path]::GetRelativePath($headRoot, $project.FullName))
                [void]$mapped.Add($relative)
            }
        }
    }
    $result.cppProjects = @($projects | Sort-Object)
    $result.unmappedCpp = @($scope.nativeAnalysis.cpp | Where-Object { -not $mapped.Contains($_) })
    $result.cppCoverage = @{}
    $result.cppChecks = @{}
    if ($projects.Count -gt 3) { throw 'More than three C++ projects are affected; native analysis requires manual scoping.' }
    if ($scope.nativeAnalysis.rust) {
        $toolchain = '1.93.0'
        & rustup component list --toolchain $toolchain --installed *> (Join-Path $output 'rust-components.log')
        if ($LASTEXITCODE -ne 0 -or -not (Select-String -LiteralPath (Join-Path $output 'rust-components.log') -Pattern '^clippy-' -Quiet)) {
            & rustup toolchain install $toolchain --profile minimal --component clippy
            if ($LASTEXITCODE -ne 0) { throw 'Rust 1.93.0 with Clippy is unavailable.' }
        }
        $env:RUSTUP_TOOLCHAIN = $toolchain
    }
    foreach ($phase in @('base', 'head')) {
        $directory = $worktrees[$phase]
        Push-Location -LiteralPath $directory
        try {
            try {
                if ($projects.Count) {
                    $missingPackages = $false
                    $configs = @(& git ls-files -- .nuget/packages.config dep/nuget/packages.config)
                    if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate native dependency manifests.' }
                    foreach ($config in $configs) {
                        [xml]$packages = Get-Content -LiteralPath $config -Raw
                        foreach ($package in $packages.SelectNodes('/packages/package')) {
                            if (-not (Test-Path -LiteralPath (Join-Path $directory "packages/$($package.id).$($package.version)"))) {
                                $missingPackages = $true
                            }
                        }
                    }
                    if ($missingPackages -and (Test-Path -LiteralPath 'OpenConsole.slnx')) {
                        $nuget = Join-Path $directory 'dep\nuget\nuget.exe'
                        foreach ($manifest in @('OpenConsole.slnx', 'dep\nuget\packages.config')) {
                            $log = "$phase-restore-$([IO.Path]::GetFileNameWithoutExtension($manifest)).log"
                            if ((Invoke-RecordedCommand $nuget @('restore', $manifest, '-Verbosity', 'quiet') $log) -ne 0) {
                                throw "C++ dependency restore failed for $phase/$manifest."
                            }
                        }
                    }
                    if ((Invoke-RecordedCommand 'MSBuild.exe' @('/nologo', '/version') "$phase-msbuild-version.log") -ne 0) {
                        throw 'MSBuild is unavailable in the Windows developer environment.'
                    }
                    $index = 0
                    foreach ($project in $result.cppProjects) {
                        $index++
                        $properties = @("/p:SolutionDir=$directory\", '/p:Configuration=AuditMode',
                            '/p:Platform=x64', '/p:PerformanceAnalysis=Extended', '/p:ClangTidyExtraArgs=-Wno-error')
                        $itemsLog = "$phase-cpp-$index-items.json"
                        $code = Invoke-RecordedCommand 'MSBuild.exe' (@($project, '/nologo', '/getItem:ClCompile,ClInclude',
                            '/getProperty:EnableClangTidyCodeAnalysis,PerformanceAnalysis,ClangTidyChecks') + $properties) $itemsLog
                        if ($code -ne 0) { throw "Could not evaluate native project membership for $project." }
                        $evaluated = Get-Content -LiteralPath (Join-Path $output $itemsLog) -Raw | ConvertFrom-Json
                        if ($evaluated.Properties.EnableClangTidyCodeAnalysis -ne 'true' -or
                            $evaluated.Properties.PerformanceAnalysis -ne 'Extended') {
                            throw "C++ performance analysis is not enabled for $project."
                        }
                        $checks = $evaluated.Properties.ClangTidyChecks
                        if ($phase -eq 'head' -and $result.cppChecks[$project] -cne $checks) {
                            throw "Evaluated Clang-Tidy checks changed between base and head for $project."
                        }
                        $result.cppChecks[$project] = $checks
                        $members = @($evaluated.Items.ClCompile) + @($evaluated.Items.ClInclude)
                        $owned = @($members | Where-Object { $_.ExcludedFromBuild -ne 'true' } | ForEach-Object {
                            [IO.Path]::GetRelativePath($directory, $_.FullPath).Replace('\', '/')
                        })
                        if (-not @($scope.nativeAnalysis.cpp | Where-Object { $owned -contains $_ }).Count) {
                            throw "Evaluated project $project contains no changed native analysis input."
                        }
                        $result.cppCoverage[$phase] = @($result.cppCoverage[$phase] | Where-Object { $_ }) +
                            @($scope.nativeAnalysis.cpp | Where-Object { $owned -contains $_ })
                        $code = Invoke-RecordedCommand 'MSBuild.exe' (@($project, '/nologo', '/t:Build;ClangTidy', '/m:2') + $properties) "$phase-cpp-$index.log"
                        if ($code -ne 0) { throw "C++ performance analysis failed for $phase/$project." }
                    }
                }
            }
            catch {
                $result.failures += "$phase/C++: $($_.Exception.Message)"
                Write-Warning $result.failures[-1]
            }
            try {
                if ($scope.nativeAnalysis.rust) {
                    $env:CARGO_TARGET_DIR = Join-Path $output "rust-target-$phase"
                    if ((Invoke-RecordedCommand 'cargo.exe' @('clippy', '--version') "$phase-clippy-version.log") -ne 0) {
                        throw 'The selected Rust toolchain has no usable Clippy.'
                    }
                    $code = Invoke-RecordedCommand 'cargo.exe' @('wta-perf-extended') "$phase-rust.jsonl"
                    if ($code -ne 0) {
                        if (Select-String -LiteralPath (Join-Path $output "$phase-rust.jsonl") -Pattern 'attempting to make an HTTP request|--frozen was specified|no matching package named' -Quiet) {
                            $beforeRestore = "$phase-rust-before-restore.log"
                            Move-Item -LiteralPath (Join-Path $output "$phase-rust.jsonl") -Destination (Join-Path $output $beforeRestore)
                            $result.phases[-1].log = $beforeRestore
                            if ((Invoke-RecordedCommand 'cargo.exe' @('fetch', '--locked', '--target', 'x86_64-pc-windows-msvc',
                                '--manifest-path', 'tools\wta\Cargo.toml') "$phase-rust-fetch.log") -ne 0) {
                                throw "Could not restore locked Rust dependencies for $phase."
                            }
                            $code = Invoke-RecordedCommand 'cargo.exe' @('wta-perf-extended') "$phase-rust.jsonl"
                        }
                        if ($code -ne 0) { throw "Rust performance analysis failed for $phase." }
                    }
                }
            }
            catch {
                $result.failures += "$phase/Rust: $($_.Exception.Message)"
                Write-Warning $result.failures[-1]
            }
        }
        finally { Pop-Location }
    }
    $result.unmappedCpp = @($scope.nativeAnalysis.cpp | Where-Object {
        $result.cppCoverage.base -notcontains $_ -or $result.cppCoverage.head -notcontains $_
    })
    if ($result.unmappedCpp.Count) { throw 'Some changed C++ inputs have no verified direct project membership in both revisions; see unmappedCpp.' }
    if ($result.failures.Count) { throw ($result.failures -join '; ') }
    $result.status = 'completed'
}
catch {
    $result.reason = $_.Exception.Message
    Write-Warning $result.reason
}
finally {
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $output 'native-results.json') -Encoding utf8
}
if ($result.status -eq 'incomplete') { exit 1 }
