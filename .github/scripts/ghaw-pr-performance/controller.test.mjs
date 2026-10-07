import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import process from 'node:process';
import test from 'node:test';

const workflow = fs.readFileSync(new URL('../../workflows/ghaw-pr-performance-controller.yml', import.meta.url), 'utf8');
const match = workflow.match(/script: \|\r?\n((?: {12}.+(?:\r?\n|$)|\s*\r?\n)+)/);
assert.ok(match, 'controller script must be extractable from the actual workflow');
const source = match[1].split(/\r?\n/).map(line => line.slice(12)).join('\n');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
const execute = new AsyncFunction('github', 'context', 'core', 'process', 'Date', 'setTimeout', source);

test('the controller run name preserves the PR identity instead of starting a YAML comment', () => {
    assert.match(workflow, /^run-name: 'Performance Review for PR #\$\{\{ github\.event\.pull_request\.number \}\}'$/m);
});

test('repair publication uses the repository Actions token, not a recursion-triggering override', () => {
    assert.match(workflow, /name: Publish only the tested tree with immutable-head CAS[\s\S]*?with:\s+github-token: \$\{\{ github\.token \}\}/);
});

async function simulate({ fork = false, conclusion = 'success', agent = true, timeout = false } = {}) {
    const calls = [];
    const outputs = {};
    let clock = 100000;
    const github = {
        rest: { actions: {
            async createWorkflowDispatch(value) { calls.push(['dispatch', value]); },
            async listWorkflowRuns() {
                return { data: { workflow_runs: [{
                    id: 123, created_at: new Date(100000).toISOString(),
                    display_title: 'Performance ghaw-pr-performance-42-head-1-1',
                }] } };
            },
            async getWorkflowRun() {
                return { data: { id: 123, status: timeout ? 'in_progress' : 'completed',
                    conclusion, html_url: 'https://example.invalid/run/123' } };
            },
            async cancelWorkflowRun(value) { calls.push(['cancel', value]); },
            listJobsForWorkflowRun: 'jobs',
        } },
        async paginate() {
            return [{ name: 'agent', conclusion: agent ? 'success' : 'skipped',
                steps: [{ name: 'Execute GitHub Copilot CLI', conclusion: agent ? 'success' : 'skipped' }] }];
        },
    };
    const context = { payload: { pull_request: { number: 42 } },
        repo: { owner: 'owner', repo: 'repo' }, runId: 1, runAttempt: 1 };
    const process = { env: {
        EXPECTED_HEAD_SHA: 'head', EXPECTED_BASE_SHA: 'base', COMPARISON_BASE_SHA: 'merge',
        HEAD_REF: 'topic', BASE_REF: 'main', HEAD_REPO: fork ? 'fork/repo' : 'owner/repo',
        SAME_REPO: String(!fork),
    } };
    let error;
    try {
        await execute(github, context, { setOutput(key, value) { outputs[key] = value; } },
            process, { now: () => clock, parse: Date.parse },
            (callback, delay) => { clock += delay; callback(); });
    } catch (caught) { error = caught; }
    return { calls, outputs, error };
}

test('actual controller dispatches repair with native PR context and checks inference ran', async () => {
    const result = await simulate();
    assert.equal(result.error, undefined);
    const dispatch = result.calls[0][1];
    assert.equal(dispatch.workflow_id, 'ghaw-pr-performance.lock.yml');
    assert.equal(dispatch.ref, 'main');
    assert.deepEqual(JSON.parse(dispatch.inputs.aw_context), {
        run_id: '1', workflow_id: 'ghaw-pr-performance-controller',
        item_type: 'pull_request', item_number: 42, repo: 'owner/repo',
        event_type: 'pull_request', head_sha: 'head', base_sha: 'base', comparison_base_sha: 'merge',
    });
    assert.equal(result.outputs.run_id, '123');
});

test('actual controller isolates fork guide from native PR branch mutation context', async () => {
    const result = await simulate({ fork: true });
    assert.equal(result.error, undefined);
    const dispatch = result.calls[0][1];
    assert.equal(dispatch.workflow_id, 'ghaw-pr-performance-guide-forkedrepo.lock.yml');
    assert.equal(JSON.parse(dispatch.inputs.aw_context).item_type, undefined);
});

test('actual controller rejects a green worker with a skipped agent', async () => {
    assert.match((await simulate({ agent: false })).error.message, /did not complete the agent/);
});

test('actual controller rejects failed worker conclusion', async () => {
    assert.match((await simulate({ conclusion: 'failure' })).error.message, /concluded failure/);
});

test('actual controller timeout cancels only its correlated run', async () => {
    const result = await simulate({ timeout: true });
    assert.match(result.error.message, /exceeded 68 minutes/);
    assert.deepEqual(result.calls.find(([kind]) => kind === 'cancel')[1], {
        owner: 'owner', repo: 'repo', run_id: 123,
    });
});

