import assert from 'node:assert/strict';
import test from 'node:test';
import { classifyChanges, prepareReview, verifySnapshot } from '../scripts/architecture-design-review.mjs';

const file = (filename, additions = 0, deletions = 0) => ({ filename, additions, deletions });
const repository = { id: 123, full_name: 'example/project' };
const expected = { number: 7, state: 'open', draft: false, changed_files: 1,
    head: { sha: 'a'.repeat(40), ref: 'feature', repo: repository },
    base: { sha: 'b'.repeat(40), ref: 'main', repo: repository } };
const context = { payload: { pull_request: expected, repository }, repo: { owner: 'example', repo: 'project' } };
const comparisonBase = 'c'.repeat(40);

function api(files = [file('src/core.cpp', 100)], states = [expected, expected], mergeBase = comparisonBase) {
    let reads = 0;
    const listFiles = () => { throw Error('Use pagination'); };
    return {
        rest: { pulls: { get: async request => {
            assert.equal(request.pull_number, expected.number);
            return { data: states[reads++] };
        }, listFiles },
        repos: { compareCommits: async request => {
            assert.equal(request.base, expected.base.sha);
            assert.equal(request.head, expected.head.sha);
            return { data: { merge_base_commit: { sha: mergeBase } } };
        } } },
        paginate: async (method, request) => {
            assert.equal(method, listFiles);
            assert.equal(request.per_page, 100);
            return files;
        },
    };
}

test('admission is inclusive OR with independent line-only and file-only boundaries', () => {
    assert.equal(classifyChanges([file('src/core.cpp', 49, 50)]).admitted, false);
    assert.equal(classifyChanges([file('src/core.cpp', 49, 51)]).admitted, true);
    assert.equal(classifyChanges(Array.from({ length: 4 }, (_, i) => file(`src/${i}.cpp`))).admitted, false);
    assert.equal(classifyChanges(Array.from({ length: 5 }, (_, i) => file(`src/${i}.cpp`))).admitted, true);
    assert.equal(classifyChanges([]).admitted, false);
});

test('count exclusions preserve declarative changes and renames across exclusion boundaries', () => {
    const ignored = ['dep/library.cpp', 'oss/library.cpp', 'vendor/library.cpp', 'node_modules/p/index.js',
        'Generated Files/report.md', 'tools/wta/Cargo.lock', 'nested/package-lock.json',
        'yarn.lock', 'pnpm-lock.yaml', 'packages.lock.json', '.github/workflows/review.lock.yml'];
    assert.equal(classifyChanges(ignored.map(name => file(name, 1000))).admitted, false);
    for (const name of ['.github/workflows/review.md', '.github/agents/reviewer.agent.md',
        '.github/skills/review/SKILL.md', 'AGENTS.md', 'docs/design.md', 'src/generated/contract.h']) {
        assert.equal(classifyChanges([file(name, 100)]).admitted, true, name);
    }
    assert.equal(classifyChanges([{ ...file('vendor/new.cpp', 100), previous_filename: 'src/old.cpp' }]).admitted, true);
    assert.equal(classifyChanges([{ ...file('vendor/new.cpp', 100), previous_filename: 'vendor/old.cpp' }]).admitted, false);
    const counted = classifyChanges([file('src/renamed.cpp'), file('src/binary.bin'), file('src/deleted.cpp', 0, 100)]);
    assert.equal(counted.relevantFiles, 3);
    assert.equal(counted.relevantLines, 100);
});

test('invalid or duplicate API counts fail instead of admitting or declaring clean', () => {
    for (const files of [[file('src/x', -1)], [file('src/x', 1.5)], [file('src/x', NaN)],
        [file('src/x'), file('src/x')], [file('src/x', Number.MAX_SAFE_INTEGER, 1)]]) {
        assert.throws(() => classifyChanges(files));
    }
});

test('live identity rejects changed repository, refs, revisions, PR state and draft', () => {
    verifySnapshot(expected, expected, repository);
    for (const mutate of [
        pr => { pr.head.sha = 'd'.repeat(40); }, pr => { pr.base.sha = 'd'.repeat(40); },
        pr => { pr.number++; }, pr => { pr.state = 'closed'; }, pr => { pr.draft = true; },
        pr => { pr.head.ref = 'other'; }, pr => { pr.base.ref = 'other'; },
        pr => { pr.head.repo.id++; }, pr => { pr.base.repo.full_name = 'other/project'; },
    ]) {
        const pr = structuredClone(expected);
        mutate(pr);
        assert.throws(() => verifySnapshot(pr, expected, repository));
    }
    assert.throws(() => verifySnapshot(expected, { ...expected, number: -1 }, repository));
    assert.throws(() => verifySnapshot(expected, { ...expected, head: { ...expected.head, sha: 'main' } }, repository));
});

test('preparation paginates API metadata and labels the actual comparison merge base', async () => {
    const files = Array.from({ length: 101 }, (_, i) => file(`src/${i}.cpp`, 1));
    const pr = { ...expected, changed_files: files.length };
    assert.deepEqual(await prepareReview(api(files, [pr, pr]), context), {
        comparisonBase, relevantLines: 101, relevantFiles: 101, admitted: true });
    assert.equal((await prepareReview(api([file('src/small.cpp', 99)]), context)).admitted, false);
});

test('incomplete API results, limits, failures and drift prevent review preparation', async () => {
    await assert.rejects(prepareReview(api([], [expected]), context), /Incomplete/);
    await assert.rejects(prepareReview(api([], [{ ...expected, changed_files: 3001 }]), context), /3,000/);
    await assert.rejects(prepareReview(api([], [{ ...expected, state: 'closed' }]), context), /stale/);
    await assert.rejects(prepareReview(api(undefined, [expected, { ...expected, draft: true }]), context), /draft/);
    await assert.rejects(prepareReview(api(undefined, undefined, 'main'), context), /merge base/);
    const failed = api();
    failed.paginate = async () => { throw Error('API rate limit'); };
    await assert.rejects(prepareReview(failed, context), /API rate limit/);
});
