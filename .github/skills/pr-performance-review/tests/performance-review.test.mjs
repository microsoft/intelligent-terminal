import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { execFileSync, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
    classifyPullRequest,
    gatePublication,
    renderReport,
    validateReport,
    verifyPullRequest,
    validateVerdict,
    prepareScope,
    validateProposal,
    reconstructTree,
    sealProposal,
} from '../scripts/performance-review.mjs';

const identity = {
    prNumber: 42,
    baseSha: 'a'.repeat(40),
    headSha: 'b'.repeat(40),
    mode: 'guide',
};

function finding(overrides = {}) {
    return {
        id: 'PERF-ABC12345',
        severity: 'medium',
        confidence: 'medium',
        dimension: 'responsiveness',
        category: 'ui-thread',
        title: 'Repeated synchronous enumeration',
        affectedScenario: 'Opening a tab with many panes',
        location: 'src/cascadia/TerminalApp/TerminalPage.cpp:100',
        observed: 'The changed path enumerates every pane on each event.',
        expected: 'The event should perform bounded work per affected pane.',
        impact: 'Large layouts may add UI-thread delay.',
        nativeEnvironment: { architecture: 'not-measured', details: 'Static review; Windows trace unavailable.' },
        evidence: [{ type: 'source', detail: 'Caller and loop inspected at immutable head.' }],
        proposedFix: 'Consider preserving the affected pane identity through dispatch.',
        validation: 'Run the pane-context end-to-end measurement on Windows x64.',
        fixDisposition: 'advice_only',
        ...overrides,
    };
}

function report(findings = [], checks = []) {
    return {
        version: 1,
        review: 'performance',
        mode: identity.mode,
        identity,
        status: findings.some(item => item.severity === 'high') ? 'action_required' :
            findings.length ? 'advisory' : checks.some(check => check.status === 'error') ? 'blocked' : 'pass',
        findings,
        checks,
    };
}

function proposal() {
    const value = report([finding({
        severity: 'high', confidence: 'high', fixDisposition: 'proposed', category: 'wta-runtime',
        location: 'tools/wta/src/master/mod.rs:1',
        evidence: [{ type: 'complexity-proof', detail: 'Repeated nested enumeration requires quadratic work.' }],
    })]);
    value.mode = 'repair';
    value.status = 'pending_validation';
    value.validationPlan = { type: 'wta-unit', testFilter: 'master::tests::preserves_state' };
    return {
        version: 1, identity, treeSha: 'c'.repeat(40), report: value,
        validationPlan: { ...value.validationPlan },
        files: [{ path: 'tools/wta/src/master/mod.rs', mode: '100644', contents: Buffer.from('fn repaired() {}\n').toString('base64') }],
    };
}

const helperPath = fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url));

function withRepository(action) {
    const previous = process.cwd();
    const root = fs.mkdtempSync(path.join(previous, 'performance-unit-'));
    const git = args => execFileSync('git', ['--no-pager', ...args], { cwd: root, encoding: 'utf8', stdio: ['pipe', 'pipe', 'pipe'] }).trim();
    const write = (filename, contents) => {
        const target = path.join(root, filename);
        fs.mkdirSync(path.dirname(target), { recursive: true });
        fs.writeFileSync(target, contents);
    };
    try {
        git(['init', '--quiet']);
        git(['config', 'user.name', 'Performance unit test']);
        git(['config', 'user.email', 'performance-unit@example.invalid']);
        git(['config', 'core.autocrlf', 'false']);
        write('tools\\wta\\src\\master\\mod.rs', 'fn base() {}\n');
        write('tools\\wta\\src\\unchanged.rs', 'fn unchanged() {}\n');
        write('doc\\policy.md', 'Reviewed policy.\n');
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'base']);
        const baseSha = git(['rev-parse', 'HEAD']);
        write('tools\\wta\\src\\master\\mod.rs', 'fn head() {}\n');
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'head']);
        const expected = { prNumber: 42, baseSha, headSha: git(['rev-parse', 'HEAD']), mode: 'repair' };
        process.chdir(root);
        return action({ root, git, write, expected, output: path.join(root, '.git', 'performance-artifacts') });
    } finally {
        process.chdir(previous);
        fs.rmSync(root, { recursive: true, force: true });
    }
}

test('selects repository hot-path source and keeps tests as supporting evidence', () => {
    const scope = classifyPullRequest([
        { filename: 'src/renderer/atlas/AtlasEngine.cpp', additions: 8, deletions: 2 },
        { filename: 'tools/wta/src/master/mod.rs', additions: 4, deletions: 1 },
        { filename: 'src/renderer/ut_renderer/AtlasTests.cpp', additions: 20, deletions: 0 },
    ], identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.categories, ['rendering', 'wta-runtime']);
    assert.equal(scope.candidates.length, 2);
    assert.equal(scope.supporting.length, 1);
});

