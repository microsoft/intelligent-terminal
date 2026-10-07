import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const runtime = fileURLToPath(new URL('../../skills/pr-performance-review/scripts/performance-review.mjs', import.meta.url));

function fixture(t) {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'performance runtime test '));
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const repo = path.join(root, 'repo');
    fs.mkdirSync(path.join(repo, 'src', 'renderer'), { recursive: true });
    fs.mkdirSync(path.join(repo, 'tools', 'wta', 'src'), { recursive: true });
    const source = path.join(repo, 'src', 'renderer', 'fixture.cpp');
    const rustSource = path.join(repo, 'tools', 'wta', 'src', 'fixture.rs');
    const script = path.join(root, 'trusted runtime.mjs');
    fs.copyFileSync(runtime, script);
    const git = args => execFileSync('git', args, { cwd: repo, encoding: 'utf8', timeout: 15000 }).trim();
    git(['init', '--quiet', '--initial-branch=main']);
    git(['config', 'core.autocrlf', 'false']);
    const commit = () => {
        git(['add', '.']);
        git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
            'commit', '--quiet', '-m', 'fixture']);
        return git(['rev-parse', 'HEAD']);
    };
    fs.writeFileSync(source, 'int render() { return 1; }\n');
    fs.writeFileSync(rustSource, 'pub fn render() -> usize { 1 }\n');
    const baseSha = commit();
    fs.writeFileSync(source, 'int render() { return 2; }\n');
    fs.writeFileSync(rustSource, 'pub fn render() -> usize { 2 }\n');
    const headSha = commit();
    const output = path.join(root, 'output');
    const identity = ['--pr', '42', '--base', baseSha, '--head', headSha];
    const run = args => spawnSync(process.execPath, [script, ...args], {
        cwd: repo, encoding: 'utf8', timeout: 15000,
    });
    return { root, repo, source, rustSource, git, commit, output, identity, run, baseSha, headSha };
}

test('actual CLI prepares immutable scope and exact patch from a path containing spaces', t => {
    const f = fixture(t);
    const result = f.run(['prepare', '--output-dir', f.output, ...f.identity]);
    assert.equal(result.status, 0, result.stderr);
    const scope = JSON.parse(fs.readFileSync(path.join(f.output, 'performance-scope.json')));
    assert.equal(scope.applicable, true);
    assert.equal(scope.candidates[0].filename, 'src/renderer/fixture.cpp');
    assert.match(fs.readFileSync(path.join(f.output, 'performance-patch.txt'), 'utf8'), /return 2/);
});

test('actual repair gate rejects an uncommitted edit behind a noop queue', t => {
    const f = fixture(t);
    const reportPath = path.join(f.root, 'report.json');
    const queuedPath = path.join(f.root, 'queued.json');
    fs.writeFileSync(reportPath, JSON.stringify({
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'pass', findings: [], checks: [],
    }));
    fs.writeFileSync(queuedPath, JSON.stringify({ items: [{ type: 'noop' }], errors: [] }));
    const args = ['gate', '--output-dir', f.output, '--report', reportPath,
        '--agent-output', queuedPath, '--mode', 'repair', ...f.identity,
        '--baseline', path.join(f.root, 'baseline.json')];
    assert.equal(f.run(['prepare', '--output-dir', f.output,
        '--baseline', path.join(f.root, 'baseline.json'), ...f.identity]).status, 0);
    assert.equal(f.run(args).status, 0);
    fs.writeFileSync(f.source, 'int render() { return 3; }\n');
    const dirty = f.run(args);
    assert.equal(dirty.status, 1);
    assert.match(dirty.stderr, /cannot discard/);
});

