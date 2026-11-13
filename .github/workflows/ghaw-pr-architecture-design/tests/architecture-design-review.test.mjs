import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { classifyChanges, parseNumstat, prepareReview, resolveScope, verifySnapshot } from '../scripts/architecture-design-review.mjs';

const root = fileURLToPath(new URL('../../../../', import.meta.url));
const file = (filename, additions = 0, deletions = 0) => ({ filename, additions, deletions });
const head = 'a'.repeat(40);
const base = 'b'.repeat(40);
const repository = { id: 123, full_name: 'example/project' };
const expected = { number: 7, state: 'open', draft: false,
    head: { sha: head, ref: 'feature', repo: repository },
    base: { sha: base, ref: 'main', repo: repository } };

test('admission is inclusive OR with independent line-only and file-only boundaries', () => {
    assert.equal(classifyChanges([file('src/core.cpp', 49, 50)]).admitted, false);
    assert.equal(classifyChanges([file('src/core.cpp', 49, 51)]).admitted, true);
    assert.equal(classifyChanges(Array.from({ length: 4 }, (_, i) => file(`src/${i}.cpp`))).admitted, false);
    const scope = classifyChanges(Array.from({ length: 5 }, (_, i) => file(`src/${i}.cpp`)));
    assert.equal(scope.relevantLines, 0);
    assert.equal(scope.admitted, true);
    assert.equal(classifyChanges([]).admitted, false);
});

test('generated/vendor/lock churn is ignored without excluding declarative architecture', () => {
    const ignored = ['dep/library.cpp', 'oss/library.cpp', 'vendor/library.cpp', 'node_modules/p/index.js',
        'Generated Files/report.md', 'tools/wta/Cargo.lock', 'nested/package-lock.json',
        'yarn.lock', 'pnpm-lock.yaml', 'packages.lock.json', '.github/workflows/review.lock.yml'];
    const scope = classifyChanges(ignored.map(filename => file(filename, 1000)));
    assert.equal(scope.admitted, false);
    assert.equal(scope.relevantFiles, 0);
    assert.equal(scope.ignored.length, ignored.length);
    for (const filename of ['.github/workflows/review.md', '.github/workflows/pr-ci.yml',
        '.github/agents/reviewer.agent.md', '.github/skills/review/SKILL.md',
        'AGENTS.md', 'docs/design.md', 'settings.json', 'src/generated/contract.h', 'tools/wta/Cargo.toml']) {
        assert.equal(classifyChanges([file(filename, 100)]).admitted, true, filename);
    }
});

test('rename paths, removals and binary metadata have explicit counting semantics', () => {
    const renamed = parseNumstat('0\t0\t\0old/name.cpp\0new/name.cpp\0');
    assert.deepEqual(renamed, [{ filename: 'new/name.cpp', previousFilename: 'old/name.cpp', additions: 0, deletions: 0 }]);
    assert.equal(classifyChanges(renamed).relevantFiles, 1);
    assert.equal(classifyChanges(renamed).relevantLines, 0);
    const crossing = classifyChanges([{ ...file('vendor/name.cpp', 100), previousFilename: 'src/name.cpp' }]);
    assert.equal(crossing.admitted, true);
    assert.equal(classifyChanges([{ ...file('src/name.cpp'), previousFilename: 'vendor/name.cpp' }]).relevantFiles, 1);
    assert.equal(classifyChanges([{ ...file('vendor/new.cpp', 300), previousFilename: 'vendor/old.cpp' }]).admitted, false);
    assert.equal(classifyChanges(parseNumstat('0\t100\tsrc/deleted.cpp\0')).admitted, true);
    assert.equal(classifyChanges(parseNumstat('-\t-\tsrc/asset.bin\0')).relevantLines, 0);
});

test('malformed metadata fails closed instead of producing a clean or admitted result', () => {
    for (const text of ['1\t2\tx', 'x\t2\tx\0', '-\t2\tx\0', '1\t2\t\0old\0', '1\t2\t../x\0',
        '1\t2\tC:\\x\0', '1\t2\tfoo\nbar\0', '1\t2\tx\0\0']) {
        assert.throws(() => parseNumstat(text), undefined, JSON.stringify(text));
    }
    for (const changes of [null, [file('src/x', -1)], [file('src/x', 1.5)], [file('src/x', null, 0)],
        [file('src/x', Number.MAX_SAFE_INTEGER, 1)], [file('src/x'), file('src/x')],
        [{ filename: 'src/x', deletions: 0 }], [file('src/x', NaN)],
        [file('src/x', 9007199254740992)]]) {
        assert.throws(() => classifyChanges(changes));
    }
});

