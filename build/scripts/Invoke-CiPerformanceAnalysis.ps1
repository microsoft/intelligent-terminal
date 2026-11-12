[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('cpp', 'rust')][string]$Language,
    [Parameter(Mandatory)][string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$root = (& git rev-parse --show-toplevel)
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the analyzed checkout.' }
$head = (& git rev-parse HEAD)
if ($LASTEXITCODE -ne 0 -or $head -cnotmatch '^[0-9a-f]{40}$' -or $head -cne $env:EXPECTED_HEAD_SHA) {
    throw 'Performance analysis checkout does not match the CI head.'
}
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $output | Out-Null
$result = [ordered]@{
    headSha = $head
    ciRunId = $env:GITHUB_RUN_ID
    ciRunAttempt = $env:GITHUB_RUN_ATTEMPT
    language = $Language
    status = 'incomplete'
    reason = $null
    phases = @()
    projects = @()
}

function Invoke-AnalysisCommand {
    param([string]$File, [string[]]$Arguments, [string]$Log)
    & $File @Arguments 2>&1 | Out-File -LiteralPath (Join-Path $output $Log) -Encoding utf8
    $code = $LASTEXITCODE
    $result.phases += @{ command = @($File) + $Arguments; log = $Log; exitCode = $code }
    if ($code -ne 0) { throw "Performance analyzer failed; see $Log (exit $code)." }
}

try {
    $base = $env:PR_BASE_SHA
    if ($base -cnotmatch '^[0-9a-f]{40}$') {
        $result.status = 'skipped'
        $result.reason = 'No PR comparison base; scoped performance analysis is PR-only.'
        return
    }
    & git fetch --no-tags origin $base
    if ($LASTEXITCODE -ne 0) { throw 'Cannot fetch the CI comparison base.' }
    $mergeBase = & git merge-base $base $head
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the CI comparison base.' }
    $result.comparisonBaseSha = $mergeBase
    $changed = @(& git diff --name-only --no-renames $mergeBase $head -- src tools/wta)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot enumerate changed performance inputs.' }
    $inputs = @($changed | Where-Object {
        ($_ -notmatch '(^|/)(tests?|ut_[^/]*|ft_[^/]*|LocalTests_[^/]*|UnitTests_[^/]*|WindowsTerminal_UIATests)(/|$)') -and
        ($_ -notmatch '(^|/)(tests|test_support|[^/]+_tests)\.rs$') -and
        (($Language -eq 'cpp' -and $_ -match '^src/.+\.(c|cc|cpp|cxx|h|hh|hpp|ixx)$') -or
         ($Language -eq 'rust' -and $_ -match '^tools/wta/(src/.+\.rs|build\.rs|Cargo\.(toml|lock))$'))
    })
    $sizes = @(& git diff --numstat --no-renames $mergeBase $head -- src tools/wta)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot measure changed performance inputs.' }
    $lines = 0
    $binary = $false
    foreach ($row in $sizes) {
        $added, $removed, $file = $row -split "`t", 3
        if ($inputs -contains $file) {
            if ($added -eq '-' -or $removed -eq '-') { $binary = $true }
            else { $lines += [int]$added + [int]$removed }
        }
    }
    if (-not $inputs.Count -or $inputs.Count -gt 20 -or $lines -gt 2000 -or $binary) {
        $result.status = 'skipped'
        $result.reason = "No production $Language inputs, or outside the 20-input/2,000-line text scope; source review owns remaining coverage."
        return
    }
    if ($Language -eq 'rust') {
        Invoke-AnalysisCommand 'cargo.exe' @('clippy', '--version') 'tool-version.log'
        Invoke-AnalysisCommand 'cargo.exe' @('wta-perf-extended') 'diagnostics.jsonl'
    }
    else {
        $projects = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $mapped = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        foreach ($project in Get-ChildItem -LiteralPath (Join-Path $root 'src') -Filter *.vcxproj -Recurse) {
            [xml]$xml = Get-Content -LiteralPath $project.FullName -Raw
            foreach ($item in $xml.SelectNodes("//*[local-name()='ClCompile' or local-name()='ClInclude'][@Include]")) {
                $include = $item.GetAttribute('Include')
                if ($include -match '[$*?;%]') { continue }
                $full = [IO.Path]::GetFullPath((Join-Path $project.DirectoryName $include))
                $relative = [IO.Path]::GetRelativePath($root, $full).Replace('\', '/')
                if ($inputs -contains $relative) {
                    [void]$projects.Add($project.FullName)
                    [void]$mapped.Add($relative)
                }
            }
        }
        $result.projects = @($projects | Sort-Object)
        $result.unmappedInputs = @($inputs | Where-Object { -not $mapped.Contains($_) })
        if ($projects.Count -gt 3 -or $result.unmappedInputs.Count) {
            $result.status = 'skipped'
            $result.reason = 'More than three direct projects or unmapped inputs; manual translation-unit scoping is required.'
            return
        }
        Invoke-AnalysisCommand 'MSBuild.exe' @('/nologo', '/version') 'tool-version.log'
        $coverage = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $index = 0
        foreach ($project in $result.projects) {
            $index++
            $properties = @("/p:SolutionDir=$root\", '/p:Configuration=Debug', '/p:Platform=x64',
                '/p:PerformanceAnalysis=Extended', '/p:ClangTidyExtraArgs=-Wno-error')
            Invoke-AnalysisCommand 'MSBuild.exe' (@($project, '/nologo', '/getItem:ClCompile,ClInclude',
                '/getProperty:EnableClangTidyCodeAnalysis,PerformanceAnalysis,ClangTidyChecks') + $properties) "project-$index.json"
            $evaluated = Get-Content -LiteralPath (Join-Path $output "project-$index.json") -Raw | ConvertFrom-Json
            if ($evaluated.Properties.EnableClangTidyCodeAnalysis -ne 'true' -or
                $evaluated.Properties.PerformanceAnalysis -ne 'Extended') { throw 'C++ performance profile was not enabled.' }
            $owned = @(@($evaluated.Items.ClCompile) + @($evaluated.Items.ClInclude) |
                Where-Object { $_.ExcludedFromBuild -ne 'true' } | ForEach-Object {
                    [IO.Path]::GetRelativePath($root, $_.FullPath).Replace('\', '/')
                })
            if (@($inputs | Where-Object { $mapped.Contains($_) -and $owned -contains $_ }).Count -eq 0) {
                throw 'Selected C++ project has no evaluated changed input.'
            }
            foreach ($file in $inputs | Where-Object { $owned -contains $_ }) { [void]$coverage.Add($file) }
            Invoke-AnalysisCommand 'MSBuild.exe' (@($project, '/nologo', '/t:ClangTidy', '/m:2') + $properties) "diagnostics-$index.log"
        }
        if (@($inputs | Where-Object { -not $coverage.Contains($_) }).Count) {
            throw 'Not every changed C++ input has evaluated direct membership.'
        }
    }
    $result.status = 'completed'
}
catch {
    $result.reason = $_.Exception.Message
    Write-Error $result.reason -ErrorAction Continue
}
finally {
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $output 'native-results.json') -Encoding utf8
}
if ($result.status -eq 'incomplete') { exit 1 }
