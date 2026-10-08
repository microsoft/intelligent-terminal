import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { publishRepair } from '../scripts/performance-review.mjs';

const runtime = fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url));
const validator = fileURLToPath(new URL('../scripts/run-native-performance-checks.ps1', import.meta.url));
const compiledWorkflow = fileURLToPath(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url));

test('sealed HIGH proposal runs real Windows tests and reaches staged CAS publication', async t => {
    assert.equal(process.platform, 'win32', 'this integration check needs the supported Windows environment');
    const workspace = fs.mkdtempSync(path.join(process.cwd(), '.performance-native-pipeline-'));
    const root = path.join(workspace, 'candidate');
    const previous = process.cwd();
    fs.mkdirSync(root);
    process.chdir(root);
    t.after(() => { process.chdir(previous); fs.rmSync(workspace, { recursive: true, force: true }); });
    fs.mkdirSync(path.join(root, 'tools', 'wta', 'src'), { recursive: true });
    fs.writeFileSync(path.join(root, 'tools', 'wta', 'Cargo.toml'),
        '[package]\nname="performance-pipeline-fixture"\nversion="0.1.0"\nedition="2021"\n');
    fs.writeFileSync(path.join(root, '.gitignore'), '/tools/wta/target/\n');
    fs.writeFileSync(path.join(root, 'tools', 'wta', 'build.rs'), `
fn main() {
    if let Ok(phase_root) = std::env::var("PERFORMANCE_PHASE_ROOT") {
        let phase_root = std::path::Path::new(&phase_root);
        let marker = phase_root.join("toolchain-config-marker");
        assert!(!marker.exists(), "phase-local toolchain/config state was imported");
        std::fs::write(marker, b"simulated modified toolchain/config state").unwrap();
    }
    let home = std::path::PathBuf::from(std::env::var_os("CARGO_HOME").unwrap());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    assert!(!home.canonicalize().unwrap().starts_with(root));
    assert!(!home.join("config.toml").exists(), "reused Cargo configuration");
    assert!(!home.join("registry/data/poison-sentinel").exists(), "reused registry data");
    std::fs::create_dir_all(home.join("registry/data")).unwrap();
    std::fs::write(home.join("registry/data/poison-sentinel"), b"poisoned registry source").unwrap();
    std::fs::write(home.join("fake-runner.cmd"), r"@echo off
echo test tests::work_is_linear ... ok
echo test result: ok. 1 passed; 0 failed;
exit /b 0
").unwrap();
    let runner = home.join("fake-runner.cmd").to_string_lossy().replace(char::from(92), "/");
    std::fs::write(home.join("config.toml"),
        format!("[target.x86_64-pc-windows-msvc]\\nrunner = '{}'\\n", runner)).unwrap();
    if let (Ok(downloads), Ok(workspace)) = (std::env::var("RUNNER_TEMP"), std::env::var("GITHUB_WORKSPACE")) {
        let downloads = std::path::Path::new(&downloads);
        if downloads.join("alternative.json").exists() {
            std::fs::copy(downloads.join("alternative.json"), downloads.join("performance-proposal/performance-proposal.json")).unwrap();
            std::fs::copy(downloads.join("fake-helper.mjs"), std::path::Path::new(&workspace)
                .join("trust/.github/skills/pr-performance-review/scripts/performance-review.mjs")).unwrap();
            std::fs::write(downloads.join("tampering-observed"), b"compiled original build script ran").unwrap();
        }
    }
}
`);
    const sourcePath = path.join(root, 'tools', 'wta', 'src', 'lib.rs');
    let baselineSource = 'pub fn work(n: usize) -> usize { n }\n' +
        `#[cfg(test)] mod tests { #[test] fn work_is_linear() {
            let home = std::path::PathBuf::from(std::env::var_os("CARGO_HOME").unwrap());
            assert!(home.join("registry/data/poison-sentinel").exists());
            std::fs::write(home.join("actual-test-executed"), b"real compiled test ran").unwrap();
            assert_eq!(super::work(1000), 1000);
        } }\n`;
    fs.writeFileSync(sourcePath, baselineSource);
    const formatFixture = () => execFileSync('cargo', ['fmt', '--manifest-path', path.join(root, 'tools', 'wta', 'Cargo.toml')], {
        encoding: 'utf8', timeout: 15000,
    });
    formatFixture();
    execFileSync('cargo', ['generate-lockfile', '--manifest-path', path.join(root, 'tools', 'wta', 'Cargo.toml')], {
        encoding: 'utf8', timeout: 15000,
    });
    baselineSource = fs.readFileSync(sourcePath, 'utf8');
    const git = args => execFileSync('git', args, { encoding: 'utf8', timeout: 15000 }).trim();
    git(['init', '--quiet', '--initial-branch=main']);
    git(['config', 'core.autocrlf', 'false']);
    const commit = () => {
        git(['add', '.']);
        git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
            'commit', '--quiet', '-m', 'fixture']);
        return git(['rev-parse', 'HEAD']);
    };
    const baseSha = commit();
    const headSource = baselineSource.replace('    n\n', '    (1..=n).sum()\n');
    assert.notEqual(headSource, baselineSource);
    fs.writeFileSync(sourcePath, headSource);
    formatFixture();
    const headSha = commit();
    const sealingTrust = path.join(workspace, 'sealing-trust');
    execFileSync('git', ['clone', '--quiet', '--no-local', root, sealingTrust]);
    const artifactRoot = fs.mkdtempSync(path.join(previous, '.performance-pipeline-artifacts-'));
    t.after(() => fs.rmSync(artifactRoot, { recursive: true, force: true }));
    const baseline = path.join(artifactRoot, 'baseline.json');
    const out = path.join(artifactRoot, 'performance-proposal');
    const run = args => spawnSync(process.execPath, [runtime, ...args], {
        encoding: 'utf8', timeout: 30000,
    });
    const identity = ['--pr', '42', '--base', baseSha, '--head', headSha];
    assert.equal(run(['prepare', '--output-dir', out, '--baseline', baseline, ...identity]).status, 0);
    fs.writeFileSync(sourcePath, baselineSource);
    formatFixture();
    const report = {
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha, headSha }, status: 'pending_validation',
        validationPlan: { type: 'wta-unit', testFilter: 'tests::work_is_linear' },
        findings: [{
            id: 'PERF-ABC12345', severity: 'high', confidence: 'high', dimension: 'application-performance',
            category: 'wta-runtime', title: 'Amplified repeated work', affectedScenario: 'Large repeated refresh',
            location: 'tools/wta/src/lib.rs:1', observed: 'Sum grows quadratically across callers',
            expected: 'Linear repeated work', impact: 'Amplified repeated path',
            nativeEnvironment: { architecture: 'not-measured', details: 'Operation-count proof; native validation pending' },
            evidence: [{ type: 'complexity-proof', detail: 'Input-dependent accumulation adds unnecessary repeated work.' }],
            proposedFix: 'Return the unchanged input directly', validation: 'tests::work_is_linear',
            fixDisposition: 'proposed',
        }], checks: [],
    };
    const reportPath = path.join(artifactRoot, 'report.json');
    const queuePath = path.join(artifactRoot, 'queue.json');
    fs.writeFileSync(reportPath, JSON.stringify(report));
    fs.writeFileSync(queuePath, JSON.stringify({
        items: ['validate_performance_original_tests', 'validate_performance_focused_tests', 'validate_performance_repair']
            .map(type => ({ type, confirm: true })), errors: [],
    }));
    const sealed = run(['gate', '--output-dir', out, '--baseline', baseline,
        '--report', reportPath, '--agent-output', queuePath, '--mode', 'repair',
        '--trusted-repository-root', sealingTrust, '--agent-worktree-root', root, ...identity]);
    assert.equal(sealed.status, 0, sealed.stderr);
    const proposalPath = path.join(out, 'performance-proposal.json');
    const immutableArtifact = fs.readFileSync(proposalPath);
    const proposal = JSON.parse(immutableArtifact);
    const nativeArtifacts = path.join(workspace, 'downloads');
    fs.mkdirSync(path.join(nativeArtifacts, 'performance-proposal'), { recursive: true });
    const alternativeSource = baselineSource.replace('    n\n', '    n + 1\n');
    fs.writeFileSync(sourcePath, alternativeSource);
    git(['add', 'tools/wta/src/lib.rs']);
    const alternative = { ...proposal, treeSha: git(['write-tree']),
        files: [{ ...proposal.files[0], contents: Buffer.from(alternativeSource).toString('base64') }] };
    const alternativePath = path.join(nativeArtifacts, 'alternative.json');
    fs.writeFileSync(alternativePath, JSON.stringify(alternative));
    assert.equal(run(['validate-proposal', '--input', alternativePath, ...identity]).status, 0,
        'the attacker replacement is independently valid, not malformed input');
    fs.writeFileSync(path.join(nativeArtifacts, 'fake-helper.mjs'),
        `import fs from 'node:fs'; fs.writeFileSync('tools/wta/src/lib.rs', Buffer.from('${alternative.files[0].contents}', 'base64'));` +
        `fs.writeFileSync(${JSON.stringify(path.join(nativeArtifacts, 'fake-helper-executed'))}, 'executed');`);
    // Native jobs start from a fresh immutable checkout, not the worker's staged candidate.
    git(['read-tree', headSha]);
    fs.writeFileSync(sourcePath, headSource);
    const nativeOut = path.join(artifactRoot, 'native');
    const wrongDirectory = spawnSync(process.execPath, [runtime, 'validate-proposal',
        '--input', path.join(out, 'performance-proposal.json'), ...identity], {
        cwd: workspace, env: { ...process.env, GIT_CEILING_DIRECTORIES: previous }, encoding: 'utf8', timeout: 30000,
    });
    assert.notEqual(wrongDirectory.status, 0, 'the hosted workspace root is not a Git checkout');
    assert.notEqual(spawnSync('git', ['rev-parse', '--is-inside-work-tree'], {
        cwd: workspace, env: { ...process.env, GIT_CEILING_DIRECTORIES: previous }, timeout: 15000,
    }).status, 0);
    assert.match(wrongDirectory.stderr, /performance-review: Command failed: git/);
    const phases = [
        ['OriginalListing', 'validate_performance_original_tests', 'List the exact test on original HEAD'],
        ['Focused', 'validate_performance_focused_tests', 'Format and test the exact focused candidate'],
        ['FullSuite', 'validate_performance_repair', 'Test the exact candidate full suite'],
    ];
    const nativeOutputs = [];
    const nativeRoots = [];
    for (const [phase, jobName, stepName] of phases) {
        const job = fs.readFileSync(compiledWorkflow, 'utf8')
            .match(new RegExp(`^  ${jobName}:\\r?\\n([\\s\\S]*?)(?=^  [a-z_]+:|(?![\\s\\S]))`, 'm'))?.[1];
        assert.ok(job);
        assert.match(job, /runs-on: windows-latest/);
        assert.match(job, /needs:\r?\n      - agent\r?\n      - detection/);
        assert.match(job, new RegExp(`contains\\(needs.agent.outputs.output_types, '${jobName}'\\)`));
        assert.match(job, /permissions:\r?\n      contents: read/);
        assert.match(job, /name: performance-result/);
        assert.match(job, /timeout-minutes: 32/);
        assert.doesNotMatch(job, /secrets\.|actions\/cache|upload-artifact|contents: write|copilot-requests:/);
        assert.match(job, new RegExp(`-Phase ${phase} `));
        const freshRoot = path.join(workspace, phase);
        nativeRoots.push(freshRoot);
        fs.mkdirSync(freshRoot);
        for (const marker of ['toolchain-config-marker', 'descendant-marker', 'descendant-pid']) {
            assert.equal(fs.existsSync(path.join(freshRoot, marker)), false, 'phase-local state is never imported');
        }
        execFileSync('git', ['-c', 'core.autocrlf=false', 'clone', '--quiet', '--no-local', sealingTrust, path.join(freshRoot, 'candidate')]);
        const freshDownloads = path.join(freshRoot, 'downloads');
        fs.mkdirSync(path.join(freshDownloads, 'performance-proposal'), { recursive: true });
        fs.writeFileSync(path.join(freshDownloads, 'performance-proposal', 'performance-proposal.json'), immutableArtifact);
        fs.copyFileSync(alternativePath, path.join(freshDownloads, 'alternative.json'));
        fs.copyFileSync(path.join(nativeArtifacts, 'fake-helper.mjs'), path.join(freshDownloads, 'fake-helper.mjs'));
        for (const [source, destination] of [
            [runtime, path.join(freshRoot, 'trust', '.github', 'skills', 'pr-performance-review', 'scripts', 'performance-review.mjs')],
            [validator, path.join(freshRoot, 'trust', '.github', 'skills', 'pr-performance-review', 'scripts', 'run-native-performance-checks.ps1')],
        ]) {
            fs.mkdirSync(path.dirname(destination), { recursive: true });
            fs.copyFileSync(source, destination);
        }
        const step = fs.readFileSync(compiledWorkflow, 'utf8')
            .match(new RegExp(`      - name: ${stepName}\\r?\\n([\\s\\S]*?)(?=\\r?\\n      - name:|\\n  [a-z_]+:|$)`))?.[1];
        assert.ok(step, 'execute the actual compiled Windows validation step');
        const workingDirectory = step.match(/^        working-directory: (.+)$/m)?.[1].trim();
        assert.equal(workingDirectory, 'candidate');
        const runBlock = step.match(/        run: \|\r?\n((?: {10}[^\n]*(?:\n|$))+)/)?.[1];
        assert.ok(runBlock);
        const command = runBlock.split(/\r?\n/).map(line => line.slice(10)).join('\n');
        // Local lifecycle simulation, not a fresh-VM or within-phase sandbox claim.
        const descendant = spawn('pwsh', ['-NoProfile', '-Command',
            `while ($true) { [IO.File]::WriteAllText('${path.join(freshRoot, 'descendant-marker')}', 'background descendant'); Start-Sleep -Milliseconds 100 }`],
            { cwd: freshRoot, stdio: 'ignore' });
        t.after(() => { if (descendant.exitCode === null) descendant.kill(); });
        const native = spawnSync('pwsh', ['-NoProfile', '-Command', command], {
            cwd: path.join(freshRoot, workingDirectory),
            env: { ...process.env, GITHUB_WORKSPACE: freshRoot, RUNNER_TEMP: freshDownloads,
                PERFORMANCE_PHASE_ROOT: freshRoot,
                CARGO_HOME: path.join(root, 'caller-cargo-home'),
                PR_NUMBER: '42', BASE_SHA: baseSha, HEAD_SHA: headSha },
            encoding: 'utf8', timeout: 180000,
        });
        assert.ok(Number.isSafeInteger(descendant.pid) && descendant.pid > 0);
        const cleanup = spawnSync('pwsh', ['-NoProfile', '-Command',
            `Start-Sleep -Milliseconds 500; if (!(Test-Path -LiteralPath '${path.join(freshRoot, 'descendant-marker')}')) { throw 'descendant not responsive' }; Stop-Process -Id ${descendant.pid} -Force -ErrorAction Stop; Start-Sleep -Milliseconds 100; if (Get-Process -Id ${descendant.pid} -ErrorAction SilentlyContinue) { throw 'descendant still running' }`],
            { encoding: 'utf8', timeout: 15000 });
        assert.equal(cleanup.status, 0, cleanup.stderr);
        assert.equal(native.status, 0, `${native.stdout}\n${native.stderr}`);
        assert.ok(fs.existsSync(path.join(freshRoot, 'toolchain-config-marker')));
        nativeOutputs.push(native.stdout);
        const nativeSource = path.join(freshRoot, 'candidate', 'tools', 'wta', 'src', 'lib.rs');
        assert.equal(fs.readFileSync(nativeSource, 'utf8'), phase === 'OriginalListing' ? headSource : baselineSource);
        assert.equal(fs.existsSync(path.join(freshDownloads, 'tampering-observed')), true);
        assert.deepEqual(JSON.parse(fs.readFileSync(path.join(freshDownloads, 'performance-proposal', 'performance-proposal.json'))), alternative);
        assert.equal(fs.readFileSync(path.join(freshRoot, 'trust', '.github', 'skills', 'pr-performance-review', 'scripts', 'performance-review.mjs'), 'utf8'),
            fs.readFileSync(path.join(freshDownloads, 'fake-helper.mjs'), 'utf8'), 'native code overwrote only this phase runtime helper');
        assert.equal(fs.existsSync(path.join(nativeArtifacts, 'fake-helper-executed')), false);
        const stages = [...native.stdout.matchAll(/^([\w-]+): cargo /gm)].map(match => match[1]);
        assert.deepEqual(stages, phase === 'OriginalListing' ? ['original-test-listing'] :
            phase === 'Focused' ? ['format-check', 'focused-tests'] : ['full-suite']);
    }
    const native = { stdout: nativeOutputs.join('\n') };
    assert.match(native.stdout, /original-test-listing: cargo test --locked --target x86_64-pc-windows-msvc --manifest-path tools\\wta\\Cargo.toml -- --list/);
    assert.match(native.stdout, /original HEAD contains tests::work_is_linear: test \(listing only, not a passing test claim\)/);
    assert.ok(native.stdout.indexOf('original-test-listing: cargo') < native.stdout.indexOf('format-check: cargo'));
    assert.match(native.stdout, /format-check: cargo fmt .* -- --check/);
    assert.match(native.stdout, /focused-tests: cargo test .* tests::work_is_linear -- --exact\r?$/m);
    assert.match(native.stdout, /^test tests::work_is_linear \.\.\. ok\r?$/m);
    assert.match(native.stdout, /focused-tests: 1 native test\(s\) passed/);
    assert.match(native.stdout, /full-suite: 1 native test\(s\) passed/);
    assert.equal(fs.existsSync(path.join(root, 'tools', 'wta', 'target')), false);
    const homes = nativeRoots.flatMap(freshRoot => {
        const downloads = path.join(freshRoot, 'downloads');
        assert.equal(fs.readdirSync(downloads).filter(name => name.startsWith('performance-native-target-')).length, 1);
        return fs.readdirSync(downloads).filter(name => name.startsWith('performance-native-cargo-home-'))
            .map(name => path.join(downloads, name));
    });
    assert.equal(homes.length, 4, 'listing, formatting, focused and full stages each have a fresh Cargo home');
    const poisonedHomes = homes.filter(home => fs.existsSync(path.join(home, 'registry', 'data', 'poison-sentinel')));
    assert.equal(poisonedHomes.length, 3, 'all three real compiled build scripts poison only their own Cargo home');
    for (const home of poisonedHomes) {
        assert.ok(fs.existsSync(path.join(home, 'config.toml')));
        assert.ok(fs.existsSync(path.join(home, 'fake-runner.cmd')));
    }
    assert.equal(homes.filter(home => fs.existsSync(path.join(home, 'actual-test-executed'))).length, 2,
        'focused and full stages execute actual compiled tests, not the fake runner output');
    assert.equal(fs.existsSync(path.join(root, 'caller-cargo-home')), false, 'caller Cargo home override is ignored');
    assert.equal(git(['ls-files', '--others']), '', 'all build artifacts stay outside the checkout');
    assert.equal(fs.existsSync(nativeOut), false, 'native test step must not produce publication authority');
    assert.equal(fs.existsSync(path.join(nativeArtifacts, 'fake-helper-executed')), false, 'no mutable helper executes after --list');
    assert.deepEqual(fs.readFileSync(proposalPath), immutableArtifact, 'publisher still consumes original immutable A');
    const expected = { prNumber: 42, baseSha, headSha, expectedBaseSha: baseSha,
        repository: 'owner/repo', headRepository: 'owner/repo', headRef: 'topic', baseRef: 'main' };
    const github = { rest: { actions: { listJobsForWorkflowRun: 'list-jobs' }, pulls: { async get() { return { data: {
        number: 42, state: 'open',
        head: { sha: headSha, ref: 'topic', repo: { id: 1, full_name: 'owner/repo' } },
        base: { sha: baseSha, ref: 'main', repo: { id: 1, full_name: 'owner/repo' } },
    } }; } } },
    async paginate(route, options) {
        assert.equal(options.run_id, 123);
        return phases.map(([, name, stepName]) => ({ name, conclusion: 'success',
            steps: [{ name: stepName, conclusion: 'success' }] }));
    },
    async graphql() { throw new Error('staged integration must not publish'); } };
    const result = await publishRepair({ github, expected, proposalPath, workerRunId: 123, staged: true });
    assert.equal(result.treeSha, proposal.treeSha);
    assert.equal(result.published, false);
    for (let missing = 0; missing < phases.length; missing++) {
        for (const failure of [false, true]) {
            github.paginate = async () => phases.flatMap(([, name, stepName], index) =>
                index === missing && !failure ? [] : [{ name,
                    conclusion: index === missing ? 'failure' : 'success',
                    steps: [{ name: stepName, conclusion: 'success' }] }]);
            await assert.rejects(() => publishRepair({ github, expected, proposalPath, workerRunId: 123, staged: true }),
                /GitHub did not record/);
        }
    }
});
