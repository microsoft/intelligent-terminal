import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { validateCiDefinition, validateCiPull, validateCiRun, verifyCiSecurity } from '../../../skills/ghaw-pr-security/scripts/security-review.mjs';

export async function admitPerformanceCi(github, context, trustedSha) {
    if (!/^[0-9a-f]{40}$/.test(trustedSha ?? '')) throw Error('Invalid trusted CI revision.');
    const source = context.payload.workflow_run;
    const run = (await github.rest.actions.getWorkflowRun({ ...context.repo, run_id: source.id })).data;
    const identity = { repository: `${context.repo.owner}/${context.repo.repo}`,
        repositoryId: context.payload.repository.id, runId: String(source.id),
        runAttempt: source.run_attempt, headSha: source.head_sha,
        defaultBranch: context.payload.repository.default_branch };
    validateCiRun(run, identity);
    const workflow = (await github.rest.actions.getWorkflow({ ...context.repo, workflow_id: 'pr-ci.yml' })).data;
    if (workflow.id !== run.workflow_id || workflow.path !== '.github/workflows/pr-ci.yml' || workflow.state !== 'active')
        throw Error('CI workflow is not the active trusted workflow.');
    const comparison = (await github.rest.repos.compareCommits({
        ...context.repo, base: trustedSha, head: run.head_sha, per_page: 1 })).data;
    if (!/^[0-9a-f]{40}$/.test(comparison.merge_base_commit?.sha ?? '')) throw Error('Invalid CI comparison base.');
    const definitions = await Promise.all([run.head_sha, comparison.merge_base_commit.sha].map(async tree_sha =>
        (await github.rest.git.getTree({ ...context.repo, tree_sha, recursive: '1' })).data));
    validateCiDefinition(...definitions);
    const associated = await github.paginate(github.rest.repos.listPullRequestsAssociatedWithCommit,
        { ...context.repo, commit_sha: run.head_sha, per_page: 100 });
    const candidates = associated.filter(pr => pr.state === 'open' && pr.head?.sha === run.head_sha &&
        pr.head?.ref === run.head_branch && pr.head?.repo?.id === identity.repositoryId &&
        pr.base?.repo?.id === identity.repositoryId);
    if (candidates.length !== 1) throw Error('Expected exactly one current same-repository CI pull request.');
    const pr = (await github.rest.pulls.get({ ...context.repo, pull_number: candidates[0].number })).data;
    validateCiPull(pr, run, { ...identity, prNumber: pr.number });
    return pr.draft ? null : { pr, run };
}

export function validatePerformanceArtifacts(artifacts, run) {
    return ['cpp', 'rust'].map(language => {
        const name = `ci-performance-${language}-${run.id}-${run.run_attempt}`;
        const matches = artifacts.filter(item => item.name === name);
        if (matches.length !== 1 || matches[0].expired !== false ||
            !Number.isSafeInteger(matches[0].id) || matches[0].id < 1 ||
            matches[0].workflow_run?.id !== run.id || matches[0].workflow_run?.head_sha !== run.head_sha)
            throw Error('Performance CI artifact is missing, expired or ambiguous.');
        return { language, id: matches[0].id, name };
    });
}

export function validatePerformanceResult(result, language, identity) {
    if (result?.headSha !== identity.headSha || result.ciRunId !== identity.runId ||
        result.ciRunAttempt !== String(identity.runAttempt) || result.language !== language ||
        !['completed', 'skipped'].includes(result.status) || !Array.isArray(result.phases) ||
        result.phases.some(phase => phase.exitCode !== 0) ||
        (result.status === 'completed' && !result.phases.length) ||
        (result.status === 'skipped' && (typeof result.reason !== 'string' || !result.reason.trim())))
        throw Error('Performance CI evidence does not match the successful analysis identity.');
}

export function verifyPerformanceCi({ environment = process.env, request } = {}) {
    const repository = environment.GITHUB_REPOSITORY;
    const api = request ?? (endpoint => JSON.parse(execFileSync('gh',
        ['api', `/repos/${repository}${endpoint ? `/${endpoint}` : ''}`],
        { encoding: 'utf8', timeout: 30000, maxBuffer: 16 * 1024 * 1024 })));
    const verified = verifyCiSecurity({ environment, request: api });
    const artifacts = [];
    for (let page = 1; ; page++) {
        if (page > 20) throw Error('CI artifact list exceeds the admission limit.');
        const batch = api(`actions/runs/${verified.run.id}/artifacts?per_page=100&page=${page}`).artifacts;
        if (!Array.isArray(batch)) throw Error('Invalid CI artifact list.');
        artifacts.push(...batch);
        if (batch.length < 100) break;
    }
    return { ...verified, artifacts: validatePerformanceArtifacts(artifacts, verified.run) };
}

function collect(output) {
    const verified = verifyPerformanceCi();
    fs.mkdirSync(output, { recursive: true });
    for (const artifact of verified.artifacts) {
        const directory = path.join(output, artifact.language);
        fs.mkdirSync(directory);
        execFileSync('gh', ['run', 'download', verified.identity.runId, '--repo',
            process.env.GITHUB_REPOSITORY, '--name', artifact.name, '--dir', directory],
        { stdio: 'pipe', timeout: 30000 });
        const names = fs.readdirSync(directory);
        if (!names.includes('native-results.json') || names.length > 12)
            throw Error('Invalid performance artifact layout.');
        for (const name of names) {
            const stat = fs.lstatSync(path.join(directory, name));
            if (!/^(native-results\.json|tool-version\.log|diagnostics(?:-\d+\.log|\.jsonl)|project-\d+\.json)$/.test(name) ||
                !stat.isFile() || stat.isSymbolicLink() || stat.size > 16 * 1024 * 1024)
                throw Error('Invalid performance artifact file.');
        }
        validatePerformanceResult(JSON.parse(fs.readFileSync(path.join(directory, 'native-results.json'), 'utf8')
            .replace(/^\uFEFF/, '')), artifact.language, verified.identity);
    }
    const confirmed = verifyPerformanceCi();
    if (JSON.stringify(confirmed.identity) !== JSON.stringify(verified.identity) ||
        JSON.stringify(confirmed.artifacts) !== JSON.stringify(verified.artifacts))
        throw Error('CI identity changed during evidence download.');
    fs.writeFileSync(path.join(output, 'identity.json'), JSON.stringify(verified.identity));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
    if (process.argv[2] !== 'collect' || !process.argv[3]) throw Error('Expected collect and an output directory.');
    collect(path.resolve(process.argv[3]));
}
