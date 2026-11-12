import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import { admitPerformanceCi, validatePerformanceArtifacts, validatePerformanceResult, verifyPerformanceCi } from '../scripts/performance-ci.mjs';
import { CI_DEFINITION_PATHS } from '../../../skills/ghaw-pr-security/scripts/security-review.mjs';

function fixture() {
    const head = 'a'.repeat(40), base = 'b'.repeat(40), trusted = 'c'.repeat(40);
    const repository = { id: 1, full_name: 'owner/repo', default_branch: 'main' };
    const run = { id: 99, run_attempt: 2, workflow_id: 3, repository, head_repository: repository,
        path: '.github/workflows/pr-ci.yml', name: 'PR CI', event: 'pull_request',
        head_sha: head, head_branch: 'topic', status: 'completed', conclusion: 'success' };
    const pr = { number: 42, state: 'open', draft: false,
        head: { sha: head, ref: 'topic', repo: repository }, base: { sha: base, ref: 'main', repo: repository } };
    const tree = { truncated: false, tree: CI_DEFINITION_PATHS.map(path =>
        ({ path, mode: '100644', type: 'blob', sha: trusted })) };
    const workflow = { id: 3, path: '.github/workflows/pr-ci.yml', state: 'active' };
    const comparison = { merge_base_commit: { sha: base } };
    const artifacts = ['ci-security-cpp', 'ci-security-rust', 'ci-performance-cpp', 'ci-performance-rust'].map((prefix, i) =>
        ({ id: i + 1, name: `${prefix}-99-2`, expired: false, workflow_run: { id: 99, head_sha: head } }));
    const environment = { GITHUB_REPOSITORY: 'owner/repo', GITHUB_REPOSITORY_ID: '1',
        CI_RUN_ID: '99', CI_RUN_ATTEMPT: '2', EXPECTED_HEAD_SHA: head, PR_NUMBER: '42',
        EXPECTED_BASE_SHA: base, HEAD_REF: 'topic', EXPECTED_WORKFLOW_SHA: trusted };
    const request = endpoint => {
        if (!endpoint) return repository;
        if (endpoint === 'actions/runs/99') return run;
        if (endpoint === 'actions/workflows/pr-ci.yml') return workflow;
        if (endpoint.startsWith('compare/')) return comparison;
        if (endpoint.startsWith('git/trees/')) return tree;
        if (endpoint === 'pulls/42') return pr;
        if (endpoint.startsWith('actions/runs/99/artifacts?')) return { artifacts };
        throw Error(`Unexpected API request: ${endpoint}`);
    };
    const context = { repo: { owner: 'owner', repo: 'repo' }, payload: { repository, workflow_run: structuredClone(run) } };
    const github = { rest: {
        actions: { async getWorkflowRun() { return { data: run }; }, async getWorkflow() { return { data: workflow }; } },
        repos: { async compareCommits() { return { data: comparison }; }, listPullRequestsAssociatedWithCommit: 'associated' },
        git: { async getTree() { return { data: tree }; } },
        pulls: { async get() { return { data: pr }; } },
    }, async paginate() { return [pr]; } };
    return { run, pr, tree, artifacts, environment, request, context, github, head, trusted };
}

test('controller admits only successful current same-repository CI; drafts cost no inference', async () => {
    const f = fixture();
    assert.equal((await admitPerformanceCi(f.github, f.context, f.trusted)).pr.number, 42);
    for (const change of [
        { conclusion: 'failure' }, { conclusion: 'cancelled' }, { status: 'in_progress' },
        { event: 'push' }, { head_sha: 'd'.repeat(40) }, { run_attempt: 3 },
        { path: '.github/workflows/other.yml' }, { head_repository: { id: 2 } },
    ]) {
        const bad = fixture();
        Object.assign(bad.run, change);
        await assert.rejects(admitPerformanceCi(bad.github, bad.context, bad.trusted), /CI run/);
    }
    f.pr.head.sha = 'd'.repeat(40);
    await assert.rejects(admitPerformanceCi(f.github, f.context, f.trusted), /exactly one/);
    const draft = fixture();
    draft.pr.draft = true;
    assert.equal(await admitPerformanceCi(draft.github, draft.context, draft.trusted), null);
});

test('worker revalidates CI and requires both matching performance artifacts', () => {
    const f = fixture();
    assert.deepEqual(verifyPerformanceCi(f).identity, { runId: '99', runAttempt: 2, headSha: f.head });
    for (const change of [
        item => { item.expired = true; }, item => { item.name = 'ci-performance-cpp-99-1'; },
        item => { item.workflow_run.head_sha = 'd'.repeat(40); },
    ]) {
        const bad = fixture();
        change(bad.artifacts[2]);
        assert.throws(() => verifyPerformanceCi(bad), /artifact/);
    }
    assert.throws(() => validatePerformanceArtifacts([...f.artifacts, f.artifacts[2]], f.run), /artifact/);
    f.run.conclusion = 'failure';
    assert.throws(() => verifyPerformanceCi(f), /CI run/);
});

test('changed CI analysis definitions and a different base require manual review', async () => {
    const f = fixture();
    const changed = structuredClone(f.tree);
    changed.tree.find(item => item.path === 'build/scripts/Invoke-CiPerformanceAnalysis.ps1').sha = 'd'.repeat(40);
    f.github.rest.git.getTree = async ({ tree_sha }) => ({ data: tree_sha === f.head ? changed : f.tree });
    await assert.rejects(admitPerformanceCi(f.github, f.context, f.trusted), /CI implementation differs/);
    const wrongBase = fixture();
    wrongBase.pr.base.ref = 'release';
    await assert.rejects(admitPerformanceCi(wrongBase.github, wrongBase.context, wrongBase.trusted), /non-default-target/);
});

test('head/run/attempt and successful tool execution bind the analyzer receipt', () => {
    const identity = { headSha: 'a'.repeat(40), runId: '99', runAttempt: 2 };
    const result = { headSha: identity.headSha, ciRunId: '99', ciRunAttempt: '2',
        language: 'rust', status: 'completed', phases: [{ exitCode: 0 }] };
    validatePerformanceResult(result, 'rust', identity);
    for (const change of [{ headSha: 'b'.repeat(40) }, { ciRunAttempt: '1' },
        { status: 'incomplete' }, { phases: [{ exitCode: 1 }] }, { phases: [] }])
        assert.throws(() => validatePerformanceResult({ ...result, ...change }, 'rust', identity));
    validatePerformanceResult({ ...result, status: 'skipped', phases: [], reason: 'Unsupported scope.' }, 'rust', identity);
});

test('workflow wiring has no direct PR inference and no native rebuild in the worker', () => {
    const controller = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-controller.yml', import.meta.url), 'utf8');
    const worker = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    const ci = fs.readFileSync(new URL('../../../workflows/pr-ci.yml', import.meta.url), 'utf8');
    assert.match(controller, /workflow_run:\s+workflows: \['PR CI'\]\s+types: \[completed\]/);
    assert.doesNotMatch(controller, /^  pull_request:/m);
    assert.match(controller, /workflow_run\.conclusion == 'success'/);
    assert.match(worker, /Verify successful CI admission/);
    assert.match(worker, /Collect verified CI performance evidence/);
    assert.match(worker, /Do not rely only on CI findings/);
    assert.doesNotMatch(worker, /\/t:ClangTidy|cargo wta-perf/);
    assert.match(ci, /Collect advisory C\+\+ performance diagnostics/);
    assert.match(ci, /Collect advisory Rust performance diagnostics/);
});