test('trusted instruction restoration is preparation, not an unauthorized model edit', t => {
    const f = fixture(t);
    fs.writeFileSync(path.join(f.repo, 'AGENTS.md'), 'Restored trusted instructions.\n');
    const baseline = path.join(f.root, 'baseline.json');
    assert.equal(f.run(['prepare', '--output-dir', f.output,
        '--baseline', baseline, ...f.identity]).status, 0);
    const reportPath = path.join(f.root, 'report.json');
    const queuedPath = path.join(f.root, 'queued.json');
    fs.writeFileSync(reportPath, JSON.stringify({
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'pass', findings: [], checks: [],
    }));
    fs.writeFileSync(queuedPath, JSON.stringify({ items: [{ type: 'noop' }], errors: [] }));
    const result = f.run(['gate', '--output-dir', f.output, '--report', reportPath,
        '--agent-output', queuedPath, '--mode', 'repair', ...f.identity, '--baseline', baseline]);
    assert.equal(result.status, 0, result.stderr);
    fs.writeFileSync(path.join(f.repo, 'AGENTS.md'), 'Model tampered with restored instructions.\n');
    assert.equal(f.run(['gate', '--output-dir', f.output, '--report', reportPath,
        '--agent-output', queuedPath, '--mode', 'repair', ...f.identity, '--baseline', baseline]).status, 1);
});

test('actual CLI rejects missing report and malformed queued output', t => {
    const f = fixture(t);
    const queuedPath = path.join(f.root, 'queued.json');
    fs.writeFileSync(queuedPath, '{"items":[]}');
    const result = f.run(['gate', '--output-dir', f.output,
        '--report', path.join(f.root, 'missing.json'),
        '--agent-output', queuedPath, '--mode', 'guide', ...f.identity]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /version 1 performance review/);
});

test('actual verdict CLI refuses remaining HIGH instead of passing a green worker', t => {
    const f = fixture(t);
    const verdict = path.join(f.root, 'verdict.json');
    fs.writeFileSync(verdict, JSON.stringify({
        version: 1, identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'action_required',
    }));
    const result = f.run(['verdict', '--input', verdict, ...f.identity]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /requires action/);
});

test('native seal captures exact final candidate blobs, not an earlier push transport', t => {
    const f = fixture(t);
    const baseline = path.join(f.root, 'baseline.json');
    assert.equal(f.run(['prepare', '--output-dir', f.output,
        '--baseline', baseline, ...f.identity]).status, 0);
    fs.writeFileSync(f.rustSource, 'pub fn render() -> usize { 3 }\n');
    const reportPath = path.join(f.root, 'report.json');
    const queuePath = path.join(f.root, 'queue.json');
    const report = {
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'pending_validation',
        validationPlan: { type: 'wta-unit', testFilter: 'fixture::tests' },
        findings: [{
            id: 'PERF-ABC12345', severity: 'high', confidence: 'high', dimension: 'application-performance',
            category: 'wta-runtime', title: 'Repeated quadratic work', affectedScenario: 'Large session refresh',
            location: 'tools/wta/src/fixture.rs:1', observed: 'Repeated quadratic traversal',
            expected: 'Linear traversal', impact: 'Large repeated-path work',
            nativeEnvironment: { architecture: 'not-measured', details: 'Native validation is pending' },
            evidence: [{ type: 'complexity-proof', detail: 'Nested traversal repeats for every input.' }],
            proposedFix: 'Use direct iteration', validation: 'Windows validation requested', fixDisposition: 'proposed',
        }], checks: [],
    };
    fs.writeFileSync(reportPath, JSON.stringify(report));
    fs.writeFileSync(queuePath, JSON.stringify({
        items: [{ type: 'validate_performance_repair', confirm: true }], errors: [],
    }));
    const args = ['gate', '--output-dir', f.output, '--report', reportPath,
        '--agent-output', queuePath, '--mode', 'repair', ...f.identity, '--baseline', baseline];
    const result = f.run(args);
    assert.equal(result.status, 0, result.stderr);
    const proposal = JSON.parse(fs.readFileSync(path.join(f.output, 'performance-proposal.json')));
    assert.equal(Buffer.from(proposal.files[0].contents, 'base64').toString('utf8'),
        'pub fn render() -> usize { 3 }\n');
    assert.equal(proposal.report.status, 'pending_validation');
    fs.writeFileSync(path.join(f.repo, 'README.md'), 'Unauthorized extra file.\n');
    const escaped = f.run(args);
    assert.equal(escaped.status, 1);
    assert.match(escaped.stderr, /confined to original candidate/);
});
