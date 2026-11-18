export const MIN_RELEVANT_LINES = 100;
export const MIN_RELEVANT_FILES = 5;

function excluded(filename) {
    return /^(dep|oss|vendor|node_modules|Generated Files)\//.test(filename) ||
        /(^|\/)(Cargo\.lock|package-lock\.json|npm-shrinkwrap\.json|yarn\.lock|pnpm-lock\.yaml|packages\.lock\.json)$/.test(filename) ||
        /^\.github\/workflows\/[^/]+\.lock\.yml$/.test(filename);
}

export function classifyChanges(files) {
    let relevantLines = 0;
    let relevantFiles = 0;
    const seen = new Set();
    for (const file of files) {
        if (typeof file.filename !== 'string' || seen.has(file.filename) ||
            ![file.additions, file.deletions].every(value => Number.isSafeInteger(value) && value >= 0))
            throw Error('Invalid or duplicate changed-file metadata.');
        seen.add(file.filename);
        if (excluded(file.filename) && (!file.previous_filename || excluded(file.previous_filename))) continue;
        relevantFiles++;
        relevantLines += file.additions + file.deletions;
        if (!Number.isSafeInteger(relevantLines)) throw Error('Change-size count overflow.');
    }
    return { relevantLines, relevantFiles,
        admitted: relevantLines >= MIN_RELEVANT_LINES || relevantFiles >= MIN_RELEVANT_FILES };
}

export function verifySnapshot(pr, expected, repository) {
    if (!Number.isSafeInteger(expected?.number) || expected.number < 1 ||
        ![expected.head?.sha, expected.base?.sha].every(value => /^[0-9a-f]{40}$/.test(value ?? '')) ||
        !Number.isSafeInteger(repository?.id) || typeof repository.full_name !== 'string')
        throw Error('Invalid review identity.');
    for (const candidate of [expected, pr]) {
        if (candidate?.number !== expected.number || candidate.state !== 'open' || candidate.draft !== false ||
            candidate.head?.sha !== expected.head.sha || candidate.base?.sha !== expected.base.sha ||
            candidate.head?.ref !== expected.head.ref || candidate.base?.ref !== expected.base.ref ||
            candidate.head?.repo?.id !== repository.id || candidate.base?.repo?.id !== repository.id ||
            candidate.head?.repo?.full_name !== repository.full_name ||
            candidate.base?.repo?.full_name !== repository.full_name)
            throw Error('PR is stale, draft, closed or outside the admitted repository.');
    }
}

export async function prepareReview(github, context) {
    const expected = context.payload.pull_request;
    const repository = context.payload.repository;
    const request = { ...context.repo, pull_number: expected.number };
    const readPr = async () => (await github.rest.pulls.get(request)).data;
    verifySnapshot(expected, expected, repository);
    const pr = await readPr();
    verifySnapshot(pr, expected, repository);
    // GitHub returns at most 3,000 PR files; never treat a partial list as complete.
    if (!Number.isSafeInteger(pr.changed_files) || pr.changed_files < 0 || pr.changed_files > 3000)
        throw Error('Complete PR file metadata is unavailable (GitHub limit: 3,000 files).');
    const files = await github.paginate(github.rest.pulls.listFiles, { ...request, per_page: 100 });
    if (files.length !== pr.changed_files) throw Error('Incomplete or changing PR file list.');
    const { data: comparison } = await github.rest.repos.compareCommits({
        ...context.repo, base: expected.base.sha, head: expected.head.sha, per_page: 1 });
    const comparisonBase = comparison.merge_base_commit?.sha;
    if (!/^[0-9a-f]{40}$/.test(comparisonBase ?? '')) throw Error('Missing comparison merge base.');
    verifySnapshot(await readPr(), expected, repository);
    return { comparisonBase, ...classifyChanges(files) };
}
