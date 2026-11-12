import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-controller.yml', import.meta.url), 'utf8');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const extract = name => {
    const match = workflow.match(new RegExp(`name: ${name}[\\s\\S]*?script: \\|\\r?\\n((?: {12}.+(?:\\r?\\n|$)|\\s*\\r?\\n)+)`));
    assert.ok(match, `actual controller script missing: ${name}`);
    return match[1].split(/\r?\n/).map(line => line.slice(12)).join('\n');
};
const reportScript = new AsyncFunction('github', 'context', 'core', 'process', 'require', extract('Publish linked PR run report'));

function workflowJob(id) {
    const match = workflow.match(new RegExp(`^  ${id}:\\r?\\n([\\s\\S]*?)(?=^  [\\w-]+:\\r?$|(?![\\s\\S]))`, 'm'));
    assert.ok(match, `controller job missing: ${id}`);
    return match[1];
}

function expression(value, { needs = {}, github = {}, steps = {}, job = {}, cancelled = false } = {}) {
    const source = value.replace(/^\$\{\{\s*|\s*\}\}$/g, '').replace(/needs\.([\w-]+)/g, 'needs["$1"]');
    return new Function('needs', 'github', 'steps', 'job', 'cancelled', `return (${source});`)(
        needs, github, steps, job, () => cancelled);
}

function publicationAllowed(result, applicable = 'true', cancelled = false) {
    const condition = workflowJob('publish-report').match(/^ {4}if: (.+)$/m)?.[1];
    assert.ok(condition, 'publication must explicitly handle failed and cancelled reviews');
    return expression(condition, {
        needs: { 'performance-review': { result, outputs: { applicable } } }, cancelled,
    });
}

function publicationQueue() {
    const review = workflowJob('performance-review');
    const publisher = workflowJob('publish-report');
    assert.doesNotMatch(workflow, /^concurrency:/m, 'parent workflow must not cancel an active publisher');
    assert.match(review, /^ {6}cancel-in-progress: true$/m);
    assert.match(publisher, /^ {6}cancel-in-progress: false$/m);
    assert.match(publisher, /^ {6}queue: max$/m);
    assert.match(publisher, /^ {4}needs: performance-review$/m);
    const group = block => block.match(/^ {6}group: (.+)$/m)?.[1];
    assert.ok(group(review));
    assert.ok(group(publisher));
    assert.notEqual(group(review), group(publisher), 'review preemption must not cancel publication');
    assert.match(group(publisher), /\$\{\{\s*github\.repository\s*\}\}/);
    assert.match(group(publisher), /\$\{\{\s*needs\.performance-review\.outputs\.pr_number\s*\}\}/);
    assert.doesNotMatch(group(publisher), /run_id|run_attempt|sha|ref/);
    assert.doesNotMatch(review, /github\.rest\.issues\.(?:createComment|updateComment)/);
    assert.match(publisher, /github\.rest\.issues\.createComment/);
    assert.match(publisher, /github\.rest\.issues\.updateComment/);
    let tail = Promise.resolve();
    return options => {
        assert.ok(publicationAllowed(options.outcome ?? 'success'));
        const result = tail.then(() => reportRun('Queued result.\n', options));
        tail = result.then(() => {}, () => {});
        return result;
    };
}

test('stale CI completions cannot preempt a different admitted run or attempt', () => {
    const group = workflowJob('performance-review').match(/^ {6}group: (.+)$/m)?.[1];
    assert.ok(group);
    const resolve = (id, attempt) => group.replace(/\$\{\{\s*([\s\S]*?)\s*\}\}/g, (_, source) =>
        expression(source, { github: { repository: 'owner/repo',
            event: { workflow_run: { id, run_attempt: attempt, head_branch: 'same-branch' } } } }));
    assert.notEqual(resolve(99, 1), resolve(100, 1));
    assert.notEqual(resolve(100, 1), resolve(100, 2));
    assert.equal(resolve(100, 2), resolve(100, 2), 'duplicate deliveries of one CI attempt may coalesce');
});