const reportMatch = workflow.match(/name: Publish linked PR run report[\s\S]*?script: \|\r?\n((?: {12}.+(?:\r?\n|$)|\s*\r?\n)+)/);
assert.ok(reportMatch, 'the actual PR report script must be extractable');
const reportScript = new AsyncFunction('github', 'context', 'core', 'process',
    reportMatch[1].split(/\r?\n/).map(line => line.slice(12)).join('\n'));

async function reportRun(overrides = {}) {
    const reports = [];
    let summary;
    const github = { rest: { checks: { async create(report) { reports.push(report); } } } };
    const core = { summary: { addRaw(value) { summary = value; return this; }, async write() {} } };
    await reportScript(github, { repo: { owner: 'owner', repo: 'repo' }, runId: 100, runAttempt: 1 }, core, {
        env: {
            RUN_OUTCOME: 'success', REVIEW_STATUS: 'pass', REVIEWED_HEAD_SHA: 'a'.repeat(40),
            PUBLISHED_HEAD_SHA: '', PUBLISHED_COMMIT_URL: '',
            CONTROLLER_RUN_URL: 'https://github.com/owner/repo/actions/runs/100',
            WORKER_RUN_URL: 'https://github.com/owner/repo/actions/runs/101',
            WORKER_CONCLUSION: 'success', ...overrides,
        },
    });
    assert.equal(reports.length, 1);
    assert.equal(summary, reports[0].output.summary);
    return reports[0];
}

test('every applicable controller completion publishes a linked PR report using the existing check pattern', async () => {
    assert.match(workflow, /^  checks: write$/m);
    assert.match(workflow, /name: Publish linked PR run report\s+if: always\(\) && steps\.gate\.outputs\.applicable == 'true'/);
    const report = await reportRun();
    assert.equal(report.name, 'Performance review');
    assert.equal(report.head_sha, 'a'.repeat(40));
    assert.equal(report.status, 'completed');
    assert.equal(report.conclusion, 'success');
    assert.equal(report.details_url, 'https://github.com/owner/repo/actions/runs/100');
    assert.match(report.output.summary, /\[Agentic worker report\]\(https:\/\/github\.com\/owner\/repo\/actions\/runs\/101\)/);
});

test('a published repair keeps the run-report link on its new validated head', async () => {
    const report = await reportRun({
        REVIEW_STATUS: 'pending_validation', PUBLISHED_HEAD_SHA: 'b'.repeat(40),
        PUBLISHED_COMMIT_URL: 'https://github.com/owner/repo/commit/' + 'b'.repeat(40),
    });
    assert.equal(report.head_sha, 'b'.repeat(40));
    assert.match(report.output.summary, /review result: \*\*fixed\*\*/);
    assert.match(report.output.summary, /Reviewed head: `a{40}`/);
    assert.match(report.output.summary, /\[Validated repair commit\]/);
    assert.match(workflow, /id: publish_repair/);
    assert.match(workflow, /core\.setOutput\("head_sha", result\.commitSha\)/);
});

test('failed or cancelled applicable runs keep report links without success-shaped conclusions', async () => {
    for (const outcome of ['failure', 'cancelled']) {
        const report = await reportRun({
            RUN_OUTCOME: outcome, REVIEW_STATUS: '', WORKER_CONCLUSION: outcome,
        });
        assert.equal(report.conclusion, outcome);
        assert.equal(report.head_sha, 'a'.repeat(40));
        assert.match(report.output.summary, /Agentic worker report/);
        assert.doesNotMatch(report.output.summary, /review result: \*\*fixed\*\*/);
    }
});

test('a failed worker dispatch still leaves the applicable PR a controller report link', async () => {
    const report = await reportRun({
        RUN_OUTCOME: 'failure', REVIEW_STATUS: '', WORKER_RUN_URL: '', WORKER_CONCLUSION: '',
    });
    assert.equal(report.conclusion, 'failure');
    assert.match(report.output.summary, /Controller run report/);
    assert.match(report.output.summary, /not started or could not be correlated/);
});

