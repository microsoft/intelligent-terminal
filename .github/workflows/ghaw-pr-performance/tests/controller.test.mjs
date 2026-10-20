import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import test from 'node:test';

const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-controller.yml', import.meta.url), 'utf8');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const extract = name => {
    const match = workflow.match(new RegExp(`name: ${name}[\\s\\S]*?script: \\|\\r?\\n((?: {12}.+(?:\\r?\\n|$)|\\s*\\r?\\n)+)`));
    assert.ok(match, `actual controller script missing: ${name}`);
    return match[1].split(/\r?\n/).map(line => line.slice(12)).join('\n');
};
const reportScript = new AsyncFunction('github', 'context', 'core', 'process', 'require', extract('Publish linked PR run report'));

async function reportRun(markdown, { currentHead = 'a'.repeat(40), recordedHead = 'a'.repeat(40),
    outcome = 'success', comments = [], failComment = false, commentSequence, headSequence } = {}) {
    const root = fs.mkdtempSync(path.join(process.cwd(), '.performance-delivery-'));
    const checks = [];
    const posted = [];
    const warnings = [];
    let initialConclusion;
    let commentRead = 0;
    let headRead = 0;
    const github = { rest: {
        checks: {
            async create(check) { initialConclusion = check.conclusion; checks.push(check); return { data: { id: 99 } }; },
            async update(check) { Object.assign(checks[0], check); },
        },
        issues: {
            listComments: 'comments',
            async createComment(value) { if (failComment) throw new Error('Comment denied'); posted.push(value); },
            async updateComment(value) { if (failComment) throw new Error('Comment denied'); posted.push(value); },
        },
        pulls: { async get() {
            return { data: { head: { sha: headSequence?.[Math.min(headRead++, headSequence.length - 1)] ?? currentHead } } };
        } },
    }, async paginate() { return commentSequence?.[Math.min(commentRead++, commentSequence.length - 1)] ?? comments; } };
    const nativeRequire = createRequire(import.meta.url);
    const require = name => name === 'child_process' ? {
        execFileSync(command, args) {
            assert.equal(command, 'gh');
            assert.equal(args[2], '101');
            assert.equal(args[args.indexOf('--name') + 1], `performance-summary-42-${'a'.repeat(40)}`);
            if (markdown === undefined) throw new Error('Summary unavailable');
            const destination = args[args.indexOf('--dir') + 1];
            fs.writeFileSync(path.join(destination, '.performance-summary.md'), markdown);
            fs.writeFileSync(path.join(destination, 'review-head.txt'), recordedHead);
        },
    } : name === 'timers/promises' ? { async setTimeout(delay) { assert.equal(delay, 2000); } } : nativeRequire(name);
    const core = { warning(message) { warnings.push(message); },
        summary: { addRaw() { return this; }, async write() {} } };
    let error;
    try {
        await reportScript(github, { repo: { owner: 'owner', repo: 'repo' }, runId: 100 }, core, {
            env: { GITHUB_RUN_ATTEMPT: '1', GITHUB_WORKSPACE: process.cwd(), RUNNER_TEMP: root,
                PR_NUMBER: '42', WORKER_RUN_ID: '101', RUN_OUTCOME: outcome,
                REVIEWED_HEAD_SHA: 'a'.repeat(40),
                CONTROLLER_RUN_URL: 'https://github.com/owner/repo/actions/runs/100',
                WORKER_RUN_URL: 'https://github.com/owner/repo/actions/runs/101', WORKER_CONCLUSION: outcome },
        }, require);
    } catch (caught) { error = caught; }
    finally { fs.rmSync(root, { recursive: true, force: true }); }
    return { check: checks[0], checks, posted, warnings, initialConclusion, error, headRead };
}