test('Rust test basenames are supporting while runtime files remain candidates', () => {
    const supporting = [
        'tools/wta/src/master/tests.rs',
        'tools/wta/src/master/app_tests.rs',
        'tools/wta/src/test_support.rs',
        'tools/wta/src/tests/fix.rs',
    ];
    const testOnly = classifyPullRequest(supporting.map(filename => ({ filename })), identity);
    assert.equal(testOnly.applicable, false);
    assert.deepEqual(testOnly.supporting.map(file => file.filename), supporting);
    assert.deepEqual(testOnly.candidates, []);
    const runtime = 'tools/wta/src/master/app.rs';
    const scope = classifyPullRequest([...supporting, runtime].map(filename => ({ filename })), identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.candidates.map(file => file.filename), [runtime]);
    assert.deepEqual(scope.supporting.map(file => file.filename), supporting);
    // Classification is file-level: inline test blocks do not exclude runtime modules.
    assert.equal(classifyPullRequest([{ filename: 'tools/wta/src/master/mod.rs' }], identity).applicable, true);
    for (const filename of supporting) {
        const value = proposal();
        value.files[0].path = filename;
        assert.throws(() => validateProposal(value, { ...identity, mode: 'repair' }), /unique, regular WTA/);
    }
});

test('renderer HLSL alone is an applicable rendering candidate', () => {
    const filename = 'src/renderer/atlas/shader_ps.hlsl';
    const scope = classifyPullRequest([{ filename }], identity);
    assert.equal(scope.applicable, true);
    assert.deepEqual(scope.categories, ['rendering']);
    assert.deepEqual(scope.candidates.map(file => file.filename), [filename]);
});

test('only the exact WTA Cargo manifest and lockfile are review candidates', () => {
    for (const filename of ['tools/wta/Cargo.toml', 'tools/wta/Cargo.lock']) {
        const scope = classifyPullRequest([{ filename }], identity);
        assert.equal(scope.applicable, true);
        assert.deepEqual(scope.categories, ['wta-runtime']);
        assert.deepEqual(scope.candidates.map(file => file.filename), [filename]);
    }
    const scope = classifyPullRequest([
        'tools/other/Cargo.toml', 'tools/other/Cargo.lock',
        'tools/wta/src/Cargo.toml', 'tools/wta/src/Cargo.lock',
        'tools/wta/other.toml', 'tools/wta/other.lock',
    ].map(filename => ({ filename })), identity);
    assert.equal(scope.applicable, false);
    assert.equal(scope.excluded.length, 6);
});

test('prepared input includes exact immutable Git file and line counts', () => {
    withRepository(({ expected, output }) => {
        const scope = prepareScope(expected, output);
        assert.deepEqual(scope.totals, { files: 1, candidates: 1, additions: 1, deletions: 1, binaryFiles: 0 });
        assert.equal(scope.candidates[0].additions, 1);
        assert.equal(scope.candidates[0].deletions, 1);
        assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'performance-scope.json'))).totals, scope.totals);
    });
});

test('prepared change-size input counts all files and does not invent binary line counts', () => {
    withRepository(({ git, write, expected, output }) => {
        write('doc\\extra.md', 'First line.\nSecond line.\n');
        write('image.bin', Buffer.from([0, 1, 2, 3]));
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'size fixture']);
        const scope = prepareScope({ ...expected, headSha: git(['rev-parse', 'HEAD']) }, output);
        assert.deepEqual(scope.totals, { files: 3, candidates: 1, additions: 3, deletions: 1, binaryFiles: 1 });
        const binary = scope.excluded.find(file => file.filename === 'image.bin');
        assert.equal(binary.additions, null);
        assert.equal(binary.deletions, null);
    });
});

test('does not infer a hot path from a filename, documentation, benchmark, or test alone', () => {
    const scope = classifyPullRequest([
        { filename: 'doc/performance-plan.md' },
        { filename: 'test/e2e/PerformanceRegression.Tests.ps1' },
        { filename: 'src/tools/ConsoleBench/main.cpp' },
        { filename: 'README.md' },
    ], identity);
    assert.equal(scope.applicable, false);
    assert.equal(scope.candidates.length, 0);
});

test('requires measurement evidence to identify microbenchmark versus end-to-end', () => {
    const bad = report([finding({
        evidence: [{ type: 'measurement', detail: 'Median rose by 20 percent.', kind: 'timing' }],
    })]);
    assert.throws(() => validateReport(bad, identity), /must distinguish microbenchmark/);
});

test('rejects noisy measurements without samples and spread', () => {
    const bad = report([finding({
        evidence: [{ type: 'measurement', kind: 'end-to-end', noisy: true, detail: 'Results varied.' }],
    })]);
    assert.throws(() => validateReport(bad, identity), /at least three samples/);
});

test('concurrency is a schema category, not a keyword-based proof classifier', () => {
    const valid = report([finding({
        category: 'concurrency',
        evidence: [{ type: 'source', detail: 'A generic function changed; synchronization impact is uncertain.' }],
    })]);
    assert.equal(validateReport(valid, identity), valid);
    const bad = report([finding({
        category: 'concurrency',
        severity: 'high',
        confidence: 'high',
        fixDisposition: 'manual_required',
        evidence: [{ type: 'source', detail: 'lock channel await task deadlock race synchronization' }],
    })]);
    assert.throws(() => validateReport(bad, identity), /strong proof/);
    assert.throws(() => validateReport(report([finding({ category: 'invented' })]), identity), /category/);
});

test('medium and low findings remain advice-only', () => {
    const bad = report([finding({ fixDisposition: 'manual_required' })]);
    assert.throws(() => validateReport(bad, identity), /must be advice_only/);
});

