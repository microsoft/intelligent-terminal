import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import test from 'node:test';
import { admitPerformanceCi, preparePrDiagnostics, scopePerformanceDiagnostics, validatePerformanceArtifacts, validatePerformanceFiles, validatePerformanceResult, verifyPerformanceCi } from '../scripts/performance-ci.mjs';
import { CI_DEFINITION_PATHS } from '../../../skills/ghaw-pr-security/scripts/security-review.mjs';

function fixture() {
    const head = 'a'.repeat(40), base = 'b'.repeat(40), trusted = 'c'.repeat(40), merge = 'd'.repeat(40);
    const repository = { id: 1, full_name: 'owner/repo', default_branch: 'main' };
    const run = { id: 99, run_attempt: 2, workflow_id: 3, repository, head_repository: repository,
        path: '.github/workflows/pr-ci.yml', name: 'PR CI', event: 'pull_request',
        head_sha: head, head_branch: 'topic', status: 'completed', conclusion: 'success' };
    run.referenced_workflows = [{ path: 'owner/repo/.github/workflows/pr-ci-native.yml@refs/pull/42/merge', sha: merge }];
    const commit = { sha: merge, parents: [{ sha: base }, { sha: head }] };
    const jobs = ['native / performance-scope', 'native / C++ build and unit tests', 'native / Rust build and unit tests']
        .map(name => ({ name, run_id: 99, run_attempt: 2, status: 'completed', conclusion: 'success',
            steps: [{ name: 'Confirm default-target CI origin', status: 'completed', conclusion: 'success' }] }));
    const pr = { number: 42, state: 'open', draft: false,
        head: { sha: head, ref: 'topic', repo: repository }, base: { sha: base, ref: 'main', repo: repository } };
    const tree = { truncated: false, tree: CI_DEFINITION_PATHS.map(path =>
        ({ path, mode: '100644', type: 'blob', sha: trusted })) };
    const workflow = { id: 3, path: '.github/workflows/pr-ci.yml', state: 'active' };
    const comparison = { merge_base_commit: { sha: base }, status: 'ahead' };
    const artifacts = ['ci-security-cpp', 'ci-security-rust', 'ci-performance-cpp', 'ci-performance-rust'].map((prefix, i) =>
        ({ id: i + 1, name: `${prefix}-99-2`, expired: false, workflow_run: { id: 99, head_sha: head } }));
    const environment = { GITHUB_REPOSITORY: 'owner/repo', GITHUB_REPOSITORY_ID: '1',
        CI_RUN_ID: '99', CI_RUN_ATTEMPT: '2', EXPECTED_HEAD_SHA: head, PR_NUMBER: '42',
        EXPECTED_BASE_SHA: base, HEAD_REF: 'topic', EXPECTED_WORKFLOW_SHA: trusted, COMPARISON_BASE_SHA: base };
    const request = endpoint => {
        if (!endpoint) return repository;
        if (endpoint === 'actions/runs/99') return run;
        if (endpoint === 'actions/workflows/pr-ci.yml') return workflow;
        if (endpoint.startsWith('compare/')) return comparison;
        if (endpoint.startsWith('git/trees/')) return tree;
        if (endpoint === `git/commits/${merge}`) return commit;
        if (endpoint.startsWith('actions/runs/99/jobs?')) return { jobs };
        if (endpoint === 'pulls/42') return pr;
        if (endpoint.startsWith('actions/runs/99/artifacts?')) return { artifacts };
        throw Error(`Unexpected API request: ${endpoint}`);
    };
    const context = { repo: { owner: 'owner', repo: 'repo' }, payload: { repository, workflow_run: structuredClone(run) } };
    const github = { rest: {
        actions: { async getWorkflowRun() { return { data: run }; }, async getWorkflow() { return { data: workflow }; },
            listJobsForWorkflowRun: 'jobs' },
        repos: { async compareCommits() { return { data: comparison }; }, listPullRequestsAssociatedWithCommit: 'associated' },
        git: { async getTree() { return { data: tree }; }, async getCommit() { return { data: commit }; } },
        pulls: { async get() { return { data: pr }; } },
    }, async paginate(method) { return method === 'jobs' ? jobs : [pr]; } };
    return { run, pr, tree, artifacts, environment, request, context, github, head, trusted, merge, jobs, commit, comparison };
}

