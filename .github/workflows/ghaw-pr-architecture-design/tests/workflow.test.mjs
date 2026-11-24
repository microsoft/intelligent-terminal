import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const source = fs.readFileSync(new URL('../../ghaw-pr-architecture-design.md', import.meta.url), 'utf8');
const compiled = fs.readFileSync(new URL('../../ghaw-pr-architecture-design.lock.yml', import.meta.url), 'utf8');
const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;

function job(text, name) {
    const block = text.match(new RegExp(`^  ${name}:\\r?\\n([\\s\\S]*?)(?=^  [\\w-]+:\\r?$|^\\S|(?![\\s\\S]))`, 'm'))?.[1];
    assert.ok(block, `Missing ${name} job`);
    return block;
}

test('compiled dependency graph gates inference and detection before publication', () => {
    assert.match(job(compiled, 'activation'), /needs:[\s\S]*?- prepare/);
    assert.match(job(compiled, 'activation'), /needs\.prepare\.outputs\.admitted == 'true'/);
    const agent = job(compiled, 'agent');
    assert.match(agent, /needs:[\s\S]*?- prepare/);
    const condition = agent.match(/^ {4}if: (.+)$/m)?.[1];
    assert.ok(condition);
    const evaluate = new Function('needs', `return ${condition};`);
    for (const admitted of ['false', '', undefined]) {
        assert.equal(evaluate({ activation: { outputs: {} }, prepare: { outputs: { admitted } } }), false);
    }
    assert.equal(evaluate({ activation: { outputs: {} }, prepare: { outputs: { admitted: 'true' } } }), true);
    assert.match(job(compiled, 'detection'), /needs\.agent\.result != 'skipped'/);
    const publisher = job(compiled, 'safe_outputs');
    assert.match(publisher, /runs-on: ubuntu-latest/);
    for (const dependency of ['agent', 'detection'])
        assert.match(publisher, new RegExp(`needs:[\\s\\S]*?- ${dependency}`));
    assert.doesNotMatch(publisher, /needs\.freshness/);
    const publishCondition = publisher.match(/^ {4}if: >\r?\n([\s\S]*?)(?=^ {4}\S)/m)?.[1].trim();
    assert.ok(publishCondition);
    const publish = new Function('needs', 'cancelled', `return ${publishCondition};`);
    for (const agentResult of ['success', 'failure', 'skipped', 'cancelled']) {
        for (const detection of ['success', 'failure', 'skipped']) {
            assert.equal(publish({ agent: { result: agentResult },
                detection: { result: detection, outputs: {} } }, () => false),
            agentResult === 'success' && detection === 'success');
        }
    }
});