test('failed validation is blocked rather than presented as a passing review', () => {
    const check = { name: 'Native benchmark', status: 'error', command: 'ConsoleBench.exe', exitCode: 1, detail: 'Runner failed.' };
    const bad = report([], [check]);
    bad.status = 'pass';
    assert.throws(() => validateReport(bad, identity), /must be blocked/);
});

test('only evidenced high findings produce action_required status', () => {
    const high = finding({
        severity: 'high',
        confidence: 'high',
        evidence: [
            { type: 'source', detail: 'The immutable head introduces an unbounded loop.' },
            { type: 'complexity-proof', detail: 'Caller nesting changes work from O(n) to O(n squared).' },
        ],
        fixDisposition: 'manual_required',
    });
    assert.equal(validateReport(report([high]), identity).status, 'action_required');
    const bad = report([finding()]);
    bad.status = 'action_required';
    assert.throws(() => validateReport(bad, identity), /must be advisory/);
});

test('regression checks require findings without overriding their severity-derived status', () => {
    const checks = [{
        name: 'Repeated path', status: 'regression', command: 'Inspect immutable source',
        exitCode: null, detail: 'Repeated work increased at the reviewed head.',
    }];
    assert.throws(() => validateReport(report([], checks), identity), /regression checks require at least one finding/);
    for (const severity of ['medium', 'low']) {
        const value = report([finding({ severity })], checks);
        assert.equal(validateReport(value, identity).status, 'advisory');
        value.status = 'action_required';
        assert.throws(() => validateReport(value, identity), /must be advisory/);
    }
    const high = report([finding({
        severity: 'high', confidence: 'high', fixDisposition: 'manual_required',
        evidence: [{ type: 'complexity-proof', detail: 'Nested callers increase repeated work from O(n) to O(n squared).' }],
    })], checks);
    assert.equal(validateReport(high, identity).status, 'action_required');
});

test('HIGH confidence, proof types, stable IDs, and guidance remain explicit schema boundaries', () => {
    const high = finding({
        severity: 'high', confidence: 'high', fixDisposition: 'manual_required',
        evidence: [{ type: 'resource-proof', detail: 'Every event retains another unreachable session.' }],
    });
    for (const confidence of ['medium', 'low']) {
        assert.throws(() => validateReport(report([{ ...high, confidence }]), identity), /high confidence/);
    }
    assert.throws(() => validateReport(report([{ ...high, evidence: [{ type: 'source', detail: 'Repeated call.' }] }]), identity), /strong proof/);
    assert.throws(() => validateReport(report([high, high]), identity), /unique stable/);
    assert.throws(() => validateReport(report([{ ...high, id: 'PERF-1' }]), identity), /unique stable/);
    assert.throws(() => validateReport(report([{ ...high, fixDisposition: 'proposed' }]), identity), /only in repair mode/);
    assert.equal(validateReport(report([finding({ severity: 'low', confidence: 'low' })]), identity).status, 'advisory');
});

test('repair mode accepts HIGH proposals but never authorizes model-authored fixes', () => {
    const repairIdentity = { ...identity, mode: 'repair' };
    const high = finding({
        severity: 'high',
        confidence: 'high',
        evidence: [
            { type: 'source', detail: 'The immutable head repeats the operation.' },
            { type: 'complexity-proof', detail: 'Nested callers make the path O(n squared).' },
        ],
        fixDisposition: 'fixed',
        validation: 'Focused regression test passed on the final patch.',
    });
    const fixed = report([high], [{
        name: 'Focused regression',
        status: 'pass',
        command: 'run-focused-test',
        exitCode: 0,
        detail: 'Final patch passed.',
    }]);
    fixed.mode = 'repair';
    fixed.identity = { prNumber: 42, baseSha: 'a'.repeat(40), headSha: 'b'.repeat(40) };
    fixed.status = 'fixed';
    assert.throws(() => validateReport(fixed, repairIdentity), /model-authored fixed/);
    const scope = classifyPullRequest([{ filename: 'src/buffer/out/TextBuffer.cpp' }], fixed.identity);
    assert.throws(() => gatePublication(
        scope,
        fixed,
        { items: [{ type: 'push_to_pull_request_branch' }], errors: [] },
        { ...repairIdentity, changedFiles: ['src/buffer/out/TextBuffer.cpp'] }
    ), /model-authored fixed/);
    const proposed = structuredClone(fixed);
    proposed.checks = [{ name: 'Focused regression', status: 'unavailable', command: 'not run',
        exitCode: null, detail: 'Awaiting trusted native validation.' }];
    proposed.findings[0].fixDisposition = 'proposed';
    proposed.findings[0].validation = 'Native validation pending.';
    proposed.status = 'pending_validation';
    proposed.validationPlan = { type: 'wta-unit', testFilter: 'tests::actual_test' };
    assert.doesNotThrow(() => gatePublication(
        scope, proposed,
        { items: [{ type: 'validate_performance_repair', confirm: true }], errors: [] },
        { ...repairIdentity, changedFiles: ['src/buffer/out/TextBuffer.cpp'] }
    ));
    assert.throws(() => gatePublication(
        scope,
        proposed,
        { items: [{ type: 'validate_performance_repair', confirm: true }], errors: [] },
        { ...repairIdentity, changedFiles: ['tools/unrelated.ps1'] }
    ), /limited to original candidate files/);
});

