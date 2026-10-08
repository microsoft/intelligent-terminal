import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { captureReviewSummary, readReviewSummary } from '../scripts/performance-review.mjs';

const runtime = fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url));

test('all fresh native repair jobs install and explicitly use Rust 1.93 with rustfmt', () => {
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    const validator = fs.readFileSync(new URL('../scripts/run-native-performance-checks.ps1', import.meta.url), 'utf8');
    const jobs = [...compiled.matchAll(/^  (validate_performance_original_tests|validate_performance_focused_tests|validate_performance_repair):\r?\n([\s\S]*?)(?=^  [a-z_]+:|(?![\s\S]))/gm)];
    assert.equal(jobs.length, 3);
    for (const [, name, job] of jobs) {
        assert.match(job, /runs-on: windows-latest/, name);
        assert.match(job, /rustup toolchain install 1\.93\.0 --profile minimal --component rustfmt/, name);
        assert.match(job, /timeout-minutes: 10/, name);
        assert.ok(job.indexOf('rustup toolchain install') < job.indexOf('-Phase '), name);
    }
    assert.match(validator, /\$start\.ArgumentList\.Add\('\+1\.93\.0'\)\s+foreach \(\$argument in \$Arguments\)/);
});

test('repair report validation belongs to trusted post-processing, not a model-side shell', () => {
    const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    const agent = fs.readFileSync(new URL('../../../agents/ghaw-pr-performance.agent.md', import.meta.url), 'utf8');
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    assert.match(workflow, /Do not run an\s+agent-side validator or renderer/);
    assert.match(workflow, /Write the fixed report and summary through the permitted file-editing tool/);
    assert.match(agent, /repair caller validates the fixed JSON report in trusted post-processing before\s+native tests/);
    assert.match(compiled, /performance-trusted\.mjs\\" gate/);
    assert.match(compiled, /--report \/tmp\/gh-aw\/performance-report\.json/);
    assert.match(compiled, /--analysis-input/);
    assert.match(compiled, /Invalid sealed native proposal/);
    assert.doesNotMatch(workflow, /validate it with `--mode repair`/);
});

test('compiled repair runs two read-only Windows analyses before inference but failed analysis cannot skip the reviewer', () => {
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    const analysis = compiled.match(/^  performance_analysis:\r?\n([\s\S]*?)(?=^  [a-z_]+:)/m)?.[1];
    assert.ok(analysis);
    assert.match(analysis, /runs-on: windows-latest/);
    assert.match(analysis, /timeout-minutes: 12/);
    assert.match(analysis, /fail-fast: false/);
    assert.match(analysis, /revision:\s+- BASE\s+- HEAD/);
    assert.match(analysis, /if: github\.event\.inputs\.same_repo == 'true'/);
    assert.match(analysis, /contents: read/);
    assert.doesNotMatch(analysis, /secrets\.|actions\/cache|contents: write/);
    const checkouts = [...analysis.matchAll(/uses: actions\/checkout@[^\n]+\r?\n([\s\S]*?)(?=      - |$)/g)];
    assert.equal(checkouts.length, 2, 'compiler must not inject an authoring checkout at the workspace root');
    assert.deepEqual(checkouts.map(match => match[1].match(/path: ([^\r\n]+)/)?.[1]), ['trust', 'analysis']);
    assert.match(analysis, /performance-analysis-\$\{\{ matrix\.revision \}\}/);
    assert.match(analysis, /if: always\(\)/);
    assert.ok(analysis.indexOf('node $runtime prepare') < analysis.indexOf('git checkout --detach'));
    const agent = compiled.match(/^  agent:\r?\n([\s\S]*?)(?=^  [a-z_]+:)/m)?.[1];
    assert.match(agent, /needs:[\s\S]*?- performance_analysis[\s\S]*?- prepare/);
    assert.match(agent, /always\(\) && needs\.prepare\.result == 'success'/);
    assert.doesNotMatch(agent.match(/    if: >\r?\n([^\n]+)/)?.[1] ?? '', /performance_analysis\.result == 'success'/);
    assert.match(agent, /pattern: performance-analysis-\*/);
    assert.match(agent, /PERFORMANCE_ANALYSIS_JOB_RESULT: \$\{\{ needs\.performance_analysis\.result \}\}/);
    assert.match(agent, /--analysis-input/);
    const fork = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-fork-guidance.lock.yml', import.meta.url), 'utf8');
    assert.doesNotMatch(fork, /performance_analysis:|Install explicit comparison Clippy|Phase Analysis/);
});

function fixture(t, trackedSummary = false) {
    const root = fs.mkdtempSync(path.join(process.cwd(), '.performance-runtime-test-'));
    t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }));
    const repo = path.join(root, 'repo');
    fs.mkdirSync(path.join(repo, 'src', 'renderer'), { recursive: true });
    fs.mkdirSync(path.join(repo, 'tools', 'wta', 'src'), { recursive: true });
    const source = path.join(repo, 'src', 'renderer', 'fixture.cpp');
    const rustSource = path.join(repo, 'tools', 'wta', 'src', 'fixture.rs');
    const script = path.join(root, 'trusted runtime.mjs');
    fs.copyFileSync(runtime, script);
    const runtimeRelative = '.github/skills/pr-performance-review/scripts/performance-review.mjs';
    fs.mkdirSync(path.dirname(path.join(repo, runtimeRelative)), { recursive: true });
    fs.copyFileSync(runtime, path.join(repo, runtimeRelative));
    fs.writeFileSync(path.join(repo, '.gitignore'), '/ignored-attack.txt\n');
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
    if (trackedSummary) fs.writeFileSync(path.join(repo, '.performance-summary.md'), 'Original tracked source.\n');
    const baseSha = commit();
    fs.writeFileSync(source, 'int render() { return 2; }\n');
    fs.writeFileSync(rustSource, 'pub fn render() -> usize { 2 }\n');
    const headSha = commit();
    const trust = path.join(root, 'trust');
    execFileSync('git', ['clone', '--quiet', '--no-local', repo, trust]);
    const output = path.join(root, 'output');
    const identity = ['--pr', '42', '--base', baseSha, '--head', headSha];
    const trustedScript = path.join(trust, runtimeRelative);
    const run = (args, env = {}) => spawnSync(process.execPath, [trustedScript, ...args,
        ...(args[0] === 'gate' && args.includes('repair') ?
            ['--trusted-repository-root', trust, '--agent-worktree-root', repo] : [])], {
        cwd: repo, env: { ...process.env, ...env }, encoding: 'utf8', timeout: 15000,
    });
    return { root, repo, trust, script: trustedScript, runtimeRelative, source, rustSource, git, commit, output, identity, run, baseSha, headSha };
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
    captureReviewSummary(f.repo, 'Review-time findings, not repair authority.\n');
    assert.equal(f.run(args).status, 0, 'only the exact regular summary output is exempt from source inventory');
    fs.writeFileSync(path.join(f.repo, '.performance-summary-extra.md'), 'Not an allowed output');
    assert.equal(f.run(args).status, 1, 'similarly named output remains an unauthorized edit');
    fs.rmSync(path.join(f.repo, '.performance-summary-extra.md'));
    fs.writeFileSync(f.source, 'int render() { return 3; }\n');
    const dirty = f.run(args);
    assert.equal(dirty.status, 1);
    assert.match(dirty.stderr, /cannot discard/);
});

