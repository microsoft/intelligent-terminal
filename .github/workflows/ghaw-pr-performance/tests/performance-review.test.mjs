import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { createRequire } from 'node:module';
import { classifyPullRequest, prepareScope, readReviewSummary, verifyFixCommits, verifyProposalTransport, verifyPullRequest } from '../scripts/performance-review.mjs';

const identity = { prNumber: 42, baseSha: 'a'.repeat(40), headSha: 'b'.repeat(40) };

test('source scope includes product code but not tests or documentation', () => {
    const scope = classifyPullRequest([
        'src/new/worker.cpp', 'src/renderer/atlas/shader_ps.hlsl', 'tools/wta/src/lib.rs',
        'tools/wta/Cargo.toml', 'tools/wta/src/master/tests.rs', 'doc/spec.md',
    ].map(filename => ({ filename })), identity);
    assert.deepEqual(scope.candidates.map(file => file.filename), [
        'src/new/worker.cpp', 'src/renderer/atlas/shader_ps.hlsl', 'tools/wta/src/lib.rs', 'tools/wta/Cargo.toml',
    ]);
    assert.equal(scope.applicable, true);
    assert.equal(classifyPullRequest([{ filename: 'README.md' }], identity).applicable, false);
    for (const filename of ['../src/a.cpp', 'src\\a.cpp', '/src/a.cpp', 'src/a.cpp:1'])
        assert.throws(() => classifyPullRequest([{ filename }], identity), /canonical/);
});

test('C++ test-only PRs are supporting evidence, not applicable product scope', () => {
    for (const filename of [
        'src/cascadia/LocalTests_TerminalApp/TabTests.cpp',
        'src/host/UnitTests_Host/BufferTests.cpp',
        'src/cascadia/WindowsTerminal_UIATests/Tests.cpp',
    ]) {
        const scope = classifyPullRequest([{ filename }], identity);
        assert.equal(scope.applicable, false);
        assert.deepEqual(scope.supporting, [{ filename }]);
    }
});

test('live PR verification binds immutable head, base, repository and branch identity', () => {
    const pr = { number: 42, state: 'open',
        head: { sha: identity.headSha, ref: 'topic', repo: { id: 1, full_name: 'owner/repo' } },
        base: { sha: identity.baseSha, ref: 'main', repo: { id: 1, full_name: 'owner/repo' } } };
    const expected = { ...identity, expectedBaseSha: identity.baseSha, repository: 'owner/repo',
        headRepository: 'owner/repo', headRef: 'topic', baseRef: 'main', sameRepo: 'true' };
    verifyPullRequest(pr, expected);
    for (const change of [
        value => { value.head.sha = 'c'.repeat(40); },
        value => { value.head.ref = 'other'; },
        value => { value.base.repo.full_name = 'other/repo'; },
        value => { value.head.repo.id = 2; },
        value => { delete value.head.repo.id; },
        value => { value.state = 'closed'; },
    ]) {
        const value = structuredClone(pr);
        change(value);
        assert.throws(() => verifyPullRequest(value, expected), /immutable/);
    }
});

function fixture(t) {
    const previous = process.cwd();
    const root = fs.mkdtempSync(path.join(previous, '.performance-commits-'));
    process.chdir(root);
    t.after(() => { process.chdir(previous); fs.rmSync(root, { recursive: true, force: true }); });
    const git = args => execFileSync('git', args, { encoding: 'utf8', timeout: 15000 }).trim();
    git(['init', '--quiet', '--initial-branch=topic']);
    const file = 'tools/wta/src/lib.rs';
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const suffix = '\n#[cfg(test)]\nmod tests { #[test] fn unchanged() { assert!(true); } }\n';
    const write = source => fs.writeFileSync(file, source + suffix);
    const commit = subject => {
        git(['add', '--', file]);
        git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', subject]);
        return git(['rev-parse', 'HEAD']);
    };
    write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 1 }\n');
    const baseSha = commit('baseline');
    write('pub fn first() -> usize { 2 }\npub fn second() -> usize { 2 }\n');
    const headSha = commit('reviewed change');
    return { root, git, write, commit, file, suffix, expected: { prNumber: 42, baseSha, headSha } };
}