test('repair mode cannot edit for medium findings or failed validation', () => {
    const repairIdentity = { ...identity, mode: 'repair' };
    const medium = report([finding({ fixDisposition: 'fixed' })]);
    medium.mode = 'repair';
    assert.throws(() => validateReport(medium, repairIdentity), /model-authored fixed/);

    const high = finding({
        severity: 'high',
        confidence: 'high',
        evidence: [{ type: 'blocking-proof', detail: 'A lock is held across an awaited channel receive.' }],
        fixDisposition: 'proposed',
    });
    const failed = report([high], [{
        name: 'Focused regression',
        status: 'error',
        command: 'run-focused-test',
        exitCode: 1,
        detail: 'Test failed.',
    }]);
    failed.mode = 'repair';
    failed.status = 'blocked';
    assert.equal(validateReport(failed, repairIdentity).status, 'blocked');
    const scope = classifyPullRequest([{ filename: 'tools/wta/src/master/mod.rs' }], identity);
    assert.throws(() => gatePublication(scope, failed, {
        items: [{ type: 'validate_performance_repair', confirm: true }],
    }, { ...repairIdentity, changedFiles: ['tools/wta/src/master/mod.rs'] }), /require pending_validation/);
});

test('rejects malformed output and stale head identity', () => {
    assert.throws(() => validateReport({ version: 1 }, identity), /performance review/);
    const stale = report();
    stale.identity = { ...identity, headSha: 'c'.repeat(40) };
    assert.throws(() => validateReport(stale, identity), /does not match/);
});

test('fork-compatible publication gate allows only an exact deterministic comment', () => {
    const scope = classifyPullRequest([{ filename: 'src/buffer/out/TextBuffer.cpp' }], identity);
    const valid = report([finding()]);
    const body = renderReport(valid);
    assert.doesNotThrow(() => gatePublication(scope, valid, { items: [{ type: 'add_comment', body }], errors: [] }, identity));
    assert.throws(() => gatePublication(scope, valid, { items: [{ type: 'add_comment', body: `${body}changed` }] }, identity), /exactly match/);
    for (const key of ['item_number', 'pr', 'pr_number', 'issue', 'issue_number', 'repo', 'target', 'target_repo',
        'target-repo', 'comment_id', 'reply_to_id', 'discussion_id']) {
        assert.throws(() => gatePublication(scope, valid, {
            items: [{ type: 'add_comment', body, [key]: key === 'repo' ? 'other/repo' : 999 }],
        }, identity), /must not override/);
    }
    assert.throws(() => gatePublication(scope, valid, {
        items: [{ type: 'add_comment', body }, { type: 'noop' }],
    }, identity), /exactly one add_comment/);
});

test('non-applicable scope short-circuits with one noop and no report', () => {
    const scope = classifyPullRequest([{ filename: 'doc/performance.md' }], identity);
    assert.doesNotThrow(() => gatePublication(scope, null, { items: [{ type: 'noop' }], errors: [] }, identity));
    assert.throws(() => gatePublication(scope, report(), { items: [{ type: 'add_comment', body: 'x' }], errors: [] }, identity), /exactly one noop/);
});

test('scope rejects malformed and unsafe paths instead of creating false candidates', () => {
    for (const filename of [
        'src/renderer/../secret.cpp', '/src/renderer/file.cpp',
        'src\\renderer\\file.cpp', 'src/renderer/file.cpp\nother.cpp',
        'src/renderer//file.cpp', 'src/renderer/file.cpp:stream',
    ]) {
        assert.throws(() => classifyPullRequest([{ filename }], identity), /safe repository-relative/);
    }
    assert.throws(() => classifyPullRequest({}, identity), /must be an array/);
    assert.throws(() => classifyPullRequest([[{ filename: 'tools/wta/src/master/mod.rs' }]], identity), /valid filename/);
    assert.throws(() => classifyPullRequest(Array(3001).fill({ filename: 'README.md' }), identity), /at most 3000/);
});

test('repair noop rejects dirty product source and requires an inventory', () => {
    const expected = { ...identity, mode: 'repair' };
    const scope = classifyPullRequest([{ filename: 'src/renderer/base/renderer.cpp' }], identity);
    const valid = report();
    valid.mode = 'repair';
    assert.throws(() => gatePublication(scope, valid, { items: [{ type: 'noop' }] }, expected),
        /final changed-file inventory/);
    assert.throws(() => gatePublication(scope, valid, { items: [{ type: 'noop' }] }, {
        ...expected, changedFiles: ['src/renderer/base/renderer.cpp'],
    }), /cannot discard/);
    assert.doesNotThrow(() => gatePublication(scope, valid, { items: [{ type: 'noop' }] }, {
        ...expected, changedFiles: [],
    }));
});

test('malformed ingestion errors and tampered applicability fail closed', () => {
    const scope = classifyPullRequest([{ filename: 'src/renderer/base/renderer.cpp' }], identity);
    assert.throws(() => gatePublication(scope, report(), { items: [{ type: 'noop' }], errors: {} }, identity),
        /envelope is invalid/);
    scope.applicable = false;
    assert.throws(() => gatePublication(scope, null, { items: [{ type: 'noop' }] }, identity),
        /correctly classified/);
});