test('controller delivers the agent table, separate commit links and one shared notice unchanged', async () => {
    const markdown = '| Severity | Commit |\n| HIGH | [first](/commit/111) |\n| HIGH | [second](/commit/222) |\n\n' +
        'Proposed fixes committed without build/test. The original reviewer owns validation and may revert the fix commit.\n';
    const result = await reportRun(markdown, { recordedHead: 'b'.repeat(40), currentHead: 'b'.repeat(40) });
    assert.equal(result.error, undefined);
    assert.equal(result.initialConclusion, 'action_required');
    assert.equal(result.check.conclusion, 'success');
    assert.equal(result.posted.length, 1);
    assert.ok(result.posted[0].body.endsWith(markdown));
    assert.equal(result.posted[0].body.match(/without build\/test/g).length, 1);
    assert.doesNotMatch(result.posted[0].body, /Validated repair|review result: \*\*fixed/);
    assert.match(result.posted[0].body, /workflow execution, not build\/test validation/);
    assert.equal(result.check.head_sha, 'b'.repeat(40));
});

test('failed comment delivery never leaves green and report transport suppresses mentions', async () => {
    const denied = await reportRun('Agent findings.\n', { failComment: true });
    assert.match(denied.error.message, /Comment denied/);
    assert.equal(denied.check.conclusion, 'action_required');
    const safe = await reportRun('Mention @copilot in source.\n');
    assert.match(safe.posted[0].body, /@\u200bcopilot/);
    assert.doesNotMatch(safe.posted[0].body, /@copilot/);
    assert.ok(safe.check.output.summary.endsWith('Mention @copilot in source.\n'));
});

test('missing, invalid UTF-8 or malformed report head cannot produce green', async () => {
    for (const [markdown, options] of [[undefined, {}], [' \t\r\n', {}], [Buffer.from([0xff]), {}],
        ['Summary\n', { recordedHead: 'invalid' }]]) {
        const result = await reportRun(markdown, options);
        assert.equal(result.check.conclusion, 'action_required');
        assert.equal(result.warnings.length, 1);
        assert.match(result.error.message, /delivery requires action/);
    }
});

test('newer PR head is not overwritten; a known pushed head may propagate briefly', async () => {
    const stale = await reportRun('Older findings.\n', { currentHead: 'c'.repeat(40) });
    assert.equal(stale.posted.length, 0);
    assert.equal(stale.check.conclusion, 'action_required');
    assert.match(stale.error.message, /delivery requires action/);
    const converging = await reportRun('Pushed proposal.\n', {
        recordedHead: 'b'.repeat(40), headSequence: ['a'.repeat(40), 'b'.repeat(40)],
    });
    assert.equal(converging.posted.length, 1);
    assert.equal(converging.headRead, 2);
});

test('older same-head runs cannot replace a newer report, including a final-read race', async () => {
    const newer = { id: 77, user: { login: 'github-actions[bot]' },
        body: '<!-- ghaw-pr-performance-report -->\n<!-- ghaw-pr-performance-run:101:1 -->' };
    const early = await reportRun('Old result.\n', { comments: [newer] });
    assert.equal(early.checks.length, 0);
    assert.equal(early.posted.length, 0);
    const late = await reportRun('Old result.\n', { commentSequence: [[], [newer]] });
    assert.equal(late.check.conclusion, 'cancelled');
    assert.equal(late.posted.length, 0);
});

test('duplicate marked comments select the greatest run and attempt, not the first comment', async () => {
    const comment = (id, run, attempt) => ({ id, user: { login: 'github-actions[bot]' },
        body: `<!-- ghaw-pr-performance-report -->\n<!-- ghaw-pr-performance-run:${run}:${attempt} -->` });
    const future = await reportRun('Old result.\n', { comments: [comment(1, 99, 1), comment(2, 101, 1)] });
    assert.equal(future.checks.length, 0);
    assert.equal(future.posted.length, 0);
    const latest = await reportRun('Current result.\n', { comments: [comment(3, 99, 2), comment(4, 99, 3), comment(5, 98, 9)] });
    assert.equal(latest.posted[0].comment_id, 4);
    const injected = await reportRun('Current result.\n', {
        comments: [{ id: 6, user: { login: 'github-actions[bot]' },
            body: '<!-- ghaw-pr-performance-report -->\nLegacy summary mentions <!-- ghaw-pr-performance-run:999:1 -->' }],
    });
    assert.equal(injected.posted.length, 1, 'only caller metadata directly after the marker establishes ordering');
});

