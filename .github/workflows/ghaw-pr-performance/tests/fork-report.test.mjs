import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import * as runtime from '../scripts/performance-review.mjs';
import { processGuideSubmission } from '../scripts/performance-review.mjs';

const identity = { prNumber: 42, baseSha: 'a'.repeat(40), headSha: 'b'.repeat(40) };
const expected = { ...identity, mode: 'guide' };
const scope = runtime.classifyPullRequest([{ filename: 'src/renderer/fixture.cpp' }], identity);
const finding = {
    id: 'PERF-RENDER-WAIT', severity: 'high', confidence: 'high',
    dimension: 'responsiveness', category: 'rendering', title: 'Repeated blocking wait',
    affectedScenario: 'Every repaint', location: 'src/renderer/fixture.cpp:10',
    observed: 'Waits synchronously on each repaint', expected: 'Never block the renderer',
    impact: 'Each repaint stalls the UI thread', proposedFix: 'Remove the blocking wait',
    validation: 'Native measurement unavailable; source blocking proof only',
    fixDisposition: 'manual_required',
    nativeEnvironment: { architecture: 'not-measured', details: 'Windows runner unavailable' },
    evidence: [{ type: 'blocking-proof', detail: 'The synchronous wait is called on every repaint' }],
};
function report() {
    return {
        version: 1, review: 'performance', mode: 'guide', identity: { ...identity },
        status: 'action_required', findings: [structuredClone(finding)],
        checks: [{ name: 'Native timing', status: 'unavailable', command: 'Source inspection',
            exitCode: null, detail: 'No Windows runner available' }],
    };
}
function submission(value = report()) {
    return { report: value, queued: { items: [{ type: 'noop' }], errors: [] } };
}
const processSubmission = (input, selectedScope = scope) =>
    processGuideSubmission(selectedScope, input.queued, expected, runtime, input.report);

test('validated guide report produces a verdict without a worker comment or input mutation', () => {
    const input = submission();
    const snapshot = structuredClone(input);
    const result = processSubmission(input);
    assert.deepEqual(result.report, report());
    assert.equal(result.queued.items[0].type, 'noop');
    assert.deepEqual(result.queued, input.queued);
    assert.deepEqual(input, snapshot);
    assert.deepEqual(Object.keys(result), ['report', 'queued', 'verdict']);
    assert.deepEqual(result.verdict, { version: 1, identity, status: 'action_required' });
    assert.equal(result.queued.items.length, 1);
});

test('malformed JSON, rendered cards, invalid identity/mode and mismatched outcomes fail', () => {
    for (const body of ['{', runtime.renderReport(report()), 'null'])
        assert.throws(() => processSubmission(submission(body)));
    for (const mutate of [
        r => { r.identity.prNumber++; },
        r => { r.identity.headSha = 'c'.repeat(40); },
        r => { r.identity.baseSha = 'c'.repeat(40); },
        r => { r.mode = 'repair'; },
        r => { r.status = 'pass'; },
        r => { r.findings[0].fixDisposition = 'fixed'; },
    ]) {
        const value = report();
        mutate(value);
        assert.throws(() => processSubmission(submission(value)));
    }
});

test('target aliases, ingestion errors, missing or extra outputs are rejected', () => {
    for (const alias of ['item_number', 'pr', 'pr_number', 'issue', 'issue_number', 'repo',
        'target', 'target_repo', 'target-repo', 'comment_id', 'reply_to_id', 'discussion_id']) {
        const input = submission();
        input.queued.items[0][alias] = 42;
        assert.throws(() => processSubmission(input), /override/);
    }
    for (const queued of [{ items: [] }, { ...submission().queued, errors: ['bad'] },
        { ...submission().queued, errors: 'bad' }, { items: [{ type: 'noop' }, { type: 'noop' }] },
        { items: [{ type: 'add_comment', body: JSON.stringify(report()) }] }])
        assert.throws(() => processSubmission({ report: report(), queued }));
    assert.throws(() => processSubmission(submission(null)));
});

test('only a single noop with null report passes for a non-applicable scope', () => {
    const excluded = runtime.classifyPullRequest([{ filename: 'README.md' }], identity);
    const result = processSubmission(submission(null), excluded);
    assert.equal(result.report, null);
    assert.equal(result.verdict.status, 'pass');
    assert.throws(() => processSubmission(submission(), excluded));
    for (const queued of [{ items: [] }, { items: [{ type: 'noop' }, { type: 'noop' }] },
        { items: [{ type: 'noop' }], errors: ['bad'] }])
        assert.throws(() => processSubmission({ report: null, queued }, excluded));
});

test('zero findings requires pass, or blocked when a check errors', () => {
    const value = report();
    value.findings = [];
    value.status = 'pass';
    assert.equal(processSubmission(submission(value)).verdict.status, 'pass');
    value.status = 'advisory';
    assert.throws(() => processSubmission(submission(value)), /must be pass/);
    value.checks[0].status = 'error';
    value.checks[0].exitCode = 1;
    value.status = 'blocked';
    assert.equal(processSubmission(submission(value)).verdict.status, 'blocked');
});