test('summary reader accepts bounded UTF-8 Markdown and rejects missing, oversized and invalid bytes', t => {
    const directory = fs.mkdtempSync(path.join(process.cwd(), '.performance-summary-reader-'));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    assert.throws(() => readReviewSummary(directory), /ENOENT/);
    captureReviewSummary(directory, 'Résumé — review-time proposal.\n');
    assert.equal(readReviewSummary(directory), 'Résumé — review-time proposal.\n');
    assert.throws(() => captureReviewSummary(directory, 'x'.repeat(48 * 1024 + 1)), /48 KiB/);
    fs.writeFileSync(path.join(directory, '.performance-summary.md'), Buffer.from([0xff]));
    assert.throws(() => readReviewSummary(directory), /encoded data/);
});

test('summary reader and writer reject reparse output paths rather than following them', t => {
    const directory = fs.mkdtempSync(path.join(process.cwd(), '.performance-summary-reparse-'));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    const target = path.join(directory, 'target');
    fs.mkdirSync(target);
    fs.symlinkSync(target, path.join(directory, '.performance-summary.md'),
        process.platform === 'win32' ? 'junction' : 'dir');
    assert.throws(() => readReviewSummary(directory), /symlinks or reparse/);
    assert.throws(() => captureReviewSummary(directory, 'Not permitted'), /symlinks or reparse/);
    assert.deepEqual(fs.readdirSync(target), []);
});

