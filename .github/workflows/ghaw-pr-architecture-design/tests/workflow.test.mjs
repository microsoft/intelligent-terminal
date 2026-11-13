import assert from 'node:assert/strict';
import fs from 'node:fs';
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

test('hosted evidence has no Git shell wildcards and imports only fixed trusted readers and scope', () => {
    assert.match(source, /bash: false/);
    assert.doesNotMatch(source, /git (diff|grep|show|ls-tree):\*/);
    assert.match(source, /Allowed enum: diff, source, search, tree/);
    assert.match(source, /Allowed enum: base, head/);
    assert.match(compiled, /evidence-reader\.mjs/);
    assert.match(compiled, /ARCHITECTURE_RUN_DIR.*|scope\.json/);
});

test('selected reviewer exposes the compiled evidence and report MCP tools', () => {
    const selected = source.match(/^ {2}agent: ([\w-]+)$/m)?.[1];
    assert.ok(selected);
    const profilePath = `.github/agents/${selected}.agent.md`;
    assert.ok(compiled.includes(`GH_AW_AGENT_FILE: "${profilePath}"`));
    const profile = fs.readFileSync(new URL(`../../../agents/${selected}.agent.md`, import.meta.url), 'utf8');
    const tools = JSON.parse(profile.match(/^tools: (\[.+\])$/m)[1].replaceAll("'", '"'));
    const manifest = JSON.parse(compiled.match(/^# gh-aw-manifest: (.+)$/m)[1]);
    for (const name of ['read-architecture-evidence', 'add_comment']) {
        const server = manifest.mcp_servers.find(server => server.tools.includes(name));
        assert.ok(server, `Missing compiled tool ${name}`);
        assert.ok(tools.includes(`${server.name}/${name}`), `Selected profile excludes ${server.name}/${name}`);
    }
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
    const check = steps.findIndex(step => step.startsWith('name: Reject stale review before publication'));
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

test('actual publisher check rejects changes during detection before handler execution', async () => {
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
    for (const mutation of [null, 'head', 'base', 'closed', 'draft']) {
        const pr = structuredClone(expected);
        if (mutation === 'head') pr.head.sha = 'c'.repeat(40);
        if (mutation === 'base') pr.base.sha = 'c'.repeat(40);
        if (mutation === 'closed') pr.state = 'closed';
        if (mutation === 'draft') pr.draft = true;
        const github = { rest: { pulls: { get: async request => {
            assert.equal(request.pull_number, 2);
            return { data: pr };
        } } } };
        let published = false;
        const publishAfterDetection = async () => {
            await execute(github, context, environment, createRequire(import.meta.url));
            published = true;
        };
        if (mutation) await assert.rejects(publishAfterDetection(), /stale/);
        else await publishAfterDetection();
        assert.equal(published, mutation === null);
    }
});
