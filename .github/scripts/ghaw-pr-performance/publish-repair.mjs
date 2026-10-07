import fs from 'node:fs';
import { reconstructTree, renderReport, validateProposal, verifyPullRequest } from '../../skills/pr-performance-review/scripts/performance-review.mjs';

export function nativeValidationSucceeded(jobs) {
    const phases = [
        ['validate_performance_original_tests', 'List the exact test on original HEAD'],
        ['validate_performance_focused_tests', 'Format and test the exact focused candidate'],
        ['validate_performance_repair', 'Test the exact candidate full suite'],
    ];
    return phases.every(([name, stepName]) => {
        const matches = jobs.filter(job => job.name === name);
        const steps = matches[0]?.steps?.filter(step => step.name === stepName) ?? [];
        return matches.length === 1 && matches[0].conclusion === 'success' &&
            steps.length === 1 && steps[0].conclusion === 'success';
    });
}

export async function publishRepair({ github, expected, proposalPath, workerRunId, staged = false }) {
    if (!Number.isSafeInteger(workerRunId) || workerRunId < 1) throw new Error('A correlated worker run is required.');
    const [owner, repo] = expected.repository.split('/');
    const jobs = await github.paginate(github.rest.actions.listJobsForWorkflowRun, {
        owner, repo, run_id: workerRunId, per_page: 100,
    });
    if (!nativeValidationSucceeded(jobs)) throw new Error('GitHub did not record successful native validation.');
    const stat = fs.lstatSync(proposalPath);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 2 * 1024 * 1024) {
        throw new Error('Sealed proposal must be a bounded regular file.');
    }
    const proposal = validateProposal(JSON.parse(fs.readFileSync(proposalPath, 'utf8')), expected);
    const reconstructed = reconstructTree(proposal.files, expected.headSha, expected.baseSha);
    if (reconstructed !== proposal.treeSha) {
        throw new Error('Publication blobs do not reconstruct the exact natively tested tree.');
    }
    const pr = (await github.rest.pulls.get({ owner, repo, pull_number: expected.prNumber })).data;
    verifyPullRequest(pr, { ...expected, sameRepo: 'true' });
    const card = renderReport(proposal.report, { publishedRunId: workerRunId });
    if (staged) return { published: false, treeSha: reconstructed, staged: true };

    // expectedHeadOid is the immutable reviewed head, never a freshly adopted remote head.
    const result = await github.graphql(`
      mutation($input: CreateCommitOnBranchInput!) {
        createCommitOnBranch(input: $input) { commit { oid url tree { oid } } }
      }`, {
        input: {
            branch: { repositoryNameWithOwner: expected.repository, branchName: expected.headRef },
            expectedHeadOid: expected.headSha,
            message: {
                headline: 'Fix evidenced performance regression [performance-reviewer]',
                body: 'Validated against the exact candidate tree on Windows.\n\nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>',
            },
            fileChanges: {
                additions: proposal.files.map(file => ({ path: file.path, contents: file.contents })),
            },
        },
    });
    const commit = result?.createCommitOnBranch?.commit;
    if (!commit?.oid || commit.tree?.oid !== reconstructed) {
        throw new Error('GitHub did not confirm publication of the exact validated tree; do not retry automatically.');
    }
    return {
        published: true, commitSha: commit.oid, url: commit.url, treeSha: reconstructed,
        card,
    };
}
