import test from 'node:test';
import assert from 'node:assert/strict';
import { admit, relevantPath } from './admission.mjs';
import { CI_DEFINITION_PATHS } from '../../skills/ghaw-pr-security/scripts/security-review.mjs';

const HEAD = 'a'.repeat(40), BASE = 'b'.repeat(40), TRUSTED = 'c'.repeat(40);
function fixture(dispatch = false) {
  const repo = { id: 42, full_name: 'microsoft/intelligent-terminal', default_branch: 'main' };
  const run = { id: 91, run_attempt: 2, repository: repo, head_repository: repo,
    path: '.github/workflows/pr-ci.yml', name: 'PR CI', workflow_id: 7,
    event: 'pull_request', head_sha: HEAD, head_branch: 'dev/test',
    status: 'completed', conclusion: 'success' };
  const pr = { number: 17, state: 'open', draft: false,
    head: { sha: HEAD, ref: 'dev/test', repo },
    base: { sha: BASE, ref: 'main', repo } };
  const context = { repo: { owner: 'microsoft', repo: 'intelligent-terminal' },
    runId: 100, ref: 'refs/heads/main', payload: dispatch ? { inputs: {
      ci_run_id: '91', ci_run_attempt: '2', expected_workflow_sha: TRUSTED,
      pull_request: JSON.stringify(pr),
      aw_context: JSON.stringify({ item_type: 'pull_request', item_number: 17,
        repo: repo.full_name, head_sha: HEAD }),
    } } : { workflow_run: structuredClone(run) } };
  const trees = [0, 1].map(() => ({ truncated: false, tree: CI_DEFINITION_PATHS.map(path =>
    ({ path, mode: '100644', type: 'blob', sha: BASE })) }));
  const workflow = { id: 7, path: run.path, state: 'active' };
  const caller = { id: 100, path: '.github/workflows/ghaw-pr-accessibility.lock.yml',
    event: 'workflow_dispatch', head_sha: TRUSTED, repository: repo };
  let associated = [pr], previous = [], files = [{ filename: 'src/cascadia/TerminalApp/Tab.cpp' }];
  const github = { rest: { repos: {
    get: async () => ({ data: repo }),
    compareCommits: async () => ({ data: { merge_base_commit: { sha: BASE } } }),
    listPullRequestsAssociatedWithCommit: 'associated',
  }, actions: {
    getWorkflowRun: async ({ run_id }) => ({ data: Number(run_id) === 100 ? caller : run }),
    getWorkflow: async () => ({ data: workflow }), listWorkflowRuns: 'runs',
  }, git: {
    getTree: async ({ tree_sha }) => ({ data: trees[tree_sha === HEAD ? 0 : 1] }),
  }, pulls: { get: async () => ({ data: pr }), listFiles: 'files' } },
  paginate: async method => ({ associated, runs: previous, files })[method] };
  return { github, context, trustedSha: TRUSTED, repo, run, pr, trees, workflow, caller,
    setAssociated: value => { associated = value; }, setPrevious: value => { previous = value; },
    setFiles: value => { files = value; } };
}

// Negative controls first: no costly work may follow these admission failures.
for (const [name, change] of [
  ['absent CI identity', f => { delete f.context.payload.workflow_run.id; }],
  ['pending', f => { f.run.status = 'in_progress'; f.run.conclusion = null; }],
  ['failed', f => { f.run.conclusion = 'failure'; }],
  ['cancelled', f => { f.run.conclusion = 'cancelled'; }],
  ['stale CI rerun attempt', f => { f.run.run_attempt++; }],
  ['green unrelated job workflow', f => { f.run.workflow_id = 8; }],
  ['wrong workflow path', f => { f.run.path = '.github/workflows/other.yml'; }],
  ['manual CI', f => { f.run.event = 'workflow_dispatch'; }],
  ['push CI', f => { f.run.event = 'push'; }],
  ['merge SHA instead of exact head', f => { f.run.head_sha = BASE; }],
  ['foreign repository', f => { f.run.repository = { ...f.repo, id: 43 }; }],
  ['fork', f => { f.run.head_repository = { ...f.repo, id: 43 }; }],
  ['stale PR head', f => { f.pr.head.sha = BASE; }],
  ['closed PR', f => { f.pr.state = 'closed'; }],
  ['draft', f => { f.pr.draft = true; }],
  ['nondefault target', f => { f.pr.base.ref = 'release'; }],
  ['ambiguous PR fanout', f => { f.setAssociated([f.pr, f.pr]); }],
  ['modified CI implementation', f => { f.trees[0].tree[0].sha = HEAD; }],
  ['truncated provenance', f => { f.trees[0].truncated = true; }],
  ['inactive workflow', f => { f.workflow.state = 'disabled_manually'; }],
]) test(`reject ${name}`, async () => {
  const f = fixture(); change(f);
  await assert.rejects(admit(f));
});

