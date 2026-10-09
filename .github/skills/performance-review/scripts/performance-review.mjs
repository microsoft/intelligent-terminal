#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const SHA_PATTERN = /^[0-9a-f]{40}$/;
const MAX_REPAIR_FILES = 3;
const MAX_REPAIR_LINES = 100;
const MAX_REPAIR_DIFF_BYTES = 16 * 1024;
const MAX_REPAIR_BLOB_BYTES = 256 * 1024;
const MAX_SUMMARY_BYTES = 48 * 1024;
const SOURCE_EXTENSIONS = new Set(['.c', '.cc', '.cpp', '.cxx', '.h', '.hh', '.hpp', '.hlsl', '.idl', '.ixx', '.rs', '.xaml']);
const LEVELS = ['high', 'medium', 'low'];
const DIMENSIONS = ['application-performance', 'responsiveness', 'memory-growth', 'ci-runtime-cost'];
const PROOF_TYPES = ['measurement', 'complexity-proof', 'blocking-proof', 'resource-proof'];
const STATUSES = ['pass', 'advisory', 'action_required', 'pending_validation', 'blocked'];
const TARGET_ALIASES = ['item_number', 'pr', 'pr_number', 'issue', 'issue_number', 'repo', 'target',
    'target_repo', 'target-repo', 'comment_id', 'reply_to_id', 'discussion_id'];