test('live snapshot validates repository, refs, revisions, PR state and draft', () => {
    verifySnapshot(expected, expected, repository);
    for (const mutate of [
        pr => { pr.head.sha = 'c'.repeat(40); }, pr => { pr.base.sha = 'd'.repeat(40); },
        pr => { pr.number++; }, pr => { pr.state = 'closed'; }, pr => { pr.draft = true; },
        pr => { delete pr.draft; }, pr => { pr.head.ref = 'other'; }, pr => { pr.base.ref = 'other'; },
        pr => { pr.head.repo.id++; }, pr => { pr.base.repo.full_name = 'other/project'; },
        pr => { pr.head.repo.full_name = 'other/project'; },
    ]) {
        const pr = structuredClone(expected);
        mutate(pr);
        assert.throws(() => verifySnapshot(pr, expected, repository));
    }
    assert.throws(() => verifySnapshot(expected, { ...expected, number: -1 }, repository));
    assert.throws(() => verifySnapshot(expected, { ...expected, head: { ...expected.head, sha: 'main' } }, repository));
});

test('invalid or stale metadata prevents fetching source or invoking review', async () => {
    let reads = 0;
    const github = { rest: { pulls: { get: async () => { reads++; return { data: { ...expected, state: 'closed' } }; } } } };
    await assert.rejects(prepareReview(github, { payload: { pull_request: expected, repository }, repo: {} },
        'not-a-checkout'), /stale/);
    assert.equal(reads, 1);
    await assert.rejects(prepareReview(github, { payload: { pull_request: { ...expected, draft: true }, repository } },
        'not-a-checkout'), /draft/);
    assert.equal(reads, 1);
});

test('real Git metadata uses merge base and trusted preparation rejects post-fetch drift', async () => {
    const directory = path.join(root, 'Generated Files', 'architecture-design-review');
    fs.mkdirSync(directory, { recursive: true });
    const fixture = fs.mkdtempSync(path.join(directory, 'git-fixture-'));
    const git = (...args) => execFileSync('git', ['-c', 'core.hooksPath=' +
        (process.platform === 'win32' ? 'NUL' : '/dev/null'), '-c', 'commit.gpgsign=false', ...args],
    { cwd: fixture, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    try {
        git('init', '-q');
        git('config', 'user.name', 'Fixture');
        git('config', 'user.email', 'fixture@example.invalid');
        fs.writeFileSync(path.join(fixture, 'deleted.cpp'), Array.from({ length: 100 }, (_, i) => `old ${i}\n`).join(''));
        fs.writeFileSync(path.join(fixture, 'old.cpp'), 'rename-only\n');
        git('add', '.');
        git('commit', '-qm', 'base');
        const comparisonBase = git('rev-parse', 'HEAD');
        git('checkout', '-qb', 'feature');
        fs.unlinkSync(path.join(fixture, 'deleted.cpp'));
        fs.renameSync(path.join(fixture, 'old.cpp'), path.join(fixture, 'new.cpp'));
        fs.writeFileSync(path.join(fixture, 'binary.bin'), Buffer.from([0, 1, 2]));
        git('add', '-A');
        git('commit', '-qm', 'feature');
        const headSha = git('rev-parse', 'HEAD');
        git('checkout', '-qb', 'advanced-base', comparisonBase);
        fs.writeFileSync(path.join(fixture, 'base-only.cpp'), 'base-only change\n');
        git('add', '.');
        git('commit', '-qm', 'advance base');
        const trustedSha = git('rev-parse', 'HEAD');
        const snapshot = { ...expected, head: { ...expected.head, sha: headSha },
            base: { ...expected.base, sha: trustedSha, ref: 'advanced-base' } };
        const scope = resolveScope(fixture, snapshot);
        assert.equal(scope.comparisonBase, comparisonBase);
        assert.equal(scope.relevantLines, 100);
        assert.equal(scope.relevantFiles, 3);
        assert.equal(scope.admitted, true);
        assert.equal(scope.relevant.find(change => change.filename === 'new.cpp').previousFilename, 'old.cpp');
        assert.equal(scope.relevant.some(change => change.filename === 'base-only.cpp'), false);
        assert.throws(() => resolveScope(fixture, { head: { sha: 'feature' }, base: { sha: comparisonBase } }));
        git('remote', 'add', 'origin', fixture);
        git('update-ref', 'refs/pull/7/head', headSha);
        const context = { payload: { pull_request: snapshot, repository }, repo: { owner: 'example', repo: 'project' } };
        const output = path.join(fixture, '.git', 'review-data');
        let reads = 0;
        const github = { rest: { pulls: { get: async () => {
            reads++;
            return { data: snapshot };
        } } } };
        const prepared = await prepareReview(github, context, fixture, output);
        assert.equal(reads, 2);
        assert.deepEqual(prepared, scope);
        assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'scope.json'), 'utf8')), scope);
        assert.equal(git('rev-parse', 'HEAD'), trustedSha, 'preparation never checks out candidate code');
        let driftReads = 0;
        const staleGithub = { rest: { pulls: { get: async () => ({ data: ++driftReads === 1 ? snapshot :
            { ...snapshot, head: { ...snapshot.head, sha: 'c'.repeat(40) } } }) } } };
        const staleOutput = path.join(fixture, '.git', 'stale-review');
        await assert.rejects(prepareReview(staleGithub, context, fixture, staleOutput), /stale/);
        assert.equal(fs.existsSync(staleOutput), false);
        assert.equal(git('rev-parse', 'HEAD'), trustedSha);
    } finally {
        fs.rmSync(fixture, { recursive: true, force: true });
    }
});