test('medium and low advice stays advisory, never claims a repair', () => {
    for (const severity of ['medium', 'low']) {
        const value = report();
        value.findings[0].severity = severity;
        value.findings[0].fixDisposition = 'advice_only';
        value.status = 'advisory';
        const result = processSubmission(submission(value));
        assert.equal(result.verdict.status, 'advisory');
        assert.equal(result.report.findings[0].fixDisposition, 'advice_only');
        value.findings[0].fixDisposition = 'proposed';
        assert.throws(() => processSubmission(submission(value)), /advice_only/);
    }
});

test('CLI refuses command/path overrides before touching submission files', () => {
    const script = fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url));
    const result = spawnSync(process.execPath, [script, 'fork-report', '--report', 'untrusted.json'], { encoding: 'utf8' });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.match(result.stderr, /accepts no command or path arguments/);
    const invalidRuntime = spawnSync(process.execPath, [script, 'fork-report'], {
        encoding: 'utf8', env: { ...process.env, TRUSTED_REVIEW_RUNTIME: 'relative.mjs' },
    });
    assert.equal(invalidRuntime.status, 1);
    assert.equal(invalidRuntime.stdout, '');
    assert.match(invalidRuntime.stderr, /absolute environment path/);
});

test('workflow exposes no model shell/edit route and only fixed read-only GitHub tools', () => {
    const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-fork-guidance.md', import.meta.url), 'utf8');
    const tools = workflow.match(/\ntools:\n([\s\S]*?)\nmcp-scripts:/)[1];
    assert.match(tools, /edit: false/);
    assert.match(tools, /bash: false/);
    assert.match(tools, /cli-proxy: false/);
    assert.match(tools, /mode: local/);
    assert.match(tools, /allowed: \[get_file_contents, get_commit, pull_request_read, search_code\]/);
    assert.match(tools, /allowed-repos: \["\$\{\{ github.repository \}\}"\]/);
    assert.doesNotMatch(tools, /pwsh|git diff|git grep|git show/);
    assert.doesNotMatch(workflow, /mcp-servers:/);
});

test('fork worker cannot publish a comment after its agent-job freshness check', () => {
    const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-fork-guidance.md', import.meta.url), 'utf8');
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-fork-guidance.lock.yml', import.meta.url), 'utf8');
    assert.match(workflow, /safe-outputs: \{\}/);
    assert.match(workflow, /Then request exactly one `noop`/);
    assert.doesNotMatch(compiled, /add_comment|add-comment|pull-requests: write/);
    assert.match(compiled, /"noop"/);
    assert.match(compiled, /performance-report\.json/);
});

test('inline guide tool captures only fixed summary/report outputs with immutable identity', async t => {
    const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance-fork-guidance.md', import.meta.url), 'utf8');
    const script = workflow.match(/    script: \|\n([\s\S]*?)\n\njobs:/)[1]
        .split('\n').map(line => line.slice(6)).join('\n');
    const prior = { ...process.env };
    Object.assign(process.env, {
        TRUSTED_REVIEW_RUNTIME: fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url)),
        PR_NUMBER: String(identity.prNumber), BASE_SHA: identity.baseSha, HEAD_SHA: identity.headSha,
    });

    try {
        const directory = fs.mkdtempSync(path.join(process.cwd(), '.performance-fork-summary-'));
        t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
        assert.match(script, /runtime\.captureReviewSummary\('\/tmp\/gh-aw', summaryMarkdown\)/);
        const preview = new (Object.getPrototypeOf(async function () {}).constructor)(
            'report_json', 'summaryMarkdown', 'summaryDirectory', 'reportFilename',
            script.replace("runtime.captureReviewSummary('/tmp/gh-aw', summaryMarkdown)",
                'runtime.captureReviewSummary(summaryDirectory, summaryMarkdown)')
                .replace("'/tmp/gh-aw/performance-report.json'", 'reportFilename'));
        const reportFilename = path.join(directory, 'report.json');
        const markdown = 'Read-only findings.\n\n| Severity | Finding |\n| LOW | Advice |\n';
        assert.deepEqual(await preview(JSON.stringify(report()), markdown, directory, reportFilename),
            { renderedCard: runtime.renderReport(report()) });
        assert.equal(runtime.readReviewSummary(directory), markdown);
        assert.deepEqual(JSON.parse(fs.readFileSync(reportFilename, 'utf8')), report());
        const invalid = report();
        invalid.identity.prNumber++;
        const rejectedMarkdown = 'Different latest submission; validation rejected.\n';
        await assert.rejects(preview(JSON.stringify(invalid), rejectedMarkdown, directory, reportFilename), /triggering pull request/);
        assert.equal(fs.existsSync(reportFilename), false, 'rejected identity invalidates the prior accepted report');
        assert.equal(runtime.readReviewSummary(directory), rejectedMarkdown, 'latest diagnostic summary is retained independently');
        await preview(JSON.stringify(report()), markdown, directory, reportFilename);
        await assert.rejects(preview('{', rejectedMarkdown, directory, reportFilename), SyntaxError);
        assert.equal(fs.existsSync(reportFilename), false, 'malformed latest JSON leaves no report for the post-agent gate');
        assert.throws(() => processSubmission(submission(null)), /version 1 performance review/,
            'a terminal noop cannot reuse the invalidated prior report');
        assert.equal(runtime.readReviewSummary(directory), rejectedMarkdown, 'summary survives invalid JSON and identity failures');
        await preview(JSON.stringify(report()), markdown, directory, reportFilename);
        await assert.rejects(preview(JSON.stringify(report()), '', directory, reportFilename), /review summary must/);
        assert.equal(fs.existsSync(reportFilename), false, 'invalid summary also invalidates accepted JSON');
        assert.match(script, /const reportPath = '\/tmp\/gh-aw\/performance-report\.json'/);
        assert.match(script, /fs\.writeFileSync\(reportPath/);
        assert.ok(script.indexOf('fs.unlinkSync(reportPath)') < script.indexOf('runtime.captureReviewSummary'));
        assert.doesNotMatch(script.slice(script.indexOf('fs.unlinkSync(reportPath)')), /\bawait\b/,
            'artifact invalidation, capture and acceptance must remain a synchronous handler transaction');
        assert.doesNotMatch(script, /exec|spawn|fetch\(/);
    } finally {
        for (const key of ['TRUSTED_REVIEW_RUNTIME', 'PR_NUMBER', 'BASE_SHA', 'HEAD_SHA']) {
            if (prior[key] === undefined) delete process.env[key];
            else process.env[key] = prior[key];
        }
    }
});

