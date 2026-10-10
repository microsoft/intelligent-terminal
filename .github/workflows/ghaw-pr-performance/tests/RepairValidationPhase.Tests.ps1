[CmdletBinding()]
param([switch]$AnalysisOnly)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'These native fixture tests require Windows.' }
$validator = Join-Path $PSScriptRoot '..\scripts\run-native-performance-checks.ps1'
$runtime = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\scripts\performance-review.mjs'))
$workspace = Join-Path (Get-Location).Path ('.native-validation-fixture-' + [guid]::NewGuid().ToString('N'))
$analysisWorkspace = [IO.Path]::GetFullPath((Join-Path (Split-Path (Get-Location).Path -Parent) ('performance-analysis-fixture-' + [guid]::NewGuid().ToString('N'))))
$fixture = Join-Path $workspace 'candidate'
$targetParent = Join-Path $workspace 'targets'
$utf8 = [Text.UTF8Encoding]::new($false)
$baseline = "fn work() -> usize { 4 }`nfn fail() { panic!(`"expected`"); }`n#[cfg(test)] mod tests { #[test] fn focused() { assert_eq!(super::work(), 4); } #[test] fn focused_extra() { assert_eq!(super::work(), 4); } #[test] #[should_panic] fn panics() { super::fail(); } }`n"
$manifest = "[package]`nname = `"native-validation-fixture`"`nversion = `"0.0.0`"`nedition = `"2021`"`n"
$count = 0
function Git([string[]]$Arguments) {
    $result = & git.exe -C $fixture @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Fixture Git failure: $result" }
    return ($result -join "`n")
}
function Format-Fixture {
    $messages = & cargo fmt --manifest-path (Join-Path $fixture 'tools\wta\Cargo.toml') 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Fixture formatting failed: $messages" }
}
function Test-SourceAnalysis {
    $workspace = $analysisWorkspace
    $authoring = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..\..'))
    $sourceRoot = Join-Path $workspace 'analysis-source'
    $null = [IO.Directory]::CreateDirectory((Join-Path $sourceRoot 'tools\wta\src'))
    $rustSource = @'
#![allow(dead_code)]
pub mod positive {
    pub fn needless(n: usize) -> usize { (0..n).collect::<Vec<_>>().len() }
    pub async fn huge() {
        let bytes = [0u8; 20000];
        std::future::ready(()).await;
        std::hint::black_box(bytes);
    }
    pub async fn large() { huge().await; }
}
pub mod negative {
    pub fn direct_count(n: usize) -> usize { (0..n).count() }
    pub fn retained(n: usize) -> Vec<usize> { (0..n).collect() }
    pub async fn boxed() { Box::pin(super::positive::huge()).await; }
    pub async fn lexical_drop() {
        let mutex = std::sync::Mutex::new(0);
        { let guard = mutex.lock().unwrap(); std::hint::black_box(&guard); }
        std::future::ready(()).await;
    }
}
pub mod intentional_false_positive {
    pub async fn explicit_drop() {
        let mutex = std::sync::Mutex::new(0);
        let guard = mutex.lock().unwrap();
        std::hint::black_box(&guard);
        drop(guard);
        std::future::ready(()).await;
    }
}
'@
    $source = Join-Path $sourceRoot 'tools\wta\src\lib.rs'
    [IO.File]::WriteAllText($source, $rustSource, $utf8)
    [IO.File]::WriteAllText((Join-Path $sourceRoot 'tools\wta\Cargo.toml'), $manifest, $utf8)
    & cargo +1.93.0 generate-lockfile --manifest-path (Join-Path $sourceRoot 'tools\wta\Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'Rust 1.93.0 fixture lock generation failed.' }
    & git.exe -C $sourceRoot init --quiet
    & git.exe -C $sourceRoot -c core.autocrlf=false add .
    & git.exe -C $sourceRoot -c user.name=Fixture -c user.email=fixture@invalid -c core.hooksPath=NUL commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'Analysis fixture base commit failed.' }
    $baseSha = (& git.exe -C $sourceRoot rev-parse HEAD).Trim()
    [IO.File]::AppendAllText($source, "`n// Head differs without introducing a new diagnostic.`n", $utf8)
    & git.exe -C $sourceRoot -c core.autocrlf=false add .
    & git.exe -C $sourceRoot -c user.name=Fixture -c user.email=fixture@invalid -c core.hooksPath=NUL commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'Analysis fixture head commit failed.' }
    $headSha = (& git.exe -C $sourceRoot rev-parse HEAD).Trim()
    $scopeDirectory = Join-Path $workspace 'analysis-scope'
    Push-Location $sourceRoot
    try {
        & node $runtime prepare --output-dir $scopeDirectory --pr 1 --base $baseSha --head $headSha
        if ($LASTEXITCODE -ne 0) { throw 'Analysis fixture classification failed.' }
    } finally { Pop-Location }
    $scopePath = Join-Path $scopeDirectory 'performance-scope.json'
    $records = @()
    foreach ($revision in @('BASE', 'HEAD')) {
        $checkout = Join-Path $workspace "analysis-$revision"
        & git.exe clone --quiet --no-local $sourceRoot $checkout
        if ($LASTEXITCODE -ne 0) { throw 'Fresh analysis clone failed.' }
        $sha = if ($revision -eq 'BASE') { $baseSha } else { $headSha }
        & git.exe -C $checkout checkout --quiet --detach $sha
        $out = Join-Path $workspace "performance-analysis-$revision"
        & pwsh -NoProfile -File $validator -Phase Analysis -RepositoryRoot $checkout -TrustedRuntimePath $runtime `
            -TrustedRepositoryRoot $authoring -ScopePath $scopePath -Revision $revision -OutputDirectory $out
        if ($LASTEXITCODE -ne 0) {
            $failure = Get-Content (Join-Path $out 'analysis-metadata.json') -Raw
            throw "Real Rust $revision analysis failed: $failure"
        }
        $record = Get-Content (Join-Path $out 'analysis-metadata.json') -Raw | ConvertFrom-Json
        if ($record.version -ne 2 -or $record.status -cne 'completed' -or $record.analyzedSha -cne $sha -or
            -not $record.analyzedScope.wtaRustCrate -or $record.analyzedCppTranslationUnits.Count -ne 0) {
            throw 'Real source-analysis metadata did not bind complete immutable scope.'
        }
        $records += $record
        $diagnostics = @(Get-Content (Join-Path $out 'rust-analysis.stdout.log') | ForEach-Object {
            if ($_.StartsWith('{')) {
                $item = $_ | ConvertFrom-Json
                if ($item.reason -eq 'compiler-message' -and $item.message.code) { $item.message }
            }
        })
        foreach ($code in @('clippy::needless_collect', 'clippy::large_futures', 'clippy::await_holding_lock')) {
            if (@($diagnostics | Where-Object { $_.code.code -eq $code }).Count -eq 0) { throw "Rust 1.93.0 did not emit required candidate $code" }
        }
        $negativeStart = $rustSource.Substring(0, $rustSource.IndexOf('pub mod negative')).Split("`n").Count
        $negativeEnd = $rustSource.Substring(0, $rustSource.IndexOf('pub mod intentional_false_positive')).Split("`n").Count
        foreach ($diagnostic in $diagnostics) {
            foreach ($span in $diagnostic.spans | Where-Object { $_.is_primary }) {
                if ($span.line_start -gt $negativeStart -and $span.line_start -lt $negativeEnd -and
                    $diagnostic.code.code -in @('clippy::needless_collect', 'clippy::large_futures', 'clippy::await_holding_lock')) {
                    throw 'Negative retained collection, boxed future or lexically released guard emitted an extra performance candidate.'
                }
            }
        }
        $falsePositive = @($diagnostics | Where-Object { $_.code.code -eq 'clippy::await_holding_lock' })
        if (@($falsePositive.spans | Where-Object { $_.is_primary -and $_.line_start -gt $negativeEnd }).Count -eq 0) {
            throw 'Explicit drop false-positive fixture did not exercise mandatory source triage.'
        }
        $script:count++
        Write-Output "PASS Rust-1.93-$revision-extra-positive-negative-and-intentional-drop-triage"
        if ($revision -eq 'BASE') {
            Push-Location $checkout
            $savedHome = $env:CARGO_HOME; $savedTarget = $env:CARGO_TARGET_DIR
            try {
                $env:CARGO_HOME = Join-Path $workspace 'normal-profile-home'
                $env:CARGO_TARGET_DIR = Join-Path $workspace 'normal-profile-target'
                $normal = & cargo +1.93.0 wta-perf 2>&1
                if ($LASTEXITCODE -ne 0) { throw 'Normal shared profile fixture failed.' }
                $normalText = $normal -join "`n"
                if ($normalText -match '"code":"clippy::(needless_collect|large_futures)"') {
                    throw 'Selected extra rules unexpectedly ran in baseline normal profile.'
                }
                if ($normalText -notmatch '"code":"clippy::await_holding_lock"') {
                    throw 'Normal shared profile did not include its established lock rule.'
                }
            } finally {
                Pop-Location
                $env:CARGO_HOME = $savedHome; $env:CARGO_TARGET_DIR = $savedTarget
            }
            $script:count++
            Write-Output 'PASS extra-Rust-rules-only-in-selected-normal-profile'
        }
    }
    if (($records[0].plan | ConvertTo-Json -Depth 20 -Compress) -cne ($records[1].plan | ConvertTo-Json -Depth 20 -Compress) -or
        ($records[0].tools | ConvertTo-Json -Depth 20 -Compress) -cne ($records[1].tools | ConvertTo-Json -Depth 20 -Compress)) {
        throw 'Fresh BASE and HEAD did not run identical trusted profiles/tools.'
    }
    $script:count++
    Write-Output 'PASS same-profile-fresh-BASE-HEAD-comparison'
    $out = Join-Path $workspace 'analysis-failed'
    $missing = & pwsh -NoProfile -File $validator -Phase Analysis -RepositoryRoot (Join-Path $workspace 'analysis-HEAD') `
        -TrustedRuntimePath $runtime -TrustedRepositoryRoot (Join-Path $workspace 'missing-trust') `
        -ScopePath $scopePath -Revision HEAD -OutputDirectory $out 2>&1
    if ($LASTEXITCODE -eq 0 -or (Get-Content (Join-Path $out 'analysis-metadata.json') -Raw | ConvertFrom-Json).status -ne 'incomplete') {
        throw "Missing trusted prerequisites did not capture fixed incomplete metadata: $missing"
    }
    $script:count++
    Write-Output 'PASS failed-analysis-fixed-artifact'
    $null = [IO.Directory]::CreateDirectory((Join-Path $workspace '.cargo'))
    Copy-Item -LiteralPath (Join-Path $authoring '.cargo\config.toml') -Destination (Join-Path $workspace '.cargo\config.toml')
    $ancestorOut = Join-Path $workspace 'analysis-ancestor-block'
    $null = & pwsh -NoProfile -File $validator -Phase Analysis -RepositoryRoot (Join-Path $workspace 'analysis-HEAD') `
        -TrustedRuntimePath $runtime -TrustedRepositoryRoot $authoring -ScopePath $scopePath -Revision HEAD -OutputDirectory $ancestorOut 2>&1
    $ancestorRecord = Get-Content (Join-Path $ancestorOut 'analysis-metadata.json') -Raw | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $ancestorRecord.status -cne 'incomplete' -or
        $ancestorRecord.missingPrerequisites -notmatch 'Inherited Cargo configuration' -or
        (Test-Path -LiteralPath (Join-Path $ancestorOut 'rust-analysis.stdout.log'))) {
        throw 'Identical ancestor array aliases were not blocked before Cargo could concatenate them.'
    }
    Remove-Item -LiteralPath (Join-Path $workspace '.cargo') -Recurse -Force
    $script:count++
    Write-Output 'PASS identical-ancestor-config-explicitly-blocked'
    $scope = Get-Content $scopePath -Raw | ConvertFrom-Json
    $scope.analysisPlan.rust.required = $false
    $scope.analysisPlan.rust.paths = @()
    $scope.analysisPlan | Add-Member -NotePropertyName fixtureScope -NotePropertyValue 'No native source is selected in this runner not-applicable boundary fixture.'
    $emptyScope = Join-Path $workspace 'empty-analysis-scope.json'
    [IO.File]::WriteAllText($emptyScope, ($scope | ConvertTo-Json -Depth 20), $utf8)
    $emptyOutput = Join-Path $workspace 'empty-analysis-output'
    & pwsh -NoProfile -File $validator -Phase Analysis -RepositoryRoot (Join-Path $workspace 'analysis-HEAD') `
        -TrustedRuntimePath $runtime -TrustedRepositoryRoot $authoring -ScopePath $emptyScope -Revision HEAD -OutputDirectory $emptyOutput
    if ($LASTEXITCODE -ne 0 -or (Get-Content (Join-Path $emptyOutput 'analysis-metadata.json') -Raw | ConvertFrom-Json).status -ne 'not_applicable') {
        throw 'No C++/Rust scope required an unnecessary compiler.'
    }
    $script:count++
    Write-Output 'PASS not-applicable-analysis-without-compiler'
    $cppRoot = Join-Path $workspace 'cpp-source'
    $cppDirectory = Join-Path $cppRoot 'src\types\lib'
    $null = [IO.Directory]::CreateDirectory($cppDirectory)
    $cppSource = @'