test('two HIGH findings produce two preserved commit IDs without building or testing', t => {
    const f = fixture(t);
    const scope = prepareScope(f.expected, path.join(f.root, 'inputs'));
    assert.equal(scope.candidates.length, 1);
    fs.rmSync(path.join(f.root, 'inputs'), { recursive: true });
    const branch = 'performance-proposal-42';
    f.git(['checkout', '--quiet', '-b', branch]);
    f.write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 2 }\n');
    const first = f.commit('Propose HIGH PERF-FIRST: restore bounded first operation');
    f.write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 1 }\n');
    const second = f.commit('Propose HIGH PERF-SECOND: restore bounded second operation');
    fs.writeFileSync('.performance-summary.md', `| HIGH | [first](/commit/${first}) |\n| HIGH | [second](/commit/${second}) |\n`);
    assert.equal(verifyFixCommits(f.expected), second);
    const bundle = path.join(f.root, '.git', 'proposal.bundle');
    f.git(['bundle', 'create', bundle, `${f.expected.headSha}..${branch}`]);
    assert.equal(verifyProposalTransport(f.expected, { items: [{
        type: 'push_to_pull_request_branch', branch, base_commit: f.expected.headSha,
    }] }, () => ({ bundlePath: bundle }), 'owner/repo'), second);
    const receiver = path.join(f.root, 'receiver');
    fs.mkdirSync(receiver);
    const receive = args => execFileSync('git', ['-C', receiver, ...args], { encoding: 'utf8', timeout: 15000 }).trim();
    receive(['init', '--quiet']);
    receive(['fetch', '--quiet', f.root, f.expected.headSha]);
    receive(['fetch', '--quiet', bundle, `${branch}:topic`]);
    assert.equal(receive(['rev-parse', 'topic']), second);
    assert.deepEqual(receive(['rev-list', '--reverse', `${f.expected.headSha}..topic`]).split('\n'), [first, second]);
});

test('no-fix handoff leaves the reviewed head unchanged', t => {
    const f = fixture(t);
    assert.equal(verifyFixCommits(f.expected), f.expected.headSha);
});

function capturedProposal(f) {
    const branch = 'performance-proposal-42';
    f.git(['checkout', '--quiet', '-b', branch]);
    f.write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 2 }\n');
    const tip = f.commit('Propose HIGH PERF-FIRST: bounded source correction');
    const bundle = path.join(f.root, '.git', 'proposal.bundle');
    f.git(['bundle', 'create', bundle, `${f.expected.headSha}..${branch}`]);
    const output = { items: [{ type: 'push_to_pull_request_branch', branch, base_commit: f.expected.headSha }] };
    const verify = () => verifyProposalTransport(f.expected, output, () => ({ bundlePath: bundle }), 'owner/repo');
    return { branch, tip, bundle, output, verify };
}

