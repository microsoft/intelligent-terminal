#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const SHA = /^[0-9a-f]{40}$/;
const MAX_SUMMARY_BYTES = 48 * 1024;
const SOURCE = /\.(c|cc|cpp|cxx|h|hh|hpp|hlsl|idl|ixx|rs|xaml)$/i;

function fail(message) {
    throw new Error(message);
}

function safePath(filename) {
    if (typeof filename !== 'string' || !filename || filename.startsWith('/') ||
        /[\\:\x00-\x1f\x7f]/.test(filename) ||
        filename.split('/').some(part => !part || part === '.' || part === '..'))
        fail('source path must be canonical and repository-relative');
}

function identity(value) {
    if (!Number.isInteger(value?.prNumber) || value.prNumber < 1 ||
        !SHA.test(value.baseSha ?? '') || !SHA.test(value.headSha ?? ''))
        fail('review identity requires a positive PR number and immutable SHAs');
}

function git(args, options = {}) {
    return execFileSync('git', ['--no-pager', '-c', 'core.fsmonitor=false',
        '-c', `core.hooksPath=${process.platform === 'win32' ? 'NUL' : '/dev/null'}`, ...args], {
        encoding: 'utf8', timeout: 30000, maxBuffer: 16 * 1024 * 1024,
        env: { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_'))),
            GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: process.platform === 'win32' ? 'NUL' : '/dev/null',
            GIT_NO_REPLACE_OBJECTS: '1' },
        ...options,
    });
}

export function classifyPullRequest(files, expected) {
    identity(expected);
    if (!Array.isArray(files) || files.length > 3000) fail('source scope must be a bounded file list');
    const candidates = [];
    const supporting = [];
    for (const file of files) {
        safePath(file.filename);
        const support = /(^|\/)(tests?|ut_[^/]*|ft_[^/]*|LocalTests_[^/]*|UnitTests_[^/]*|WindowsTerminal_UIATests|doc|docs|specs)(\/|$)/i.test(file.filename) ||
            /(^|\/)(tests|test_support|[^/]+_tests)\.rs$/i.test(file.filename);
        const candidate = !support && (file.filename.startsWith('src/') || file.filename.startsWith('tools/wta/')) &&
            (SOURCE.test(file.filename) || /^tools\/wta\/Cargo\.(toml|lock)$/.test(file.filename));
        if (candidate) candidates.push(file);
        else if (support) supporting.push(file);
    }
    return { version: 1, identity: expected, applicable: candidates.length > 0, candidates, supporting };
}

export function prepareScope(expected, directory) {
    identity(expected);
    if (git(['merge-base', expected.baseSha, expected.headSha]).trim() !== expected.baseSha)
        fail('comparison base must be the immutable merge base');
    const files = git(['diff', '--no-ext-diff', '--no-textconv', '--numstat', '--no-renames', '-z',
        expected.baseSha, expected.headSha, '--']).split('\0').filter(Boolean).map(row => {
        const [added, removed, filename, extra] = row.split('\t');
        if (extra !== undefined || !/^(\d+|-)$/.test(added) || !/^(\d+|-)$/.test(removed))
            fail('unexpected source change-size record');
        return { filename, additions: added === '-' ? null : Number(added),
            deletions: removed === '-' ? null : Number(removed) };
    });
    const scope = classifyPullRequest(files, expected);
    fs.mkdirSync(directory, { recursive: true });
    fs.writeFileSync(path.join(directory, 'performance-scope.json'), `${JSON.stringify(scope, null, 2)}\n`);
    fs.writeFileSync(path.join(directory, 'performance-patch.txt'), scope.candidates.length ?
        git(['diff', '--no-ext-diff', '--no-textconv', '--unified=5', expected.baseSha, expected.headSha,
            '--', ...scope.candidates.map(file => `:(literal)${file.filename}`)]) : '');
    return scope;
}

export function verifyPullRequest(pr, expected) {
    identity(expected);
    if (pr?.state !== 'open' || pr.number !== expected.prNumber || pr.head?.sha !== expected.headSha ||
        pr.base?.sha !== expected.expectedBaseSha || pr.base?.repo?.full_name !== expected.repository ||
        pr.head?.repo?.full_name !== expected.headRepository || pr.head?.ref !== expected.headRef ||
        pr.base?.ref !== expected.baseRef || !Number.isInteger(pr.head?.repo?.id) ||
        !Number.isInteger(pr.base?.repo?.id) ||
        (pr.head?.repo?.id === pr.base?.repo?.id) !== (expected.sameRepo === 'true'))
        fail('live PR metadata does not match immutable review inputs');
}