for (const [name, change] of [
  ['missing CI', f => { delete f.context.payload.inputs.ci_run_id; }],
  ['wrong PR', f => { const p = JSON.parse(f.context.payload.inputs.pull_request);
    p.number++; f.context.payload.inputs.pull_request = JSON.stringify(p); }],
  ['wrong aw context', f => { f.context.payload.inputs.aw_context = '{}'; }],
  ['stale base', f => { f.pr.base.sha = HEAD; }],
  ['candidate worker ref', f => { f.context.ref = 'refs/heads/dev/test'; }],
  ['wrong trusted revision', f => { f.context.payload.inputs.expected_workflow_sha = HEAD; }],
  ['reusable caller bypass', f => { f.caller.path = '.github/workflows/other.yml'; }],
  ['candidate-controlled worker', f => { f.caller.head_sha = HEAD; }],
]) test(`dispatch rejects ${name}`, async () => {
  const f = fixture(true); change(f);
  await assert.rejects(admit(f));
});

test('duplicate admitted CI attempt fails; current worker is not its own duplicate', async () => {
  const f = fixture(true);
  f.deduplicate = true;
  f.setPrevious([{ id: 99, display_title: 'Accessibility CI 91-2' }]);
  await assert.rejects(admit(f), /already admitted/);
  f.setPrevious([{ id: 100, display_title: 'Accessibility CI 91-2' }]);
  assert.equal((await admit(f)).ci_run_attempt, '2');
});
test('controller and dispatch accept exact successful PR CI including an empty webhook PR list', async () => {
  for (const dispatch of [false, true]) {
    const f = fixture(dispatch);
    const result = await admit(f);
    assert.equal(result.ci_run_id, '91');
    assert.equal(result.applicable, 'true');
    assert.equal(JSON.parse(result.pull_request).head.sha, HEAD);
  }
});
test('new successful CI attempt is independent of an earlier attempt', async () => {
  const f = fixture(true); f.deduplicate = true;
  f.setPrevious([{ id: 99, display_title: 'Accessibility CI 91-1' }]);
  assert.equal((await admit(f)).ci_run_attempt, '2');
});
test('no relevant scenario retains the zero-model path; rename origins and list limits retain review', async () => {
  const f = fixture();
  f.setFiles([{ filename: 'README.md' }]);
  assert.equal((await admit(f)).applicable, 'false');
  f.setFiles([{ filename: 'archive/old.txt', previous_filename: 'src/cascadia/TerminalApp/Tab.cpp' }]);
  assert.equal((await admit(f)).applicable, 'true');
  f.setFiles(Array(3000).fill({ filename: 'README.md' }));
  assert.equal((await admit(f)).applicable, 'true');
});
test('publication summary can describe confirmed own repairs without claiming fix CI', async () => {
  const f = fixture(true);
  f.pr.head.sha = BASE;
  await assert.rejects(admit(f));
  assert.equal((await admit({ ...f, publishedSha: BASE })).ci_run_id, '91');
  await assert.rejects(admit({ ...f, publishedSha: TRUSTED }));
  f.caller.path = '.github/workflows/other.yml';
  await assert.rejects(admit({ ...f, publishedSha: BASE }));
});
test('original safety and relevance path categories remain eligible', () => {
  for (const path of ['AGENTS.md', '.github/x', 'src/AGENTS.md', 'src/.agents/x',
    'test/accessibility/x', 'src/cascadia/LocalTests_TerminalApp/TestHostApp/x',
    '.github/agents/pr-accessibility.agent.md', '.github/skills/pr-accessibility/SKILL.md',
    '.github/workflows/ghaw-pr-accessibility.lock.yml', 'src/CLAUDE.md', 'src/GEMINI.md',
    'src/copilot-instructions.md', 'src/SKILL.md', 'src/x.instructions.md',
    'src/x.agent.md', 'src/.mcp.json', 'src/.claude/x', 'src/.copilot/x', 'src/.gemini/x',
    'build/scripts/Get-DependenciesFromAppxRecipe.ps1']) assert(relevantPath(path), path);
  assert.equal(relevantPath('tools/wta/src/main.rs'), false);
});