test('a tracked summary is still protected source, not an exempt output deletion', t => {
    const f = fixture(t, true);
    const baseline = path.join(f.root, 'baseline.json');
    assert.equal(f.run(['prepare', '--output-dir', f.output, '--baseline', baseline, ...f.identity]).status, 0);
    const report = path.join(f.root, 'report.json');
    const queue = path.join(f.root, 'queue.json');
    fs.writeFileSync(report, JSON.stringify({
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'pass', findings: [], checks: [],
    }));
    fs.writeFileSync(queue, JSON.stringify({ items: [{ type: 'noop' }] }));
    fs.rmSync(path.join(f.repo, '.performance-summary.md'));
    const result = f.run(['gate', '--output-dir', f.output, '--report', report,
        '--agent-output', queue, '--mode', 'repair', ...f.identity, '--baseline', baseline]);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /cannot discard unreported changes/);
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

test('fresh trust seals raw bytes despite malicious clean filter, worktree redirect and replaced runtime blob', t => {
    const f = fixture(t);
    const baseline = path.join(f.root, 'baseline.json');
    assert.equal(f.run(['prepare', '--output-dir', f.output,
        '--baseline', baseline, ...f.identity]).status, 0);
    const raw = Buffer.from('pub fn render() -> usize { 3 }\r\n');
    fs.writeFileSync(f.rustSource, raw);
    const sentinel = path.join(f.root, 'host-clean-filter-executed');
    fs.writeFileSync(path.join(f.repo, '.git', 'clean.cjs'),
        `require('fs').writeFileSync(${JSON.stringify(sentinel)}, 'executed'); process.stdout.write('forged bytes');`);
    f.git(['config', 'filter.hostattack.clean', 'node .git/clean.cjs']);
    f.git(['config', 'filter.hostattack.required', 'true']);
    fs.writeFileSync(path.join(f.repo, '.git', 'info', 'attributes'), '* filter=hostattack\n');
    const runtimeBlob = f.git(['rev-parse', `${f.headSha}:${f.runtimeRelative}`]);
    const spoof = execFileSync('git', ['hash-object', '-w', '--stdin'], {
        cwd: f.repo, input: 'throw new Error("attacker runtime executed");\n', encoding: 'utf8',
    }).trim();
    f.git(['replace', runtimeBlob, spoof]);
    assert.match(f.git(['show', `${f.headSha}:${f.runtimeRelative}`]), /attacker runtime/);
    const redirect = path.join(f.root, 'redirect');
    fs.mkdirSync(redirect);
    fs.writeFileSync(path.join(redirect, 'hidden.txt'), 'core.worktree must not control source inventory');
    f.git(['config', 'core.worktree', redirect]);
    const reportPath = path.join(f.root, 'report.json');
    const queuePath = path.join(f.root, 'queue.json');
    const report = {
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'pending_validation',
        validationPlan: { type: 'wta-unit', testFilter: 'tests::fixture_preserves_behavior' },
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
        items: ['validate_performance_original_tests', 'validate_performance_focused_tests', 'validate_performance_repair']
            .map(type => ({ type, confirm: true })), errors: [],
    }));
    const args = ['gate', '--output-dir', f.output, '--report', reportPath,
        '--agent-output', queuePath, '--mode', 'repair', ...f.identity, '--baseline', baseline];
    const requiredAnalysis = [...args, '--analysis-input', path.join(f.root, 'missing-analysis')];
    const failedAnalysis = f.run(requiredAnalysis, { PERFORMANCE_ANALYSIS_JOB_RESULT: 'failure' });
    assert.equal(failedAnalysis.status, 1);
    assert.match(failedAnalysis.stderr, /required base\/head source analysis is incomplete/);
    assert.equal(fs.existsSync(path.join(f.output, 'performance-proposal.json')), false,
        'a required failed analysis cannot seal native repair authority');
    const missingAnalysis = f.run(requiredAnalysis, { PERFORMANCE_ANALYSIS_JOB_RESULT: 'success' });
    assert.equal(missingAnalysis.status, 1, 'a successful job claim without bound diagnostic metadata is insufficient');
    assert.equal(fs.existsSync(path.join(f.output, 'performance-proposal.json')), false);
    const result = f.run(args);
    assert.equal(result.status, 0, result.stderr);
    const proposal = JSON.parse(fs.readFileSync(path.join(f.output, 'performance-proposal.json')));
    assert.deepEqual(Buffer.from(proposal.files[0].contents, 'base64'), raw);
    assert.equal(fs.existsSync(sentinel), false, 'post-agent sealing must never invoke agent-configured clean filters');
    assert.deepEqual(execFileSync('git', ['cat-file', 'blob', `${proposal.treeSha}:tools/wta/src/fixture.rs`],
        { cwd: f.trust }), raw);
    assert.equal(proposal.report.status, 'pending_validation');
    fs.writeFileSync(path.join(f.repo, 'README.md'), 'Unauthorized extra file.\n');
    const escaped = f.run(args);
    assert.equal(escaped.status, 1);
    assert.match(escaped.stderr, /confined to original candidate/);
    assert.equal(fs.existsSync(sentinel), false);
    fs.rmSync(path.join(f.repo, 'README.md'));
    fs.writeFileSync(path.join(f.repo, '.gitignore'), 'ignored-attack.txt\n');
    assert.equal(f.run(args).status, 1, 'an untracked ignore rule is also an unauthorized edit');
    fs.rmSync(path.join(f.repo, '.gitignore'));
    fs.writeFileSync(path.join(f.repo, '.gitignore'), '/ignored-attack.txt\n');
    fs.writeFileSync(path.join(f.repo, 'ignored-attack.txt'), 'Ignored source injection');
    assert.equal(f.run(args).status, 1, 'ignored untracked files remain in the raw inventory');
    fs.rmSync(path.join(f.repo, 'ignored-attack.txt'));
    fs.rmSync(f.source);
    assert.equal(f.run(args).status, 1, 'deleted source remains in the raw inventory diff despite redirected Git');
});