function temporaryDirectory(prefix) {
    const generated = path.join(process.cwd(), 'Generated Files');
    fs.mkdirSync(generated, { recursive: true });
    return fs.mkdtempSync(path.join(generated, prefix));
}

test('controller loads its helper from the workflow revision when the PR base predates it', t => {
    const root = temporaryDirectory('performance-controller-checkout-');
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8', timeout: 15000 }).trim();
    const commit = subject => {
        git(['add', '--all']);
        git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', subject]);
        return git(['rev-parse', 'HEAD']);
    };
    git(['init', '--quiet', '--initial-branch=main']);
    const source = path.join(root, 'tools', 'wta', 'src', 'lib.rs');
    fs.mkdirSync(path.dirname(source), { recursive: true });
    fs.writeFileSync(source, 'pub fn value() -> usize { 1 }\n');
    const base = commit('PR base before the reviewer existed');
    git(['checkout', '--quiet', '-b', 'topic']);
    fs.writeFileSync(source, 'pub fn value() -> usize { 2 }\n');
    const head = commit('Reviewed source change');
    git(['checkout', '--quiet', 'main']);
    const helper = path.join(root, '.github', 'workflows', 'ghaw-pr-performance', 'scripts', 'performance-review.mjs');
    fs.mkdirSync(path.dirname(helper), { recursive: true });
    fs.copyFileSync(new URL('../scripts/performance-review.mjs', import.meta.url), helper);
    commit('Install the reviewer after the PR base');
    git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
        'merge', '--quiet', '--no-ff', '-m', 'Workflow merge revision', 'topic']);
    const trusted = git(['rev-parse', 'HEAD']);
    assert.equal(git(['merge-base', trusted, head]), head);
    const context = {
        'github.workflow_sha': trusted,
        'github.event.pull_request.base.sha': base,
        'github.event.pull_request.head.sha': head,
    };
    const expression = workflow.match(/uses: actions\/checkout@[^\r\n]+\r?\n\s+with:\r?\n\s+ref: \$\{\{\s*([^}]+?)\s*\}\}/)?.[1];
    assert.ok(Object.hasOwn(context, expression), 'resolve the actual controller checkout expression');
    git(['checkout', '--quiet', '--detach', context[expression]]);
    const scopeDirectory = path.join(root, '.git', 'scope');
    const comparisonBase = git(['merge-base', base, head]);
    execFileSync(process.execPath, [helper, 'prepare', '--output-dir', scopeDirectory,
        '--pr', '42', '--base', comparisonBase, '--head', head], { cwd: root, timeout: 15000 });
    const scope = JSON.parse(fs.readFileSync(path.join(scopeDirectory, 'performance-scope.json'), 'utf8'));
    assert.deepEqual(scope.identity, { prNumber: 42, baseSha: base, headSha: head });
    assert.deepEqual(scope.candidates.map(file => file.filename), ['tools/wta/src/lib.rs']);
    assert.equal(git(['rev-parse', 'HEAD']), trusted);
    assert.match(workflow, /BASE_SHA: \$\{\{ steps\.admission\.outputs\.base_sha \}\}/);
    assert.match(workflow, /HEAD_SHA: \$\{\{ steps\.admission\.outputs\.head_sha \}\}/);
});

test('repair worker collects the agent-written report from its pre-created output directory', t => {
    const worker = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    const block = worker.match(/RUNTIME=.*node --input-type=module <<'NODE'\r?\n([\s\S]*?)\r?\n      NODE/)?.[1];
    assert.ok(block, 'actual report collection step exists');
    const root = temporaryDirectory('performance-agent-summary-');
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const agent = path.join(root, 'agent');
    const output = path.join(root, 'artifacts');
    fs.mkdirSync(agent);
    fs.mkdirSync(output);
    const file = path.join(agent, 'performance-summary.md');
    const markdown = '## Performance review\n\nNo actionable findings.\n\n| Check | Result |\n| --- | --- |\n| Source review | Completed |\n';
    fs.writeFileSync(file, markdown);
    const script = block.replace(/^ {6}/gm, '').replaceAll("'/tmp/gh-aw/agent/performance-summary.md'", JSON.stringify(file));
    execFileSync(process.execPath, ['--input-type=module', '--eval', script], {
        cwd: root, timeout: 15000,
        env: { ...process.env, OUTPUT: output,
            RUNTIME: fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url)) },
    });
    assert.equal(fs.readFileSync(path.join(output, '.performance-summary.md'), 'utf8'), markdown);
    assert.match(worker, /Always write[\s\S]*?even when there are no findings or no eligible commits/);
    assert.match(worker, /before requesting either[^]*?noop/);
    assert.match(worker, /noop\.message[^]*?does not replace/);
});