test('repair report tool rejects a missing second location and accepts correction before native handoff', async () => {
    const workflow = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.md', import.meta.url), 'utf8');
    const script = workflow.match(/    script: \|\n([\s\S]*?)\n\njobs:/)[1]
        .split('\n').map(line => line.slice(6)).join('\n');
    assert.doesNotMatch(script, /writeFile|readFile|exec|spawn|fetch\(/);
    const inline = new (Object.getPrototypeOf(async function () {}).constructor)('report_json', script);
    const compiled = fs.readFileSync(new URL('../../../workflows/ghaw-pr-performance.lock.yml', import.meta.url), 'utf8');
    const handler = compiled.match(/cat > "[^"]*\/validate_performance_report\.cjs" << '([^'\r\n]+)'\r?\n([\s\S]*?)\r?\n          \1/)[2];
    const module = { exports: {} };
    new Function('module', 'require', handler)(module, { main: null });
    const validate = async report_json => {
        const expected = await inline(report_json);
        assert.deepEqual(await module.exports.execute({ report_json }), expected);
        return expected;
    };
    assert.match(compiled, /--allow-tool mcpscripts --allow-tool safeoutputs/);
    const prior = { ...process.env };
    Object.assign(process.env, {
        TRUSTED_REVIEW_RUNTIME: fileURLToPath(new URL('../scripts/performance-review.mjs', import.meta.url)),
        PR_NUMBER: String(identity.prNumber), BASE_SHA: identity.baseSha, HEAD_SHA: identity.headSha,
    });
    try {
        const value = report();
        value.mode = 'repair';
        value.status = 'pending_validation';
        value.validationPlan = { type: 'wta-unit', testFilter: 'tests::focused' };
        value.findings[0].category = 'wta-runtime';
        value.findings[0].location = 'tools/wta/src/lib.rs:1';
        value.findings[0].fixDisposition = 'proposed';
        value.findings.push({ ...structuredClone(value.findings[0]), id: 'PERF-SECOND-FINDING' });
        delete value.findings[1].location;
        await assert.rejects(validate(JSON.stringify(value)), /findings\[1\]\.location must be a non-empty string/);
        await assert.rejects(module.exports.execute({ report_json: JSON.stringify(value) }),
            /findings\[1\]\.location must be a non-empty string/);
        value.findings[1].location = 'tools/wta/src/lib.rs:16';
        assert.deepEqual(await validate(JSON.stringify(value)), { status: 'pending_validation', findings: 2 });
        for (const mutate of [
            r => { r.identity.prNumber++; },
            r => { r.identity.baseSha = 'c'.repeat(40); },
            r => { r.identity.headSha = 'c'.repeat(40); },
            r => { r.mode = 'guide'; },
            r => { r.findings[1].fixDisposition = 'fixed'; },
        ]) {
            const invalid = structuredClone(value);
            mutate(invalid);
            await assert.rejects(validate(JSON.stringify(invalid)));
        }
        await assert.rejects(validate('{'), SyntaxError);
    } finally {
        for (const key of ['TRUSTED_REVIEW_RUNTIME', 'PR_NUMBER', 'BASE_SHA', 'HEAD_SHA']) {
            if (prior[key] === undefined) delete process.env[key];
            else process.env[key] = prior[key];
        }
    }
});
