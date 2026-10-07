[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'These native fixture tests require Windows.' }
$validator = Join-Path $PSScriptRoot 'validate-native.ps1'
$runtime = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\skills\pr-performance-review\scripts\performance-review.mjs'))
$fixture = Join-Path (Get-Location).Path ('.native-validation-fixture-' + [guid]::NewGuid().ToString('N'))
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
function Test-Case([string]$Name, [string]$Source, [string]$Filter, [string]$ErrorPattern = '', [bool]$Unformatted = $false, [bool]$DirtyStart = $false) {
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
    $proposalPath = Join-Path $fixture "$Name.json"
    [IO.File]::WriteAllText($proposalPath, ($proposal | ConvertTo-Json -Depth 20), $utf8)
    if ($DirtyStart) { [IO.File]::WriteAllText($sourcePath, $Source, $utf8) }
    $messages = & pwsh -NoProfile -File $validator -ProposalPath $proposalPath -RepositoryRoot $fixture -TrustedRuntimePath $runtime 2>&1
    $exit = $LASTEXITCODE
    if ($ErrorPattern) {
        if ($exit -eq 0 -or ($messages -join "`n") -notmatch $ErrorPattern) {
            throw "$Name did not fail as expected (exit ${exit}): $messages"
        }
    } elseif ($exit -ne 0 -or ($messages -join "`n") -notmatch 'focused-tests: 1 native test\(s\) passed' -or
        ($messages -join "`n") -notmatch 'full-suite: [1-9][0-9]* native test\(s\) passed' -or
        ($messages -join "`n") -notmatch ('original HEAD contains ' + [regex]::Escape($Filter) + ': test') -or
        ($messages -join "`n") -notmatch ('(?m)^test ' + [regex]::Escape($Filter) + '(?: - should panic)? \.\.\. ok\r?$') -or
        ($messages -join "`n") -notmatch ([regex]::Escape($Filter) + ' -- --exact(?:\r?\n|$)')) {
        throw "$Name did not run focused and full native tests (exit ${exit}): $messages"
    }
    if ($Name -in @('exact-unknown-function', 'proposal-added-test') -and
        ($messages -join "`n") -match 'format-check: cargo') {
        throw "Original HEAD selector rejection ran candidate commands: $messages"
    }
    if ($Name -eq 'full-suite-failure' -and ($messages -join "`n") -notmatch 'focused-tests: 1 native test\(s\) passed') {
        throw "Full-suite regression did not first pass the focused test: $messages"
    }
    $script:count++
    Write-Output "PASS $Name"
}

try {
    $null = [IO.Directory]::CreateDirectory((Join-Path $fixture 'tools\wta\src'))
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\Cargo.toml'), $manifest, $utf8)
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\src\lib.rs'), $baseline, $utf8)
    [IO.File]::WriteAllText((Join-Path $fixture '.gitignore'), "/tools/wta/target/`n/tools/wta/Cargo.lock`n", $utf8)
    Format-Fixture
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
    Write-Output "Passed $count native step fixture cases."
} finally {
    if (Test-Path -LiteralPath $fixture) { Remove-Item -LiteralPath $fixture -Recurse -Force }
}