test('passing checks cannot hide nonzero or missing exit codes', () => {
    for (const exitCode of [1, null]) {
        const value = report([], [{
            name: 'Fake pass', command: 'test', detail: 'Claimed pass', status: 'pass', exitCode,
        }]);
        assert.throws(() => validateReport(value, identity), /passing checks must exit 0/);
    }
});

test('live PR verification does not trust a caller-supplied same-repository flag', () => {
    const expected = {
        ...identity, expectedBaseSha: identity.baseSha, repository: 'owner/repo',
        headRepository: 'owner/repo', headRef: 'topic', baseRef: 'main', sameRepo: 'true',
    };
    const pr = {
        number: 42, state: 'open',
        head: { sha: identity.headSha, ref: 'topic', repo: { id: 1, full_name: 'owner/repo' } },
        base: { sha: identity.baseSha, ref: 'main', repo: { id: 1, full_name: 'owner/repo' } },
    };
    assert.doesNotThrow(() => verifyPullRequest(pr, expected));
    for (const mutate of [
        p => { p.head.repo.id = 2; },
        p => { p.head.sha = 'c'.repeat(40); },
        p => { p.base.sha = 'c'.repeat(40); },
        p => { p.state = 'closed'; },
        p => { p.head.ref = 'other'; },
    ]) {
        const modified = structuredClone(pr);
        mutate(modified);
        assert.throws(() => verifyPullRequest(modified, expected), /metadata does not match/);
    }
});

test('successful workflow runs do not override blocking domain verdicts', () => {
    for (const status of ['action_required', 'blocked']) {
        assert.throws(() => validateVerdict({ version: 1, identity, status }, identity), /requires action/);
    }
    for (const status of ['pass', 'advisory', 'pending_validation']) {
        assert.doesNotThrow(() => validateVerdict({ version: 1, identity, status }, identity));
    }
    assert.throws(() => validateVerdict({ version: 1, identity, status: 'fixed' }, identity), /malformed/);
    assert.throws(() => validateVerdict({ version: 1, identity, status: 'unknown' }, identity), /malformed/);
});

test('proposals accept only bounded regular WTA replacements and agreeing fixed test plans', () => {
    const valid = proposal();
    const expected = { ...identity, mode: 'repair' };
    assert.equal(validateProposal(valid, expected), valid);
    const invalid = [
        [value => { value.identity.headSha = 'd'.repeat(40); }, /does not match/],
        [value => { value.report.identity.headSha = 'd'.repeat(40); }, /does not match/],
        [value => { value.treeSha = 'not-a-tree'; }, /Git tree SHA/],
        [value => { value.report.findings[0].fixDisposition = 'manual_required'; value.report.status = 'action_required'; }, /only repair-eligible/],
        [value => { value.validationPlan.type = 'arbitrary-command'; }, /supported focused native/],
        [value => { value.validationPlan.testFilter = 'tests; cmd.exe'; }, /supported focused native/],
        [value => { value.validationPlan.testFilter = 'other::tests::preserves_state'; }, /must agree/],
        [value => { delete value.report.validationPlan; }, /supported focused native/],
        [value => { value.files = []; }, /between one and five/],
        [value => { value.files = Array(6).fill(value.files[0]); }, /between one and five/],
        [value => { value.files.push({ ...value.files[0] }); }, /unique, regular WTA/],
        [value => { value.files[0].mode = '100755'; }, /unique, regular WTA/],
        [value => { value.files[0].mode = '120000'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'src/renderer/base/renderer.cpp'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'src/renderer/atlas/shader_ps.hlsl'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'tools/wta/Cargo.toml'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'tools/wta/Cargo.lock'; }, /unique, regular WTA/],
        [value => { value.files[0].path = 'tools/wta/src/../policy.rs'; }, /safe repository-relative/],
        [value => { value.files[0].path = 'tools/wta/src/tests/fix.rs'; }, /unique, regular WTA/],
        [value => { value.files[0].contents = 'not base64'; }, /unique, regular WTA/],
        [value => { value.files[0].contents = Buffer.alloc(256 * 1024 + 1).toString('base64'); }, /size limit/],
        [value => { value.files.push({ ...value.files[0], path: 'tools/wta/src/extra.rs' });
            for (const file of value.files) file.contents = Buffer.alloc(128 * 1024 + 1).toString('base64'); }, /size limit/],
    ];
    for (const [mutate, message] of invalid) {
        const value = structuredClone(valid);
        mutate(value);
        assert.throws(() => validateProposal(value, expected), message);
    }
});

test('the model validator rejects the observed copied selector before native dispatch', () => {
    const value = proposal();
    value.report.validationPlan.testFilter = 'module::tests';
    value.validationPlan.testFilter = 'module::tests';
    assert.throws(() => validateReport(value.report, { ...identity, mode: 'repair' }), /not a placeholder/);
    assert.throws(() => validateProposal(value, identity), /not a placeholder/);
});