function rustWarning(file, line, code = 'clippy::needless_collect') {
    return JSON.stringify({ reason: 'compiler-message', message: { level: 'warning',
        code: { code }, message: 'Candidate warning', spans: [{ file_name: file,
            line_start: line, line_end: line, column_start: 3, is_primary: true }] } });
}

test('hundreds of unchanged warnings never enter the default PR diagnostic input', () => {
    const changed = [{ path: 'tools/wta/src/app.rs', hunks: [{ headStart: 10, headCount: 2 }] }];
    const text = [...Array.from({ length: 400 }, (_, i) => rustWarning('src\\app.rs', 100 + i)),
        rustWarning('src\\app.rs', 10), rustWarning('src\\app.rs', 10),
        rustWarning('src\\app.rs', 10, 'dead_code')].join('\n');
    const scoped = scopePerformanceDiagnostics(text, 'rust', changed);
    assert.equal(scoped.candidateOccurrences, 402);
    assert.equal(scoped.outsideChangedLines, 400);
    assert.equal(scoped.duplicates, 1);
    assert.equal(scoped.diagnostics.length, 1);
    assert.equal(scoped.diagnostics[0].file, 'tools/wta/src/app.rs');
    assert.equal(scoped.diagnostics[0].line, 10);
});

test('native locations are scoped by immutable added lines, not merely touched files', () => {
    const changed = [{ path: 'src/renderer/sample.cpp', hunks: [
        { headStart: 20, headCount: 2 }, { headStart: 40, headCount: 0 },
    ] }];
    const cpp = [19, 20, 40].map(line =>
        ` D:\\a\\repo\\src\\renderer\\sample.cpp:${line}:3: warning: copy [performance-for-range-copy]`).join('\n');
    const scoped = scopePerformanceDiagnostics(cpp, 'cpp', changed);
    assert.deepEqual(scoped.diagnostics.map(item => item.line), [20]);
    assert.equal(scoped.outsideChangedLines, 2);
    const rust = scopePerformanceDiagnostics(rustWarning('D:\\a\\repo\\tools\\wta\\src\\app.rs', 5), 'rust',
        [{ path: 'tools/wta/src/app.rs', hunks: [{ headStart: 5, headCount: 1 }] }]);
    assert.equal(rust.diagnostics.length, 1);
    assert.equal(scopePerformanceDiagnostics(rustWarning('src/app.rs', 5), 'rust', []).diagnostics.length, 0);
    assert.equal(scopePerformanceDiagnostics(rustWarning('../outside.rs', 5), 'rust', []).unlocated, 1);
    assert.equal(scopePerformanceDiagnostics(rustWarning('src/helpers/../app.rs', 5), 'rust',
        [{ path: 'tools/wta/src/app.rs', hunks: [{ headStart: 5, headCount: 1 }] }]).diagnostics.length, 1);
    const expanded = JSON.parse(rustWarning('/rustc/library/macros.rs', 30));
    expanded.message.spans[0].expansion = { span: { file_name: 'src/app.rs', line_start: 5,
        line_end: 6, column_start: 1 } };
    assert.equal(scopePerformanceDiagnostics(JSON.stringify(expanded), 'rust',
        [{ path: 'tools/wta/src/app.rs', hunks: [{ headStart: 6, headCount: 1 }] }]).diagnostics.length, 1);
    assert.equal(scopePerformanceDiagnostics('warning: unknown format [performance-for-range-copy]', 'cpp', []).unlocated, 1);
    assert.throws(() => scopePerformanceDiagnostics('{"broken":', 'rust', []), SyntaxError);
});