test('preparation reads metadata only from the owning repository and preserves it for identity verification', async t => {
    const worker = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    const block = worker.match(/name: Read live PR metadata[\s\S]*?script: \|\r?\n((?: {12}.*(?:\r?\n|$))+)/)?.[1];
    assert.ok(block);
    const read = new AsyncFunction('github', 'context', 'process', 'require', block.replace(/^ {12}/gm, ''));
    const root = temporaryDirectory('performance-pr-metadata-');
    t.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const metadata = { number: 42, head: { sha: 'a'.repeat(40) }, base: { sha: 'b'.repeat(40) } };
    const calls = [];
    const github = { rest: { pulls: { async get(value) { calls.push(value); return { data: metadata }; } } } };
    const context = { repo: { owner: 'owner', repo: 'repo' } };
    const process = { env: { PR_NUMBER: '42', RUNNER_TEMP: root } };
    await read(github, context, process, createRequire(import.meta.url));
    assert.deepEqual(calls, [{ owner: 'owner', repo: 'repo', pull_number: 42 }]);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(root, 'performance-pr.json'), 'utf8')), metadata);
    process.env.PR_NUMBER = '42/other';
    await assert.rejects(read(github, context, process, createRequire(import.meta.url)), /Invalid PR/);
    assert.equal(calls.length, 1);
    assert.match(worker, /name: Read live PR metadata[\s\S]*?retries: 2/);
    assert.match(worker, /verify-pr[\s\S]*?--expected-base/);
});

async function reportRun(markdown, { currentHead = 'a'.repeat(40), recordedHead = 'a'.repeat(40),
    outcome = 'success', comments = [], failComment = false, commentSequence, headSequence,
    store = { comments: structuredClone(comments) }, runId = 100, runAttempt = '1',
    workerRunId = '101', afterCommentRead } = {}) {
    const root = temporaryDirectory('performance-delivery-');
    const checks = [];
    const posted = [];
    const warnings = [];
    let initialConclusion;
    let commentRead = 0;
    let headRead = 0;
    const writeComment = async value => {
        if (failComment) throw new Error('Comment denied');
        let comment;
        if (value.comment_id !== undefined) {
            comment = store.comments.find(item => item.id === value.comment_id);
            assert.ok(comment, 'updateComment must target an existing comment');
            comment.body = value.body;
        } else {
            comment = { id: Math.max(0, ...store.comments.map(item => item.id)) + 1,
                user: { login: 'github-actions[bot]' }, body: value.body };
            store.comments.push(comment);
        }
        posted.push(value);
        return { data: structuredClone(comment) };
    };
    const github = { rest: {
        checks: {
            async create(check) { initialConclusion = check.conclusion; checks.push(check); return { data: { id: 99 } }; },
            async update(check) { Object.assign(checks[0], check); },
        },
        issues: {
            listComments: 'comments',
            createComment: writeComment,
            updateComment: writeComment,
        },
        pulls: { async get() {
            return { data: { head: { sha: headSequence?.[Math.min(headRead++, headSequence.length - 1)] ?? currentHead } } };
        } },
    }, async paginate() {
        const snapshot = structuredClone(commentSequence?.[Math.min(commentRead, commentSequence.length - 1)] ?? store.comments);
        commentRead++;
        await afterCommentRead?.(commentRead, snapshot);
        return snapshot;
    } };
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
        await reportScript(github, { repo: { owner: 'owner', repo: 'repo' }, runId }, core, {
            env: { GITHUB_RUN_ATTEMPT: runAttempt, GITHUB_WORKSPACE: process.cwd(), RUNNER_TEMP: root,
                PR_NUMBER: '42', WORKER_RUN_ID: workerRunId, RUN_OUTCOME: outcome,
                REVIEWED_HEAD_SHA: 'a'.repeat(40),
                CONTROLLER_RUN_URL: `https://github.com/owner/repo/actions/runs/${runId}`,
                WORKER_RUN_URL: workerRunId ? 'https://github.com/owner/repo/actions/runs/101' : '',
                WORKER_CONCLUSION: workerRunId ? outcome : '' },
        }, require);
    } catch (caught) { error = caught; }
    finally { fs.rmSync(root, { recursive: true, force: true }); }
    return { check: checks[0], checks, posted, warnings, initialConclusion, error, headRead, store };
}

