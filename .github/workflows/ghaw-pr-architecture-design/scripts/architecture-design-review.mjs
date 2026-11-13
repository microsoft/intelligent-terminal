import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

// Tune these two thresholds here; admission is inclusive OR, not AND.
export const MIN_RELEVANT_LINES = 100;
export const MIN_RELEVANT_FILES = 5;
const SHA = /^[0-9a-f]{40}$/;

function validatePath(filename) {
    if (typeof filename !== 'string' || !filename || filename.startsWith('/') ||
        /[\\:\x00-\x1f\x7f]/.test(filename) ||
        filename.split('/').some(part => !part || part === '.' || part === '..'))
        throw new Error('Change metadata requires canonical repository-relative paths.');
}

function excluded(filename) {
    return /^(dep|oss|vendor|node_modules|Generated Files)\//.test(filename) ||
        /(^|\/)(Cargo\.lock|package-lock\.json|npm-shrinkwrap\.json|yarn\.lock|pnpm-lock\.yaml|packages\.lock\.json)$/.test(filename) ||
        /^\.github\/workflows\/[^/]+\.lock\.yml$/.test(filename);
}

export function parseNumstat(text) {
    if (typeof text !== 'string' || (text && !text.endsWith('\0')))
        throw new Error('Incomplete NUL-delimited change metadata.');
    const parts = text.split('\0');
    parts.pop();
    const changes = [];
    for (let index = 0; index < parts.length; index++) {
        const match = /^(\d+|-)\t(\d+|-)\t(.*)$/.exec(parts[index]);
        if (!match || ((match[1] === '-') !== (match[2] === '-')))
            throw new Error('Invalid numstat change metadata.');
        let filename = match[3];
        let previousFilename;
        if (!filename) {
            previousFilename = parts[++index];
            filename = parts[++index];
            validatePath(previousFilename);
        }
        validatePath(filename);
        changes.push({ filename, ...(previousFilename ? { previousFilename } : {}),
            additions: match[1] === '-' ? null : Number(match[1]),
            deletions: match[2] === '-' ? null : Number(match[2]) });
    }
    return changes;
}

export function classifyChanges(changes) {
    if (!Array.isArray(changes) || changes.length > 10000)
        throw new Error('Change metadata requires a bounded complete file list.');
    let relevantLines = 0;
    const relevant = [];
    const ignored = [];
    const seen = new Set();
    for (const change of changes) {
        validatePath(change?.filename);
        if (change.previousFilename !== undefined) validatePath(change.previousFilename);
        if (seen.has(change.filename)) throw new Error('Duplicate change metadata.');
        seen.add(change.filename);
        const binary = change.additions === null && change.deletions === null;
        if (!binary && (![change.additions, change.deletions].every(
            count => Number.isSafeInteger(count) && count >= 0)))
            throw new Error('Invalid change-size counts.');
        // A rename crossing the exclusion boundary is relevant on either side.
        if (excluded(change.filename) && (!change.previousFilename || excluded(change.previousFilename))) {
            ignored.push(change);
            continue;
        }
        relevant.push(change);
        relevantLines += binary ? 0 : change.additions + change.deletions;
        if (!Number.isSafeInteger(relevantLines)) throw new Error('Change-size count overflow.');
    }
    return { relevantLines, relevantFiles: relevant.length, relevant, ignored,
        admitted: relevantLines >= MIN_RELEVANT_LINES || relevant.length >= MIN_RELEVANT_FILES };
}

export function verifySnapshot(pr, expected, repository) {
    if (!Number.isSafeInteger(expected?.number) || expected.number < 1 ||
        ![expected.head?.sha, expected.base?.sha].every(value => SHA.test(value ?? '')) ||
        !Number.isSafeInteger(repository?.id) || repository.id < 1 ||
        typeof repository.full_name !== 'string' || !repository.full_name ||
        ![expected.head?.ref, expected.base?.ref].every(value => typeof value === 'string' && value))
        throw new Error('Invalid immutable review identity.');
    for (const candidate of [expected, pr]) {
        if (candidate?.number !== expected.number || candidate.state !== 'open' || candidate.draft !== false ||
            candidate.head?.sha !== expected.head.sha || candidate.base?.sha !== expected.base.sha ||
            candidate.head?.ref !== expected.head.ref || candidate.base?.ref !== expected.base.ref ||
            candidate.head?.repo?.id !== repository.id || candidate.base?.repo?.id !== repository.id ||
            candidate.head?.repo?.full_name !== repository.full_name ||
            candidate.base?.repo?.full_name !== repository.full_name)
            throw new Error('PR is stale, draft, closed or outside the admitted repository.');
    }
}

function git(root, args) {
    return execFileSync('git', ['--no-pager', '-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=' +
        (process.platform === 'win32' ? 'NUL' : '/dev/null'), ...args], {
        cwd: root, encoding: 'utf8', timeout: 60000, maxBuffer: 16 * 1024 * 1024,
        env: { ...process.env, GIT_NO_REPLACE_OBJECTS: '1' },
    });
}

export function resolveScope(root, expected) {
    if (![expected?.base?.sha, expected?.head?.sha].every(value => SHA.test(value ?? '')))
        throw new Error('Scope requires immutable SHAs.');
    const comparisonBase = git(root, ['merge-base', expected.base.sha, expected.head.sha]).trim();
    if (!SHA.test(comparisonBase)) throw new Error('Invalid comparison merge base.');
    const changes = parseNumstat(git(root, ['diff', '--no-ext-diff', '--no-textconv',
        '--numstat', '--find-renames=50%', '-z', comparisonBase, expected.head.sha, '--']));
    return { version: 1, prNumber: expected.number, headSha: expected.head.sha,
        baseSha: expected.base.sha, comparisonBase, ...classifyChanges(changes) };
}

export async function prepareReview(github, context, root, directory) {
    const expected = context.payload.pull_request;
    const repository = context.payload.repository;
    verifySnapshot(expected, expected, repository);
    const readPr = async () => (await github.rest.pulls.get({
        ...context.repo, pull_number: expected.number })).data;
    verifySnapshot(await readPr(), expected, repository);
    const remoteRef = `refs/remotes/origin/architecture-review-${expected.number}`;
    git(root, ['-c', 'credential.helper=', '-c', 'credential.helper=!gh auth git-credential',
        'fetch', '--no-tags', 'origin', `refs/pull/${expected.number}/head:${remoteRef}`]);
    if (git(root, ['rev-parse', remoteRef]).trim() !== expected.head.sha)
        throw new Error('Fetched PR head changed before admission.');
    // Fetch the observed base as well; default-branch checkout may target a different base branch.
    git(root, ['-c', 'credential.helper=', '-c', 'credential.helper=!gh auth git-credential',
        'fetch', '--no-tags', 'origin', expected.base.sha]);
    const scope = resolveScope(root, expected);
    verifySnapshot(await readPr(), expected, repository);
    if (directory) {
        fs.mkdirSync(directory, { recursive: true });
        fs.writeFileSync(path.join(directory, 'scope.json'), `${JSON.stringify(scope, null, 2)}\n`);
    }
    return scope;
}