test('prepared PR evidence uses real immutable Git hunks and retains raw logs unchanged', t => {
    const parent = path.join(process.cwd(), 'Generated Files');
    fs.mkdirSync(parent, { recursive: true });
    const root = fs.mkdtempSync(path.join(parent, 'pr-diagnostics-'));
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
    const source = path.join(root, 'tools', 'wta', 'src', 'app.rs');
    fs.mkdirSync(path.dirname(source), { recursive: true });
    git(['init', '--quiet']);
    const commit = () => {
        git(['add', '--all']);
        git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'fixture']);
        return git(['rev-parse', 'HEAD']);
    };
    fs.writeFileSync(source, 'old\nunchanged\n');
    const baseSha = commit();
    fs.writeFileSync(source, 'new\nunchanged\n');
    const headSha = commit();
    const output = path.join(root, '.git', 'evidence');
    for (const language of ['cpp', 'rust']) fs.mkdirSync(path.join(output, language), { recursive: true });
    const raw = [rustWarning('src/app.rs', 1), rustWarning('src/app.rs', 2)].join('\n');
    fs.writeFileSync(path.join(output, 'rust', 'diagnostics.jsonl'), raw);
    const scoped = preparePrDiagnostics(output, { identity: { baseSha, headSha, prNumber: 42 },
        candidates: [{ filename: 'tools/wta/src/app.rs' }] }, root);
    assert.deepEqual(scoped.languages.rust.diagnostics.map(item => item.line), [1]);
    assert.equal(scoped.languages.rust.outsideChangedLines, 1);
    assert.equal(fs.readFileSync(path.join(output, 'rust', 'diagnostics.jsonl'), 'utf8'), raw);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(output, 'pr-diagnostics.json'), 'utf8')), scoped);
    const moved = path.join(path.dirname(source), 'moved.rs');
    fs.renameSync(source, moved);
    const renamedHead = commit();
    fs.writeFileSync(path.join(output, 'rust', 'diagnostics.jsonl'), rustWarning('src/moved.rs', 1));
    const renamed = preparePrDiagnostics(output, { identity: { baseSha: headSha, headSha: renamedHead, prNumber: 42 },
        candidates: [{ filename: 'tools/wta/src/moved.rs' }] }, root);
    assert.equal(renamed.languages.rust.diagnostics.length, 0, 'pure renames do not reintroduce the old diagnostic backlog');
});

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
    assert.deepEqual(verifyPerformanceCi(f).identity, { runId: '99', runAttempt: 2,
        headSha: f.head, comparisonBaseSha: f.environment.COMPARISON_BASE_SHA,
        workflowSha: f.merge, originalBaseSha: f.environment.EXPECTED_BASE_SHA });
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
    const identity = { headSha: 'a'.repeat(40), runId: '99', runAttempt: 2, comparisonBaseSha: 'b'.repeat(40) };
    const result = { headSha: identity.headSha, ciRunId: '99', ciRunAttempt: '2',
        comparisonBaseSha: identity.comparisonBaseSha, language: 'rust', status: 'completed', phases: [{ exitCode: 0 }] };
    validatePerformanceResult(result, 'rust', identity);
    for (const change of [{ headSha: 'b'.repeat(40) }, { ciRunAttempt: '1' },
        { comparisonBaseSha: 'c'.repeat(40) }, { status: 'incomplete' }, { phases: [{ exitCode: 1 }] }, { phases: [] }])
        assert.throws(() => validatePerformanceResult({ ...result, ...change }, 'rust', identity));
    validatePerformanceResult({ ...result, status: 'skipped', phases: [], reason: 'Unsupported scope.' }, 'rust', identity);
});

test('workflow wiring has no direct PR inference and no native rebuild in the worker', () => {
    const controller = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-controller.yml', import.meta.url), 'utf8');
    const worker = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    const entry = fs.readFileSync(new URL('../../../workflows/pr-ci.yml', import.meta.url), 'utf8');
    const ci = fs.readFileSync(new URL('../../../workflows/pr-ci-native.yml', import.meta.url), 'utf8');
    assert.match(entry, /uses: \.\/\.github\/workflows\/pr-ci-native\.yml/);
    assert.match(ci, /workflow_call:/);
    assert.match(controller, /workflow_run:\s+workflows: \['PR CI'\]\s+types: \[completed\]/);
    assert.doesNotMatch(controller, /^  pull_request:/m);
    assert.match(controller, /workflow_run\.conclusion == 'success'/);
    assert.match(worker, /Verify successful CI admission/);
    assert.match(worker, /Collect verified CI performance evidence/);
    assert.ok(worker.indexOf('name: Prepare immutable review inputs') <
        worker.indexOf('name: Collect verified CI performance evidence'));
    assert.match(worker, /collect \/tmp\/gh-aw\/performance-ci \/tmp\/gh-aw\/performance-scope\.json/);
    assert.match(worker, /`pr-diagnostics\.json`/);
    assert.match(worker, /Do not rely only on CI findings/);
    assert.doesNotMatch(worker, /\/t:ClangTidy|cargo wta-perf/);
    assert.match(ci, /Collect advisory C\+\+ performance diagnostics/);
    assert.match(ci, /Collect advisory Rust performance diagnostics/);
    assert.doesNotMatch(ci, /fetch-depth: 0/);
    assert.match(ci, /Resolve performance comparison base/);
    assert.equal((ci.match(/credential\.helper=!gh auth git-credential/g) ?? []).length, 2);
});

