#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const SHA_PATTERN = /^[0-9a-f]{40}$/;
const SOURCE_EXTENSIONS = new Set(['.c', '.cc', '.cpp', '.cxx', '.h', '.hh', '.hpp', '.hlsl', '.idl', '.ixx', '.rs', '.xaml']);
const LEVELS = ['high', 'medium', 'low'];
const DIMENSIONS = ['application-performance', 'responsiveness', 'memory-growth', 'ci-runtime-cost'];
const PROOF_TYPES = ['measurement', 'complexity-proof', 'blocking-proof', 'resource-proof'];
const STATUSES = ['pass', 'advisory', 'action_required', 'pending_validation', 'blocked'];
const TARGET_ALIASES = ['item_number', 'pr', 'pr_number', 'issue', 'issue_number', 'repo', 'target',
    'target_repo', 'target-repo', 'comment_id', 'reply_to_id', 'discussion_id'];
const CATEGORY_RULES = [
    ['rendering', /^(src\/renderer\/|src\/cascadia\/TerminalControl\/|src\/cascadia\/TerminalCore\/)/i],
    ['text-buffer', /^src\/buffer\//i],
    ['vt-parsing', /^src\/terminal\/(parser|adapter)\//i],
    ['ui-thread', /^src\/cascadia\/(TerminalApp|WindowsTerminal|TerminalSettingsEditor)\//i],
    ['tab-pane-lifecycle', /^src\/cascadia\/TerminalApp\/(Tab|Pane|TerminalPage|TabManagement|AgentPaneContent|SharedWta)/i],
    ['wta-runtime', /^tools\/wta\/(src\/|Cargo\.toml$|Cargo\.lock$)/i],
    ['session-log-enumeration', /^(tools\/wta\/src\/.*(session|log)|src\/cascadia\/TerminalApp\/.*(Session|Log))/i],
];

function fail(message) {
    throw new Error(message);
}

function readJson(filename) {
    const stat = fs.lstatSync(filename);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size < 2 || stat.size > 2 * 1024 * 1024)
        fail(`${filename} must be a regular JSON file within the size limit`);
    return JSON.parse(fs.readFileSync(filename, 'utf8'));
}

function writeJson(filename, value) {
    fs.mkdirSync(path.dirname(filename), { recursive: true });
    fs.writeFileSync(filename, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
}

function requireString(value, label, minimum = 1) {
    if (typeof value !== 'string' || value.trim().length < minimum) fail(`${label} must be a non-empty string`);
}

function validateIdentity(identity, expected) {
    if (!identity || !Number.isInteger(identity.prNumber) || identity.prNumber < 1)
        fail('identity.prNumber must be a positive integer');
    for (const key of ['baseSha', 'headSha']) {
        if (!SHA_PATTERN.test(identity[key] ?? '')) fail(`identity.${key} must be a lowercase 40-character SHA`);
        if (expected?.[key] && identity[key] !== expected[key])
            fail(`identity.${key} does not match the immutable workflow input`);
    }
    if (expected?.prNumber && identity.prNumber !== expected.prNumber)
        fail('identity.prNumber does not match the triggering pull request');
}

function classifyFile(file) {
    const filename = file?.filename;
    if (typeof filename !== 'string' || !filename) fail('every pull request file needs a valid filename');
    if (filename.includes('\\') || filename.startsWith('/') || /[\x00-\x1f\x7f:]/.test(filename) ||
        filename.split('/').some(part => !part || part === '.' || part === '..'))
        fail('pull request filename must be a safe repository-relative path');
    const categories = CATEGORY_RULES.filter(([, pattern]) => pattern.test(filename)).map(([name]) => name);
    const supporting = /(^|\/)(test|tests|ut_[^/]*|ft_[^/]*|WindowsTerminal_UIATests|doc|docs|specs)(\/|$)/i.test(filename) ||
        /\.(md|txt|png|jpg|svg)$/i.test(filename);
    const source = SOURCE_EXTENSIONS.has(path.posix.extname(filename).toLowerCase()) ||
        ['tools/wta/Cargo.toml', 'tools/wta/Cargo.lock'].includes(filename);
    const candidate = categories.length > 0 && source && !supporting;
    return {
        filename, status: file.status ?? 'modified',
        additions: file.additions === null ? null : Number.isInteger(file.additions) ? file.additions : 0,
        deletions: file.deletions === null ? null : Number.isInteger(file.deletions) ? file.deletions : 0,
        role: candidate ? 'candidate' : supporting || /(?:bench|perf|benchmark)/i.test(filename) ? 'supporting' : 'excluded',
        categories,
    };
}

export function classifyPullRequest(filesInput, identity) {
    if (!Array.isArray(filesInput)) fail('pull request file input must be an array');
    if (filesInput.length > 3000) fail('pull request file input must contain at most 3000 files');
    const files = filesInput.map(classifyFile);
    const candidates = files.filter(file => file.role === 'candidate');
    const categories = [...new Set(candidates.flatMap(file => file.categories))].sort();
    const dimensions = [];
    if (categories.some(category => ['rendering', 'text-buffer', 'vt-parsing', 'wta-runtime'].includes(category)))
        dimensions.push('application-performance', 'memory-growth');
    if (categories.some(category => ['rendering', 'ui-thread', 'tab-pane-lifecycle', 'wta-runtime'].includes(category)))
        dimensions.push('responsiveness');
    return {
        version: 1, identity, applicable: candidates.length > 0, categories, dimensions: dimensions.sort(), candidates,
        supporting: files.filter(file => file.role === 'supporting'),
        excluded: files.filter(file => file.role === 'excluded'),
        totals: {
            files: files.length, candidates: candidates.length,
            additions: files.reduce((sum, file) => sum + (file.additions ?? 0), 0),
            deletions: files.reduce((sum, file) => sum + (file.deletions ?? 0), 0),
            binaryFiles: files.filter(file => file.additions === null || file.deletions === null).length,
        },
    };
}

export function validateReport(report, expected) {
    if (!report || report.version !== 1 || report.review !== 'performance') fail('report must be a version 1 performance review');
    validateIdentity(report.identity, expected);
    if (!['repair', 'guide'].includes(report.mode) || (expected?.mode && report.mode !== expected.mode))
        fail('report.mode is invalid or does not match the caller');
    if (!Array.isArray(report.findings) || !Array.isArray(report.checks)) fail('report findings and checks must be arrays');
    if (report.status === 'fixed' || report.findings.some(finding => finding?.fixDisposition === 'fixed'))
        fail('model-authored fixed findings cannot authorize publication');
    const ids = new Set();
    for (const [index, finding] of report.findings.entries()) {
        const prefix = `findings[${index}]`;
        if (!finding || !/^PERF-[A-Z0-9][A-Z0-9_-]{2,63}$/.test(finding.id ?? '') || ids.has(finding.id))
            fail(`${prefix}.id must be a unique stable PERF identifier`);
        ids.add(finding.id);
        if (!LEVELS.includes(finding.severity) || !LEVELS.includes(finding.confidence) || !DIMENSIONS.includes(finding.dimension) ||
            ![...CATEGORY_RULES.map(([name]) => name), 'concurrency', 'other'].includes(finding.category))
            fail(`${prefix} has an invalid severity, confidence, dimension, or category`);
        for (const key of ['title', 'affectedScenario', 'location', 'observed', 'expected', 'impact', 'proposedFix', 'validation'])
            requireString(finding[key], `${prefix}.${key}`, 3);
        if (!['windows-x64', 'windows-arm64', 'not-measured'].includes(finding.nativeEnvironment?.architecture))
            fail(`${prefix}.nativeEnvironment.architecture is invalid`);
        requireString(finding.nativeEnvironment.details, `${prefix}.nativeEnvironment.details`, 3);
        if (!Array.isArray(finding.evidence) || !finding.evidence.length) fail(`${prefix}.evidence must not be empty`);
        for (const evidence of finding.evidence) {
            if (!['source', ...PROOF_TYPES].includes(evidence?.type)) fail(`${prefix} contains an invalid evidence type`);
            requireString(evidence.detail, `${prefix}.evidence.detail`, 3);
            if (evidence.type !== 'measurement') continue;
            if (!['microbenchmark', 'end-to-end', 'profile'].includes(evidence.kind))
                fail(`${prefix} measurement must distinguish microbenchmark, end-to-end, or profile`);
            if (evidence.noisy === true && (!Number.isInteger(evidence.samples) || evidence.samples < 3 ||
                typeof evidence.spread !== 'string' || evidence.spread.length < 3))
                fail(`${prefix} noisy measurement needs at least three samples and reported spread`);
        }
        if (finding.severity === 'high') {
            if (finding.confidence !== 'high' || !finding.evidence.some(evidence => PROOF_TYPES.includes(evidence.type)) ||
                !['proposed', 'manual_required', 'unsafe'].includes(finding.fixDisposition))
                fail(`${prefix} high finding needs high confidence, strong proof, and a valid disposition`);
            if (finding.fixDisposition === 'proposed' && report.mode !== 'repair')
                fail(`${prefix} can be proposed only in repair mode`);
        } else if (finding.fixDisposition !== 'advice_only') fail(`${prefix} medium/low findings must be advice_only`);
    }
    for (const [index, check] of report.checks.entries()) {
        const prefix = `checks[${index}]`;
        for (const key of ['name', 'command', 'detail']) requireString(check?.[key], `${prefix}.${key}`);
        if (!['pass', 'regression', 'noisy', 'unavailable', 'error'].includes(check.status) ||
            !(check.exitCode === null || Number.isInteger(check.exitCode))) fail(`${prefix} has an invalid status or exitCode`);
        if (check.status === 'pass' && check.exitCode !== 0) fail(`${prefix} passing checks must exit 0`);
        if (check.status === 'regression' && !report.findings.length) fail(`${prefix} regression checks require at least one finding`);
    }
    const expectedStatus = report.checks.some(check => check.status === 'error') ? 'blocked' :
        report.findings.some(finding => finding.fixDisposition === 'proposed') ? 'pending_validation' :
        report.findings.some(finding => finding.severity === 'high') ? 'action_required' : report.findings.length ? 'advisory' : 'pass';
    if (report.status !== expectedStatus) fail(`report.status must be ${expectedStatus} for its findings and checks`);
    if (report.status === 'pending_validation') validatePlan(report.validationPlan);
    return report;
}

export function renderReport(report, { publishedRunId } = {}) {
    validateReport(report);
    const published = Number.isSafeInteger(publishedRunId) && publishedRunId > 0;
    const status = published ? 'fixed' : report.status;
    const icon = { action_required: '🔴', blocked: '⚠️', advisory: '🟡', pending_validation: '⏳' }[status] ?? '✅';
    const lines = [
        `## ${icon} Intelligent Terminal performance review`, '',
        `**Status:** \`${status}\` · **Baseline:** \`${report.identity.baseSha.slice(0, 12)}\` · **Head:** \`${report.identity.headSha.slice(0, 12)}\``, '',
    ];
    if (!report.findings.length) lines.push('No evidenced performance findings were identified in the selected hot-path scope.', '');
    for (const finding of report.findings) {
        lines.push(
            `### ${finding.severity.toUpperCase()}: ${finding.title} (\`${finding.id}\`)`, '',
            `**Scenario:** ${finding.affectedScenario}`,
            `**Location:** \`${finding.location}\``,
            `**Dimension / confidence:** ${finding.dimension} / ${finding.confidence}`,
            `**Observed:** ${finding.observed}`,
            `**Expected:** ${finding.expected}`,
            `**Impact:** ${finding.impact}`,
            `**Environment:** ${finding.nativeEnvironment.architecture} — ${finding.nativeEnvironment.details}`,
            `**Evidence:** ${finding.evidence.map(evidence => `${evidence.type}: ${evidence.detail}`).join('; ')}`,
            `**Disposition:** ${published && finding.fixDisposition === 'proposed' ? 'fixed' : finding.fixDisposition} — ${finding.proposedFix}`,
            `**Validation:** ${finding.validation}`, ''
        );
    }
    lines.push('### Checks', '');
    for (const check of report.checks)
        lines.push(`- **${check.name}:** \`${check.status}\` (exit ${check.exitCode ?? 'n/a'}) — ${check.detail}; \`${check.command}\``);
    if (published) lines.push(`- **Native validation:** GitHub recorded Windows validation success in workflow run ${publishedRunId}.`);
    lines.push('', '_Microbenchmarks, end-to-end measurements, responsiveness, memory growth, and CI runtime/cost are reported as distinct evidence; unavailable native measurement is not treated as a pass._');
    return `${lines.join('\n')}\n`;
}

export function gatePublication(scope, report, agentOutput, expected) {
    validateIdentity(scope?.identity, expected);
    if (scope?.version !== 1 || !Array.isArray(scope.candidates) || scope.applicable !== (scope.candidates.length > 0) ||
        scope.candidates.some(file => classifyFile(file).role !== 'candidate')) fail('scope must contain correctly classified product candidates');
    if (!agentOutput || !Array.isArray(agentOutput.items) ||
        (agentOutput.errors !== undefined && !Array.isArray(agentOutput.errors)) || (agentOutput.errors?.length ?? 0) !== 0)
        fail('agent output envelope is invalid or contains ingestion errors');
    const [item] = agentOutput.items;
    if (!scope.applicable) {
        if (report !== null || agentOutput.items.length !== 1 || item?.type !== 'noop')
            fail('non-applicable scope requires exactly one noop and no report');
        return;
    }
    validateReport(report, expected);
    if (expected.mode === 'guide') {
        if (agentOutput.items.length !== 1 || item?.type !== 'add_comment') fail('applicable guide scope requires exactly one add_comment');
        if (TARGET_ALIASES.some(key => Object.hasOwn(item, key))) fail('guidance comment must not override the configured immutable PR target');
        if (item.body !== renderReport(report)) fail('queued comment must exactly match the deterministic report rendering');
        return;
    }
    const proposed = report.findings.some(finding => finding.fixDisposition === 'proposed');
    if (proposed && report.status !== 'pending_validation') fail('native proposals require pending_validation status');
    const type = proposed ? 'validate_performance_repair' : 'noop';
    if (agentOutput.items.length !== 1 || item?.type !== type) fail(`applicable repair scope requires exactly one ${type}`);
    if (proposed && ![true, 'true'].includes(item.confirm)) fail('native validation requires explicit proposal confirmation');
    if (!Array.isArray(expected.changedFiles)) fail('repair publication requires a final changed-file inventory');
    if (!proposed && expected.changedFiles.length) fail('repair noop cannot discard unreported changes');
    const candidates = new Set(scope.candidates.map(file => file.filename));
    if (proposed && (!expected.changedFiles.length || expected.changedFiles.some(filename => !candidates.has(filename))))
        fail('repair changes must be non-empty and limited to original candidate files');
}

export function verifyPullRequest(pr, expected) {
    validateIdentity(expected);
    if (pr?.state !== 'open' || pr.number !== expected.prNumber || pr.head?.sha !== expected.headSha ||
        pr.base?.sha !== expected.expectedBaseSha || pr.base?.repo?.full_name !== expected.repository ||
        pr.head?.repo?.full_name !== expected.headRepository || pr.head?.ref !== expected.headRef || pr.base?.ref !== expected.baseRef ||
        !Number.isInteger(pr.head?.repo?.id) || !Number.isInteger(pr.base?.repo?.id) ||
        (pr.head.repo.id === pr.base.repo.id) !== (expected.sameRepo === 'true'))
        fail('live PR metadata does not match immutable dispatch inputs');
}

export function validateVerdict(verdict, expected) {
    validateIdentity(verdict?.identity, expected);
    if (verdict.version !== 1 || !STATUSES.includes(verdict.status)) fail('worker verdict is malformed');
    if (['action_required', 'blocked'].includes(verdict.status)) fail(`performance review requires action: ${verdict.status}`);
}

function git(args, env = {}, options = {}) {
    return execFileSync('git', ['--no-pager', ...args], {
        timeout: 30000, maxBuffer: 16 * 1024 * 1024, encoding: 'utf8', env: { ...process.env, ...env }, ...options,
    });
}

function changedPaths(base, head) {
    return git(['diff', '--no-ext-diff', '--no-textconv', '--name-only', '--no-renames', '-z', base, head, '--']).split('\0').filter(Boolean);
}

function withIndex(revision, action) {
    const root = fs.mkdtempSync(path.join(git(['rev-parse', '--absolute-git-dir']).trim(), 'performance-index-'));
    const env = { GIT_INDEX_FILE: path.join(root, 'index') };
    try {
        git(['read-tree', revision], env);
        return action(env);
    } finally {
        fs.rmSync(root, { recursive: true, force: true });
    }
}

function workspaceTree() {
    return withIndex('HEAD', env => {
        git(['add', '--all', '--', '.'], env);
        return git(['write-tree'], env).trim();
    });
}

export function prepareScope(expected, outputDirectory, baselinePath) {
    validateIdentity(expected);
    if (git(['merge-base', expected.baseSha, expected.headSha]).trim() !== expected.baseSha)
        fail('comparison base must be the immutable merge base');
    const files = git(['diff', '--no-ext-diff', '--no-textconv', '--numstat', '--no-renames',
        '-z', expected.baseSha, expected.headSha, '--']).split('\0').filter(Boolean).map(row => {
        const [added, removed, filename, extra] = row.split('\t');
        if (extra !== undefined || !/^(\d+|-)$/.test(added) || !/^(\d+|-)$/.test(removed))
            fail('Unexpected Git numstat format while preparing change-size input');
        return { filename, additions: added === '-' ? null : Number(added),
            deletions: removed === '-' ? null : Number(removed) };
    });
    const scope = classifyPullRequest(files, {
        prNumber: expected.prNumber, baseSha: expected.baseSha, headSha: expected.headSha,
    });
    writeJson(path.join(outputDirectory, 'performance-scope.json'), scope);
    const candidates = scope.candidates.map(file => `:(literal)${file.filename}`);
    fs.writeFileSync(path.join(outputDirectory, 'performance-patch.txt'), candidates.length ?
        git(['diff', '--no-ext-diff', '--no-textconv', '--unified=5', expected.baseSha, expected.headSha, '--', ...candidates]) : '', 'utf8');
    if (baselinePath) writeJson(baselinePath, { identity: scope.identity, treeSha: workspaceTree() });
    return scope;
}

function validatePlan(plan) {
    if (plan?.type !== 'wta-unit' || typeof plan.testFilter !== 'string' ||
        !/^[A-Za-z0-9_:]{1,200}$/.test(plan.testFilter) || plan.testFilter === 'module::tests')
        fail('supported focused native WTA validation requires a real test selector read from source, not a placeholder');
}

export function validateProposal(proposal, expected) {
    if (proposal?.version !== 1 || !SHA_PATTERN.test(proposal.treeSha ?? '')) fail('native proposal requires a version and resulting Git tree SHA');
    validateIdentity(proposal.identity, expected);
    validateReport(proposal.report, { ...expected, mode: 'repair' });
    if (proposal.report.status !== 'pending_validation' ||
        proposal.report.findings.some(finding => finding.severity === 'high' && finding.fixDisposition !== 'proposed'))
        fail('proposal must contain only repair-eligible HIGH proposals, not claims of fixes');
    const plan = proposal.validationPlan;
    validatePlan(plan);
    if (proposal.report.validationPlan?.type !== plan.type || proposal.report.validationPlan?.testFilter !== plan.testFilter)
        fail('report and native proposal validation plans must agree');
    if (!Array.isArray(proposal.files) || !proposal.files.length || proposal.files.length > 5)
        fail('repair must replace between one and five existing candidate source files');
    const paths = new Set();
    let size = 0;
    for (const file of proposal.files) {
        if (classifyFile({ filename: file?.path }).role !== 'candidate' || !/^tools\/wta\/src\/.+\.rs$/.test(file.path) ||
            file.mode !== '100644' || paths.has(file.path) || typeof file.contents !== 'string' ||
            Buffer.from(file.contents, 'base64').toString('base64') !== file.contents)
            fail('native repair files must be unique, regular WTA Rust source replacements');
        paths.add(file.path);
        size += Buffer.from(file.contents, 'base64').length;
    }
    if (size > 256 * 1024) fail('repair source content exceeds the localized-fix size limit');
    return proposal;
}

export function reconstructTree(files, headSha, baseSha) {
    if (baseSha) {
        const allowed = new Set(changedPaths(baseSha, headSha).filter(filename => classifyFile({ filename }).role === 'candidate'));
        if (files.some(file => !allowed.has(file.path))) fail('publication replacement is outside the immutable original candidate scope');
    }
    return withIndex(headSha, env => {
        for (const file of files) {
            const entry = git(['ls-tree', headSha, '--', `:(literal)${file.path}`]).trim();
            if (!entry.startsWith(`${file.mode} blob `)) fail('repair may only replace an existing regular file without changing its mode');
            const blob = git(['hash-object', '-w', '--stdin'], {}, { input: Buffer.from(file.contents, 'base64') }).trim();
            git(['update-index', '--add', '--cacheinfo', `${file.mode},${blob},${file.path}`], env);
        }
        return git(['write-tree'], env).trim();
    });
}

export function sealProposal(scope, report, baseline, expected) {
    validateIdentity(baseline?.identity, expected);
    if (!SHA_PATTERN.test(baseline?.treeSha ?? '')) fail('trusted preparation baseline is missing');
    const tree = workspaceTree();
    const changedFiles = changedPaths(baseline.treeSha, tree);
    const files = changedFiles.map(filename => {
        if (!scope.candidates.some(file => file.filename === filename)) fail('repair changes must be confined to original candidate files');
        const entry = git(['ls-tree', tree, '--', `:(literal)${filename}`]).trim().match(/^(100644) blob ([0-9a-f]{40})\t/);
        if (!entry) fail('repair deletes or changes the mode of a source file');
        const content = git(['cat-file', 'blob', entry[2]], {}, { encoding: null, maxBuffer: 256 * 1024 });
        return { path: filename, mode: entry[1], contents: content.toString('base64') };
    });
    const proposal = {
        version: 1, identity: scope.identity, treeSha: reconstructTree(files, expected.headSha, expected.baseSha),
        files, report, validationPlan: report.validationPlan,
    };
    return { proposal: validateProposal(proposal, expected), changedFiles };
}

function option(args, name, required = true) {
    const index = args.indexOf(name);
    if (index >= 0 && index + 1 < args.length) return args[index + 1];
    if (required) fail(`missing ${name}`);
}

function expectedFromArgs(args, requireMode = true) {
    const mode = option(args, '--mode', requireMode);
    return { prNumber: Number(option(args, '--pr')), baseSha: option(args, '--base').toLowerCase(), headSha: option(args, '--head').toLowerCase(), ...(mode ? { mode } : {}) };
}

function main() {
    const [command, ...args] = process.argv.slice(2);
    const expected = expectedFromArgs(args, ['validate', 'render', 'gate'].includes(command));
    switch (command) {
        case 'verify-pr':
            verifyPullRequest(readJson(option(args, '--input')), {
                ...expected, expectedBaseSha: option(args, '--expected-base'), repository: option(args, '--repo'),
                headRepository: option(args, '--head-repo'), headRef: option(args, '--head-ref'),
                baseRef: option(args, '--base-ref'), sameRepo: option(args, '--same-repo'),
            });
            break;
        case 'verdict': {
            const verdict = readJson(option(args, '--input'));
            validateVerdict(verdict, expected);
            if (option(args, '--print-status', false) === 'true') process.stdout.write(verdict.status);
            break;
        }
        case 'prepare': {
            const scope = prepareScope(expected, option(args, '--output-dir'), option(args, '--baseline', false));
            const safeOutputs = option(args, '--safe-outputs', false);
            if (!scope.applicable && safeOutputs)
                fs.appendFileSync(safeOutputs, `${JSON.stringify({ type: 'noop', message: 'No performance-sensitive source changed.' })}\n`);
            break;
        }
        case 'validate-proposal':
        case 'apply-proposal': {
            const proposal = validateProposal(readJson(option(args, '--input')), expected);
            if (command === 'validate-proposal') break;
            if (git(['rev-parse', 'HEAD']).trim() !== expected.headSha) fail('native checkout must equal the immutable reviewed head');
            const tree = reconstructTree(proposal.files, expected.headSha, expected.baseSha);
            if (tree !== proposal.treeSha) fail('proposal blobs do not match the sealed tree');
            git(['read-tree', tree]);
            git(['checkout-index', '--all', '--force']);
            break;
        }
        case 'validate':
        case 'render': {
            const report = validateReport(readJson(option(args, '--report')), expected);
            if (command === 'render') process.stdout.write(renderReport(report));
            break;
        }
        case 'gate': {
            const directory = option(args, '--output-dir');
            // Agent-writable scope and source inventories cannot override immutable Git objects.
            const scope = prepareScope(expected, directory);
            const reportPath = option(args, '--report');
            const report = fs.existsSync(reportPath) ? readJson(reportPath) : null;
            let sealed;
            if (expected.mode === 'repair') {
                const baseline = readJson(option(args, '--baseline'));
                validateIdentity(baseline.identity, expected);
                if (!SHA_PATTERN.test(baseline.treeSha ?? '')) fail('trusted preparation baseline is missing');
                expected.changedFiles = changedPaths(baseline.treeSha, workspaceTree());
                if (report?.findings?.some(finding => finding.fixDisposition === 'proposed')) {
                    sealed = sealProposal(scope, report, baseline, expected);
                    expected.changedFiles = sealed.changedFiles;
                }
            }
            gatePublication(scope, report, readJson(option(args, '--agent-output')), expected);
            if (sealed) writeJson(path.join(directory, 'performance-proposal.json'), sealed.proposal);
            const status = report === null ? 'pass' : report.status;
            writeJson(path.join(directory, 'performance-verdict.json'), { version: 1, identity: scope.identity, status });
            process.stdout.write(status);
            break;
        }
        default:
            fail('usage: performance-review.mjs verify-pr|prepare|validate|render|gate|validate-proposal|apply-proposal|verdict ...');
    }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    try {
        main();
    } catch (error) {
        console.error(`performance-review: ${error.message}`);
        process.exitCode = 1;
    }
}
