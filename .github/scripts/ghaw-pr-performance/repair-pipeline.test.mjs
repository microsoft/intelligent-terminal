import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { publishRepair } from './publish-repair.mjs';

const runtime = fileURLToPath(new URL('../../skills/pr-performance-review/scripts/performance-review.mjs', import.meta.url));
const validator = fileURLToPath(new URL('./validate-native.ps1', import.meta.url));
const compiledWorkflow = fileURLToPath(new URL('../../workflows/ghaw-pr-performance.lock.yml', import.meta.url));

test('sealed HIGH proposal runs real Windows tests and reaches staged CAS publication', async t => {
    assert.equal(process.platform, 'win32', 'this integration check needs the supported Windows environment');
    const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'performance-native-pipeline-'));
    const root = path.join(workspace, 'candidate');
    const previous = process.cwd();
    fs.mkdirSync(root);
    process.chdir(root);
    t.after(() => { process.chdir(previous); fs.rmSync(workspace, { recursive: true, force: true }); });
    fs.mkdirSync(path.join(root, 'tools', 'wta', 'src'), { recursive: true });
    fs.writeFileSync(path.join(root, 'tools', 'wta', 'Cargo.toml'),
        '[package]\nname="performance-pipeline-fixture"\nversion="0.1.0"\nedition="2021"\n');
    fs.writeFileSync(path.join(root, '.gitignore'), '/tools/wta/target/\n');
    const sourcePath = path.join(root, 'tools', 'wta', 'src', 'lib.rs');
    let baselineSource = 'pub fn work(n: usize) -> usize { n }\n' +
        '#[cfg(test)] mod tests { #[test] fn work_is_linear() { assert_eq!(super::work(1000), 1000); } }\n';
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
        items: [{ type: 'validate_performance_repair', confirm: true }], errors: [],
    }));
    const sealed = run(['gate', '--output-dir', out, '--baseline', baseline,
        '--report', reportPath, '--agent-output', queuePath, '--mode', 'repair', ...identity]);
    assert.equal(sealed.status, 0, sealed.stderr);
    // Native jobs start from a fresh immutable checkout, not the worker's staged candidate.
    git(['read-tree', headSha]);
    fs.writeFileSync(sourcePath, headSource);
    const nativeOut = path.join(artifactRoot, 'native');
    const trustedRuntime = path.join(workspace, 'trust', '.github', 'skills', 'pr-performance-review', 'scripts', 'performance-review.mjs');
    const trustedValidator = path.join(workspace, 'trust', '.github', 'scripts', 'ghaw-pr-performance', 'validate-native.ps1');
    for (const [source, destination] of [[runtime, trustedRuntime], [validator, trustedValidator]]) {
        fs.mkdirSync(path.dirname(destination), { recursive: true });
        fs.copyFileSync(source, destination);
    }
    const wrongDirectory = spawnSync(process.execPath, [trustedRuntime, 'validate-proposal',
        '--input', path.join(out, 'performance-proposal.json'), ...identity], {
        cwd: workspace, encoding: 'utf8', timeout: 30000,
    });
    assert.notEqual(wrongDirectory.status, 0, 'the hosted workspace root is not a Git checkout');
    assert.notEqual(spawnSync('git', ['rev-parse', '--is-inside-work-tree'], {
        cwd: workspace, timeout: 15000,
    }).status, 0);
    assert.match(wrongDirectory.stderr, /performance-review: Command failed: git/);
    const step = fs.readFileSync(compiledWorkflow, 'utf8')
        .match(/      - name: Validate and test the exact candidate tree\r?\n([\s\S]*?)(?=\r?\n      - name:|$)/)?.[1];
    assert.ok(step, 'execute the actual compiled Windows validation step');
    const workingDirectory = step.match(/^        working-directory: (.+)$/m)?.[1].trim();
    assert.equal(workingDirectory, 'candidate');
    const runBlock = step.match(/        run: \|\r?\n((?: {10}[^\n]*(?:\n|$))+)/)?.[1];
    assert.ok(runBlock);
    const command = runBlock.split(/\r?\n/).map(line => line.slice(10)).join('\n');
    const native = spawnSync('pwsh', ['-NoProfile', '-Command', command], {
        cwd: path.join(workspace, workingDirectory),
        env: { ...process.env, GITHUB_WORKSPACE: workspace, RUNNER_TEMP: artifactRoot,
            PR_NUMBER: '42', BASE_SHA: baseSha, HEAD_SHA: headSha },
        encoding: 'utf8', timeout: 180000,
    });
    assert.equal(native.status, 0, `${native.stdout}\n${native.stderr}`);
    assert.match(native.stdout, /original-test-listing: cargo test --locked --target x86_64-pc-windows-msvc --manifest-path tools\\wta\\Cargo.toml -- --list/);
    assert.match(native.stdout, /original HEAD contains tests::work_is_linear: test \(listing only, not a passing test claim\)/);
    assert.ok(native.stdout.indexOf('original-test-listing: cargo') < native.stdout.indexOf('format-check: cargo'));
    assert.match(native.stdout, /format-check: cargo fmt .* -- --check/);
    assert.match(native.stdout, /focused-tests: cargo test .* tests::work_is_linear -- --exact\r?$/m);
    assert.match(native.stdout, /^test tests::work_is_linear \.\.\. ok\r?$/m);
    assert.match(native.stdout, /focused-tests: 1 native test\(s\) passed/);
    assert.match(native.stdout, /full-suite: 1 native test\(s\) passed/);
    assert.equal(fs.existsSync(path.join(root, 'tools', 'wta', 'target')), false);
    assert.equal(fs.readdirSync(artifactRoot).filter(name => name.startsWith('performance-native-target-')).length, 3);
    assert.equal(git(['ls-files', '--others']), '', 'all build artifacts stay outside the checkout');
    assert.equal(fs.existsSync(nativeOut), false, 'native test step must not produce publication authority');
    const proposalPath = path.join(out, 'performance-proposal.json');
    const proposal = JSON.parse(fs.readFileSync(proposalPath));
    const expected = { prNumber: 42, baseSha, headSha, expectedBaseSha: baseSha,
        repository: 'owner/repo', headRepository: 'owner/repo', headRef: 'topic', baseRef: 'main' };
    const github = { rest: { actions: { listJobsForWorkflowRun: 'list-jobs' }, pulls: { async get() { return { data: {
        number: 42, state: 'open',
        head: { sha: headSha, ref: 'topic', repo: { id: 1, full_name: 'owner/repo' } },
        base: { sha: baseSha, ref: 'main', repo: { id: 1, full_name: 'owner/repo' } },
    } }; } } },
    async paginate() { return [{ name: 'validate_performance_repair', conclusion: 'success',
        steps: [{ name: 'Validate and test the exact candidate tree', conclusion: 'success' }] }]; },
    async graphql() { throw new Error('staged integration must not publish'); } };
    const result = await publishRepair({ github, expected, proposalPath, workerRunId: 123, staged: true });
    assert.equal(result.treeSha, proposal.treeSha);
    assert.equal(result.published, false);
});