test('actual compiled post-agent layout loads helper and Git context only from the fresh trusted checkout', t => {
    const f = fixture(t);
    const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    const steps = workflow.replaceAll('\r\n', '\n').split(/(?=^ {6}- )/m);
    const absent = steps.find(step => step.includes('name: Require an absent post-agent trusted checkout\n'));
    const checkout = steps.find(step => step.includes('name: Checkout fresh post-agent trust context\n'));
    const gate = steps.find(step => step.includes('id: repair_gate\n'));
    assert.ok(absent && checkout && gate, 'use the actual compiled post-agent steps');
    assert.match(checkout, /path: \.performance-trusted/);
    assert.match(checkout, /repository: \$\{\{ github.event.inputs.repo \}\}/);
    assert.match(checkout, /ref: \$\{\{ github.workflow_sha \}\}/);
    assert.match(checkout, /persist-credentials: false/);
    assert.ok(workflow.indexOf('name: Execute GitHub Copilot CLI') < workflow.indexOf(absent));
    assert.ok(workflow.indexOf(absent) < workflow.indexOf(checkout) && workflow.indexOf(checkout) < workflow.indexOf(gate));
    const extract = step => {
        const quoted = step.match(/^ {8}run: (".*")$/m);
        if (quoted) return JSON.parse(quoted[1]);
        const block = step.match(/^ {8}run: \|\n((?: {10}.*\n)+)/m);
        assert.ok(block, 'compiled script must use a quoted string or literal block');
        return block[1].replace(/^ {10}/gm, '');
    };
    const bash = process.platform === 'win32' ? 'C:\\Program Files\\Git\\bin\\bash.exe' : 'bash';
    const bashPath = filename => filename.replaceAll('\\', '/');
    const runner = path.join(f.root, 'runner');
    const output = path.join(f.root, 'agent-output');
    fs.mkdirSync(path.join(runner, 'gh-aw'), { recursive: true });
    fs.mkdirSync(output);
    const baseline = path.join(runner, 'gh-aw', 'performance-baseline.json');
    assert.equal(f.run(['prepare', '--output-dir', f.output, '--baseline', baseline, ...f.identity]).status, 0);
    fs.writeFileSync(path.join(output, 'performance-report.json'), JSON.stringify({
        version: 1, review: 'performance', mode: 'repair',
        identity: { prNumber: 42, baseSha: f.baseSha, headSha: f.headSha },
        status: 'pass', findings: [], checks: [],
    }));
    fs.writeFileSync(path.join(output, 'agent_output.json'), JSON.stringify({ items: [{ type: 'noop' }], errors: [] }));
    const sentinel = path.join(f.root, 'untrusted-filter-executed');
    fs.writeFileSync(path.join(f.repo, '.git', 'clean.cjs'),
        `require('fs').writeFileSync(${JSON.stringify(sentinel)}, 'executed');`);
    f.git(['config', 'filter.attack.clean', 'node .git/clean.cjs']);
    fs.writeFileSync(path.join(f.repo, '.git', 'info', 'attributes'), '* filter=attack\n');
    const blob = f.git(['rev-parse', `${f.headSha}:${f.runtimeRelative}`]);
    const fake = execFileSync('git', ['hash-object', '-w', '--stdin'], {
        cwd: f.repo, input: 'throw new Error("replaced runtime");', encoding: 'utf8',
    }).trim();
    f.git(['replace', blob, fake]);
    f.git(['config', 'core.worktree', f.root]);
    const env = { ...process.env, GITHUB_WORKSPACE: bashPath(f.repo), RUNNER_TEMP: bashPath(runner),
        GITHUB_OUTPUT: bashPath(path.join(f.root, 'step-output')), PR_NUMBER: '42', BASE_SHA: f.baseSha, HEAD_SHA: f.headSha };
    const execute = script => spawnSync(bash, [], { cwd: f.repo, env, input: script, encoding: 'utf8', timeout: 30000 });
    const requiredAbsent = execute(extract(absent));
    assert.equal(requiredAbsent.status, 0, requiredAbsent.stderr);
    const postTrust = path.join(f.repo, '.performance-trusted');
    // Simulate the pinned checkout action using the caller's pristine origin, never the agent's remote/config/objects.
    execFileSync('git', ['clone', '--quiet', '--no-local', f.trust, postTrust]);
    const result = execute(extract(gate).replaceAll('/tmp/gh-aw', bashPath(output)));
    assert.equal(result.status, 0, result.stderr);
    assert.match(fs.readFileSync(path.join(f.root, 'step-output'), 'utf8'), /status=pass/);
    assert.deepEqual(fs.readFileSync(path.join(runner, 'performance-trusted.mjs')),
        fs.readFileSync(path.join(postTrust, f.runtimeRelative)));
    assert.equal(fs.existsSync(sentinel), false);
    assert.equal(execute(extract(absent)).status, 1, 'an agent-created or reused trust directory must fail closed');
});