#include <vector>
std::vector<int> positive(const int count)
{
    std::vector<int> result;
    for (int index = 0; index < count; ++index)
    {
        result.push_back(index);
    }
    return result;
}
std::vector<int> negative(const int count)
{
    std::vector<int> result;
    result.reserve(static_cast<std::vector<int>::size_type>(count));
    for (int index = 0; index < count; ++index)
    {
        result.push_back(index);
    }
    return result;
}
'@
    $cppProject = @"
<?xml version="1.0" encoding="utf-8"?>
<Project DefaultTargets="Build" xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <PropertyGroup>
    <ProjectGuid>{F5711003-89C5-46C4-BCA3-334BA721D58D}</ProjectGuid>
    <ProjectName>PerformanceRuleProof</ProjectName>
    <ConfigurationType>StaticLibrary</ConfigurationType>
    <EnableHybridCRT>false</EnableHybridCRT>
    <VcpkgEnabled>false</VcpkgEnabled>
    <VcpkgEnableManifest>false</VcpkgEnableManifest>
  </PropertyGroup>
  <Import Project="$authoring\src\common.build.pre.props" />
  <PropertyGroup>
    <OutDir>`$(MSBuildThisFileDirectory)out\</OutDir>
    <IntDir>`$(MSBuildThisFileDirectory)obj\</IntDir>
    <WholeProgramOptimization>false</WholeProgramOptimization>
  </PropertyGroup>
  <Import Project="`$(VCTargetsPath)\Microsoft.Cpp.props" />
  <ItemDefinitionGroup>
    <ClCompile>
      <PrecompiledHeader>NotUsing</PrecompiledHeader>
      <ExceptionHandling>Sync</ExceptionHandling>
      <TreatWarningAsError>false</TreatWarningAsError>
      <TreatSpecificWarningsAsErrors />
    </ClCompile>
  </ItemDefinitionGroup>
  <ItemGroup><ClCompile Include="fixture.cpp" /></ItemGroup>
  <Import Project="`$(MSBuildThisFileDirectory)fixture-compile-items.props" />
  <Import Project="`$(VCTargetsPath)\Microsoft.Cpp.targets" />
</Project>
"@
    [IO.File]::WriteAllText((Join-Path $cppDirectory 'fixture.cpp'), $cppSource, $utf8)
    [IO.File]::WriteAllText((Join-Path $cppDirectory 'types.vcxproj'), $cppProject, $utf8)
    $compileItems = @'