test('pinned native collector and resolver preserve a validated capture through publication',
    { skip: !process.env.GH_AW_NATIVE_ACTIONS_DIR }, async t => {
        const f = fixture(t);
        const p = capturedProposal(f);
        const directory = path.join(f.root, '.git', 'native');
        fs.mkdirSync(directory);
        const require = createRequire(path.join(path.resolve(process.env.GH_AW_NATIVE_ACTIONS_DIR), 'collect_ndjson_output.cjs'));
        const constants = require('./constants.cjs');
        t.mock.property(constants, 'TMP_GH_AW_PATH', directory);
        const exists = fs.existsSync;
        const local = value => typeof value === 'string' ? value.replace('/tmp/gh-aw/', `${directory}${path.sep}`) : value;
        t.mock.method(fs, 'existsSync', value => exists(local(value)));
        fs.copyFileSync(p.bundle, path.join(directory, `aw-${p.branch}.bundle`));
        const queue = JSON.stringify({ ...p.output.items[0],
            bundle: { path: 'agent-selected.bundle' }, patch: { path: 'agent-selected.patch' },
            bundle_path: 'agent-selected.bundle', patch_path: 'agent-selected.patch' });
        const queueFile = path.join(directory, 'outputs.jsonl');
        const configFile = path.join(directory, 'config.json');
        fs.writeFileSync(queueFile, queue);
        fs.writeFileSync(configFile, JSON.stringify({ push_to_pull_request_branch: { max: 1 } }));
        const environment = { ...process.env };
        const globals = { core: globalThis.core, context: globalThis.context, github: globalThis.github };
        t.after(() => {
            process.env = environment;
            Object.assign(globalThis, globals);
        });
        process.env.GH_AW_SAFE_OUTPUTS = queueFile;
        process.env.GH_AW_SAFE_OUTPUTS_CONFIG_PATH = configFile;
        process.env.GH_AW_VALIDATION_CONFIG_PATH = path.join(directory, 'absent.json');
        process.env.RUNNER_TEMP = directory;
        delete process.env.GH_AW_VALIDATION_CONFIG;
        globalThis.core = { info() {}, warning() {}, error() {}, setOutput() {}, exportVariable() {},
            setFailed(message) { throw new Error(message); } };
        globalThis.context = { repo: { owner: 'owner', repo: 'repo' }, payload: {} };
        globalThis.github = {};
        await require('./collect_ndjson_output.cjs').main();
        const collected = JSON.parse(fs.readFileSync(path.join(directory, 'agent_output.json'), 'utf8'));
        assert.deepEqual(collected.items, [{ type: 'push_to_pull_request_branch' }]);
        const { resolveTransportPaths } = require('./resolve_transport_paths.cjs');
        const resolve = (item, repo) => {
            const result = resolveTransportPaths(item, repo);
            return { ...result, bundlePath: local(result.bundlePath) };
        };
        assert.equal(verifyProposalTransport(f.expected, collected, resolve, 'owner/repo'), p.tip);
        assert.equal(collected.items[0].branch, p.branch);
        assert.equal(resolve(collected.items[0], 'owner/repo').bundlePath,
            path.join(directory, `aw-${p.branch}.bundle`));
        collected.items[0].branch = 'alternate';
        assert.throws(() => verifyProposalTransport(f.expected, collected, resolve, 'owner/repo'), /proposal branch/);
    });

test('captured mixed-language proposal preserves only new WTA fix commit IDs', t => {
    const f = fixture(t);
    for (const file of ['src/worker.cpp', 'tools/wta/Cargo.toml', 'doc/design.md']) {
        fs.mkdirSync(path.dirname(file), { recursive: true });
        fs.writeFileSync(file, 'PR author change\n');
        f.git(['add', file]);
    }
    f.expected.headSha = f.commit('mixed authored changes');
    const p = capturedProposal(f);
    assert.equal(p.verify(), p.tip);
    assert.deepEqual(f.git(['rev-list', `${f.expected.headSha}..refs/performance/validated-proposal`]).split('\n'), [p.tip]);
    assert.equal(f.git(['show', `${p.tip}:src/worker.cpp`]), 'PR author change');
});

test('native queue rejects alternate source branch and changed checkout after capture', t => {
    const f = fixture(t);
    const p = capturedProposal(f);
    p.output.items[0].branch = 'alternate';
    assert.throws(p.verify, /proposal branch/);
    p.output.items[0].branch = p.branch;
    f.git(['checkout', '--quiet', 'topic']);
    assert.throws(p.verify, /bundle tip/);
});

test('captured bundle cannot hide out-of-candidate commits behind a clean checkout', t => {
    const f = fixture(t);
    const p = capturedProposal(f);
    fs.writeFileSync('README.md', 'unexpected');
    f.git(['add', 'README.md']);
    const invalid = f.commit('Propose HIGH PERF-OTHER: unrelated change');
    fs.rmSync(p.bundle);
    f.git(['bundle', 'create', p.bundle, `${f.expected.headSha}..${p.branch}`]);
    assert.throws(p.verify, /original candidate/);
    f.git(['checkout', '--quiet', '--detach', p.tip]);
    assert.throws(p.verify, /bundle tip/);
    assert.notEqual(invalid, p.tip);
});