export function verifyFixCommits(expected, revision = 'HEAD') {
    identity(expected);
    const tip = git(['rev-parse', '--verify', `${revision}^{commit}`]).trim();
    git(['merge-base', '--is-ancestor', expected.headSha, tip]);
    const commits = git(['rev-list', '--reverse', `${expected.headSha}..${tip}`]).trim().split('\n').filter(Boolean);
    const allowed = new Set(classifyPullRequest(git(['diff', '--no-ext-diff', '--no-textconv', '--name-only', '--no-renames', '-z',
        expected.baseSha, expected.headSha]).split('\0').filter(Boolean).map(filename => ({ filename })), expected)
        .candidates.map(file => file.filename));
    const findings = new Set();
    const touched = new Set();
    let lines = 0;
    let bytes = 0;
    for (const commit of commits) {
        const parents = git(['show', '-s', '--format=%P', commit]).trim().split(' ');
        if (parents.length !== 1) fail('proposal commits must be linear, one commit per HIGH finding');
        const subject = git(['show', '-s', '--format=%s', commit]).trim();
        const finding = /^Propose HIGH (PERF-[A-Z0-9][A-Z0-9_-]{2,63}): .+/.exec(subject)?.[1];
        if (!finding || findings.has(finding)) fail('each proposal commit needs one distinct HIGH finding identifier');
        findings.add(finding);
        const files = git(['diff', '--no-ext-diff', '--no-textconv', '--name-only', '--no-renames', '-z', parents[0], commit])
            .split('\0').filter(Boolean);
        if (!files.length || files.some(filename => !allowed.has(filename) || !/^tools\/wta\/src\/.+\.rs$/.test(filename)))
            fail('proposal commits must change original candidate WTA source only');
        for (const filename of files) {
            touched.add(filename);
            if (!git(['ls-tree', commit, '--', `:(literal)${filename}`]).startsWith('100644 blob ') ||
                !git(['ls-tree', expected.headSha, '--', `:(literal)${filename}`]).startsWith('100644 blob '))
                fail('proposal must preserve existing regular source files and modes');
        }
        const rows = git(['diff', '--no-ext-diff', '--no-textconv', '--numstat', '--no-renames', parents[0], commit])
            .trim().split('\n').filter(Boolean);
        for (const row of rows) {
            const [added, removed] = row.split('\t');
            if (!/^\d+$/.test(added) || !/^\d+$/.test(removed)) fail('proposal cannot contain binary changes');
            lines += Number(added) + Number(removed);
        }
        bytes += git(['diff', '--no-ext-diff', '--no-textconv', '--unified=0', parents[0], commit], { encoding: null }).length;
        if (touched.size > 3 || lines > 100 || bytes > 16 * 1024)
            fail('proposal exceeds the three-file, 100-line or 16 KiB reviewable-change limit');
    }
    const status = git(['status', '--porcelain', '-z', '--no-renames', '--untracked-files=all']).split('\0').filter(Boolean);
    if (status.some(row => {
        const filename = row.slice(3);
        const instruction = filename === 'AGENTS.md' || filename.startsWith('.github/') || filename.startsWith('.agents/');
        return !instruction && row !== '?? .performance-summary.md';
    })) fail('only the agent summary and framework-restored instructions may remain outside proposal commits');
    return tip;
}

