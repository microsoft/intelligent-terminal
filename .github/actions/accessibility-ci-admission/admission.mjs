// Reuse the authoritative security CI foundation.
import {
  validateCiDefinition, validateCiRun, validateCiPull, validateCiArtifact,
} from '../../skills/ghaw-pr-security/scripts/security-review.mjs';
import { relevantPath } from '../../../build/scripts/ci-accessibility-scope.mjs';
export { relevantPath };

const sha = value => /^[0-9a-f]{40}$/.test(value ?? '');
const positive = value => /^[1-9][0-9]*$/.test(String(value ?? ''));
const worker = 'ghaw-pr-accessibility.lock.yml';

export function validateBuildJobs(jobs, run) {
  for (const [name, steps] of [
    ['C++ build', ['Build production Terminal and C++ tests', 'Stage ordinary C++ unit test payload',
      'Publish C++ unit test payload']],
    ['C++ unit tests', ['Test all C++ units']],
    ['Rust build and unit tests', ['Build x64 Rust binaries', 'Test all x64 Rust units']],
  ]) {
    const matches = jobs.filter(job => job.name === name);
    if (matches.length !== 1 || matches[0].run_id !== run.id || matches[0].run_attempt !== run.run_attempt ||
        matches[0].head_sha !== run.head_sha || matches[0].status !== 'completed' ||
        matches[0].conclusion !== 'success') throw Error('Required exact-attempt PR build did not complete.');
    for (const step of steps) {
      const executed = (matches[0].steps ?? []).filter(item => item.name === step);
      if (executed.length !== 1 || executed[0].status !== 'completed' || executed[0].conclusion !== 'success') {
        throw Error('Required PR build/test step did not execute successfully.');
      }
    }
  }
}

export function validateTestHostProducer(jobs, run) {
  const producers = jobs.filter(job => job.name === 'C++ build');
  if (producers.length !== 1 || producers[0].run_id !== run.id ||
      producers[0].run_attempt !== run.run_attempt || producers[0].head_sha !== run.head_sha ||
      producers[0].status !== 'completed' || producers[0].conclusion !== 'success') {
    throw Error('Missing successful exact-attempt TestHost producer.');
  }
  for (const name of ['Stage complete original-head TestHost layout', 'Publish accessibility TestHost payload']) {
    const uploads = (producers[0].steps ?? []).filter(step => step.name === name);
    if (uploads.length !== 1 || uploads[0].status !== 'completed' || uploads[0].conclusion !== 'success') {
      throw Error('TestHost publication did not complete successfully.');
    }
  }
}

export function validateNativeJobs(jobs, run) {
  const matches = jobs.filter(job => job.name === 'Native accessibility / Native Axe.Windows smoke');
  if (matches.length !== 1 || matches[0].run_id !== run.id || matches[0].run_attempt !== run.run_attempt ||
      matches[0].head_sha !== run.head_sha || matches[0].status !== 'completed' ||
      matches[0].conclusion !== 'success') throw Error('Native CI evidence incomplete: exact-attempt scan job did not succeed.');
  for (const name of ['Record native CI results', 'Upload native accessibility evidence', 'Enforce native scan results']) {
    const steps = (matches[0].steps ?? []).filter(step => step.name === name);
    if (steps.length !== 1 || steps[0].status !== 'completed' || steps[0].conclusion !== 'success') {
      throw Error('Native CI evidence incomplete: required scan receipt/upload/enforcement did not succeed.');
    }
  }
}

export async function admit({ github, context, trustedSha, deduplicate = false, publishedSha = '', requirePayload = false, requireNative = false }) {
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
  // REST run.pull_requests is mutable. With no immutable original-target field,
  // admit only PRs whose default target never changed; do not infer origin from
  // timing (an old event can be queued across a retarget). Manual review/new PR
  // is required for retargeted PRs, rather than laundering a foreign-base run.
  const provenance = await github.graphql(`query($owner:String!,$repo:String!,$number:Int!) {
    repository(owner:$owner,name:$repo) { pullRequest(number:$number) {
      timelineItems(first:1,itemTypes:[BASE_REF_CHANGED_EVENT,BASE_REF_FORCE_PUSHED_EVENT]) {
        nodes { __typename } pageInfo { hasNextPage }
      }
    } }
  }`, { owner: context.repo.owner, repo: context.repo.repo, number: pr.number });
  const history = provenance?.repository?.pullRequest?.timelineItems;
  // totalCount includes unrelated events even with itemTypes; inspect filtered nodes.
  if (!Array.isArray(history?.nodes) || history.nodes.length !== 0 || history.pageInfo?.hasNextPage !== false) {
    throw Error('Original default-target CI provenance is unavailable or changed; manual review is required.');
  }
  const jobs = await github.paginate(github.rest.actions.listJobsForWorkflowRunAttempt,
    { ...context.repo, run_id: run.id, attempt_number: run.run_attempt, per_page: 100 });
  validateBuildJobs(jobs, run);
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
  // A green workflow with skipped native work is not full relevant-PR CI.
  // Check this before every model entry, not only the archive consumer.
  if (applicable) validateNativeJobs(jobs, run);
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
  const outputs = { pull_request: JSON.stringify(pull), ci_run_id: String(run.id),
    ci_run_attempt: String(run.run_attempt), applicable: String(applicable) };
  if (requirePayload || requireNative) {
    validateTestHostProducer(jobs, run);
    if (requireNative) validateNativeJobs(jobs, run);
    const artifacts = await github.paginate(github.rest.actions.listWorkflowRunArtifacts,
      { ...context.repo, run_id: run.id, per_page: 100 });
    const kind = requireNative ? 'native' : 'testhost';
    const artifact = validateCiArtifact(artifacts, run, `ci-accessibility-${kind}-${run.id}-${run.run_attempt}`);
    if (!/^sha256:[0-9a-f]{64}$/.test(artifact.digest ?? '') ||
        artifact.workflow_run.repository_id !== run.repository.id ||
        artifact.workflow_run.head_repository_id !== run.head_repository.id ||
        !Number.isSafeInteger(artifact.size_in_bytes) || artifact.size_in_bytes < 1 ||
        artifact.size_in_bytes > (requireNative ? 256 * 1024 ** 2 : 2 * 1024 ** 3)) {
      throw Error(`Missing ${kind} artifact integrity identity; evidence incomplete.`);
    }
    outputs.artifact_id = String(artifact.id);
    outputs.artifact_digest = artifact.digest;
  }
  return outputs;
}