test('native capture fails closed for missing, malformed, mismatched and empty metadata', t => {
    const f = fixture(t);
    const p = capturedProposal(f);
    p.output.items[0].repo = 'other/repo';
    assert.throws(p.verify, /repository/);
    delete p.output.items[0].repo;
    fs.writeFileSync(p.bundle, 'not a git bundle');
    assert.throws(p.verify);
    fs.rmSync(p.bundle);
    assert.throws(p.verify, /captured bundle|ENOENT/);
    p.output.items = [];
    assert.throws(p.verify, /exactly one/);
    p.output.items = [{ type: 'noop' }];
    assert.throws(p.verify, /discard proposal/);
});

test('scope checking does not freeze production after an interleaved test-only helper', t => {
    const f = fixture(t);
    const prefix = '// tools/wta/src/master/mod.rs\n#[cfg(test)]\nfn helper() {}\n';
    f.write(prefix + 'pub fn first() -> usize { 2 }\npub fn second() -> usize { 2 }\n');
    f.expected.headSha = f.commit('reviewed production attributes');
    f.write(prefix + 'pub fn first() -> usize { 1 }\npub fn second() -> usize { 2 }\n');
    const proposal = f.commit('Propose HIGH PERF-FIRST: correct production code before tests');
    assert.equal(verifyFixCommits(f.expected), proposal);
});

test('framework-restored instruction overlays do not require an extra source commit', t => {
    const f = fixture(t);
    fs.mkdirSync('.github/agents', { recursive: true });
    fs.writeFileSync('.github/agents/current.agent.md', 'Trusted caller role.');
    fs.mkdirSync('.agents', { recursive: true });
    fs.writeFileSync('.agents/current.md', 'Trusted caller instructions.');
    fs.writeFileSync('AGENTS.md', 'Trusted framework restoration.');
    assert.equal(verifyFixCommits(f.expected), f.expected.headSha);
});

test('a one-line proposal in a large existing module remains eligible', t => {
    const f = fixture(t);
    const prefix = '// retained production source\n'.repeat(14000);
    f.write(prefix + 'pub fn first() -> usize { 2 }\npub fn second() -> usize { 2 }\n');
    f.expected.headSha = f.commit('large reviewed module');
    f.write(prefix + 'pub fn first() -> usize { 1 }\npub fn second() -> usize { 2 }\n');
    const proposal = f.commit('Propose HIGH PERF-FIRST: one-line correction in large module');
    assert.equal(verifyFixCommits(f.expected), proposal);
});

test('commit inspection does not execute agent-configured diff or filesystem-monitor commands', t => {
    const f = fixture(t);
    const script = path.join(f.root, '.git', 'monitor.cjs');
    const sentinel = path.join(f.root, '.git', 'executed');
    fs.writeFileSync(script, `require('fs').writeFileSync(${JSON.stringify(sentinel)}, 'unexpected');`);
    f.git(['config', 'core.fsmonitor', `node "${script}"`]);
    f.git(['config', 'diff.external', `node "${script}"`]);
    assert.equal(verifyFixCommits(f.expected), f.expected.headSha);
    assert.equal(fs.existsSync(sentinel), false);
});

test('proposal rejects missing or repeated HIGH identifiers', t => {
    const f = fixture(t);
    f.write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 2 }\n');
    f.commit('Fix unrelated cleanup');
    assert.throws(() => verifyFixCommits(f.expected), /distinct HIGH/);
    f.git(['reset', '--soft', f.expected.headSha]);
    f.commit('Propose HIGH PERF-FIRST: first correction');
    f.write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 1 }\n');
    f.commit('Propose HIGH PERF-FIRST: another finding cannot reuse the ID');
    assert.throws(() => verifyFixCommits(f.expected), /distinct HIGH/);
});