<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <ItemGroup>
    <ClCompile Include="imported.cpp" />
    <ClCompile Include="conditional.cpp" Condition="'$(Configuration)'=='Debug'" />
    <ClCompile Include="excluded.cpp"><ExcludedFromBuild Condition="'$(Configuration)'=='AuditMode'">true</ExcludedFromBuild></ClCompile>
    <ClCompile Include="removed.cpp" Condition="Exists('$(MSBuildThisFileDirectory)removed.cpp')" />
  </ItemGroup>
</Project>
'@
    [IO.File]::WriteAllText((Join-Path $cppDirectory 'fixture-compile-items.props'), $compileItems, $utf8)
    [IO.File]::WriteAllText((Join-Path $cppDirectory 'imported.cpp'), "int imported() noexcept { return 0; }`n", $utf8)
    [IO.File]::WriteAllText((Join-Path $cppDirectory 'removed.cpp'), "int removed() noexcept { return 0; }`n", $utf8)
    $uncompiled = @('unlisted.cpp', 'ut_sibling\case.cpp', 'ft_sibling\case.cpp', 'conditional.cpp', 'excluded.cpp')
    foreach ($relative in $uncompiled) {
        $localPath = Join-Path $cppDirectory $relative
        $null = [IO.Directory]::CreateDirectory((Split-Path $localPath -Parent))
        [IO.File]::WriteAllText($localPath, "#error This source is not compiled in the selected AuditMode project.`n", $utf8)
    }
    Copy-Item -LiteralPath (Join-Path $authoring 'src\StaticAnalysis.ruleset') -Destination (Join-Path $cppRoot 'src\StaticAnalysis.ruleset')
    & git.exe -C $cppRoot init --quiet
    & git.exe -C $cppRoot -c core.autocrlf=false add .
    & git.exe -C $cppRoot -c user.name=Fixture -c user.email=fixture@invalid -c core.hooksPath=NUL commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'C++ fixture base commit failed.' }
    $cppBase = (& git.exe -C $cppRoot rev-parse HEAD).Trim()
    [IO.File]::AppendAllText((Join-Path $cppDirectory 'fixture.cpp'), "`n// Same warnings at immutable HEAD.`n", $utf8)
    [IO.File]::AppendAllText((Join-Path $cppDirectory 'imported.cpp'), "`n// Imported TU remains compiled.`n", $utf8)
    & git.exe -C $cppRoot -c core.autocrlf=false add .
    & git.exe -C $cppRoot -c user.name=Fixture -c user.email=fixture@invalid -c core.hooksPath=NUL commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'C++ fixture head commit failed.' }
    $cppHead = (& git.exe -C $cppRoot rev-parse HEAD).Trim()
    $cppScope = Join-Path $workspace 'cpp-scope'
    Push-Location $cppRoot
    try {
        & node $runtime prepare --output-dir $cppScope --pr 1 --base $cppBase --head $cppHead
        if ($LASTEXITCODE -ne 0) { throw 'C++ fixture plan failed.' }
    } finally { Pop-Location }
    $cppRecords = @()
    foreach ($revision in @('BASE', 'HEAD')) {
        $checkout = Join-Path $workspace "cpp-$revision"
        & git.exe clone --quiet --no-local $cppRoot $checkout
        $sha = if ($revision -eq 'BASE') { $cppBase } else { $cppHead }
        & git.exe -C $checkout checkout --quiet --detach $sha
        $out = Join-Path $workspace "cpp-diagnostics-$revision"
        & pwsh -NoProfile -File $validator -Phase Analysis -RepositoryRoot $checkout -TrustedRuntimePath $runtime `
            -TrustedRepositoryRoot $authoring -ScopePath (Join-Path $cppScope 'performance-scope.json') -Revision $revision -OutputDirectory $out
        if ($LASTEXITCODE -ne 0) {
            $failure = @('cpp-analysis-0.stdout.log', 'cpp-analysis-0.stderr.log') | ForEach-Object {
                $logPath = Join-Path $out $_
                if (Test-Path -LiteralPath $logPath) { Get-Content -LiteralPath $logPath -Raw }
            }
            throw "Actual C++ source-analysis runner failed: $failure"
        }
        $record = Get-Content (Join-Path $out 'analysis-metadata.json') -Raw | ConvertFrom-Json
        $cppRecords += $record
        $log = Get-Content (Join-Path $out 'cpp-analysis-0.stdout.log') -Raw
        $errorLog = Get-Content (Join-Path $out 'cpp-analysis-0.stderr.log') -Raw
        $combined = $log + $errorLog
        if ($record.status -ne 'completed' -or $record.analyzedSha -cne $sha -or
            $record.analyzedScope.cppProjects -cnotcontains 'src/types/lib/types.vcxproj' -or
            $record.analyzedCppTranslationUnits.path -cnotcontains 'src/types/lib/fixture.cpp' -or
            $record.analyzedCppTranslationUnits.path -cnotcontains 'src/types/lib/imported.cpp' -or
            $combined -notmatch 'fixture\.cpp:7:9: warning:.*\[performance-inefficient-vector-operation\]' -or
            $combined -match 'fixture\.cpp:17:9: warning:.*\[performance-inefficient-vector-operation\]') {
            throw 'C++ positive/negative fixture did not validate full project native diagnostics.'
        }
        $roundTrip = (Get-Content (Join-Path $out 'cpp-checks-0.stdout.log') -Raw).Trim()
        if ($roundTrip -cne $record.tools.clangTidyChecks) { throw 'Native MSBuild percent-escaped comma checks changed in transit.' }
        $script:count++
        Write-Output "PASS C++-$revision-trusted-global-profile-vector-positive-reserve-negative"
    }
    if (($cppRecords[0].tools | ConvertTo-Json -Depth 20 -Compress) -cne ($cppRecords[1].tools | ConvertTo-Json -Depth 20 -Compress)) {
        throw 'C++ BASE/HEAD tool versions and trusted queried checks differ.'
    }
    $script:count++
    Write-Output 'PASS same-queried-C++-profile-and-tool-versions'
    foreach ($relative in $uncompiled) {
        [IO.File]::AppendAllText((Join-Path $cppDirectory $relative), "// Changed but still not compiled.`n", $utf8)
    }
    [IO.File]::WriteAllText((Join-Path $cppDirectory 'introduced.cpp'), "#error Newly introduced but unlisted source.`n", $utf8)
    Remove-Item -LiteralPath (Join-Path $cppDirectory 'removed.cpp')
    & git.exe -C $cppRoot -c core.autocrlf=false add .
    & git.exe -C $cppRoot -c user.name=Fixture -c user.email=fixture@invalid -c core.hooksPath=NUL commit --quiet -m fixture
    if ($LASTEXITCODE -ne 0) { throw 'C++ membership counterexample commit failed.' }
    $uncoveredHead = (& git.exe -C $cppRoot rev-parse HEAD).Trim()
    $uncoveredScope = Join-Path $workspace 'cpp-uncovered-scope'
    Push-Location $cppRoot
    try {
        & node $runtime prepare --output-dir $uncoveredScope --pr 1 --base $cppHead --head $uncoveredHead
        if ($LASTEXITCODE -ne 0) { throw 'C++ uncompiled-source plan failed.' }
    } finally { Pop-Location }
    foreach ($revision in @('BASE', 'HEAD')) {
        $checkout = Join-Path $workspace "cpp-uncovered-$revision"
        & git.exe clone --quiet --no-local $cppRoot $checkout
        $sha = if ($revision -eq 'BASE') { $cppHead } else { $uncoveredHead }
        & git.exe -C $checkout checkout --quiet --detach $sha
        $out = Join-Path $workspace "cpp-uncovered-diagnostics-$revision"
        & pwsh -NoProfile -File $validator -Phase Analysis -RepositoryRoot $checkout -TrustedRuntimePath $runtime `
            -TrustedRepositoryRoot $authoring -ScopePath (Join-Path $uncoveredScope 'performance-scope.json') -Revision $revision -OutputDirectory $out
        if ($LASTEXITCODE -ne 0) { throw 'Project analysis did not complete for intentionally uncompiled source counterexamples.' }
        $record = Get-Content (Join-Path $out 'analysis-metadata.json') -Raw | ConvertFrom-Json
        if ($record.status -cne 'partial') { throw 'Successful project analysis falsely claimed coverage for uncompiled paths.' }
        foreach ($relative in $uncompiled + 'introduced.cpp') {
            $path = 'src/types/lib/' + $relative.Replace('\', '/')
            if (@($record.analyzedCppTranslationUnits | Where-Object { $_.path -ceq $path }).Count -gt 0 -or
                @($record.manualScope | Where-Object { $_.path -ceq $path }).Count -ne 1) {
                throw "Native ClCompile membership did not mark $path as uncovered/manual at $revision."
            }
        }
        $removed = @($record.analyzedCppTranslationUnits | Where-Object { $_.path -ceq 'src/types/lib/removed.cpp' })
        if ($revision -eq 'BASE' -and $removed.Count -ne 1) { throw 'Existing BASE removed TU lost its genuine native coverage.' }
        if ($revision -eq 'HEAD' -and ($removed.Count -ne 0 -or
            @($record.manualScope | Where-Object { $_.path -ceq 'src/types/lib/removed.cpp' -and $_.reason -match 'absent at HEAD' }).Count -ne 1)) {
            throw 'HEAD falsely claimed the deleted translation unit was analyzed.'
        }
        if ($revision -eq 'BASE' -and @($record.manualScope | Where-Object {
            $_.path -ceq 'src/types/lib/introduced.cpp' -and $_.reason -match 'absent at BASE' }).Count -ne 1) {
            throw 'BASE falsely claimed newly introduced source was analyzed.'
        }
        $script:count++
        Write-Output "PASS C++-$revision-native-imports-conditional-exclusions-sibling-unlisted-added-removed-coverage"
    }
}

function Test-Case([string]$Name, [string]$Source, [string]$Filter, [string]$ErrorPattern = '', [bool]$Unformatted = $false, [bool]$DirtyStart = $false, [scriptblock]$BeforeValidation = {}, [string]$Tampering = '') {
    $phase = if ($Name -eq 'full-suite-failure') { 'FullSuite' }
        elseif ($Name -match '^original-listing-|^ancestor-config-|^exact-unknown-function$|^generated-lock-') { 'OriginalListing' }
        else { 'Focused' }
    $null = Git @('read-tree', '--empty')
    $null = Git @('read-tree', $script:head)
    $sourcePath = Join-Path $fixture 'tools\wta\src\lib.rs'
    [IO.File]::WriteAllText($sourcePath, $baseline, $utf8)
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\Cargo.toml'), $manifest, $utf8)
    [IO.File]::WriteAllText($sourcePath, $Source, $utf8)
    if (-not $Unformatted) { Format-Fixture }
    $Source = [IO.File]::ReadAllText($sourcePath)
    $blob = Git @('hash-object', '-w', '--no-filters', '--', $sourcePath)
    $null = Git @('update-index', '--cacheinfo', '100644', $blob, 'tools/wta/src/lib.rs')
    $tree = Git @('write-tree')
    $null = Git @('read-tree', $script:head)
    [IO.File]::WriteAllText($sourcePath, $baseline, $utf8)
    $identity = @{ prNumber = 1; baseSha = $script:base; headSha = $script:head }
    $plan = @{ type = 'wta-unit'; testFilter = $Filter }
    $proposal = @{
        version = 1; identity = $identity; treeSha = $tree; validationPlan = $plan
        files = @(@{ path = 'tools/wta/src/lib.rs'; mode = '100644'; contents = [Convert]::ToBase64String($utf8.GetBytes($Source)) })
        report = @{
            version = 1; review = 'performance'; mode = 'repair'; identity = $identity
            status = 'pending_validation'; validationPlan = $plan
            findings = @(@{
                id = 'PERF-ABC12345'; severity = 'high'; confidence = 'high'
                dimension = 'application-performance'; category = 'wta-runtime'
                title = 'Fixture repair'; affectedScenario = 'Repeated fixture operation'
                location = 'tools/wta/src/lib.rs:1'; observed = 'Repeated unnecessary work'
                expected = 'Linear operation'; impact = 'Amplified repeated work'
                nativeEnvironment = @{ architecture = 'not-measured'; details = 'Synthetic boundary fixture, not a performance claim' }
                evidence = @(@{ type = 'complexity-proof'; detail = 'Synthetic proposal exercises the native boundary only' })
                proposedFix = 'Localized fixture replacement'; validation = $Filter; fixDisposition = 'proposed'
            })
            checks = @(@{ name = 'Performance measurement'; status = 'unavailable'; command = 'not run'; exitCode = $null; detail = 'Fixture only' })
        }
    }
    $proposalPath = Join-Path $workspace "$Name.json"
    [IO.File]::WriteAllText($proposalPath, ($proposal | ConvertTo-Json -Depth 20), $utf8)
    $caseRuntime = $runtime
    if ($Tampering) {
        $proposalPath = Join-Path $workspace 'downloaded-proposal.json'
        [IO.File]::WriteAllText($proposalPath, ($proposal | ConvertTo-Json -Depth 20), $utf8)
        $alternative = $proposal | ConvertTo-Json -Depth 20 | ConvertFrom-Json
        $alternativeSource = $repair.Replace("    2 + 2`n", "    1 + 3`n")
        [IO.File]::WriteAllText($sourcePath, $alternativeSource, $utf8)
        $alternativeBlob = Git @('hash-object', '-w', '--no-filters', '--', $sourcePath)
        $null = Git @('update-index', '--cacheinfo', '100644', $alternativeBlob, 'tools/wta/src/lib.rs')
        $alternative.treeSha = Git @('write-tree')
        $alternative.files[0].contents = [Convert]::ToBase64String($utf8.GetBytes($alternativeSource))
        $alternativePath = Join-Path $workspace 'alternative.json'
        [IO.File]::WriteAllText($alternativePath, ($alternative | ConvertTo-Json -Depth 20), $utf8)
        $null = Git @('read-tree', $script:head)
        [IO.File]::WriteAllText($sourcePath, $baseline, $utf8)
        Push-Location $fixture
        try {
            & node.exe $runtime validate-proposal --input $alternativePath --pr 1 --base $script:base --head $script:head
            if ($LASTEXITCODE -ne 0) { throw 'Tampering fixture B must be a valid sealed alternative.' }
        } finally { Pop-Location }
        $caseRuntime = Join-Path $workspace 'downloaded-runtime.mjs'
        Copy-Item -LiteralPath $runtime -Destination $caseRuntime
        $fakeHelper = "import fs from 'node:fs'; fs.writeFileSync('tools/wta/src/lib.rs', Buffer.from('$($alternative.files[0].contents)', 'base64'));"
        [IO.File]::WriteAllText((Join-Path $workspace 'fake-helper.mjs'), $fakeHelper, $utf8)
        [IO.File]::WriteAllText((Join-Path $workspace 'tamper-mode'), $Tampering, $utf8)
    }
    if ($DirtyStart) { [IO.File]::WriteAllText($sourcePath, $Source, $utf8) }
    & $BeforeValidation
    $previousTemp = $env:RUNNER_TEMP
    $previousTarget = $env:CARGO_TARGET_DIR
    $previousHome = $env:CARGO_HOME
    try {
        $env:RUNNER_TEMP = $targetParent
        # The validator must override a caller's in-checkout target path, child-only.
        $env:CARGO_TARGET_DIR = Join-Path $fixture 'tools\wta\target'
        $env:CARGO_HOME = Join-Path $fixture 'caller-cargo-home'
        $messages = & pwsh -NoProfile -File $validator -Phase $phase -ProposalPath $proposalPath -RepositoryRoot $fixture -TrustedRuntimePath $caseRuntime 2>&1
        $exit = $LASTEXITCODE
        if ($env:CARGO_TARGET_DIR -cne (Join-Path $fixture 'tools\wta\target')) { throw 'Native step changed parent target directory.' }
        if ($env:CARGO_HOME -cne (Join-Path $fixture 'caller-cargo-home')) { throw 'Native step changed parent Cargo home.' }
    } finally {
        $env:RUNNER_TEMP = $previousTemp
        $env:CARGO_TARGET_DIR = $previousTarget
        $env:CARGO_HOME = $previousHome
    }
    if ($ErrorPattern) {
        if ($exit -eq 0 -or ($messages -join "`n") -notmatch $ErrorPattern) {
            throw "$Name did not fail as expected (exit ${exit}): $messages"
        }
    } elseif ($phase -eq 'OriginalListing') {
        if ($exit -ne 0 -or ($messages -join "`n") -notmatch ('original HEAD contains ' + [regex]::Escape($Filter) + ': test')) {
            throw "$Name did not list the original test (exit ${exit}): $messages"
        }
    } elseif ($exit -ne 0 -or ($messages -join "`n") -notmatch 'focused-tests: 1 native test\(s\) passed' -or
        ($messages -join "`n") -notmatch ('(?m)^test ' + [regex]::Escape($Filter) + '(?: - should panic)? \.\.\. ok\r?$') -or
        ($messages -join "`n") -notmatch ([regex]::Escape($Filter) + ' -- --exact(?:\r?\n|$)')) {
        throw "$Name did not run the exact focused native test (exit ${exit}): $messages"
    }
    if ($Name -in @('exact-unknown-function', 'proposal-added-test') -and
        ($messages -join "`n") -match 'format-check: cargo') {
        throw "Original HEAD selector rejection ran candidate commands: $messages"
    }
    if ($Name -eq 'full-suite-failure' -and ($messages -join "`n") -match 'focused-tests: cargo') {
        throw "Full-suite phase reused a focused stage: $messages"
    }
    if ($Tampering) {
        if (-not (Test-Path -LiteralPath (Join-Path $workspace 'tampering-observed'))) { throw 'Compiled build.rs did not run the attack.' }
        if ([IO.File]::ReadAllText($sourcePath) -cne $baseline) { throw 'Original listing applied candidate bytes.' }
        if (($messages -join "`n") -notmatch 'original HEAD contains tests::focused: test') { throw 'Attack did not reach original compiled listing.' }
        if ($Tampering -match 'proposal' -and [IO.File]::ReadAllText($proposalPath) -cne [IO.File]::ReadAllText($alternativePath)) {
            throw 'Build script did not overwrite the downloaded proposal with valid B.'
        }
        if ($Tampering -match 'runtime' -and [IO.File]::ReadAllText($caseRuntime) -cne $fakeHelper) {
            throw 'Build script did not overwrite the runtime helper.'
        }
        Remove-Item -LiteralPath (Join-Path $workspace 'tampering-observed'), (Join-Path $workspace 'tamper-mode')
    }
    if ($Name -match 'config|runner|membership' -and $Name -notlike 'tracked-root-config-unchanged-*' -and
        ($messages -join "`n") -match 'full-suite: cargo') {
        throw "Repository-local injection reached the full suite: $messages"
    }
    if ($Name -eq 'force-staged-config-fake-runner-injection') {
        $null = Git @('ls-files', '--error-unmatch', '.cargo/config.toml', 'fixture-fake-runner.cmd')
        if ((Git @('ls-files', '--others', '-z')).Length -gt 0) {
            throw 'Staged injection did not reproduce the empty-untracked-list bypass.'
        }
    }
    if ($Name -eq 'core-worktree-config-fake-runner-injection') {
        if ((Git @('ls-files', '--others', '-z')).Length -gt 0) {
            throw 'Worktree diversion did not hide the actual-root injection from Git.'
        }
        if (-not (Test-Path -LiteralPath (Join-Path $fixture '.cargo\config.toml'))) {
            throw 'Worktree diversion fixture did not leave the actual-root injected config.'
        }
        $null = Git @('config', '--unset', 'core.worktree')
    }
    if ($Name -match '^initial-' -and ($messages -join "`n") -match 'original-test-listing: cargo') {
        throw "Initial untracked checkout ran Cargo: $messages"
    }
    if ($Name -like 'tracked-root-config-blocked-*' -and
        (($messages -join "`n") -match ': cargo ' -or (Test-Path (Join-Path $workspace 'tracked-runner-executed')))) {
        throw "Immutable PR Cargo configuration ran a Cargo command: $messages"
    }
    if ($Name -match '^initial-|^generated-lock-' -and ($messages -join "`n") -match 'native test\(s\) passed') {
        throw "Negative pre-test fixture claimed executed tests: $messages"
    }
    if ($Name -eq 'generated-lock-blocked' -and ($messages -join "`n") -notmatch '(?s)lock file.*--locked') {
        throw "Cargo did not reject lock generation with --locked: $messages"
    }
    if ($Name -match 'fake-runner-injection' -and -not (Test-Path -LiteralPath (Join-Path $fixture 'fixture-fake-runner.cmd'))) {
        throw 'Validator silently removed the suspicious runner.'
    }
    if ($Name -eq 'initial-ignored-config' -and -not (Test-Path -LiteralPath (Join-Path $fixture '.cargo\config.toml'))) {
        throw 'Validator silently removed the initial suspicious config.'
    }
    if ($Name -eq 'external-target-artifacts') {
        if (Test-Path -LiteralPath (Join-Path $fixture 'tools\wta\target')) { throw 'Cargo used an in-checkout target directory.' }
        $targets = @(Get-ChildItem -LiteralPath $targetParent -Directory -Filter 'performance-native-target-*')
        $homes = @(Get-ChildItem -LiteralPath $targetParent -Directory -Filter 'performance-native-cargo-home-*')
        if ($targets.Count -ne 1 -or $homes.Count -ne 2 -or @($targets | Where-Object { -not (Test-Path (Join-Path $_.FullName 'x86_64-pc-windows-msvc\debug')) }).Count -gt 0) {
            throw 'Focused phase must execute only formatting and one native test stage.'
        }
    }
    if ($Name -eq 'stage-cargo-home-poisoning') {
        $homes = @(Get-ChildItem -LiteralPath $targetParent -Directory -Filter 'performance-native-cargo-home-*')
        $poisoned = @($homes | Where-Object { Test-Path (Join-Path $_.FullName 'registry\data\poison-sentinel') })
        $executed = @($homes | Where-Object { Test-Path (Join-Path $_.FullName 'actual-execution-observed') })
        if ($homes.Count -ne 2 -or $poisoned.Count -ne 1 -or $executed.Count -ne 1) {
            throw 'Focused phase must isolate formatter Cargo home and execute one real compiled test.'
        }
        foreach ($stageHome in $poisoned) {
            if (-not (Test-Path (Join-Path $stageHome.FullName 'config.toml')) -or
                -not (Test-Path (Join-Path $stageHome.FullName 'fake-runner.cmd'))) { throw 'Compiled build.rs did not poison Cargo state.' }
        }
    }
    if ($Name -like 'ancestor-config-*') {
        if (($messages -join "`n") -match 'format-check: cargo|focused-tests: cargo|full-suite: cargo') {
            throw 'Changed ancestor Cargo configuration reached candidate stages.'
        }
        if (-not (Test-Path (Join-Path $workspace 'ancestor-poisoning-observed'))) {
            throw 'Original compiled build.rs did not inject ancestor configuration.'
        }
        # Prove ordinary Cargo discovers the injected ancestor even with a fresh home.
        $savedHome = $env:CARGO_HOME
        $savedTarget = $env:CARGO_TARGET_DIR
        Push-Location $fixture
        try {
            $env:CARGO_HOME = Join-Path $workspace 'discovery-home'
            $env:CARGO_TARGET_DIR = Join-Path $workspace 'discovery-target'
            $discovery = & cargo test --locked --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml tests::focused -- --exact 2>&1
            if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $workspace 'fake-ancestor-runner-executed'))) {
                throw "Cargo did not discover the real ancestor fake runner: $discovery"
            }
        } finally {
            Pop-Location
            $env:CARGO_HOME = $savedHome
            $env:CARGO_TARGET_DIR = $savedTarget
        }
        Remove-Item -LiteralPath (Join-Path $workspace 'ancestor-poisoning-observed'), (Join-Path $workspace 'fake-ancestor-runner-executed')
        Remove-Item -LiteralPath (Join-Path $workspace 'discovery-home'), (Join-Path $workspace 'discovery-target') -Recurse -Force
    }
    foreach ($ownedAncestorPath in @('.cargo', 'ancestor-tamper-name')) {
        $ownedPath = Join-Path $workspace $ownedAncestorPath
        if (Test-Path -LiteralPath $ownedPath) { Remove-Item -LiteralPath $ownedPath -Recurse -Force }
    }
    # Remove only paths deliberately created by these isolated fixtures, never arbitrary checkout files.
    foreach ($knownPath in @('.cargo', 'fixture-fake-runner.cmd')) {
        $localPath = Join-Path $fixture $knownPath
        if (Test-Path -LiteralPath $localPath) { Remove-Item -LiteralPath $localPath -Recurse -Force }
    }
    if (Test-Path -LiteralPath $targetParent) { Remove-Item -LiteralPath $targetParent -Recurse -Force }
    $script:count++
    Write-Output "PASS $Name"
}