export function verifyProposalTransport(expected, output, resolveTransportPaths, repository) {
    identity(expected);
    if (!Array.isArray(output?.items) || output.items.length !== 1)
        fail('exactly one native push or noop is required');
    const [item] = output.items;
    const recordedHead = git(['rev-parse', 'HEAD']).trim();
    if (item.type === 'noop') {
        if (recordedHead !== expected.headSha) fail('noop cannot discard proposal commits');
        return verifyFixCommits(expected);
    }
    const branch = `performance-proposal-${expected.prNumber}`;
    if (item.type !== 'push_to_pull_request_branch' ||
        (item.branch !== undefined && item.branch !== branch) ||
        (item.repo !== undefined && item.repo !== repository) || output.errors?.length)
        fail('native push must use the proposal branch and repository without ingestion errors');
    const { bundlePath } = resolveTransportPaths({ ...item, branch }, repository);
    if (!bundlePath || !fs.lstatSync(bundlePath).isFile() || fs.lstatSync(bundlePath).isSymbolicLink())
        fail('native push requires a regular captured bundle');
    git(['bundle', 'verify', bundlePath]);
    const heads = git(['bundle', 'list-heads', bundlePath]).trim().split('\n');
    if (heads.length !== 1 || heads[0] !== `${recordedHead} refs/heads/${branch}` ||
        recordedHead === expected.headSha)
        fail('captured bundle tip must match the recorded proposal HEAD');
    const ref = 'refs/performance/validated-proposal';
    git(['fetch', '--no-tags', '--no-write-fetch-head', bundlePath, `+refs/heads/${branch}:${ref}`]);
    const tip = verifyFixCommits(expected, ref);
    if (tip !== recordedHead) fail('captured bundle differs from the recorded proposal HEAD');
    // The pinned collector retains only this native output's type. Its handler
    // needs a branch to resolve transport; restore our fixed source only after
    // validating that exact bundle, never agent-provided path or summary fields.
    item.branch = branch;
    return tip;
}

export function readReviewSummary(file) {
    const filename = path.resolve(file);
    const directory = path.dirname(filename);
    for (const item of [directory, filename]) {
        if (fs.lstatSync(item).isSymbolicLink() ||
            fs.realpathSync(item).toLowerCase() !== item.toLowerCase())
            fail('review summary path must not contain symlinks or reparse points');
    }
    const stat = fs.lstatSync(filename);
    if (!stat.isFile() || stat.size < 1 || stat.size > MAX_SUMMARY_BYTES)
        fail('review summary must be a regular UTF-8 file within 48 KiB');
    const descriptor = fs.openSync(filename, 'r');
    let contents;
    try {
        const opened = fs.fstatSync(descriptor);
        if (!opened.isFile() || opened.ino !== stat.ino || opened.dev !== stat.dev)
            fail('review summary changed during collection');
        const buffer = Buffer.alloc(MAX_SUMMARY_BYTES + 1);
        let length = 0;
        while (length < buffer.length) {
            const count = fs.readSync(descriptor, buffer, length, buffer.length - length, null);
            if (!count) break;
            length += count;
        }
        if (!length || length > MAX_SUMMARY_BYTES) fail('review summary exceeds 48 KiB');
        const current = fs.lstatSync(filename);
        if (current.isSymbolicLink() || current.ino !== opened.ino || current.dev !== opened.dev)
            fail('review summary changed during collection');
        contents = buffer.subarray(0, length);
    } finally { fs.closeSync(descriptor); }
    const summary = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(contents);
    if (!summary.trim()) fail('review summary must contain non-whitespace text');
    if (summary.replaceAll('@', '@\u200b').length > 48 * 1024)
        fail('review summary after mention escaping exceeds the Conversation budget');
    return summary;
}

function option(args, name) {
    const index = args.indexOf(name);
    if (index < 0 || index + 1 >= args.length) fail(`missing ${name}`);
    return args[index + 1];
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    try {
        const [command, ...args] = process.argv.slice(2);
        const expected = { prNumber: Number(option(args, '--pr')), baseSha: option(args, '--base'),
            headSha: option(args, '--head') };
        if (command === 'prepare') prepareScope(expected, option(args, '--output-dir'));
        else if (command === 'verify-commits') {
            const { resolveTransportPaths } = createRequire(import.meta.url)(
                path.resolve(option(args, '--actions-dir'), 'resolve_transport_paths.cjs'));
            const outputFile = option(args, '--agent-output');
            const collected = JSON.parse(fs.readFileSync(outputFile, 'utf8'));
            const tip = verifyProposalTransport(expected, collected, resolveTransportPaths, option(args, '--repo'));
            fs.writeFileSync(outputFile, JSON.stringify(collected));
            process.stdout.write(tip);
        }
        else if (command === 'verify-pr') verifyPullRequest(JSON.parse(fs.readFileSync(option(args, '--input'), 'utf8')), {
            ...expected, expectedBaseSha: option(args, '--expected-base'), repository: option(args, '--repo'),
            headRepository: option(args, '--head-repo'), headRef: option(args, '--head-ref'),
            baseRef: option(args, '--base-ref'), sameRepo: option(args, '--same-repo'),
        });
        else fail('usage: performance-review.mjs prepare|verify-pr|verify-commits');
    } catch (error) {
        console.error(`performance-review: ${error.message}`);
        process.exitCode = 1;
    }
}