test('proposal rejects unrelated files and uncommitted source changes', t => {
    const f = fixture(t);
    fs.writeFileSync('README.md', 'Unexpected edit');
    f.git(['add', 'README.md']);
    f.git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'Propose HIGH PERF-OTHER: unrelated']);
    assert.throws(() => verifyFixCommits(f.expected), /original candidate/);
    f.git(['reset', '--soft', f.expected.headSha]);
    f.git(['restore', '--staged', 'README.md']);
    fs.rmSync('README.md');
    f.write('pub fn first() -> usize { 1 }\npub fn second() -> usize { 2 }\n');
    assert.throws(() => verifyFixCommits(f.expected), /outside proposal commits/);
});

test('summary transport preserves agent Markdown and bounds UTF-8 and escaped size', t => {
    const root = fs.mkdtempSync(path.join(process.cwd(), '.performance-summary-'));
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const file = path.join(root, '.performance-summary.md');
    const summary = '| Severity | Commit |\n| HIGH | [abc](/commit/abc) |\n\nProposed fixes committed without build/test.\n';
    fs.writeFileSync(file, summary);
    assert.equal(readReviewSummary(root), summary);
    fs.writeFileSync(file, '@'.repeat(24 * 1024));
    assert.equal(readReviewSummary(root).length, 24 * 1024);
    fs.writeFileSync(file, '@'.repeat(24 * 1024 + 1));
    assert.throws(() => readReviewSummary(root), /mention escaping/);
    fs.writeFileSync(file, Buffer.from([0xff]));
    assert.throws(() => readReviewSummary(root), /encoded data/);
    fs.writeFileSync(file, ' \t\r\n');
    assert.throws(() => readReviewSummary(root), /non-whitespace/);
});

test('canceling proposal commits cannot hide an oversized published rewrite', t => {
    const f = fixture(t);
    const source = 'pub fn first() -> usize { 2 }\npub fn second() -> usize { 2 }\n';
    f.write('// temporary rewrite\n'.repeat(1000) + source);
    f.commit('Propose HIGH PERF-EXPAND: oversized first proposal');
    f.write('// retained line\n' + source);
    f.commit('Propose HIGH PERF-RESTORE: hide most of the rewrite');
    assert.throws(() => verifyFixCommits(f.expected), /reviewable-change limit/);
});

test('diff-byte budgets include separate commits even when their net content is restored', t => {
    const f = fixture(t);
    const source = 'pub fn first() -> usize { 2 }\npub fn second() -> usize { 2 }\n';
    f.write('// ' + 'x'.repeat(9000) + '\n' + source);
    f.commit('Propose HIGH PERF-EXPAND: large one-line proposal');
    f.write(source);
    f.commit('Propose HIGH PERF-RESTORE: restore content');
    assert.throws(() => verifyFixCommits(f.expected), /reviewable-change limit/);
});

test('file budgets count the union across commits, including restored files', t => {
    const f = fixture(t);
    const files = ['one.rs', 'two.rs', 'three.rs'].map(name => path.join('tools', 'wta', 'src', name));
    for (const file of files) fs.writeFileSync(file, 'pub fn value() -> usize { 1 }\n');
    f.git(['add', '--', ...files]);
    f.expected.headSha = f.commit('reviewed additional sources');
    const commit = (subject, changes) => {
        for (const [file, value] of changes) fs.writeFileSync(file, value);
        f.git(['add', '--', ...changes.map(([file]) => file)]);
        f.commit(subject);
    };
    const original = fs.readFileSync(f.file, 'utf8');
    commit('Propose HIGH PERF-FIRST: change first pair', [
        [f.file, original.replace('first() -> usize { 2 }', 'first() -> usize { 1 }')],
        [files[0], 'pub fn value() -> usize { 2 }\n'],
    ]);
    commit('Propose HIGH PERF-SECOND: restore first and change third', [
        [f.file, original], [files[1], 'pub fn value() -> usize { 2 }\n'],
    ]);
    commit('Propose HIGH PERF-THIRD: restore second and change fourth', [
        [files[0], 'pub fn value() -> usize { 1 }\n'], [files[2], 'pub fn value() -> usize { 2 }\n'],
    ]);
    assert.throws(() => verifyFixCommits(f.expected), /reviewable-change limit/);
});