try {
    $null = [IO.Directory]::CreateDirectory((Join-Path $fixture 'tools\wta\src'))
    Test-SourceAnalysis
    if ($AnalysisOnly) { Write-Output "Passed $count source-analysis fixture cases."; return }
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\Cargo.toml'), $manifest, $utf8)
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\src\lib.rs'), $baseline, $utf8)
    [IO.File]::WriteAllText((Join-Path $fixture '.gitignore'), "/tools/wta/target/`n/.cargo/`n/fixture-fake-runner.cmd`n", $utf8)
    Format-Fixture
    $lockMessages = & cargo generate-lockfile --manifest-path (Join-Path $fixture 'tools\wta\Cargo.toml') 2>&1
    if ($LASTEXITCODE -ne 0) { throw "Fixture lock generation failed: $lockMessages" }
    $baseline = [IO.File]::ReadAllText((Join-Path $fixture 'tools\wta\src\lib.rs'))
    $null = Git @('init', '--quiet')
    $null = Git @('config', 'core.autocrlf', 'false')
    $null = Git @('add', '.')
    $null = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid', '-c', 'core.hooksPath=NUL',
        'commit', '--quiet', '-m', "Local native validation fixture`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
    $script:base = Git @('rev-parse', 'HEAD')
    $baseline = "// Immutable PR candidate fixture`n" + $baseline
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\src\lib.rs'), $baseline, $utf8)
    $null = Git @('add', 'tools/wta/src/lib.rs')
    $null = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid', '-c', 'core.hooksPath=NUL',
        'commit', '--quiet', '-m', "Immutable fixture PR head`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
    $script:head = Git @('rev-parse', 'HEAD')
    $repair = $baseline.Replace("    4`n", "    2 + 2`n")
    Test-Case 'valid-windows-run' $repair 'tests::focused'
    $externalArtifacts = @'
    static CHECK: std::sync::Once = std::sync::Once::new();
    CHECK.call_once(|| {
        let target = std::path::PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").unwrap());
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
        assert!(!target.canonicalize().unwrap().starts_with(root));
        let marker = target.join("native-stage-marker");
        assert!(!marker.exists(), "reused mutable stage artifacts");
        std::fs::write(marker, b"ordinary external artifact").unwrap();
    });
    2 + 2
'@
    Test-Case 'external-target-artifacts' ($repair.Replace("    2 + 2`n", $externalArtifacts + "`n")) 'tests::focused'
    Test-Case 'initial-ignored-config' $repair 'tests::focused' 'initial-checkout: untracked workspace paths' $false $false {
        $null = [IO.Directory]::CreateDirectory((Join-Path $fixture '.cargo'))
        [IO.File]::WriteAllText((Join-Path $fixture '.cargo\config.toml'), "[target.x86_64-pc-windows-msvc]`nrunner = `"fixture-fake-runner.cmd`"`n", $utf8)
    }
    Test-Case 'initial-untracked-config' $repair 'tests::focused' 'initial-checkout: untracked workspace paths' $false $false {
        [IO.File]::WriteAllText((Join-Path $fixture 'untracked-config.toml'), '# suspicious untracked file', $utf8)
    }
    Remove-Item -LiteralPath (Join-Path $fixture 'untracked-config.toml')
    Test-Case 'initial-source-reparse-point' $repair 'tests::focused' 'initial-checkout: source checkout reparse points are not allowed' $false $false {
        $junctionTarget = Join-Path $workspace 'junction-content'
        $null = [IO.Directory]::CreateDirectory($junctionTarget)
        $null = New-Item -ItemType Junction -Path (Join-Path $fixture '.cargo') -Target $junctionTarget
    }
    Test-Case 'format-failure' ($baseline.Replace("    4`n", "  4`n")) 'tests::focused' 'format-check: native validation failed with exit code' $true
    Test-Case 'failed-test' ($baseline.Replace("    4`n", "    5`n")) 'tests::focused' 'focused-tests: native validation failed with exit code'
    Test-Case 'exact-unknown-function' $repair 'tests::unknown_function' 'selector must name exactly one existing test in compiled original HEAD'
    Test-Case 'broad-module-filter' $repair 'tests' 'focused native WTA validation|selector must name exactly one existing test'
    Test-Case 'proposal-added-test' ($baseline + "#[cfg(test)] mod added { #[test] fn new_test() {} }`n") 'added::new_test' 'immutable original Rust test'
    Test-Case 'modified-selected-test' ($baseline.Replace('super::work(), 4', 'super::work(), 5')) 'tests::focused' 'immutable original Rust test'
    Test-Case 'removed-test-gate' ($baseline.Replace('#[cfg(test)]', '')) 'tests::focused' 'immutable original Rust test'
    Test-Case 'exact-no-substring-matches' $repair 'tests::focused'
    Test-Case 'named-should-panic' $repair 'tests::panics'
    Test-Case 'dirty-original-head' $repair 'tests::focused' 'clean tracked HEAD files before original test listing' $false $true
    Test-Case 'full-suite-failure' ($baseline.Replace('panic!("expected");', 'return;')) 'tests::focused' 'full-suite: native validation failed with exit code'
    $mutation = @'
fn work() -> usize {
        use std::io::Write;
        // INDEX_FLAG
        std::fs::OpenOptions::new().append(true)
            .open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap()
            .write_all(b"\n# changed by runtime\n").unwrap();
        4
}
'@
    $runtimeStart = $baseline.IndexOf('fn work()')
    $runtimeEnd = $baseline.IndexOf('fn fail()')
    $replaceWork = { param($body) $baseline.Substring(0, $runtimeStart) + $body + "`n" + $baseline.Substring($runtimeEnd) }
    $runnerInjection = @'
fn work() -> usize {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::create_dir_all(root.join(".cargo")).unwrap();
    let runner = ["@echo off\r\necho te", "st tests::focused ... ok\r\necho te",
        "st result: ok. 1 passed; 0 failed;\r\nexit /b 0\r\n"].concat();
    std::fs::write(root.join("fixture-fake-runner.cmd"), runner).unwrap();
    std::fs::write(root.join(".cargo/config.toml"),
        "[target.x86_64-pc-windows-msvc]\nrunner = \"fixture-fake-runner.cmd\"\n").unwrap();
    4
}
'@
    Test-Case 'ignored-config-fake-runner-injection' (& $replaceWork $runnerInjection) 'tests::focused' 'focused-tests: untracked workspace paths'
    $forceStage = @'
    assert!(std::process::Command::new("git").arg("-C").arg(&root)
        .args(["add", "-f", ".cargo/config.toml", "fixture-fake-runner.cmd"]).status().unwrap().success());
'@
    Test-Case 'force-staged-config-fake-runner-injection' (& $replaceWork ($runnerInjection.Replace('    4', $forceStage + "`n    4"))) 'tests::focused' 'focused-tests: untracked workspace paths'
    $divertWorktree = @'
    let alternate = root.parent().unwrap().join("alternate-worktree");
    std::fs::create_dir_all(&alternate).unwrap();
    assert!(std::process::Command::new("git").arg("-C").arg(&root)
        .args(["config", "core.worktree"]).arg(alternate).status().unwrap().success());
    let hidden = std::process::Command::new("git").arg("-C").arg(&root)
        .args(["ls-files", "--others", "-z"]).output().unwrap();
    assert!(hidden.status.success() && hidden.stdout.is_empty());
'@
    Test-Case 'core-worktree-config-fake-runner-injection' (& $replaceWork ($runnerInjection.Replace('    4', $divertWorktree + "`n    4"))) 'tests::focused' 'focused-tests: untracked workspace paths'
    $untrackedInjection = $runnerInjection.Replace('root.join(".cargo/config.toml")', 'root.join("untracked-config.toml")')
    Test-Case 'untracked-config-fake-runner-injection' (& $replaceWork $untrackedInjection) 'tests::focused' 'focused-tests: untracked workspace paths'
    Remove-Item -LiteralPath (Join-Path $fixture 'untracked-config.toml')
    $removeMembership = @'
fn work() -> usize {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(std::process::Command::new("git").arg("-C").arg(root)
        .args(["update-index", "--force-remove", "tools/wta/Cargo.toml"]).status().unwrap().success());
    4
}
'@
    Test-Case 'tracked-index-membership-removal' (& $replaceWork $removeMembership) 'tests::focused' 'focused-tests: changed tracked workspace path membership'
    Test-Case 'tracked-manifest-mutation' (& $replaceWork $mutation) 'tests::focused' 'changed tracked workspace bytes: tools/wta/Cargo.toml'
    $flag = @'
let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(std::process::Command::new("git").arg("-C").arg(root)
            .args(["update-index", "--assume-unchanged", "tools/wta/Cargo.toml"]).status().unwrap().success());
'@
    Test-Case 'assume-unchanged-manifest-mutation' (& $replaceWork ($mutation.Replace('// INDEX_FLAG', $flag))) 'tests::focused' 'changed tracked workspace bytes: tools/wta/Cargo.toml'
    $environmentTest = @'
fn work() -> usize {
    for key in ["GH_TOKEN", "COPILOT_GITHUB_TOKEN", "ACTIONS_RUNTIME_TOKEN", "GH_AW_GITHUB_READ_TOKEN",
                "OTEL_EXPORTER_OTLP_HEADERS", "GITHUB_ENV", "GITHUB_OUTPUT"] {
        assert!(std::env::var_os(key).is_none(), "inherited {}", key);
    }
    assert_eq!(std::env::var("RUNNER_TRACKING_ID").unwrap(), "native-step-fixture");
    let home = std::path::PathBuf::from(std::env::var_os("CARGO_HOME").unwrap());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    assert!(!home.canonicalize().unwrap().starts_with(root));
    assert!(home.file_name().unwrap().to_str().unwrap().starts_with("performance-native-cargo-home-"));
    assert!(std::env::var_os("USERPROFILE").is_some());
    4
}
'@
    $originalEnvironment = @{}
    try {
        foreach ($key in @('GH_TOKEN', 'COPILOT_GITHUB_TOKEN', 'ACTIONS_RUNTIME_TOKEN', 'GH_AW_GITHUB_READ_TOKEN',
            'OTEL_EXPORTER_OTLP_HEADERS', 'GITHUB_ENV', 'GITHUB_OUTPUT', 'RUNNER_TRACKING_ID')) {
            $originalEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            [Environment]::SetEnvironmentVariable($key, 'native-step-fixture', 'Process')
        }
        Test-Case 'child-environment-only' (& $replaceWork $environmentTest) 'tests::focused'
        if ($env:GH_TOKEN -cne 'native-step-fixture') { throw 'Native step changed parent authentication.' }
    } finally {
        foreach ($key in $originalEnvironment.Keys) {
            [Environment]::SetEnvironmentVariable($key, $originalEnvironment[$key], 'Process')
        }
    }
    # Original HEAD build scripts run during --list, before application.
    $buildScript = @'
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let root = std::path::PathBuf::from(root.to_string_lossy().trim_start_matches(r"\\?\"));
    let external = root.parent().unwrap();
    if let Ok(name) = std::fs::read_to_string(external.join("ancestor-tamper-name")) {
        let config_dir = external.join(".cargo");
        std::fs::create_dir_all(&config_dir).unwrap();
        let runner_path = config_dir.join("fake-runner.cmd");
        std::fs::write(&runner_path, format!(
            "@echo off\r\necho executed > \"{}\"\r\necho test tests::focused ... ok\r\necho test result: ok. 1 passed; 0 failed;\r\nexit /b 0\r\n",
            external.join("fake-ancestor-runner-executed").display())).unwrap();
        let runner = runner_path.to_string_lossy().replace('\\', "/");
        std::fs::write(config_dir.join(name.trim()), format!(
            "[target.x86_64-pc-windows-msvc]\nrunner = '{}'\n", runner)).unwrap();
        std::fs::write(external.join("ancestor-poisoning-observed"), b"compiled original injection").unwrap();
    }
    let home = std::path::PathBuf::from(std::env::var_os("CARGO_HOME").unwrap());
    assert!(!home.canonicalize().unwrap().starts_with(&root));
    assert!(!home.join("config.toml").exists(), "reused mutable Cargo config");
    assert!(!home.join("registry/data/poison-sentinel").exists(), "reused mutable registry data");
    std::fs::create_dir_all(home.join("registry/data")).unwrap();
    std::fs::write(home.join("registry/data/poison-sentinel"), b"modified registry source").unwrap();
    std::fs::write(home.join("fake-runner.cmd"),
        "@echo off\r\necho test tests::focused ... ok\r\necho test result: ok. 1 passed; 0 failed;\r\nexit /b 0\r\n").unwrap();
    let runner = home.join("fake-runner.cmd").to_string_lossy().replace('\\', "/");
    std::fs::write(home.join("config.toml"),
        format!("[target.x86_64-pc-windows-msvc]\nrunner = '{}'\n", runner)).unwrap();
    if let Ok(mode) = std::fs::read_to_string(external.join("tamper-mode")) {
        if mode.contains("proposal") {
            std::fs::copy(external.join("alternative.json"), external.join("downloaded-proposal.json")).unwrap();
        }
        if mode.contains("runtime") {
            std::fs::copy(external.join("fake-helper.mjs"), external.join("downloaded-runtime.mjs")).unwrap();
        }
        std::fs::write(external.join("tampering-observed"), b"compiled build.rs ran").unwrap();
    }
}
'@
    $null = Git @('read-tree', $script:head)
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\src\lib.rs'), $baseline, $utf8)
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\build.rs'), $buildScript, $utf8)
    Format-Fixture
    $null = Git @('add', 'tools/wta/build.rs')
    $null = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid', '-c', 'core.hooksPath=NUL',
        'commit', '--quiet', '-m', "Immutable external-input attack fixture`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
    $script:head = Git @('rev-parse', 'HEAD')
    $executionMarker = @'
    let home = std::path::PathBuf::from(std::env::var_os("CARGO_HOME").unwrap());
    assert!(home.join("registry/data/poison-sentinel").exists());
    std::fs::write(home.join("actual-execution-observed"), b"actual compiled execution observed").unwrap();
    2 + 2
'@
    Test-Case 'stage-cargo-home-poisoning' ($repair.Replace("    2 + 2`n", $executionMarker + "`n")) 'tests::focused'
    foreach ($attack in @('proposal', 'runtime', 'proposal-runtime')) {
        Test-Case "original-listing-$attack-tampering" ($baseline.Replace("    4`n", "    5`n")) 'tests::focused' `
            '' $false $false {} $attack
    }
    foreach ($configName in @('config', 'config.toml')) {
        Test-Case "ancestor-config-created-$configName" $repair 'tests::focused' `
            'original-test-listing: changed Cargo ancestor configuration' $false $false {
                [IO.File]::WriteAllText((Join-Path $workspace 'ancestor-tamper-name'), $configName, $utf8)
            }
    }
    Test-Case 'ancestor-config-modified' $repair 'tests::focused' `
        'original-test-listing: changed Cargo ancestor configuration' $false $false {
            $null = [IO.Directory]::CreateDirectory((Join-Path $workspace '.cargo'))
            [IO.File]::WriteAllText((Join-Path $workspace '.cargo\config.toml'), "[net]`noffline = true`n", $utf8)
            [IO.File]::WriteAllText((Join-Path $workspace 'ancestor-tamper-name'), 'config.toml', $utf8)
        }
    Test-Case 'ancestor-reparse-config-directory' $repair 'tests::focused' `
        'Cargo ancestor configuration reparse points are not allowed' $false $false {
            $junctionTarget = Join-Path $workspace 'ancestor-junction-content'
            $null = [IO.Directory]::CreateDirectory($junctionTarget)
            $null = New-Item -ItemType Junction -Path (Join-Path $workspace '.cargo') -Target $junctionTarget
        }
    Test-Case 'unchanged-ancestor-inputs' $repair 'tests::focused' '' $false $false {
        $null = [IO.Directory]::CreateDirectory((Join-Path $workspace '.cargo'))
        [IO.File]::WriteAllText((Join-Path $workspace '.cargo\config.toml'), "[net]`noffline = true`n", $utf8)
    }
    $savedBase = $script:base
    $savedHead = $script:head
    foreach ($configName in @('config', 'config.toml')) {
        foreach ($change in @('addition', 'deletion', 'modification', 'mode', 'unchanged')) {
            $configPath = Join-Path $fixture ('.cargo\' + $configName)
            $runnerPath = Join-Path $fixture 'fixture-fake-runner.cmd'
            $null = [IO.Directory]::CreateDirectory((Join-Path $fixture '.cargo'))
            $runner = "@echo off`r`necho executed > `"$workspace\tracked-runner-executed`"`r`necho test tests::focused ... ok`r`necho test result: ok. 1 passed; 0 failed;`r`nexit /b 0`r`n"
            [IO.File]::WriteAllText($runnerPath, $runner, $utf8)
            $config = if ($change -eq 'unchanged') { "[net]`noffline = true`n[build]`nrustflags = []`n" } else {
                "[target.x86_64-pc-windows-msvc]`nrunner = '$runnerPath'`n"
            }
            $null = Git @('read-tree', $savedBase)
            $null = Git @('add', '-f', 'fixture-fake-runner.cmd')
            if ($change -ne 'addition') {
                [IO.File]::WriteAllText($configPath, $config, $utf8)
                $null = Git @('add', '-f', ".cargo/$configName")
            }
            $tree = Git @('write-tree')
            $script:base = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid',
                'commit-tree', $tree, '-p', $savedBase, '-m', 'Tracked configuration base fixture')
            $null = Git @('read-tree', $savedHead)
            $null = Git @('add', '-f', 'fixture-fake-runner.cmd')
            if ($change -eq 'deletion') { Remove-Item -LiteralPath $configPath } else {
                $headConfig = if ($change -eq 'modification') { $config + "[build]`nrustc-wrapper = 'fixture-fake-runner.cmd'`n" } else { $config }
                [IO.File]::WriteAllText($configPath, $headConfig, $utf8)
                $null = Git @('add', '-f', ".cargo/$configName")
                if ($change -eq 'mode') { $null = Git @('update-index', '--chmod=+x', ".cargo/$configName") }
            }
            $tree = Git @('write-tree')
            $script:head = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid',
                'commit-tree', $tree, '-p', $script:base, '-m', 'Tracked configuration PR fixture')
            $null = Git @('update-ref', 'HEAD', $script:head)
            if ($change -eq 'unchanged') {
                Test-Case "tracked-root-config-unchanged-$configName" $repair 'tests::focused' '' $true
            } else {
                Test-Case "tracked-root-config-blocked-$change-$configName" $repair 'tests::focused' `
                    'original PR changes root Cargo configuration.*manual handoff' $true
            }
        }
    }
    $script:base = $savedBase
    $script:head = $savedHead
    $null = Git @('update-ref', 'HEAD', $savedHead)
    foreach ($configName in @('.cargo/CONFIG', '.CARGO/config', '.CaRgO/CoNfIg.ToMl')) {
        $null = [IO.Directory]::CreateDirectory((Join-Path $fixture '.cargo'))
        $runnerPath = Join-Path $fixture 'fixture-fake-runner.cmd'
        [IO.File]::WriteAllText($runnerPath,
            "@echo off`r`necho executed > `"$workspace\tracked-runner-executed`"`r`necho test tests::focused ... ok`r`necho test result: ok. 1 passed; 0 failed;`r`nexit /b 0`r`n", $utf8)
        $null = Git @('read-tree', $savedBase)
        $null = Git @('add', '-f', 'fixture-fake-runner.cmd')
        $inheritedToml = -not $configName.ToLowerInvariant().EndsWith('.toml')
        if ($inheritedToml) {
            [IO.File]::WriteAllText((Join-Path $fixture '.cargo\config.toml'), "[net]`noffline = true`n", $utf8)
            $null = Git @('add', '-f', '.cargo/config.toml')
        }
        $tree = Git @('write-tree')
        $script:base = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid',
            'commit-tree', $tree, '-p', $savedBase, '-m', 'Inherited root Cargo configuration fixture')
        $null = Git @('read-tree', $savedHead)
        $null = Git @('add', '-f', 'fixture-fake-runner.cmd')
        if ($inheritedToml) { $null = Git @('add', '-f', '.cargo/config.toml') }
        $configPath = Join-Path $fixture ($configName.Replace('/', '\'))
        [IO.File]::WriteAllText($configPath, "[target.x86_64-pc-windows-msvc]`nrunner = '$runnerPath'`n", $utf8)
        $blob = Git @('hash-object', '-w', '--no-filters', '--', $configPath)
        $null = Git @('update-index', '--add', '--cacheinfo', '100644', $blob, $configName)
        $tree = Git @('write-tree')
        $script:head = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid',
            'commit-tree', $tree, '-p', $script:base, '-m', 'Windows-equivalent root configuration addition')
        $null = Git @('update-ref', 'HEAD', $script:head)
        Test-Case ('tracked-root-config-blocked-case-' + $configName.Replace('/', '-')) $repair 'tests::focused' `
            'original PR changes root Cargo configuration.*manual handoff' $true
    }
    $script:base = $savedBase
    $script:head = $savedHead
    $null = Git @('update-ref', 'HEAD', $savedHead)
    # A committed stale lock must fail Cargo's --locked check, not be regenerated.
    $null = Git @('read-tree', $script:head)
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\src\lib.rs'), $baseline, $utf8)
    $manifest = $manifest.Replace('0.0.0', '0.0.1')
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\Cargo.toml'), $manifest, $utf8)
    $null = Git @('add', 'tools/wta/Cargo.toml')
    $null = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid', '-c', 'core.hooksPath=NUL',
        'commit', '--quiet', '-m', "Immutable stale-lock fixture`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
    $script:head = Git @('rev-parse', 'HEAD')
    $lockBefore = (Get-FileHash -LiteralPath (Join-Path $fixture 'tools\wta\Cargo.lock')).Hash
    Test-Case 'generated-lock-blocked' $repair 'tests::focused' 'original-test-listing: native validation failed with exit code'
    if ((Get-FileHash -LiteralPath (Join-Path $fixture 'tools\wta\Cargo.lock')).Hash -cne $lockBefore) {
        throw '--locked changed the tracked lockfile.'
    }
    Write-Output "Passed $count native step fixture cases."
} finally {
    if (Test-Path -LiteralPath $workspace) { Remove-Item -LiteralPath $workspace -Recurse -Force }
    if (Test-Path -LiteralPath $analysisWorkspace) { Remove-Item -LiteralPath $analysisWorkspace -Recurse -Force }
}