test('same-head retargeting and unverifiable workflow revisions cannot establish CI provenance', async () => {
    for (const change of [
        f => { f.run.referenced_workflows = []; },
        f => { f.commit.parents[1].sha = 'e'.repeat(40); },
        f => { f.comparison.status = 'diverged'; },
        f => { f.jobs[0].steps[0].conclusion = 'skipped'; },
        f => { f.jobs[1].conclusion = 'skipped'; },
        f => { f.jobs[2].run_attempt = 1; },
    ]) {
        const f = fixture();
        change(f);
        await assert.rejects(admitPerformanceCi(f.github, f.context, f.trusted));
        assert.throws(() => verifyPerformanceCi(f));
    }
    const substituted = fixture();
    const otherTree = structuredClone(substituted.tree);
    otherTree.tree.find(item => item.path === '.github/workflows/pr-ci-native.yml').sha = 'e'.repeat(40);
    substituted.github.rest.git.getTree = async ({ tree_sha }) => ({ data: tree_sha === substituted.merge ? otherTree : substituted.tree });
    await assert.rejects(admitPerformanceCi(substituted.github, substituted.context, substituted.trusted), /CI implementation differs/);
    assert.equal(substituted.pr.base.ref, 'main');
    assert.equal(substituted.pr.head.sha, substituted.run.head_sha);
});

test('completed artifacts require raw diagnostics and every recorded phase log', t => {
    const parent = path.join(process.cwd(), 'Generated Files');
    fs.mkdirSync(parent, { recursive: true });
    const directory = fs.mkdtempSync(path.join(parent, 'performance-evidence-'));
    t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
    const identity = { headSha: 'a'.repeat(40), comparisonBaseSha: 'b'.repeat(40), runId: '99', runAttempt: 2 };
    const result = { headSha: identity.headSha, comparisonBaseSha: identity.comparisonBaseSha,
        ciRunId: '99', ciRunAttempt: '2', language: 'rust', status: 'completed',
        phases: [{ log: 'tool-version.log', exitCode: 0 }, { log: 'diagnostics.jsonl', exitCode: 0 }] };
    const writeReceipt = () => fs.writeFileSync(path.join(directory, 'native-results.json'), JSON.stringify(result));
    writeReceipt();
    fs.writeFileSync(path.join(directory, 'tool-version.log'), 'clippy version');
    assert.throws(() => validatePerformanceFiles(directory, 'rust', identity), /phase log is missing/);
    fs.writeFileSync(path.join(directory, 'diagnostics.jsonl'), '{"reason":"build-finished","success":true}');
    validatePerformanceFiles(directory, 'rust', identity);
    result.phases[0].stderrLog = 'project-1-stderr.log';
    writeReceipt();
    assert.throws(() => validatePerformanceFiles(directory, 'rust', identity), /phase log is missing/);
    fs.writeFileSync(path.join(directory, 'project-1-stderr.log'), 'tool warning on stderr');
    validatePerformanceFiles(directory, 'rust', identity);
    result.phases.push({ log: 'project-1.json', exitCode: 0 });
    writeReceipt();
    assert.throws(() => validatePerformanceFiles(directory, 'rust', identity), /phase log is missing/);
    result.phases = [{ log: 'tool-version.log', exitCode: 0 }];
    writeReceipt();
    assert.throws(() => validatePerformanceFiles(directory, 'rust', identity), /requires diagnostic output/);
    result.language = 'cpp';
    writeReceipt();
    assert.throws(() => validatePerformanceFiles(directory, 'cpp', identity), /requires diagnostic output/);
    result.phases.push({ log: 'diagnostics-1.log', exitCode: 0 });
    fs.writeFileSync(path.join(directory, 'diagnostics-1.log'), 'C++ diagnostics');
    writeReceipt();
    validatePerformanceFiles(directory, 'cpp', identity);
});