test('activation renders the admitted comparison base into the imported prompt', async () => {
    const needs = { prepare: { outputs: { comparison_base: 'c'.repeat(40) } } };
    const steps = job(compiled, 'activation').split(/^      - /m);
    const binding = steps.findIndex(step => step.startsWith('name: Bind admitted comparison base'));
    const rendering = steps.findIndex(step => step.startsWith('name: Interpolate variables'));
    assert.ok(binding > 0 && binding < rendering);
    const expression = steps[binding].match(/COMPARISON_BASE: \$\{\{ (needs\.[\w.]+) \}\}/)?.[1];
    assert.ok(expression);
    const value = expression.split('.').slice(1).reduce((object, key) => object[key], needs);
    const script = steps[binding].match(/script: \|\r?\n([\s\S]*)/)[1];
    const environment = {};
    await new AsyncFunction('core', 'process', script)(
        { exportVariable: (name, value) => { environment[name] = value; } },
        { env: { COMPARISON_BASE: value } });
    const prompt = source.slice(source.lastIndexOf('\n---') + 4)
        .replace(/\$\{\{\s*(needs\.[\w.]+)\s*\}\}/g, (original, expression) =>
            environment[`GH_AW_${expression.replaceAll('.', '_').toUpperCase()}`] ?? original);
    assert.ok(prompt.includes(`comparison base \`${needs.prepare.outputs.comparison_base}\``),
        'The imported prompt must contain the admitted SHA, not an unresolved needs expression.');
    assert.doesNotMatch(prompt, /\$\{\{\s*needs\./);
});

test('trusted checkout never activates candidate configuration or adds a repair path', () => {
    assert.match(source, /pull_request_target:/);
    assert.match(source, /roles: \[admin, maintainer, write\]/);
    assert.doesNotMatch(compiled, /name: Checkout PR branch/);
    assert.doesNotMatch(compiled, /ref:.*pull_request\.head/);
    assert.match(compiled, /ref: \$\{\{ github\.workflow_sha \}\}/);
    assert.match(source, /edit: false/);
    assert.doesNotMatch(source, /push-to-pull-request-branch:|create-pull-request:|contents: write/);
    assert.match(source, /add-comment:\r?\n {4}target: triggering\r?\n {4}max: 1/);
    assert.match(source, /full comparison\r?\nbase and reviewed head SHAs/);
    assert.match(source, /blob\/<revision>\/<path>#L<line>/);
    assert.doesNotMatch(job(source, 'prepare'), /copilot-requests|engine:/);
});

test('hosted research prepares inert evidence before inference without model shell access', () => {
    assert.match(source, /bash: false/);
    assert.match(source, /github: false/);
    assert.doesNotMatch(source, /mcp-scripts:/);
    const agent = job(compiled, 'agent');
    const prepare = agent.indexOf('name: Prepare read-only evidence');
    assert.ok(prepare > 0 && prepare < agent.indexOf('name: Execute GitHub Copilot CLI'));
    assert.match(agent, /BASE: \$\{\{ needs.prepare.outputs.comparison_base \}\}/);
    assert.match(agent, /HEAD_SHA: \$\{\{ github.event.pull_request.head.sha \}\}/);
});

test('the compiled gate skips forks and drafts before preparation', () => {
    const condition = job(compiled, 'prepare').match(/^ {4}if: (.+)$/m)?.[1];
    assert.ok(condition);
    const eligible = new Function('github', `return ${condition};`);
    for (const [draft, id, result] of [[false, 1, true], [false, 2, false], [true, 1, false]]) {
        assert.equal(eligible({ repository_id: 1, event: { pull_request: { draft, head: { repo: { id } } } } }), result);
    }
});

test('evidence contains both revisions and unchanged consumers without checking out candidate instructions', () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'arch-evidence-'));
    const git = (...args) => execFileSync('git', ['-c', 'core.hooksPath=' +
        (process.platform === 'win32' ? 'NUL' : '/dev/null'), '-c', 'commit.gpgsign=false',
    '-c', 'core.autocrlf=false', ...args], { cwd: root, encoding: 'utf8' }).trim();
    try {
        git('init', '-q');
        git('config', 'user.name', 'Evidence Fixture');
        git('config', 'user.email', 'fixture@example.invalid');
        const put = (name, data) => fs.writeFileSync(path.join(root, name), data);
        put('changed.txt', 'before\n');
        put('consumer.txt', 'unchanged consumer\n');
        git('add', '.');
        git('commit', '-qm', 'base');
        const base = git('rev-parse', 'HEAD');
        put('changed.txt', 'after\n');
        put('AGENTS.md', 'candidate instructions\n');
        put('binary.dat', Buffer.from([0]));
        git('add', '.');
        git('commit', '-qm', 'head');
        const head = git('rev-parse', 'HEAD');
        git('checkout', '--quiet', '--detach', base);
        const exporter = fileURLToPath(new URL('../scripts/prepare-evidence.py', import.meta.url));
        execFileSync(process.platform === 'win32' ? 'python' : 'python3', [exporter, base, head, 'evidence'], { cwd: root });
        const evidence = path.join(root, 'evidence');
        const index = revision => Object.fromEntries(fs.readFileSync(path.join(evidence, `${revision}.jsonl`), 'utf8')
            .trim().split('\n').map(line => JSON.parse(line)).map(record => [record.path, record]));
        const before = index('base');
        const after = index('head');
        const read = record => fs.readFileSync(path.join(evidence, record.source), 'utf8');
        assert.equal(read(before['changed.txt']), 'before\n');
        assert.equal(read(after['changed.txt']), 'after\n');
        assert.equal(read(after['AGENTS.md']), 'candidate instructions\n');
        assert.equal(after['consumer.txt'].source, before['consumer.txt'].source);
        assert.equal(read(after['consumer.txt']), 'unchanged consumer\n');
        assert.ok(after['binary.dat'].gap);
        assert.match(fs.readFileSync(path.join(evidence, 'diff.txt'), 'utf8'), /-before\r?\n\+after/);
        assert.equal(git('rev-parse', 'HEAD'), base);
        assert.ok(!fs.existsSync(path.join(root, 'AGENTS.md')));
    } finally {
        fs.rmSync(root, { recursive: true, force: true });
    }
});

