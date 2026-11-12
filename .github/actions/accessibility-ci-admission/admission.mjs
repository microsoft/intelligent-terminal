// Reuse the authoritative security CI foundation; accessibility consumes no CI artifacts.
import {
  validateCiDefinition, validateCiRun, validateCiPull,
} from '../../skills/ghaw-pr-security/scripts/security-review.mjs';

const sha = value => /^[0-9a-f]{40}$/.test(value ?? '');
const positive = value => /^[1-9][0-9]*$/.test(String(value ?? ''));
const worker = 'ghaw-pr-accessibility.lock.yml';

export function relevantPath(path) {
  return /^src\/cascadia\/(TerminalApp|TerminalControl|TerminalSettingsEditor|WindowsTerminal|WindowsTerminal_UIATests|UIMarkdown|LocalTests_TerminalApp\/TestHostApp)\//.test(path) ||
    /(^|\/)\.github\//.test(path) || /^test\/accessibility\//.test(path) ||
    path === 'build/scripts/Get-DependenciesFromAppxRecipe.ps1' ||
    /(^|\/)(AGENTS\.md|CLAUDE\.md|GEMINI\.md|copilot-instructions\.md|SKILL\.md|[^/]+\.instructions\.md|[^/]+\.agent\.md|\.mcp\.json)$/.test(path) ||
    /(^|\/)\.(claude|copilot|agents|gemini)\//.test(path);
}

export async function admit({ github, context, trustedSha, deduplicate = false, publishedSha = '' }) {
  const repository = `${context.repo.owner}/${context.repo.repo}`;
  const metadata = (await github.rest.repos.get(context.repo)).data;
  const inputs = context.payload.inputs;
  const source = context.payload.workflow_run;
  if (!sha(trustedSha) || !positive(source?.id ?? inputs?.ci_run_id) ||
      !positive(source?.run_attempt ?? inputs?.ci_run_attempt)) throw Error('Missing CI admission identity.');
  // Dispatch runs and all reusable callees must execute default-branch control code,
  // never a PR branch or a caller-selected workflow revision.
  if (!source && (context.ref !== `refs/heads/${metadata.default_branch}` ||
      inputs.expected_workflow_sha !== trustedSha)) throw Error('Untrusted worker revision.');
  if (!source) {
    const caller = (await github.rest.actions.getWorkflowRun({
      ...context.repo, run_id: context.runId,
    })).data;
    if (caller.path !== `.github/workflows/${worker}` || caller.event !== 'workflow_dispatch' ||
        caller.head_sha !== trustedSha || caller.repository?.id !== metadata.id) {
      throw Error('Reusable accessibility work requires the trusted admitted worker.');
    }
  }
  const run = (await github.rest.actions.getWorkflowRun({
    ...context.repo, run_id: source?.id ?? inputs.ci_run_id,
  })).data;
  const expected = source ?? JSON.parse(inputs.pull_request);
  const identity = {
    repository, repositoryId: metadata.id, defaultBranch: metadata.default_branch,
    runId: String(source?.id ?? inputs.ci_run_id),
    runAttempt: Number(source?.run_attempt ?? inputs.ci_run_attempt),
    headSha: source?.head_sha ?? expected.head.sha,
  };
  validateCiRun(run, identity);
  const workflow = (await github.rest.actions.getWorkflow({
    ...context.repo, workflow_id: 'pr-ci.yml',
  })).data;
  if (workflow.id !== run.workflow_id || workflow.path !== '.github/workflows/pr-ci.yml' ||
      workflow.state !== 'active') throw Error('CI source workflow is not trusted.');
  const comparison = (await github.rest.repos.compareCommits({
    ...context.repo, base: trustedSha, head: run.head_sha, per_page: 1,
  })).data;
  if (!sha(comparison.merge_base_commit?.sha)) throw Error('Invalid CI comparison base.');
  const trees = await Promise.all([run.head_sha, comparison.merge_base_commit.sha].map(async tree_sha =>
    (await github.rest.git.getTree({ ...context.repo, tree_sha, recursive: '1' })).data));
  validateCiDefinition(...trees);
  const associated = await github.paginate(github.rest.repos.listPullRequestsAssociatedWithCommit,
    { ...context.repo, commit_sha: run.head_sha, per_page: 100 });
  const candidates = associated.filter(pr => pr.state === 'open' &&
    (pr.head?.sha === run.head_sha || (!source && publishedSha &&
      pr.number === expected.number && pr.head?.sha === publishedSha)) && pr.head?.ref === run.head_branch &&
    pr.head?.repo?.id === metadata.id && pr.base?.repo?.id === metadata.id);
  if (candidates.length !== 1) throw Error('Expected exactly one current CI pull request.');
  const pr = (await github.rest.pulls.get({ ...context.repo, pull_number: candidates[0].number })).data;
  // The trusted worker alone wires safe_outputs' confirmed tip into discovery/summary.
  // Both still concern the original CI-tested revision, not fix validation.
  // External head changes remain stale; no arbitrary caller can supply this exception.
  let reviewedPull = pr;
  if (!source && publishedSha && pr.head.sha === publishedSha && sha(publishedSha)) {
    reviewedPull = { ...pr, head: { ...pr.head, sha: identity.headSha } };
  }
  validateCiPull(reviewedPull, run, { ...identity, prNumber: pr.number,
    ...(!source && { baseSha: expected.base.sha, headRef: expected.head.ref }) });
  if (pr.draft) throw Error('Draft pull requests are not eligible.');
  if (!source) {
    const aw = JSON.parse(inputs.aw_context ?? '{}');
    if (expected.number !== pr.number || expected.head.repo.id !== metadata.id ||
        expected.head.repo.full_name !== repository || expected.base.repo.id !== metadata.id ||
        expected.base.ref !== pr.base.ref || aw.item_type !== 'pull_request' ||
        aw.item_number !== pr.number || aw.repo !== repository ||
        aw.head_sha !== run.head_sha) throw Error('Dispatch PR context differs from admitted CI.');
  }
  const files = await github.paginate(github.rest.pulls.listFiles,
    { ...context.repo, pull_number: pr.number, per_page: 100 });
  // listFiles is capped at 3000 by GitHub; uncertainty must retain review.
  const applicable = files.length >= 3000 ||
    files.some(file => relevantPath(file.filename) || relevantPath(file.previous_filename ?? ''));
  if (deduplicate) {
    const runs = await github.paginate(github.rest.actions.listWorkflowRuns,
      { ...context.repo, workflow_id: worker, event: 'workflow_dispatch', per_page: 100 });
    const title = `Accessibility CI ${run.id}-${run.run_attempt}`;
    if (runs.some(other => other.display_title === title &&
        Number(other.id) < Number(context.runId))) throw Error('CI attempt already admitted.');
  }
  // Pass only immutable routing fields, not PR prose or other candidate-controlled data.
  const pull = { number: pr.number, head: { sha: pr.head.sha, ref: pr.head.ref,
    repo: { id: metadata.id, full_name: repository } },
    base: { sha: pr.base.sha, ref: pr.base.ref, repo: { id: metadata.id, full_name: repository } } };
  return { pull_request: JSON.stringify(pull), ci_run_id: String(run.id),
    ci_run_attempt: String(run.run_attempt), applicable: String(applicable) };
}