test('focused selectors require qualified Rust identifiers, not module substring filters', () => {
    for (const selector of [
        'tests', 'master::tests', 'fixture::tests', 'preserves_state',
        'master:tests:preserves_state', 'master::::preserves_state', '::master::preserves_state',
        'master::preserves_state::', '1master::preserves_state', 'master::1test',
        'master::preserves-state', 'master::préserves_state', 'master::' + 'a'.repeat(200),
    ]) {
        const value = proposal();
        value.validationPlan.testFilter = selector;
        value.report.validationPlan.testFilter = selector;
        assert.throws(() => validateReport(value.report, { ...identity, mode: 'repair' }), /supported focused native/);
        assert.throws(() => validateProposal(value, identity), /supported focused native/);
    }
    // Syntax alone is not source proof; native listing on the original head is decisive.
    for (const selector of ['master::tests::preserves_state', 'fixture::tests::preserves_state', 'other_module::_test2']) {
        const value = proposal();
        value.validationPlan.testFilter = selector;
        value.report.validationPlan.testFilter = selector;
        assert.doesNotThrow(() => validateReport(value.report, { ...identity, mode: 'repair' }));
        assert.equal(validateProposal(value, identity), value);
    }
});

test('stable finding IDs are bounded identifiers, not an arbitrary eight-character protocol', () => {
    const value = proposal();
    value.report.findings[0].id = 'PERF-WTAQUAD01';
    assert.doesNotThrow(() => validateReport(value.report, { ...identity, mode: 'repair' }));
    value.report.findings[0].id = 'PERF-' + 'A'.repeat(65);
    assert.throws(() => validateReport(value.report, { ...identity, mode: 'repair' }), /stable PERF identifier/);
});