test('queued publication protects the entire final read/write interval for updates, first creation and reruns', async t => {
    for (const [name, existing, first, second] of [
        ['update', true, [100, '1'], [101, '1']],
        ['create', false, [100, '1'], [101, '1']],
        ['rerun', true, [100, '9'], [100, '10']],
        ['newer first', true, [101, '1'], [100, '1']],
    ]) {
        await t.test(name, async () => {
            const enqueue = publicationQueue();
            const store = { comments: existing ? [{ id: 77, user: { login: 'github-actions[bot]' },
                body: '<!-- ghaw-pr-performance-report -->\n<!-- ghaw-pr-performance-run:99:1 -->' }] : [] };
            let reached;
            let release;
            const atFinalRead = new Promise(resolve => { reached = resolve; });
            const released = new Promise(resolve => { release = resolve; });
            const firstRun = enqueue({ store, runId: first[0], runAttempt: first[1],
                async afterCommentRead(read) {
                    if (read === 2) { reached(); await released; }
                } });
            await Promise.race([atFinalRead, firstRun.then(() => assert.fail('final read was not reached'))]);
            let secondStarted = false;
            let secondRun;
            try {
                secondRun = enqueue({ store, runId: second[0], runAttempt: second[1],
                    afterCommentRead() { secondStarted = true; } });
                await new Promise(resolve => setImmediate(resolve));
                assert.equal(secondStarted, false, 'another publisher must wait until the first mutation completes');
                assert.equal(store.comments.length, existing ? 1 : 0);
            } finally { release(); }
            const results = await Promise.all([firstRun, secondRun]);
            for (const result of results) assert.equal(result.error, undefined);
            assert.equal(store.comments.length, 1, 'first publication must not create duplicate reports');
            const newest = name === 'newer first' ? first : second;
            assert.ok(store.comments[0].body.includes(`<!-- ghaw-pr-performance-run:${newest[0]}:${newest[1]} -->`));
            if (name === 'newer first') assert.equal(results[1].posted.length, 0);
            else assert.equal(results[1].posted[0].comment_id, store.comments[0].id);
        });
    }
});

test('publication continues after a failed writer without turning its check green', async () => {
    const enqueue = publicationQueue();
    const store = { comments: [] };
    const failed = enqueue({ store, failComment: true, runId: 100 });
    const next = enqueue({ store, runId: 101 });
    const [first, second] = await Promise.all([failed, next]);
    assert.match(first.error.message, /Comment denied/);
    assert.equal(first.check.conclusion, 'action_required');
    assert.equal(second.error, undefined);
    assert.equal(store.comments.length, 1);
    assert.ok(store.comments[0].body.includes('<!-- ghaw-pr-performance-run:101:1 -->'));
});