test('worker/controller source contains no build/test, proposal renderer or native publication gate', () => {
    const worker = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    assert.doesNotMatch(worker, /validate-performance-(original|focused|repair)|performance_analysis:|cargo (test|build|fmt)|\/t:Build/);
    assert.match(worker, /signed-commits: false/);
    assert.match(worker, /patch-format: bundle/);
    assert.doesNotMatch(worker, /max-patch-size:/, 'use the native default instead of rejecting small edits in large source files');
    assert.match(worker, /fallback-as-pull-request: false/);
    assert.match(worker, /github-token-for-extra-empty-commit: "\$\{\{ '' \}\}"/);
    assert.match(worker, /github-token: \$\{\{ secrets.GITHUB_TOKEN \}\}/);
    assert.match(worker, /one separate local Git commit/);
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    const output = compiled.split(/^  safe_outputs:\r?\n/m)[1];
    assert.ok(output);
    assert.match(output, /needs.agent.result == 'success'/);
    assert.match(output, /needs.detection.result == 'success'/);
    assert.match(output, /GH_AW_CI_TRIGGER_TOKEN: \$\{\{ '' \}\}/);
    assert.doesNotMatch(workflow, /publishRepair|verdict-status|performance-proposal\.json|renderReport/);
    assert.match(workflow, /readReviewSummary/);
});

test('controller dispatches only the repair worker with immutable pull-request context and correlates its run', async () => {
    const dispatch = new AsyncFunction('github', 'context', 'core', 'process', 'setTimeout',
        extract('Dispatch and wait for isolated worker'));
    const head = 'a'.repeat(40);
    const base = 'b'.repeat(40);
    const mergeBase = 'c'.repeat(40);
    const dispatchId = `ghaw-pr-performance-42-${head}-100-2`;
    const calls = [];
    const outputs = {};
    const run = { id: 101, display_title: `Performance repair ${dispatchId}`,
        created_at: new Date().toISOString(), status: 'completed', conclusion: 'success',
        html_url: 'https://github.com/owner/repo/actions/runs/101' };
    const github = { rest: { actions: {
        async createWorkflowDispatch(value) { calls.push(['dispatch', value]); },
        async listWorkflowRuns(value) {
            calls.push(['list', value]);
            return { data: { workflow_runs: [
                { ...run, id: 102, display_title: 'Unrelated repair' }, run,
            ] } };
        },
        async getWorkflowRun(value) { assert.equal(value.run_id, 101); return { data: run }; },
        listJobsForWorkflowRun: 'jobs',
    } }, async paginate(method, value) {
        assert.equal(method, 'jobs');
        assert.equal(value.run_id, 101);
        return [{ name: 'agent', conclusion: 'success',
            steps: [{ name: 'Execute GitHub Copilot CLI', conclusion: 'success' }] }];
    } };
    await dispatch(github, { repo: { owner: 'owner', repo: 'repo' }, runId: 100,
        payload: { pull_request: { number: 42 } } },
    { setOutput(name, value) { outputs[name] = value; } },
    { env: { GITHUB_RUN_ATTEMPT: '2', EXPECTED_HEAD_SHA: head, EXPECTED_BASE_SHA: base,
        COMPARISON_BASE_SHA: mergeBase, HEAD_REF: 'feature', BASE_REF: 'main', HEAD_REPO: 'owner/repo' } },
    callback => callback());
    assert.deepEqual(calls.map(([method]) => method), ['dispatch', 'list']);
    const inputs = calls[0][1].inputs;
    for (const [, value] of calls) assert.equal(value.workflow_id, 'ghaw-pr-performance.lock.yml');
    assert.equal(calls[0][1].ref, 'main');
    assert.equal(calls[1][1].branch, 'main');
    assert.equal(inputs.dispatch_id, dispatchId);
    assert.equal(inputs.same_repo, 'true');
    assert.equal(inputs.head_repo, 'owner/repo');
    assert.equal(inputs.expected_head_sha, head);
    assert.equal(inputs.expected_base_sha, base);
    assert.equal(inputs.comparison_base_sha, mergeBase);
    assert.deepEqual(JSON.parse(inputs.aw_context), {
        run_id: '100', workflow_id: 'ghaw-pr-performance-controller',
        item_type: 'pull_request', item_number: 42, repo: 'owner/repo',
        event_type: 'pull_request', head_sha: head, base_sha: base, comparison_base_sha: mergeBase,
    });
    assert.equal(outputs.run_id, '101');
    assert.equal(outputs.conclusion, 'success');
});