for (const worker of [
    'ghaw-pr-performance',
    'ghaw-pr-performance-guide-forkedrepo',
]) {
    const markdown = fs.readFileSync(new URL(`../../workflows/${worker}.md`, import.meta.url), 'utf8').replaceAll('\r\n', '\n');
    const compiled = fs.readFileSync(new URL(`../../workflows/${worker}.lock.yml`, import.meta.url), 'utf8').replaceAll('\r\n', '\n');
    const steps = compiled.split(/(?=^ {6}- )/m);
    const runtime = steps.find(step => step.includes('id: set-runtime-paths\n'));
    const classify = steps.find(step => step.includes('name: Classify immutable performance scope\n'));
    assert.ok(runtime && classify, `${worker}: actual compiled steps must be extractable`);
    const run = classify.match(/^ {8}run: (".*")$/m);
    assert.ok(run, `${worker}: compiled classification script must be extractable`);
    const script = JSON.parse(run[1]);

    test(`${worker}: compiled pre-agent step forwards runtime output before safe-output setup and inference`, () => {
        const original = markdown.match(/pre-agent-steps:\n[\s\S]*?    run: \|\n((?: {6}.*\n)+)/);
        assert.ok(original, 'actual source pre-agent script must be extractable');
        assert.equal(script, original[1].replace(/^ {6}/gm, ''), 'compiled pre-agent script must match its source');
        assert.match(runtime, /echo "GH_AW_SAFE_OUTPUTS=\$\{RUNNER_TEMP\}\/gh-aw\/safeoutputs\/outputs\.jsonl"/);
        assert.match(runtime, /\} >> "\$GITHUB_OUTPUT"/);
        assert.match(markdown, /^ {6}GH_AW_SAFE_OUTPUTS: \$\{\{ steps\.set-runtime-paths\.outputs\.GH_AW_SAFE_OUTPUTS \}\}$/m);
        assert.match(classify, /^ {10}GH_AW_SAFE_OUTPUTS: \$\{\{ steps\.set-runtime-paths\.outputs\.GH_AW_SAFE_OUTPUTS \}\}$/m);
        const runtimeIndex = compiled.indexOf(runtime);
        const classifyIndex = compiled.indexOf(classify);
        const setupIndex = compiled.indexOf('      - name: Prepare Safe Outputs Directories\n');
        const inferenceIndex = compiled.indexOf('      - name: Execute GitHub Copilot CLI\n');
        assert.ok(runtimeIndex < classifyIndex && classifyIndex < setupIndex && setupIndex < inferenceIndex,
            'classification cannot depend on the later compiler-generated safe-output directory setup');
        const mkdir = 'mkdir -p /tmp/gh-aw "$(dirname "$GH_AW_SAFE_OUTPUTS")"';
        assert.ok(script.includes(mkdir), 'pre-agent step must create the forwarded safe-output parent itself');
        assert.ok(script.indexOf(mkdir) < script.indexOf('node "$RUNNER_TEMP/performance-trusted.mjs" prepare'));
        assert.match(script, /--safe-outputs "\$GH_AW_SAFE_OUTPUTS"/);
    });

    test(`${worker}: actual pre-agent path expression reproduces Trial 1 when step env omits the output`, t => {
        const expression = script.match(/\$\(dirname "\$GH_AW_SAFE_OUTPUTS"\)/);
        assert.ok(expression, 'extract the actual pre-agent expression, not a rewritten shell check');
        const env = { ...process.env };
        delete env.GH_AW_SAFE_OUTPUTS;
        const command = `set -euo pipefail\n${expression[0].slice(2, -1)}`;
        const bash = process.platform === 'win32' ? 'C:\\Program Files\\Git\\bin\\bash.exe' : 'bash';
        const omitted = spawnSync(bash, [], { input: `unset GH_AW_SAFE_OUTPUTS\n${command}`, env, encoding: 'utf8', timeout: 10000 });
        if (omitted.error?.code === 'ENOENT') return t.skip('Native Bash unavailable; extracted pre-agent expression was not executed.');
        assert.equal(omitted.error, undefined);
        assert.equal(omitted.status, 1);
        assert.match(omitted.stderr, /GH_AW_SAFE_OUTPUTS: unbound variable/);
        const forwarded = spawnSync(bash, [], {
            input: `export GH_AW_SAFE_OUTPUTS='safeoutputs/outputs.jsonl'\n${command}`,
            env,
            encoding: 'utf8', timeout: 10000,
        });
        assert.equal(forwarded.error, undefined);
        assert.equal(forwarded.status, 0, forwarded.stderr);
        assert.equal(forwarded.stdout.trim(), 'safeoutputs');
        // Replay only the actual failing expression, without the full step's hosted filesystem writes.
    });

    test(`${worker}: actual compiled tool arguments enforce the caller-specific checker route`, () => {
        const inference = steps.find(step => step.includes('name: Execute GitHub Copilot CLI\n'));
        assert.ok(inference, 'extract the actual inference step, not its tool documentation');
        const command = inference.split('\n').find(line => line.includes('copilot_harness.cjs') && line.includes('--allow-tool'));
        assert.ok(command, 'actual compiled Copilot invocation must be extractable');
        const allowTools = command.match(/--allow-tool \S+/g);
        if (worker === 'ghaw-pr-performance') {
            assert.match(markdown, /^ {4}- 'pwsh:\*'$/m);
            assert.ok(allowTools.some(tool => tool.includes('shell(pwsh:*)')), 'repair CLI arguments must permit pwsh');
        } else {
            assert.match(markdown, /^  bash: false$/m);
            assert.match(markdown, /^  cli-proxy: false$/m);
            assert.ok(allowTools.every(tool => !tool.includes('shell(')), 'fork CLI must not expose any shell route');
            assert.ok(allowTools.some(tool => tool === '--allow-tool mcpscripts'), 'fork CLI must expose the read-only checker');
            assert.ok(allowTools.every(tool => tool !== '--allow-tool write'), 'fork CLI must not expose edits');
        }
        assert.ok(allowTools.every(tool => !tool.includes('shell(node')), 'direct Node is not the permitted model route');
    });
}