test('publication handles failed reviews but skips cancelled, skipped and inapplicable reviews', () => {
    for (const result of ['success', 'failure', 'cancelled', 'skipped']) {
        for (const applicable of ['true', 'false', '']) {
            const expected = applicable === 'true' && ['success', 'failure'].includes(result);
            assert.equal(publicationAllowed(result, applicable), expected, `${result}/${applicable}`);
            assert.equal(publicationAllowed(result, applicable, true), false, 'a cancelled workflow must not start publishing');
        }
    }
    const condition = workflowJob('performance-review').match(/^ {4}if: (.+)$/m)?.[1];
    assert.ok(condition);
    for (const conclusion of ['success', 'failure', 'cancelled']) {
        for (const event of ['pull_request', 'push']) {
            for (const headId of [1, 2]) {
                assert.equal(expression(condition, { github: {
                    event: { repository: { id: 1 }, workflow_run: { conclusion, event, head_repository: { id: headId } } },
                } }), conclusion === 'success' && event === 'pull_request' && headId === 1);
            }
        }
    }
});

test('publication has its own trusted checkout and receives producer outputs rather than its own job status', () => {
    const review = workflowJob('performance-review');
    const publisher = workflowJob('publish-report');
    assert.match(publisher, /uses: actions\/checkout@[^\r\n]+\r?\n\s+with:\r?\n\s+ref: \$\{\{ github\.workflow_sha \}\}/);
    assert.match(publisher, /persist-credentials: false/);
    for (const [output, step] of [
        ['applicable', 'gate.outputs.applicable'],
        ['worker_run_id', 'worker.outputs.run_id'],
        ['worker_run_url', 'worker.outputs.run_url'],
        ['worker_conclusion', 'worker.outputs.conclusion'],
    ]) {
        const value = review.match(new RegExp(`^ {6}${output}: (.+)$`, 'm'))?.[1];
        assert.ok(value, `producer output missing: ${output}`);
        assert.equal(expression(value, { steps: {
            gate: { outputs: { applicable: 'true' } },
            worker: { outputs: { run_id: '101', run_url: 'worker-url', conclusion: 'failure' } },
        } }), { 'gate.outputs.applicable': 'true', 'worker.outputs.run_id': '101',
            'worker.outputs.run_url': 'worker-url', 'worker.outputs.conclusion': 'failure' }[step]);
    }
    const env = publisher.match(/name: Publish linked PR run report[\s\S]*?\r?\n {8}env:\r?\n([\s\S]*?)\r?\n {8}with:/)?.[1];
    assert.ok(env);
    const producer = { result: 'failure', outputs: {
        worker_run_id: '101', worker_run_url: 'worker-url', worker_conclusion: 'failure',
        head_sha: 'a'.repeat(40), pr_number: '42',
    } };
    const github = { event: { pull_request: { head: { sha: 'a'.repeat(40) }, number: 42 } },
        server_url: 'https://github.com', repository: 'owner/repo', run_id: '100', token: 'fixture' };
    const resolved = Object.fromEntries([...env.matchAll(/^ {10}([A-Z_]+): (.+)$/gm)].map(([, name, value]) => [
        name, value.trim().replace(/\$\{\{\s*([\s\S]*?)\s*\}\}/g, (_, source) =>
            expression(source, { needs: { 'performance-review': producer }, github, job: { status: 'success' } }) ?? ''),
    ]));
    assert.equal(resolved.RUN_OUTCOME, 'failure');
    assert.equal(resolved.WORKER_RUN_ID, '101');
    assert.equal(resolved.WORKER_RUN_URL, 'worker-url');
    assert.equal(resolved.WORKER_CONCLUSION, 'failure');
    assert.equal(resolved.REVIEWED_HEAD_SHA, 'a'.repeat(40));
    assert.equal(resolved.PR_NUMBER, '42');
    assert.equal(resolved.CONTROLLER_RUN_URL, 'https://github.com/owner/repo/actions/runs/100');
    producer.outputs = {};
    for (const [, value] of env.matchAll(/^ {10}WORKER_[A-Z_]+: (.+)$/gm)) {
        assert.equal(value.trim().replace(/\$\{\{\s*([\s\S]*?)\s*\}\}/g, (_, source) =>
            expression(source, { needs: { 'performance-review': producer } }) ?? ''), '');
    }
});