test('an unvalidated proposal card is visibly pending, not a green success', () => {
    const card = renderReport(proposal().report);
    assert.match(card, /^## ⏳ Intelligent Terminal performance review/);
    assert.match(card, /\*\*Status:\*\* `pending_validation`/);
    assert.match(card, /\*\*Disposition:\*\* proposed/);
    assert.doesNotMatch(card, /✅|GitHub recorded Windows validation success/);
});

test('repair proposals require exactly one confirmed validation tool; fake pass cannot permit a push', () => {
    const valid = proposal();
    const expected = { ...identity, mode: 'repair', changedFiles: valid.files.map(file => file.path) };
    const scope = classifyPullRequest(valid.files.map(file => ({ filename: file.path })), identity);
    valid.report.checks.push({ name: 'Fake native pass', status: 'pass', command: 'invented', exitCode: 0, detail: 'Model claimed native success.' });
    assert.throws(() => validateReport(valid.report, expected), /cannot contain model-authored pass checks/);
    assert.throws(() => gatePublication(scope, valid.report, {
        items: [{ type: 'validate_performance_repair', confirm: true }],
    }, expected), /cannot contain model-authored pass checks/);
    valid.report.checks[0] = { name: 'Native validation', status: 'unavailable', command: 'not run', exitCode: null, detail: 'Awaiting trusted Windows validation.' };
    for (const items of [
        [{ type: 'push_to_pull_request_branch' }],
        [{ type: 'add_comment', body: renderReport(valid.report) }],
        [{ type: 'validate_performance_repair', confirm: true }, { type: 'noop' }],
        [],
    ]) {
        assert.throws(() => gatePublication(scope, valid.report, { items }, expected), /exactly one validate_performance_repair/);
    }
    for (const confirm of [undefined, false, 'false', 1]) {
        assert.throws(() => gatePublication(scope, valid.report, {
            items: [{ type: 'validate_performance_repair', confirm }],
        }, expected), /explicit proposal confirmation/);
    }
    for (const confirm of [true, 'true']) {
        assert.doesNotThrow(() => gatePublication(scope, valid.report, {
            items: [{ type: 'validate_performance_repair', confirm }],
        }, expected));
    }
});

test('every proposed location is canonical and bound to a covered sealed replacement, independently of category', () => {
    const expected = { ...identity, mode: 'repair' };
    for (const category of ['concurrency', 'session-log-enumeration', 'wta-runtime']) {
        const value = proposal();
        value.report.findings[0].category = category;
        assert.equal(validateProposal(value, expected), value);
    }
    for (const location of [
        'src/renderer/atlas/AtlasEngine.cpp:1', 'tools/wta/src/unchanged.rs:1',
        'tools\\wta\\src\\master\\mod.rs:1', './tools/wta/src/master/mod.rs:1',
        'tools/wta/src/master/mod.rs:0', 'tools/wta/src/master/mod.rs:01',
        'tools/wta/src/master/mod.rs:1:2', 'tools/wta/src/master/mod.rs',
    ]) {
        const value = proposal();
        value.report.findings[0].location = location;
        assert.throws(() => validateProposal(value, expected), /canonical|safe repository-relative/);
    }
    const value = proposal();
    value.files.push({ ...value.files[0], path: 'tools/wta/src/extra.rs' });
    assert.throws(() => validateProposal(value, expected), /covered by a proposed finding/);
    value.report.findings.push(finding({
        ...value.report.findings[0], id: 'PERF-EXTRA', location: 'tools/wta/src/extra.rs:3',
    }));
    assert.equal(validateProposal(value, expected), value);
    value.report.findings[1].location = 'src/renderer/atlas/AtlasEngine.cpp:1';
    assert.throws(() => validateProposal(value, expected), /canonical/);
});

test('inline Rust test source is immutable Git data, including markers, gates, modules and every trailing test', () => {
    withRepository(({ git, write, expected, output }) => {
        const filename = 'tools/wta/src/master/mod.rs';
        const suffix = '#[cfg(test)]\nmod tests {\n    #[test]\n    fn selected() { assert_eq!(super::work(), 2); }\n' +
            '    #[tokio::test]\n    async fn other() { assert!(true); }\n}\n';
        const original = 'fn work() -> usize { 2 }\n' + suffix;
        write('tools\\wta\\src\\master\\mod.rs', original);
        git(['add', '--all']);
        git(['commit', '--quiet', '-m', 'inline original']);
        expected.headSha = git(['rev-parse', 'HEAD']);
        const replacement = contents => [{ path: filename, mode: '100644', contents: Buffer.from(contents).toString('base64') }];
        const candidate = original.replace('usize { 2 }', 'usize { 3 }');
        assert.doesNotThrow(() => reconstructTree(replacement(candidate), expected.headSha, expected.baseSha));
        for (const changed of [
            candidate.replace('super::work(), 2', 'super::work(), 3'),
            candidate.replace('assert!(true)', 'assert!(false)'),
            candidate.replace('cfg(test)', 'cfg(any())'),
            candidate.replace('mod tests', 'mod renamed'),
            candidate.replace('#[test]', '#[ignore]'),
            candidate.replace('#[tokio::test]', ''),
            candidate.replace('fn selected', 'fn renamed'),
            candidate.replace(suffix, ''),
            candidate + '#[test] fn added() {}\n',
            '#[test] fn added() {}\n' + candidate,
        ]) {
            // A model-mutated workspace must never become the comparison authority.
            write('tools\\wta\\src\\master\\mod.rs', changed);
            assert.throws(() => reconstructTree(replacement(changed), expected.headSha), /immutable original Rust test/);
            const baselinePath = path.join(output, 'baseline.json');
            const scope = prepareScope(expected, output, baselinePath);
            const value = proposal();
            value.report.identity = expected;
            write('tools\\wta\\src\\master\\mod.rs', changed + '// model edit\n');
            assert.throws(() => sealProposal(scope, value.report, JSON.parse(fs.readFileSync(baselinePath)), expected),
                /immutable original Rust test/);
        }
        for (const gate of [
            '#[\n cfg(\n any(windows, test)\n )\n]\nmod tests { #[test] fn selected() {} }\n',
            '#[cfg_attr(\n test,\n allow(dead_code)\n)]\nfn helper() {}\n',
            '#[tokio::test]\nasync fn selected() {}\n',
            '#[cfg(windows)]\nmod custom {\n#[tokio::test]\nasync fn selected() {}\n}\n',
            '    #[cfg(windows)]\n    mod custom {\n#[tokio::test]\nasync fn selected() {}\n}\n',
            '    mod custom {\n#[test]\nfn selected() {}\n}\n',
        ]) {
            write('tools\\wta\\src\\master\\mod.rs', 'fn runtime() {}\n' + gate);
            git(['add', filename]);
            git(['commit', '--quiet', '-m', 'gate pattern']);
            const head = git(['rev-parse', 'HEAD']);
            assert.doesNotThrow(() => reconstructTree(replacement('fn repaired() {}\n' + gate), head));
            assert.throws(() => reconstructTree(replacement('fn repaired() {}\n' + gate.replace(/test/g, 'renamed')), head),
                /immutable original Rust test/);
            for (const token of ['#[cfg(windows)]', 'mod custom', '#[test]', '#[tokio::test]']) {
                if (!gate.includes(token)) continue;
                assert.throws(() => reconstructTree(replacement('fn repaired() {}\n' + gate.replace(token, `//${token}`)), head),
                    /immutable original Rust test/, `commenting ${token} must not bypass the suffix guard`);
            }
        }
        assert.throws(() => reconstructTree(replacement('fn runtime() {}\n#[test] fn added() {}\n'), expected.baseSha),
            /immutable original Rust test/);
    });
});

test('Git preparation and sealing separate trusted restoration from model source edits and preserve raw blobs', () => {
    withRepository(({ root, git, write, expected, output }) => {
        write('doc\\policy.md', 'Trusted restoration, not a model edit.\n');
        const baselinePath = path.join(output, 'baseline.json');
        const scope = prepareScope(expected, output, baselinePath);
        const baseline = JSON.parse(fs.readFileSync(baselinePath, 'utf8'));
        assert.deepEqual(scope.candidates.map(file => file.filename), ['tools/wta/src/master/mod.rs']);
        assert.match(fs.readFileSync(path.join(output, 'performance-patch.txt'), 'utf8'), /fn head/);
        assert.notEqual(baseline.treeSha, git(['rev-parse', 'HEAD^{tree}']));
        const unchangedTree = git(['write-tree']);
        git(['config', 'core.autocrlf', 'true']);
        const raw = Buffer.from('fn repaired() {}\r\n');
        write('tools\\wta\\src\\master\\mod.rs', raw);
        const value = proposal();
        value.report.identity = expected;
        const sealed = sealProposal(scope, value.report, baseline, expected);
        assert.deepEqual(sealed.changedFiles, ['tools/wta/src/master/mod.rs']);
        assert.equal(validateProposal(sealed.proposal, expected), sealed.proposal);
        const rawBlob = execFileSync('git', ['cat-file', 'blob', `${sealed.proposal.treeSha}:tools/wta/src/master/mod.rs`], { cwd: root });
        assert.deepEqual(Buffer.from(sealed.proposal.files[0].contents, 'base64'), rawBlob);
        assert.deepEqual(rawBlob, Buffer.from('fn repaired() {}\n'));
        assert.equal(git(['show', `${sealed.proposal.treeSha}:doc/policy.md`]), 'Reviewed policy.');
        assert.equal(git(['write-tree']), unchangedTree);
        assert.equal(reconstructTree(sealed.proposal.files, expected.headSha, expected.baseSha), sealed.proposal.treeSha);
        const rawReplacement = { ...sealed.proposal.files[0], contents: raw.toString('base64') };
        const rawTree = reconstructTree([rawReplacement], expected.headSha, expected.baseSha);
        assert.deepEqual(execFileSync('git', ['cat-file', 'blob', `${rawTree}:tools/wta/src/master/mod.rs`], { cwd: root }), raw);
        assert.throws(() => reconstructTree([{
            ...sealed.proposal.files[0], path: 'tools/wta/src/unchanged.rs',
        }], expected.headSha, expected.baseSha), /outside the immutable original/);
        assert.throws(() => reconstructTree([{
            ...sealed.proposal.files[0], path: 'tools/wta/src/missing.rs',
        }], expected.headSha), /existing regular file/);
        assert.throws(() => reconstructTree([{
            ...sealed.proposal.files[0], mode: '100755',
        }], expected.headSha), /existing regular file/);
        write('doc\\policy.md', 'Model tried an unrelated edit.\n');
        assert.throws(() => sealProposal(scope, value.report, baseline, expected), /confined to original candidate/);
        assert.equal(fs.readdirSync(path.join(root, '.git')).some(name => name.startsWith('performance-index-')), false);
    });
});

test('CLI re-reads source scope from Git and checks the sealed tree before applying replacements', () => {
    withRepository(({ root, git, write, expected, output }) => {
        const baselinePath = path.join(output, 'baseline.json');
        const scope = prepareScope(expected, output, baselinePath);
        const baseline = JSON.parse(fs.readFileSync(baselinePath, 'utf8'));
        const value = proposal();
        value.report.identity = expected;
        write('tools\\wta\\src\\master\\mod.rs', 'fn repaired() {}\n');
        const sealed = sealProposal(scope, value.report, baseline, expected).proposal;
        const proposalPath = path.join(output, 'proposal.json');
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        const identityArgs = ['--pr', String(expected.prNumber), '--base', expected.baseSha, '--head', expected.headSha];
        const run = (command, args = []) => spawnSync(process.execPath, [helperPath, command, ...identityArgs, ...args], { cwd: root, encoding: 'utf8' });
        assert.equal(run('validate-proposal', ['--input', proposalPath]).status, 0);
        sealed.treeSha = 'c'.repeat(40);
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        const wrongTree = run('apply-proposal', ['--input', proposalPath]);
        assert.equal(wrongTree.status, 1);
        assert.match(wrongTree.stderr, /do not match the sealed tree/);
        sealed.treeSha = reconstructTree(sealed.files, expected.headSha, expected.baseSha);
        fs.writeFileSync(proposalPath, JSON.stringify(sealed));
        git(['checkout', '--', 'tools/wta/src/master/mod.rs']);
        assert.equal(run('apply-proposal', ['--input', proposalPath]).status, 0);
        assert.equal(git(['write-tree']), sealed.treeSha);
        assert.equal(fs.readFileSync(path.join(root, 'tools', 'wta', 'src', 'master', 'mod.rs'), 'utf8'), 'fn repaired() {}\n');
        git(['reset', '--hard', expected.headSha]);
        const reportPath = path.join(output, 'report.json');
        const agentPath = path.join(output, 'agent.json');
        fs.writeFileSync(reportPath, JSON.stringify(sealed.report));
        fs.writeFileSync(agentPath, JSON.stringify({ items: [{ type: 'validate_performance_repair', confirm: true }] }));
        fs.writeFileSync(path.join(output, 'performance-scope.json'), JSON.stringify({
            ...scope, candidates: [{ filename: 'tools/wta/src/unchanged.rs' }],
        }));
        write('tools\\wta\\src\\unchanged.rs', 'fn model_tried_outside_scope() {}\n');
        const rejected = run('gate', [
            '--mode', 'repair', '--output-dir', output, '--baseline', baselinePath,
            '--report', reportPath, '--agent-output', agentPath,
        ]);
        assert.equal(rejected.status, 1);
        assert.match(rejected.stderr, /confined to original candidate/);
        const refreshed = JSON.parse(fs.readFileSync(path.join(output, 'performance-scope.json'), 'utf8'));
        assert.deepEqual(refreshed.candidates.map(file => file.filename), ['tools/wta/src/master/mod.rs']);
        assert.equal(fs.existsSync(path.join(output, 'performance-proposal.json')), false);
    });
});
