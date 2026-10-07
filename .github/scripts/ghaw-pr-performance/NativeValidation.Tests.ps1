[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows) { throw 'These native fixture tests require Windows.' }
$validator = Join-Path $PSScriptRoot 'validate-native.ps1'
$runtime = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\skills\pr-performance-review\scripts\performance-review.mjs'))
$workspace = Join-Path (Get-Location).Path ('.native-validation-fixture-' + [guid]::NewGuid().ToString('N'))
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
function Test-Case([string]$Name, [string]$Source, [string]$Filter, [string]$ErrorPattern = '', [bool]$Unformatted = $false, [bool]$DirtyStart = $false, [scriptblock]$BeforeValidation = {}, [string]$Tampering = '') {
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
    try {
        $env:RUNNER_TEMP = $targetParent
        # The validator must override a caller's in-checkout target path, child-only.
        $env:CARGO_TARGET_DIR = Join-Path $fixture 'tools\wta\target'
        $messages = & pwsh -NoProfile -File $validator -ProposalPath $proposalPath -RepositoryRoot $fixture -TrustedRuntimePath $caseRuntime 2>&1
        $exit = $LASTEXITCODE
        if ($env:CARGO_TARGET_DIR -cne (Join-Path $fixture 'tools\wta\target')) { throw 'Native step changed parent target directory.' }
    } finally {
        $env:RUNNER_TEMP = $previousTemp
        $env:CARGO_TARGET_DIR = $previousTarget
    }
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
    if ($Tampering) {
        if (-not (Test-Path -LiteralPath (Join-Path $workspace 'tampering-observed'))) { throw 'Compiled build.rs did not run the attack.' }
        if ([IO.File]::ReadAllText($sourcePath) -cne $Source) { throw 'Native validation applied alternative B instead of the original snapshot A.' }
        if (($messages -join "`n") -notmatch 'original HEAD contains tests::focused: test') { throw 'Attack did not reach original compiled listing.' }
        if ($Tampering -match 'proposal' -and [IO.File]::ReadAllText($proposalPath) -cne [IO.File]::ReadAllText($alternativePath)) {
            throw 'Build script did not overwrite the downloaded proposal with valid B.'
        }
        if ($Tampering -match 'runtime' -and [IO.File]::ReadAllText($caseRuntime) -cne $fakeHelper) {
            throw 'Build script did not overwrite the runtime helper.'
        }
        Remove-Item -LiteralPath (Join-Path $workspace 'tampering-observed'), (Join-Path $workspace 'tamper-mode')
    }
    if ($Name -match 'config|runner|membership' -and ($messages -join "`n") -match 'full-suite: cargo') {
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
        $targets = @(Get-ChildItem -LiteralPath $targetParent -Directory)
        if ($targets.Count -ne 3 -or @($targets | Where-Object { -not (Test-Path (Join-Path $_.FullName 'x86_64-pc-windows-msvc\debug')) }).Count -gt 0) {
            throw 'Compiled listing, focused test and full suite did not use distinct external target directories.'
        }
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
    let external = root.parent().unwrap();
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
    foreach ($attack in @('proposal', 'runtime', 'proposal-runtime')) {
        Test-Case "original-listing-$attack-tampering" ($baseline.Replace("    4`n", "    5`n")) 'tests::focused' `
            'focused-tests: native validation failed with exit code' $false $false {} $attack
    }
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
}