test('failed reviews still publish their failure even without a correlated worker report', async () => {
    for (const workerRunId of ['101', '']) {
        const result = await reportRun(undefined, { outcome: 'failure', workerRunId });
        assert.equal(result.error, undefined);
        assert.equal(result.check.conclusion, 'failure');
        assert.match(result.store.comments[0].body, /Run outcome: \*\*failure\*\*/);
        assert.match(result.store.comments[0].body, /Summary delivery: \*\*missing/);
    }
});

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

async function workerRun({ readErrors = {}, jobsSequence, runSequence, dispatchError, cancelError } = {}) {
    const dispatch = new AsyncFunction('github', 'context', 'core', 'process', 'setTimeout',
        extract('Dispatch and wait for isolated worker'));
    const head = 'a'.repeat(40);
    const base = 'b'.repeat(40);
    const mergeBase = 'c'.repeat(40);
    const dispatchId = `ghaw-pr-performance-42-${head}-100-2`;
    const calls = [];
    const outputs = {};
    const warnings = [];
    const waits = [];
    const reads = { list: 0, run: 0, jobs: 0 };
    const workflowSha = 'd'.repeat(40);
    const run = { id: 101, display_title: `Performance repair ${dispatchId}`,
        head_sha: workflowSha, path: '.github/workflows/ghaw-pr-performance.lock.yml',
        created_at: new Date().toISOString(), status: 'completed', conclusion: 'success',
        html_url: 'https://github.com/owner/repo/actions/runs/101' };
    const agent = { name: 'agent', conclusion: 'success',
        steps: [{ name: 'Execute GitHub Copilot CLI', conclusion: 'success' }] };
    const read = kind => {
        const index = reads[kind]++;
        const status = readErrors[kind]?.[index];
        if (status) throw Object.assign(new Error(`Read failed: ${status}`), { status });
        return index;
    };
    const github = { rest: { actions: {
        async createWorkflowDispatch(value) {
            calls.push(['dispatch', value]);
            if (dispatchError) throw Object.assign(new Error('Dispatch failed'), { status: dispatchError });
        },
        async listWorkflowRuns(value) {
            read('list');
            calls.push(['list', value]);
            return { data: { workflow_runs: [
                { ...run, id: 102, display_title: 'Unrelated repair' }, run,
            ] } };
        },
        async getWorkflowRun(value) {
            assert.equal(value.run_id, 101);
            const index = read('run');
            return { data: { ...run, ...runSequence?.[Math.min(index, runSequence.length - 1)] } };
        },
        async cancelWorkflowRun(value) {
            calls.push(['cancel', value]);
            if (cancelError) throw Object.assign(new Error('Cancellation failed'), { status: cancelError });
        },
        listJobsForWorkflowRun: 'jobs',
    }, pulls: { async get() {
        return { data: { number: 42, state: 'open', draft: false,
            head: { sha: head, ref: 'feature', repo: { full_name: 'owner/repo' } },
            base: { sha: base, ref: 'main' } } };
    } }, git: { async getRef() {
        return { data: { ref: 'refs/heads/main', object: { type: 'commit', sha: workflowSha } } };
    } } }, async paginate(method, value) {
        assert.equal(method, 'jobs');
        assert.equal(value.run_id, 101);
        const index = read('jobs');
        return jobsSequence?.[Math.min(index, jobsSequence.length - 1)] ?? [agent];
    } };
    let error;
    try {
        await dispatch(github, { repo: { owner: 'owner', repo: 'repo' }, runId: 100,
            payload: { repository: { default_branch: 'main' } } },
        { setOutput(name, value) { outputs[name] = value; }, warning(message) { warnings.push(message); } },
        { env: { GITHUB_RUN_ATTEMPT: '2', PR_NUMBER: '42', CI_RUN_ID: '99', CI_RUN_ATTEMPT: '3',
            EXPECTED_HEAD_SHA: head, EXPECTED_BASE_SHA: base,
            COMPARISON_BASE_SHA: mergeBase, HEAD_REF: 'feature', BASE_REF: 'main', HEAD_REPO: 'owner/repo' } },
        (callback, delay) => { waits.push(delay); callback(); });
    } catch (caught) { error = caught; }
    return { calls, outputs, warnings, waits, reads, error, head, base, mergeBase, dispatchId, agent };
}