const CATEGORY_RULES = [
    ['other', /^src\/(inc|til|types)\//i],
    ['rendering', /^(src\/renderer\/|src\/cascadia\/TerminalControl\/|src\/cascadia\/TerminalCore\/)/i],
    ['text-buffer', /^src\/buffer\//i],
    ['vt-parsing', /^src\/terminal\/(parser|adapter)\//i],
    ['ui-thread', /^src\/cascadia\/(TerminalApp|WindowsTerminal|TerminalSettingsEditor)\//i],
    ['tab-pane-lifecycle', /^src\/cascadia\/TerminalApp\/(Tab|Pane|TerminalPage|TabManagement|AgentPaneContent|SharedWta)/i],
    ['wta-runtime', /^tools\/wta\/(src\/|Cargo\.toml$|Cargo\.lock$|build\.rs$)/i],
    ['session-log-enumeration', /^(tools\/wta\/src\/.*(session|log)|src\/cascadia\/TerminalApp\/.*(Session|Log))/i],
];

const CPP_PROJECTS = [
    ['src/renderer/atlas/', 'src/renderer/atlas/atlas.vcxproj'],
    ['src/renderer/base/', 'src/renderer/base/lib/base.vcxproj'],
    ['src/renderer/gdi/', 'src/renderer/gdi/lib/gdi.vcxproj'],
    ['src/renderer/uia/', 'src/renderer/uia/lib/uia.vcxproj'],
    ['src/renderer/wddmcon/', 'src/renderer/wddmcon/lib/wddmcon.vcxproj'],
    ['src/buffer/out/', 'src/buffer/out/lib/bufferout.vcxproj'],
    ['src/terminal/parser/', 'src/terminal/parser/lib/parser.vcxproj'],
    ['src/terminal/adapter/', 'src/terminal/adapter/lib/adapter.vcxproj'],
    ['src/cascadia/TerminalCore/', 'src/cascadia/TerminalCore/lib/terminalcore-lib.vcxproj'],
    ['src/cascadia/TerminalControl/', 'src/cascadia/TerminalControl/TerminalControlLib.vcxproj'],
    ['src/cascadia/TerminalApp/', 'src/cascadia/TerminalApp/TerminalAppLib.vcxproj'],
    ['src/cascadia/WindowsTerminal/', 'src/cascadia/WindowsTerminal/WindowsTerminal.vcxproj'],
    ['src/cascadia/TerminalSettingsEditor/', 'src/cascadia/TerminalSettingsEditor/Microsoft.Terminal.Settings.Editor.vcxproj'],
    ['src/types/', 'src/types/lib/types.vcxproj'],
];

export function createAnalysisPlan(files) {
    const projects = new Set();
    const candidatePaths = [];
    const manualScope = [];
    const rustPaths = [];
    for (const file of files) {
        const filename = file.filename;
        if (/^tools\/wta\/(?:src\/.*\.rs|Cargo\.(?:toml|lock)|build\.rs)$/.test(filename)) {
            rustPaths.push(filename);
            continue;
        }

        if (!/^src\//.test(filename) || !/\.(?:c|cc|cpp|cxx|h|hh|hpp|ixx|hlsl|idl|xaml)$/.test(filename)) continue;
        const owner = CPP_PROJECTS.find(([prefix]) => filename.startsWith(prefix));
        if (!owner || /\.(?:hlsl|idl|xaml)$/.test(filename)) {
            manualScope.push({ path: filename, reason: owner ? 'Unsupported source kind; native C++ checks do not analyze this language.' :
                'Shared or unmapped source; no bounded owning-project recipe. Inspect callers manually.' });
            continue;
        }
        projects.add(owner[1]);
        candidatePaths.push({ path: filename, project: owner[1] });
        if (/\.(?:h|hh|hpp)$/.test(filename))
            manualScope.push({ path: filename, reason: 'Owning project includes headers, but cross-project caller coverage requires manual tracing.' });
    }
    return {
        version: 1, target: 'x86_64-pc-windows-msvc',
        rust: { required: rustPaths.length > 0, paths: rustPaths, toolchain: '1.93.0', alias: 'wta-perf-extended',
            configuration: '.cargo/config.toml', scope: 'Entire WTA crate and all targets, not edited lines.' },
        cpp: { required: projects.size > 0, projects: [...projects].sort(), candidatePaths,
            profile: 'Extended', configuration: 'AuditMode', platform: 'x64',
            profileProject: 'src/types/lib/types.vcxproj',
            scope: 'ClangTidy on entire selected projects with normal reference/generated-header builds; references are not promised analyzer coverage. No full-solution fallback.',
            callerCoverage: 'Prefix recipes are provisional. Verify translation units with native evaluated ClCompile items; trace headers and callers manually.' },
        manualScope, coverage: manualScope.length ? 'partial' : projects.size ? 'unverified-projects' : 'selected-projects',
        authority: 'Diagnostic input only; never native repair validation or publication authority.',
    };
}

export function analysisComparisonComplete(directory, expected) {
    const records = ['BASE', 'HEAD'].map(revision => {
        const record = readJson(path.join(directory, `performance-analysis-${revision}`, 'analysis-metadata.json'));
        validateIdentity(record.identity, expected);
        if (record.version !== 2 || record.revision !== revision ||
            record.analyzedSha !== expected[revision === 'BASE' ? 'baseSha' : 'headSha'] ||
            !['completed', 'not_applicable'].includes(record.status)) return null;
        if (!record.plan || !Array.isArray(record.checks) || !Array.isArray(record.missingPrerequisites) ||
            !Array.isArray(record.analyzedCppTranslationUnits) || !Array.isArray(record.manualScope) ||
            record.missingPrerequisites.length || record.manualScope.length || record.plan.manualScope?.length ||
            (expected.analysisPlan && JSON.stringify(record.plan) !== JSON.stringify(expected.analysisPlan))) return null;
        const completed = name => record.checks.some(check => check.name === name &&
            check.exitCode === 0 && check.status === 'completed');
        if (record.plan.rust.required && (!record.analyzedScope?.wtaRustCrate || !completed('rust-analysis'))) return null;
        if (record.plan.cpp.projects.some((project, index) =>
            !record.analyzedScope?.cppProjects?.includes(project) ||
            !completed(`cpp-items-${index}`) || !completed(`cpp-analysis-${index}`))) return null;
        if (!Array.isArray(record.plan.cpp.candidatePaths) || record.plan.cpp.candidatePaths
            .filter(candidate => /\.(?:c|cc|cpp|cxx|ixx)$/.test(candidate.path))
            .some(candidate => !record.analyzedCppTranslationUnits.some(covered => covered.path === candidate.path &&
                covered.project === candidate.project && covered.membership === 'MSBuild.ClCompile'))) return null;
        if (record.status === 'not_applicable' && (record.plan.rust.required || record.plan.cpp.required)) return null;
        return record;
    });
    if (records.some(record => !record)) return false;
    const [base, head] = records;
    return SHA_PATTERN.test(base.authoringSha ?? '') && base.authoringSha === head.authoringSha &&
        JSON.stringify(base.plan) === JSON.stringify(head.plan) &&
        JSON.stringify(base.tools) === JSON.stringify(head.tools);
}

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
        filename === 'tools/wta/src/test_support.rs' ||
        /(^|\/)(tests|[^/]+_tests)\.rs$/i.test(filename) ||
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
    if (candidates.some(file => ['tools/wta/build.rs', 'tools/wta/Cargo.toml', 'tools/wta/Cargo.lock'].includes(file.filename)))
        dimensions.push('ci-runtime-cost');
    return {
        version: 1, identity, applicable: candidates.length > 0, categories, dimensions: dimensions.sort(), candidates,
        supporting: files.filter(file => file.role === 'supporting'),
        excluded: files.filter(file => file.role === 'excluded'),
        analysisPlan: createAnalysisPlan(files),
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
            if (evidence.type !== 'measurement') {
                if (['kind', 'noisy', 'samples', 'spread'].some(key => Object.hasOwn(evidence, key)))
                    fail(`${prefix} source/proof evidence cannot contain measurement-only fields`);
                continue;
            }
            if (!['microbenchmark', 'end-to-end', 'profile'].includes(evidence.kind))
                fail(`${prefix} measurement must distinguish microbenchmark, end-to-end, or profile`);
            if (evidence.noisy !== undefined && typeof evidence.noisy !== 'boolean')
                fail(`${prefix} measurement noisy must be boolean when present`);
            if (evidence.samples !== undefined && (!Number.isInteger(evidence.samples) || evidence.samples < 1))
                fail(`${prefix} measurement samples must be a positive integer when present`);
            if (evidence.spread !== undefined) requireString(evidence.spread, `${prefix}.evidence.spread`, 3);
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
        if (check.status === 'unavailable' && check.exitCode !== null)
            fail(`${prefix} unavailable checks must have null exitCode`);
        if (report.status === 'pending_validation' && check.status === 'pass')
            fail(`${prefix} pending_validation reports cannot contain model-authored pass checks`);
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
            `**Evidence:** ${finding.evidence.map(evidence => {
                const metadata = ['kind', 'samples', 'spread'].filter(key => evidence[key] !== undefined)
                    .map(key => `${key}: ${evidence[key]}`);
                return `${evidence.type}${metadata.length ? ` (${metadata.join(', ')})` : ''}: ${evidence.detail}`;
            }).join('; ')}`,
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
        if (agentOutput.items.length !== 1 || item?.type !== 'noop') fail('applicable guide scope requires exactly one noop; only the controller publishes comments');
        if (TARGET_ALIASES.some(key => Object.hasOwn(item, key))) fail('guidance output must not override the configured immutable PR target');
        return;
    }
    const proposed = report.findings.some(finding => finding.fixDisposition === 'proposed');
    if (proposed && report.status !== 'pending_validation') fail('native proposals require pending_validation status');
    const types = proposed ? ['validate_performance_original_tests', 'validate_performance_focused_tests', 'validate_performance_repair'] : ['noop'];
    if (agentOutput.items.length !== types.length ||
        types.some(type => agentOutput.items.filter(output => output.type === type).length !== 1))
        fail(`applicable repair scope requires exactly one each of ${types.join(', ')}`);
    if (proposed && agentOutput.items.some(output => ![true, 'true'].includes(output.confirm)))
        fail('native validation requires explicit proposal confirmation');
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

let repositoryRoot;

function git(args, env = {}, options = {}) {
    const inherited = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
    return execFileSync('git', ['--no-pager', ...args], {
        timeout: 30000, maxBuffer: 16 * 1024 * 1024, encoding: 'utf8', cwd: repositoryRoot,
        env: { ...inherited, GIT_CONFIG_NOSYSTEM: '1',
            GIT_CONFIG_GLOBAL: process.platform === 'win32' ? 'NUL' : '/dev/null', GIT_NO_REPLACE_OBJECTS: '1', ...env }, ...options,
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

export function readReviewSummary(root) {
    const directory = path.resolve(root);
    const filename = path.join(directory, '.performance-summary.md');
    for (const item of [directory, filename]) {
        if (fs.lstatSync(item).isSymbolicLink() ||
            (process.platform === 'win32' ? fs.realpathSync(item).toLowerCase() !== item.toLowerCase() :
                fs.realpathSync(item) !== item))
            fail('review summary path must not contain symlinks or reparse points');
    }
    const stat = fs.lstatSync(filename);
    if (!stat.isFile() || stat.size < 1 || stat.size > MAX_SUMMARY_BYTES)
        fail('review summary must be a regular UTF-8 file within 48 KiB');
    const descriptor = fs.openSync(filename, 'r');
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
        if (length < 1 || length > MAX_SUMMARY_BYTES)
            fail('review summary must be within 48 KiB');
        const current = fs.lstatSync(filename);
        if (current.isSymbolicLink() || current.ino !== opened.ino || current.dev !== opened.dev)
            fail('review summary changed during collection');
        return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(buffer.subarray(0, length));
    } finally {
        fs.closeSync(descriptor);
    }
}

export function captureReviewSummary(root, summaryMarkdown) {
    if (typeof summaryMarkdown !== 'string' ||
        Buffer.byteLength(summaryMarkdown, 'utf8') < 1 || Buffer.byteLength(summaryMarkdown, 'utf8') > MAX_SUMMARY_BYTES ||
        new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(Buffer.from(summaryMarkdown)) !== summaryMarkdown)
        fail('review summary must be valid UTF-8 Markdown within 48 KiB');
    const directory = path.resolve(root);
    if (fs.lstatSync(directory).isSymbolicLink() ||
        (process.platform === 'win32' ? fs.realpathSync(directory).toLowerCase() !== directory.toLowerCase() :
            fs.realpathSync(directory) !== directory))
        fail('review summary directory must not contain symlinks or reparse points');
    const filename = path.join(directory, '.performance-summary.md');
    let exists = true;
    try { fs.lstatSync(filename); } catch (error) {
        if (error.code !== 'ENOENT') throw error;
        exists = false;
    }
    if (exists) readReviewSummary(directory);
    fs.writeFileSync(filename, summaryMarkdown, 'utf8');
}

function sourceInventory(root, excludedRoot, summaryOutput = false) {
    const inventory = Object.create(null);
    const walk = (directory, prefix = '') => {
        for (const name of fs.readdirSync(directory).sort()) {
            if (!prefix && name === '.git') continue;
            const filename = path.join(directory, name);
            if (excludedRoot && filename === excludedRoot) continue;
            const relative = prefix ? `${prefix}/${name}` : name;
            if (summaryOutput && relative === '.performance-summary.md') {
                readReviewSummary(root);
                continue;
            }
            classifyFile({ filename: relative });
            const stat = fs.lstatSync(filename);
            if (stat.isSymbolicLink()) {
                inventory[relative] = { mode: '120000', hash: createHash('sha256').update(fs.readlinkSync(filename)).digest('hex') };
            } else if (stat.isDirectory()) walk(filename, relative);
            else if (stat.isFile()) {
                inventory[relative] = { mode: process.platform !== 'win32' && (stat.mode & 0o111) ? '100755' : '100644',
                    hash: createHash('sha256').update(fs.readFileSync(filename)).digest('hex') };
            } else fail('source inventory requires regular files, directories, or symlinks');
        }
    };
    if (fs.lstatSync(root).isSymbolicLink()) fail('agent worktree root must not be a symlink');
    walk(root);
    return inventory;
}

function inventoryChanges(baseline, current) {
    if (!baseline || typeof baseline !== 'object' || Array.isArray(baseline)) fail('trusted source inventory is missing');
    return [...new Set([...Object.keys(baseline), ...Object.keys(current)])].sort()
        .filter(filename => JSON.stringify(baseline[filename]) !== JSON.stringify(current[filename]));
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
    if (baselinePath) {
        const root = git(['rev-parse', '--show-toplevel']).trim();
        const inventory = sourceInventory(root, undefined,
            !git(['ls-tree', expected.headSha, '--', ':(literal).performance-summary.md']).trim());
        writeJson(baselinePath, { identity: scope.identity, treeSha: git(['rev-parse', `${expected.headSha}^{tree}`]).trim(), inventory });
    }
    return scope;
}

function validatePlan(plan) {
    if (plan?.type !== 'wta-unit' || typeof plan.testFilter !== 'string' ||
        plan.testFilter.length > 200 ||
        !/^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+$/.test(plan.testFilter) ||
        plan.testFilter.endsWith('::tests') || plan.testFilter === 'module::tests')
        fail('supported focused native WTA validation requires a qualified test function selector, not a placeholder; native validation must verify it exists');
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
    validateRepairFileCount(proposal.files);
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
    if (size > MAX_REPAIR_BLOB_BYTES) fail('repair source content exceeds the transport size limit');
    const covered = new Set();
    for (const finding of proposal.report.findings.filter(finding => finding.fixDisposition === 'proposed')) {
        const location = /^([^:]+):([1-9][0-9]*)$/.exec(finding.location);
        if (!location || classifyFile({ filename: location[1] }).role !== 'candidate' || !paths.has(location[1]))
            fail('every proposed finding needs a canonical path:line location in a sealed WTA Rust replacement');
        covered.add(location[1]);
    }
    if ([...paths].some(filename => !covered.has(filename)))
        fail('every replacement file must be covered by a proposed finding');
    return proposal;
}

function protectedTestSuffix(contents) {
    // Deliberately lexical: comments/strings can block repairs, never justify ignoring a marker.
    const source = contents.toString('latin1');
    const marker = /\b(?:test|rstest)\b/.exec(source);
    if (!marker) return null;
    let start = source.lastIndexOf('\n', marker.index) + 1;
    const declarations = /#\s*!?\s*\[|\bmod\b/g;
    for (const match of source.matchAll(declarations)) {
        if (match.index > marker.index) break;
        // Include the full line so a comment prefix cannot disable a preserved token.
        start = Math.min(start, source.lastIndexOf('\n', match.index) + 1);
    }
    return contents.subarray(start);
}

function preserveOriginalTests(original, candidate) {
    const before = protectedTestSuffix(original);
    const after = protectedTestSuffix(candidate);
    if ((before === null) !== (after === null) || (before !== null && !before.equals(after)))
        fail('repair must preserve the immutable original Rust test markers and protected suffix byte-for-byte');
}

function validateRepairFileCount(files) {
    if (!Array.isArray(files) || !files.length || files.length > MAX_REPAIR_FILES)
        fail('repair must replace between one and three existing candidate source files');
}

function validateRepairDiff(headSha, treeSha) {
    const args = ['diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--no-color'];
    const rows = git([...args, '--numstat', '-z', headSha, treeSha, '--']).split('\0').filter(Boolean);
    let lines = 0;
    for (const row of rows) {
        const [added, deleted, filename, extra] = row.split('\t');
        if (extra !== undefined || !filename || !/^\d+$/.test(added) || !/^\d+$/.test(deleted))
            fail('repair actual diff must have numeric Git line counts');
        lines += Number(added) + Number(deleted);
    }
    if (rows.length > MAX_REPAIR_FILES || lines > MAX_REPAIR_LINES)
        fail('repair actual diff exceeds the three-file / 100 added-plus-deleted-line limit; use manual handoff');
    const patch = git([...args, '--unified=0', headSha, treeSha, '--'], {}, { encoding: null });
    if (patch.length > MAX_REPAIR_DIFF_BYTES)
        fail('repair actual zero-context diff exceeds the 16 KiB byte limit; use manual handoff');
}

export function reconstructTree(files, headSha, baseSha) {
    validateRepairFileCount(files);
    if (files.reduce((size, file) => size + Buffer.from(file.contents, 'base64').length, 0) > MAX_REPAIR_BLOB_BYTES)
        fail('repair source content exceeds the transport size limit');
    if (baseSha) {
        // The native backend is Windows, including when sealing runs on a case-sensitive host.
        const originalChanges = changedPaths(baseSha, headSha);
        if (originalChanges.some(filename => ['.cargo/config', '.cargo/config.toml'].includes(filename.toLowerCase())))
            fail('immutable original PR changes root Cargo configuration presence, mode, or blob; use manual handoff');
        const allowed = new Set(originalChanges.filter(filename => classifyFile({ filename }).role === 'candidate'));
        if (files.some(file => !allowed.has(file.path))) fail('publication replacement is outside the immutable original candidate scope');
    }
    return withIndex(headSha, env => {
        for (const file of files) {
            const entry = git(['ls-tree', headSha, '--', `:(literal)${file.path}`]).trim();
            if (!entry.startsWith(`${file.mode} blob `)) fail('repair may only replace an existing regular file without changing its mode');
            const candidate = Buffer.from(file.contents, 'base64');
            const original = git(['cat-file', 'blob', `${headSha}:${file.path}`], {}, { encoding: null });
            preserveOriginalTests(original, candidate);
            const blob = git(['hash-object', '-w', '--stdin'], {}, { input: candidate }).trim();
            git(['update-index', '--add', '--cacheinfo', `${file.mode},${blob},${file.path}`], env);
        }
        const tree = git(['write-tree'], env).trim();
        // Immutable-head diff ceilings are necessary, not proof of behavioral locality.
        validateRepairDiff(headSha, tree);
        return tree;
    });
}

export function sealProposal(scope, report, baseline, expected) {
    validateIdentity(baseline?.identity, expected);
    if (!SHA_PATTERN.test(baseline?.treeSha ?? '')) fail('trusted preparation baseline is missing');
    const root = expected.agentWorktreeRoot ?? git(['rev-parse', '--show-toplevel']).trim();
    const inventory = sourceInventory(root, repositoryRoot,
        !git(['ls-tree', expected.headSha, '--', ':(literal).performance-summary.md']).trim());
    const changedFiles = inventoryChanges(baseline.inventory, inventory);
    validateRepairFileCount(changedFiles);
    const files = changedFiles.map(filename => {
        if (!scope.candidates.some(file => file.filename === filename)) fail('repair changes must be confined to original candidate files');
        const entry = inventory[filename];
        if (entry?.mode !== '100644' || baseline.inventory[filename]?.mode !== entry.mode)
            fail('repair deletes or changes the mode of a source file');
        const content = fs.readFileSync(path.join(root, ...filename.split('/')));
        if (content.length > MAX_REPAIR_BLOB_BYTES || createHash('sha256').update(content).digest('hex') !== entry.hash)
            fail('repair source changed during collection or exceeds the transport size limit');
        return { path: filename, mode: entry.mode, contents: content.toString('base64') };
    });
    const proposal = {
        version: 1, identity: scope.identity, treeSha: reconstructTree(files, expected.headSha, expected.baseSha),
        files, report, validationPlan: report.validationPlan,
    };
    return { proposal: validateProposal(proposal, expected), changedFiles };
}

export function processGuideSubmission(scope, queued, expected, runtime, submittedReport) {
    if (expected.mode !== 'guide') throw new Error('guide submission requires guide mode');
    const report = submittedReport ?? null;
    runtime.gatePublication(scope, report, queued, expected);
    return {
        report, queued,
        verdict: { version: 1, identity: scope.identity, status: report?.status ?? 'pass' },
    };
}

async function validateForkReport(args) {
    if (args.length !== 0) throw new Error('guide submission accepts no command or path arguments');
    const runtimePath = process.env.TRUSTED_REVIEW_RUNTIME;
    if (!runtimePath || !path.isAbsolute(runtimePath)) throw new Error('trusted runtime must be an absolute environment path');
    const runtime = await import(pathToFileURL(runtimePath).href);
    const expected = {
        mode: 'guide', prNumber: Number(process.env.PR_NUMBER),
        baseSha: process.env.BASE_SHA, headSha: process.env.HEAD_SHA,
    };
    const queuePath = '/tmp/gh-aw/agent_output.json';
    const stat = fs.lstatSync(queuePath);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size < 2 || stat.size > 2 * 1024 * 1024)
        throw new Error('queued guidance must be a regular JSON file within the 2 MiB size limit');
    const queued = JSON.parse(fs.readFileSync(queuePath, 'utf8'));
    const resultDirectory = '/tmp/gh-aw/performance-result';
    const scope = runtime.prepareScope(expected, resultDirectory);
    const reportPath = '/tmp/gh-aw/performance-report.json';
    const report = fs.existsSync(reportPath) ? readJson(reportPath) : null;
    const result = processGuideSubmission(scope, queued, expected, runtime, report);
    const verdictJson = `${JSON.stringify(result.verdict, null, 2)}\n`;
    fs.writeFileSync(`${resultDirectory}/performance-verdict.json`, verdictJson, 'utf8');
}

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

function option(args, name, required = true) {
    const index = args.indexOf(name);
    if (index >= 0 && index + 1 < args.length) return args[index + 1];
    if (required) fail(`missing ${name}`);
}

function expectedFromArgs(args, requireMode = true) {
    const mode = option(args, '--mode', requireMode);
    return { prNumber: Number(option(args, '--pr')), baseSha: option(args, '--base').toLowerCase(), headSha: option(args, '--head').toLowerCase(), ...(mode ? { mode } : {}) };
}

async function main() {
    const [command, ...args] = process.argv.slice(2);
    if (command === 'fork-report') return validateForkReport(args);
    if (command === 'summary') {
        const summary = readReviewSummary(option(args, '--root'));
        captureReviewSummary(option(args, '--output-dir'), summary);
        return;
    }
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
            if (command === 'validate-proposal') {
                if (reconstructTree(proposal.files, expected.headSha, expected.baseSha) !== proposal.treeSha)
                    fail('proposal blobs do not match the sealed tree');
                break;
            }
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
            if (expected.mode === 'repair') {
                repositoryRoot = fs.realpathSync(option(args, '--trusted-repository-root'));
                expected.agentWorktreeRoot = fs.realpathSync(option(args, '--agent-worktree-root'));
                if (repositoryRoot === expected.agentWorktreeRoot ||
                    repositoryRoot.startsWith(`${expected.agentWorktreeRoot}${path.sep}.git${path.sep}`))
                    fail('post-agent Git requires a separate fresh trusted repository');
            }
            // Agent-writable scope and source inventories cannot override immutable Git objects.
            const scope = prepareScope(expected, directory);
            const reportPath = option(args, '--report');
            const report = fs.existsSync(reportPath) ? readJson(reportPath) : null;
            let sealed;
            if (expected.mode === 'repair') {
                const baseline = readJson(option(args, '--baseline'));
                validateIdentity(baseline.identity, expected);
                if (!SHA_PATTERN.test(baseline.treeSha ?? '')) fail('trusted preparation baseline is missing');
                if (report?.findings?.some(finding => finding.fixDisposition === 'proposed')) {
                    const analysisDirectory = option(args, '--analysis-input', false);
                    if (analysisDirectory && (process.env.PERFORMANCE_ANALYSIS_JOB_RESULT !== 'success' ||
                        !analysisComparisonComplete(analysisDirectory, { ...expected, analysisPlan: scope.analysisPlan })))
                        fail('required base/head source analysis is incomplete; repair must remain manual');
                    sealed = sealProposal(scope, report, baseline, expected);
                    expected.changedFiles = sealed.changedFiles;
                } else expected.changedFiles = inventoryChanges(baseline.inventory,
                    sourceInventory(expected.agentWorktreeRoot, repositoryRoot,
                        !git(['ls-tree', expected.headSha, '--', ':(literal).performance-summary.md']).trim()));
            }
            gatePublication(scope, report, readJson(option(args, '--agent-output')), expected);
            if (sealed) writeJson(path.join(directory, 'performance-proposal.json'), sealed.proposal);
            const status = report === null ? 'pass' : report.status;
            writeJson(path.join(directory, 'performance-verdict.json'), { version: 1, identity: scope.identity, status });
            process.stdout.write(status);
            break;
        }
        default:
            fail('usage: performance-review.mjs verify-pr|prepare|validate|render|gate|validate-proposal|apply-proposal|verdict|fork-report|summary ...');
    }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    main().catch(error => {
        console.error(`performance-review: ${error.message}`);
        process.exitCode = 1;
    });
}
