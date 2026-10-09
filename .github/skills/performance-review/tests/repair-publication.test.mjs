import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { publishRepair, reconstructTree } from '../scripts/performance-review.mjs';

function fixture(t) {
    const root = fs.mkdtempSync(path.join(process.cwd(), 'performance-publisher-'));
    const previous = process.cwd();
    process.chdir(root);
    t.after(() => { process.chdir(previous); fs.rmSync(root, { recursive: true, force: true }); });
    const git = args => execFileSync('git', args, { encoding: 'utf8', timeout: 15000 }).trim();
    git(['init', '--quiet', '--initial-branch=main']);
    fs.mkdirSync(path.join(root, 'tools', 'wta', 'src'), { recursive: true });
    const filename = 'tools/wta/src/fixture.rs';
    fs.writeFileSync(path.join(root, 'tools', 'wta', 'src', 'fixture.rs'), 'pub fn render() -> usize { 1 }\n');
    fs.writeFileSync(path.join(root, 'tools', 'wta', 'src', 'unrelated.rs'), 'pub fn unrelated() {}\n');
    git(['add', '.']);
    git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'head']);
    const baseSha = git(['rev-parse', 'HEAD']);
    fs.writeFileSync(path.join(root, 'tools', 'wta', 'src', 'fixture.rs'), 'pub fn render() -> usize { 2 }\n');
    git(['add', '.']);
    git(['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'candidate']);
    const headSha = git(['rev-parse', 'HEAD']);
    const expected = { prNumber: 42, baseSha, headSha, expectedBaseSha: baseSha,
        repository: 'owner/repo', headRepository: 'owner/repo', headRef: 'topic', baseRef: 'main' };
    const files = [{ path: filename, mode: '100644',
        contents: Buffer.from('pub fn render() -> usize { 3 }\n').toString('base64') }];
    const treeSha = reconstructTree(files, headSha);
    const proposal = {
        version: 1, identity: { prNumber: 42, baseSha, headSha }, treeSha, files,
        validationPlan: { type: 'wta-unit', testFilter: 'tests::fixture_preserves_behavior' },
        report: { version: 1, review: 'performance', mode: 'repair',
            identity: { prNumber: 42, baseSha, headSha }, status: 'pending_validation',
            validationPlan: { type: 'wta-unit', testFilter: 'tests::fixture_preserves_behavior' },
            findings: [{
                id: 'PERF-ABC12345', severity: 'high', confidence: 'high', dimension: 'application-performance',
                category: 'wta-runtime', title: 'Repeated quadratic work', affectedScenario: 'Large session refresh',
                location: `${filename}:1`, observed: 'Repeated quadratic traversal', expected: 'Linear traversal',
                impact: 'Repeated path doubles work by input size',
                nativeEnvironment: { architecture: 'windows-x64', details: 'Native focused test fixture' },
                evidence: [{ type: 'complexity-proof', detail: 'Nested traversal repeats for every input item.' }],
                proposedFix: 'Restore the linear iteration', validation: 'Native test requested', fixDisposition: 'proposed',
            }], checks: [{ name: 'Native test', command: 'not run', exitCode: null, status: 'unavailable', detail: 'Awaiting trusted native job' }] },
    };
    const proposalPath = path.join(root, 'proposal.json');
    const write = () => fs.writeFileSync(proposalPath, JSON.stringify(proposal));
    write();
    const calls = [];
    const pr = { number: 42, state: 'open',
        head: { sha: headSha, ref: 'topic', repo: { id: 1, full_name: 'owner/repo' } },
        base: { sha: baseSha, ref: 'main', repo: { id: 1, full_name: 'owner/repo' } } };
    const github = {
        rest: { pulls: { async get() { return { data: pr }; } } },
        async graphql(query, args) {
            calls.push(args.input);
            return { createCommitOnBranch: { commit: { oid: 'c'.repeat(40),
                url: 'https://example.invalid/commit', tree: { oid: treeSha } } } };
        },
    };
    const jobs = [
        ['validate_performance_original_tests', 'List the exact test on original HEAD'],
        ['validate_performance_focused_tests', 'Format and test the exact focused candidate'],
        ['validate_performance_repair', 'Test the exact candidate full suite'],
    ].map(([name, stepName]) => ({ name, conclusion: 'success',
        steps: [{ name: stepName, conclusion: 'success' }] }));
    github.rest.actions = { listJobsForWorkflowRun: 'list-jobs' };
    github.paginate = async (route, options) => {
        assert.equal(options.run_id, 123, 'proof must come from the correlated worker run');
        return jobs;
    };
    return { expected, proposal, proposalPath, workerRunId: 123, write, calls, github, pr, jobs };
}

test('publisher uses immutable head CAS and exactly the native-tested blobs', async t => {
    const f = fixture(t);
    const result = await publishRepair(f);
    assert.equal(result.published, true);
    assert.equal(f.calls[0].expectedHeadOid, f.expected.headSha);
    assert.deepEqual(f.calls[0].fileChanges.additions, f.proposal.files.map(file => ({
        path: file.path, contents: file.contents,
    })));
});

test('publisher rejects an advanced live head without adopting or rebasing it', async t => {
    const f = fixture(t);
    f.pr.head.sha = 'd'.repeat(40);
    await assert.rejects(() => publishRepair(f), /metadata does not match/);
    assert.equal(f.calls.length, 0);
});

test('publisher rejects content altered after native validation', async t => {
    const f = fixture(t);
    f.proposal.files[0].contents = Buffer.from('pub fn tampered() {}\n').toString('base64');
    f.write();
    await assert.rejects(() => publishRepair(f), /exact natively tested tree/);
    assert.equal(f.calls.length, 0);
});

test('publisher independently rejects a replacement outside original PR candidates', async t => {
    const f = fixture(t);
    f.proposal.files[0].path = 'tools/wta/src/unrelated.rs';
    f.proposal.report.findings[0].location = 'tools/wta/src/unrelated.rs:1';
    f.write();
    await assert.rejects(() => publishRepair(f), /outside the immutable original candidate/);
    assert.equal(f.calls.length, 0);
});

test('publisher rejects model-authored pass checks even after GitHub records native success', async t => {
    const f = fixture(t);
    f.proposal.report.checks.push({ name: 'Invented benchmark', command: 'not run', exitCode: 0,
        status: 'pass', detail: 'Model-authored success claim' });
    f.write();
    await assert.rejects(() => publishRepair(f), /cannot contain model-authored pass checks/);
    assert.equal(f.calls.length, 0);
});

test('publisher requires server-recorded native success, not a mutable receipt claim', async t => {
    const f = fixture(t);
    f.jobs[0].conclusion = 'failure';
    f.proposal.nativeValidation = { testsPassed: 999, exitCode: 0, platform: 'windows' };
    f.write();
    await assert.rejects(() => publishRepair(f), /GitHub did not record/);
    assert.equal(f.calls.length, 0);
});

test('publisher does not retry a CAS race after the final metadata check', async t => {
    const f = fixture(t);
    f.github.graphql = async (query, args) => {
        f.calls.push(args.input);
        throw new Error('expectedHeadOid mismatch');
    };
    await assert.rejects(() => publishRepair(f), /expectedHeadOid mismatch/);
    assert.equal(f.calls.length, 1);
});

test('staged publication validates the tree but never mutates GitHub', async t => {
    const f = fixture(t);
    const result = await publishRepair({ ...f, staged: true });
    assert.equal(result.published, false);
    assert.equal(f.calls.length, 0);
});

test('publisher requires every exact phase job and step, rejecting partial or ambiguous proof', async t => {
    const f = fixture(t);
    const original = structuredClone(f.jobs);
    for (let phase = 0; phase < 3; phase++) {
        for (const defect of ['missing', 'skipped', 'failure', 'step-missing', 'step-skipped', 'step-failed', 'step-duplicate', 'wrong-job', 'wrong-step', 'duplicate']) {
            f.jobs.splice(0, f.jobs.length, ...structuredClone(original));
            if (defect === 'missing') f.jobs.splice(phase, 1);
            else if (defect === 'duplicate') f.jobs.push(structuredClone(f.jobs[phase]));
            else if (defect === 'wrong-job') f.jobs[phase].name += '_other';
            else if (defect === 'wrong-step') f.jobs[phase].steps[0].name += ' other';
            else if (defect === 'step-missing') f.jobs[phase].steps = [];
            else if (defect === 'step-duplicate') f.jobs[phase].steps.push({ ...f.jobs[phase].steps[0], conclusion: 'failure' });
            else if (defect.startsWith('step-')) f.jobs[phase].steps[0].conclusion = defect.slice(5);
            else f.jobs[phase].conclusion = defect;
            await assert.rejects(() => publishRepair(f), /GitHub did not record/, `${phase}: ${defect}`);
            assert.equal(f.calls.length, 0);
        }
    }
});