test('controller dispatches only the repair worker with immutable pull-request context and correlates its run', async () => {
    const { calls, outputs, error, head, base, mergeBase, dispatchId } = await workerRun();
    assert.equal(error, undefined);
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
    assert.equal(inputs.expected_workflow_sha, 'd'.repeat(40));
    assert.equal(inputs.ci_run_id, '99');
    assert.equal(inputs.ci_run_attempt, '3');
    assert.equal(inputs.comparison_base_sha, mergeBase);
    assert.deepEqual(JSON.parse(inputs.aw_context), {
        run_id: '100', workflow_id: 'ghaw-pr-performance-controller',
        item_type: 'pull_request', item_number: 42, repo: 'owner/repo',
        event_type: 'pull_request', head_sha: head, base_sha: base, comparison_base_sha: mergeBase,
    });
    assert.equal(outputs.run_id, '101');
    assert.equal(outputs.conclusion, 'success');
});

test('controller retries transient reads without dispatching another paid worker', async () => {
    const result = await workerRun({ readErrors: { list: [503], run: [502, 504], jobs: [502] } });
    assert.equal(result.error, undefined);
    assert.equal(result.calls.filter(([name]) => name === 'dispatch').length, 1);
    assert.equal(result.warnings.length, 4);
    assert.equal(result.outputs.conclusion, 'success');
});

test('controller bounds transient read retries and does not retry permanent reads or uncertain dispatch', async () => {
    const exhausted = await workerRun({ readErrors: { run: [503, 503, 503, 503] } });
    assert.equal(exhausted.reads.run, 3);
    assert.equal(exhausted.error.status, 503);
    const denied = await workerRun({ readErrors: { run: [403] } });
    assert.equal(denied.reads.run, 1);
    assert.equal(denied.warnings.length, 0);
    const dispatch = await workerRun({ dispatchError: 503 });
    assert.equal(dispatch.calls.length, 1);
    assert.equal(dispatch.error.status, 503);
    assert.equal(dispatch.reads.run, 0);
});

test('controller waits for completed agent-step visibility but still rejects missing execution', async () => {
    const { agent } = await workerRun();
    const delayed = await workerRun({ jobsSequence: [[], [{ ...agent, steps: [] }], [agent]] });
    assert.equal(delayed.error, undefined);
    assert.equal(delayed.reads.jobs, 3);
    assert.equal(delayed.warnings.length, 2);
    const missing = await workerRun({ jobsSequence: [[]] });
    assert.match(missing.error.message, /did not complete the agent execution step/);
    assert.equal(missing.reads.jobs, 3);
});

test('controller handles a cancellation conflict only when the worker actually completed', async () => {
    const overdue = { name: 'activation', status: 'in_progress', started_at: '2000-01-01T00:00:00Z' };
    const { agent } = await workerRun();
    const completed = await workerRun({ runSequence: [{ status: 'in_progress' }, { status: 'completed' }],
        jobsSequence: [[overdue], [agent]], cancelError: 409 });
    assert.equal(completed.error, undefined);
    assert.equal(completed.outputs.conclusion, 'success');
    assert.equal(completed.calls.filter(([name]) => name === 'cancel').length, 1);
    const stillRunning = await workerRun({ runSequence: [{ status: 'in_progress' }],
        jobsSequence: [[overdue]], cancelError: 409 });
    assert.equal(stillRunning.error.status, 409);
    const denied = await workerRun({ runSequence: [{ status: 'in_progress' }],
        jobsSequence: [[overdue]], cancelError: 403 });
    assert.equal(denied.error.status, 403);
    const failed = await workerRun({ runSequence: [{ status: 'in_progress' }, { status: 'completed', conclusion: 'failure' }],
        jobsSequence: [[overdue]], cancelError: 409 });
    assert.match(failed.error.message, /phase deadline/);
});
