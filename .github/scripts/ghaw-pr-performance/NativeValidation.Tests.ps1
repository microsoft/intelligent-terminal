[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'These native fixture tests require Windows.' }
$validator = Join-Path $PSScriptRoot 'validate-native.ps1'
$runtime = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\skills\pr-performance-review\scripts\performance-review.mjs'))
$fixture = Join-Path (Get-Location).Path ('.native-validation-fixture-' + [guid]::NewGuid().ToString('N'))
$utf8 = [Text.UTF8Encoding]::new($false)
$baseline = "#[cfg(test)] mod tests { #[test] fn focused() { assert_eq!(2 + 2, 4); } }`n"
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
function Test-Case([string]$Name, [string]$Source, [string]$Filter, [string]$ErrorPattern = '', [bool]$Unformatted = $false) {
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
    $messages = & pwsh -NoProfile -File $validator -ProposalPath $proposalPath -RepositoryRoot $fixture -TrustedRuntimePath $runtime 2>&1
    $exit = $LASTEXITCODE
    if ($ErrorPattern) {
        if ($exit -eq 0 -or ($messages -join "`n") -notmatch $ErrorPattern) {
            throw "$Name did not fail as expected (exit ${exit}): $messages"
        }
    } elseif ($exit -ne 0 -or ($messages -join "`n") -notmatch 'focused-tests: 1 native test\(s\) passed' -or
        ($messages -join "`n") -notmatch 'full-suite: 1 native test\(s\) passed') {
        throw "$Name did not run focused and full native tests (exit ${exit}): $messages"
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
    $baseline += "// Immutable PR candidate fixture`n"
    [IO.File]::WriteAllText((Join-Path $fixture 'tools\wta\src\lib.rs'), $baseline, $utf8)
    $null = Git @('add', 'tools/wta/src/lib.rs')
    $null = Git @('-c', 'user.name=Native Fixture', '-c', 'user.email=fixture@invalid', '-c', 'core.hooksPath=NUL',
        'commit', '--quiet', '-m', "Immutable fixture PR head`n`nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
    $script:head = Git @('rev-parse', 'HEAD')
    Test-Case 'valid-windows-run' ($baseline.Replace('2 + 2, 4', '3 + 3, 6')) 'tests::focused'
    Test-Case 'format-failure' "#[cfg(test)] mod tests { #[test] fn focused() { assert_eq!(2 + 2, 4); } }`n" 'tests::focused' 'format-check: native validation failed with exit code' $true
    Test-Case 'failed-test' ($baseline.Replace('2 + 2, 4', '2 + 2, 5')) 'tests::focused' 'focused-tests: native validation failed with exit code'
    Test-Case 'zero-matches' ($baseline.Replace('2 + 2, 4', '3 + 3, 6')) 'nonexistent_test_filter' 'focused-tests: native validation did not execute any passing tests'
    Test-Case 'full-suite-failure' ($baseline + "#[test] fn outside_focus_fails() { assert_eq!(2 + 2, 5); }`n") 'tests::focused' 'full-suite: native validation failed with exit code'
    $mutation = @'
#[cfg(test)] mod tests {
    #[test] fn focused() {
        use std::io::Write;
        // INDEX_FLAG
        std::fs::OpenOptions::new().append(true)
            .open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap()
            .write_all(b"\n# changed by test\n").unwrap();
    }
}
'@
    Test-Case 'tracked-manifest-mutation' $mutation 'tests::focused' 'changed tracked workspace bytes: tools/wta/Cargo.toml'
    $flag = @'
let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(std::process::Command::new("git").arg("-C").arg(root)
            .args(["update-index", "--assume-unchanged", "tools/wta/Cargo.toml"]).status().unwrap().success());
'@
    Test-Case 'assume-unchanged-manifest-mutation' ($mutation.Replace('// INDEX_FLAG', $flag)) 'tests::focused' 'changed tracked workspace bytes: tools/wta/Cargo.toml'
    $environmentTest = @'
#[cfg(test)] mod tests { #[test] fn focused() {
    for key in ["GH_TOKEN", "COPILOT_GITHUB_TOKEN", "ACTIONS_RUNTIME_TOKEN", "GH_AW_GITHUB_READ_TOKEN",
                "OTEL_EXPORTER_OTLP_HEADERS", "GITHUB_ENV", "GITHUB_OUTPUT"] {
        assert!(std::env::var_os(key).is_none(), "inherited {}", key);
    }
    assert_eq!(std::env::var("RUNNER_TRACKING_ID").unwrap(), "native-step-fixture");
} }
'@
    $originalEnvironment = @{}
    try {
        foreach ($key in @('GH_TOKEN', 'COPILOT_GITHUB_TOKEN', 'ACTIONS_RUNTIME_TOKEN', 'GH_AW_GITHUB_READ_TOKEN',
            'OTEL_EXPORTER_OTLP_HEADERS', 'GITHUB_ENV', 'GITHUB_OUTPUT', 'RUNNER_TRACKING_ID')) {
            $originalEnvironment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            [Environment]::SetEnvironmentVariable($key, 'native-step-fixture', 'Process')
        }
        Test-Case 'child-environment-only' $environmentTest 'tests::focused'
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