test('selected reviewer can report insufficient evidence and the run fails visibly', () => {
    const selected = source.match(/^ {2}agent: ([\w-]+)$/m)?.[1];
    assert.ok(selected);
    const profilePath = `.github/agents/${selected}.agent.md`;
    assert.ok(compiled.includes(`GH_AW_AGENT_FILE: "${profilePath}"`));
    const profile = fs.readFileSync(new URL(`../../../agents/${selected}.agent.md`, import.meta.url), 'utf8');
    const tools = JSON.parse(profile.match(/^tools: (\[.+\])$/m)[1].replaceAll("'", '"'));
    const manifest = JSON.parse(compiled.match(/^# gh-aw-manifest: (.+)$/m)[1]);
    assert.ok(tools.includes('read'));
    assert.ok(manifest.mcp_servers.some(server => server.name === 'safeoutputs' && server.tools.includes('add_comment')));
    assert.match(job(compiled, 'agent'), /"report_incomplete": \{/);
    assert.ok(tools.includes('safeoutputs/add_comment'));
    assert.ok(tools.includes('safeoutputs/report_incomplete'));
});

test('new PR heads preempt the old run independently of size admission or CI events', () => {
    const concurrency = compiled.match(/^concurrency:\r?\n([\s\S]*?)(?=^\S)/m)?.[1];
    assert.ok(concurrency);
    assert.match(concurrency, /cancel-in-progress: true/);
    const group = concurrency.match(/^ {2}group: (.+)$/m)?.[1];
    assert.equal(group, 'ghaw-pr-architecture-design-${{ github.event.pull_request.number }}');
    assert.match(source, /types: \[opened, reopened, synchronize, ready_for_review\]/);
    assert.doesNotMatch(source, /workflow_run:|workflows: \['PR CI'\]/);
    assert.doesNotMatch(source.match(/^if: (.+)$/m)?.[1] ?? '', /admitted|relevantLines|relevantFiles/);
});

test('compiled inference steps isolate telemetry while native runner configuration remains available', () => {
    assert.match(compiled, /^ {2}OTEL_EXPORTER_OTLP_HEADERS:.*secrets\.GH_AW_DEFAULT_OTLP_HEADERS/m);
    for (const id of ['agent', 'detection']) {
        const inference = job(compiled, id).split(/^      - /m).find(step => step.includes('--env-all'));
        assert.ok(inference, `${id} has an inference environment`);
        for (const variable of ['OTEL_EXPORTER_OTLP_HEADERS', 'GH_AW_OTLP_ENDPOINTS', 'GH_AW_OTLP_ALL_HEADERS'])
            assert.match(inference, new RegExp(`${variable}: \\$\\{\\{ '' \\}\\}`));
    }
});

test('compiled publisher revalidates after detection and artifact setup before native mutation', () => {
    const publisher = job(compiled, 'safe_outputs');
    const steps = publisher.split(/^      - /m);
    const check = steps.findIndex(step => step.startsWith('name: Reject stale or incomplete review before publication'));
    const handler = steps.findIndex(step => step.startsWith('name: Process Safe Outputs'));
    const download = steps.findIndex(step => step.includes('uses: actions/download-artifact@'));
    assert.ok(check > download && download > 0);
    assert.ok(handler > check);
    assert.match(steps[check - 1], /uses: actions\/checkout@/);
    assert.match(steps[check - 1], /ref: \$\{\{ github\.workflow_sha \}\}/);
    assert.doesNotMatch(steps[check], /continue-on-error: true/);
    assert.doesNotMatch(steps[handler], /\b(?:always|failure|cancelled)\(\)/);
    assert.match(publisher, /permissions:\r?\n {6}contents: read/);
});

test('actual publisher check rejects drift and missing or incomplete reports', async () => {
    const block = source.match(/^safe-outputs:\r?\n([\s\S]*?)(?=^\S)/m)?.[1];
    const script = block?.match(/script: \|\r?\n((?: {10}[^\r\n]*(?:\r?\n|$))+)/)?.[1].split(/\r?\n/)
        .map(line => line.slice(10)).join('\n');
    assert.ok(script);
    const execute = new AsyncFunction('github', 'context', 'process', 'require', script);
    const repo = { id: 4, full_name: 'example/project' };
    const expected = { number: 2, state: 'open', draft: false,
        base: { sha: 'b'.repeat(40), ref: 'main', repo },
        head: { sha: 'a'.repeat(40), ref: 'feature', repo } };
    const context = { payload: { pull_request: expected, repository: repo }, repo: { owner: 'example', repo: 'project' } };
    const environment = { env: { GITHUB_WORKSPACE: fileURLToPath(new URL('../../../../', import.meta.url)) } };
    const require = createRequire(import.meta.url);
    for (const mutation of [null, 'head', 'base', 'closed', 'draft', 'incomplete', 'missing', 'mixed']) {
        const pr = structuredClone(expected);
        if (mutation === 'head') pr.head.sha = 'c'.repeat(40);
        if (mutation === 'base') pr.base.sha = 'c'.repeat(40);
        if (mutation === 'closed') pr.state = 'closed';
        if (mutation === 'draft') pr.draft = true;
        const github = { rest: { pulls: { get: async request => {
            assert.equal(request.pull_number, 2);
            return { data: pr };
        } } } };
        const report = { type: 'add_comment', body: 'Review at the admitted head' };
        const incomplete = { type: 'report_incomplete', reason: 'Source unavailable' };
        const items = mutation === 'incomplete' ? [incomplete] : mutation === 'missing' ? [] :
            mutation === 'mixed' ? [report, incomplete] : [report];
        const read = module => module === 'node:fs' ? { readFileSync: () => JSON.stringify({ items }) } :
            require(module);
        let published = false;
        const publishAfterDetection = async () => {
            await execute(github, context, environment, read);
            published = true;
        };
        if (mutation) await assert.rejects(publishAfterDetection(), /stale|complete report/);
        else await publishAfterDetection();
        assert.equal(published, mutation === null);
    }
});
