#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, closeSync, constants, copyFileSync, lstatSync, openSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { resolve, sep } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

export const SECURITY_REPORT_MAX_BYTES = 64 * 1024;

const SHA = /^[0-9a-f]{40}$/;
const SAFE_RULE = /^[a-z][a-z0-9-]{2,63}$/;
const CATEGORIES = new Set([
  'cpp-lifetime', 'memory-safety', 'com-authorization', 'command-path',
  'session-routing', 'agent-input', 'confirmation', 'secret-handling',
  'filesystem', 'packaging', 'workflow-security', 'dependency-security',
]);
const CHECKS = new Set([
  'deterministic-scope', 'codeql-cpp', 'codeql-rust', 'cargo-audit',
  'cpp-audit-mode', 'native-windows', 'wta-tests', 'manual-review',
]);
const SECRET_PATTERNS = [
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/i,
  /\bgh[pousr]_[A-Za-z0-9_]{20,}\b/,
  /\bgithub_pat_[A-Za-z0-9_]{20,}\b/,
  /\b(?:token|secret|password|authorization)["']?\s*[:=]\s*["']?[A-Za-z0-9+/_.=-]{16,}/i,
  /\bbearer[ \t]+[A-Za-z0-9+/_.~=-]{16,}/i,
  /\b(?:session[_-]?capability|capability|mcp[_-]?token|session[_-]?token)["']?\s*[:=]\s*["']?[A-Za-z0-9+/_.~=-]{16,}/i,
];

function fail(message) {
  throw new Error(message);
}

function text(value, name, max = 600) {
  if (typeof value !== 'string' || value.trim().length === 0 || value.length > max) {
    fail(`${name} must be a non-empty string of at most ${max} characters; received ${typeof value === 'string' ? value.length : 'non-string'} characters`);
  }
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(value)) {
    fail(`${name} contains a control character`);
  }
  if (SECRET_PATTERNS.some(pattern => pattern.test(value))) {
    fail(`${name} appears to contain secret material`);
  }
  return value.trim();
}

export function normalizePath(value, name = 'path') {
  if (typeof value !== 'string' || value.length === 0 || value.length > 300) {
    fail(`${name} is invalid`);
  }
  if (value.includes('\\') || value.startsWith('/') || /^[A-Za-z]:/.test(value) ||
      value.split('/').some(part => part === '' || part === '.' || part === '..') ||
      /[\u0000-\u001f]/.test(value)) {
    fail(`${name} must be a normalized repository-relative path`);
  }
  return value;
}

export function classifyPath(path) {
  const domains = new Set();
  if (/^src\//.test(path)) {
    domains.add('product-source');
  }
  if (/^test\//.test(path)) {
    domains.add('product-tests');
  }
  if (/^tools\/wta\//.test(path)) {
    domains.add('wta');
  }
  if (/^(?:tools|\.cargo)\//.test(path)) domains.add('build-tooling');
  if (/^src\/.*\.(?:cpp|c|h|hpp|idl)$/.test(path)) {
    domains.add('cpp-memory');
  }
  if (/^src\/cascadia\/(?:TerminalProtocol|WindowsTerminal|TerminalApp)\//.test(path) ||
      /^src\/tools\/wtcli\//.test(path)) {
    domains.add('com-protocol');
  }
  if (/^tools\/wta\/.*\.rs$/.test(path)) {
    domains.add('wta-rust');
  }
  if (/^tools\/wta\/src\/(?:master|helper|protocol|agent_tools)\//.test(path)) {
    domains.add('session-routing');
  }
  if (/(?:hook|agent_event|osc)/i.test(path)) {
    domains.add('hooks-untrusted-input');
  }
  if (/^\.github\//.test(path)) {
    domains.add('workflow-credentials');
  }
  if (/^(?:src\/cascadia\/CascadiaPackage\/|build\/|tools\/wta\/.*(?:runtime_paths|logging))/.test(path)) {
    domains.add('packaging-paths-diagnostics');
  }
  if (/^(?:tools\/wta\/Cargo\.(?:toml|lock)|NOTICE\.md|tools\/wta\/cgmanifest\.json)$/.test(path)) {
    domains.add('dependency-supply-chain');
  }
  if (path === 'doc/security-model.md') {
    domains.add('security-model');
  }
  return [...domains].sort();
}

function git(args, cwd) {
  return execFileSync('git', ['-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=/dev/null', '--no-pager', ...args], {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    timeout: 30_000,
    ...(cwd ? { cwd } : {}),
    env: { ...process.env, GIT_NO_REPLACE_OBJECTS: '1', GIT_PAGER: 'cat' },
  });
}

function parseNameStatus(raw) {
  const parts = raw.split('\0');
  if (parts.at(-1) === '') parts.pop();
  const files = [];
  for (let index = 0; index < parts.length;) {
    const status = parts[index++];
    if (!/^[ACDMRTUXB][0-9]*$/.test(status)) fail(`unexpected git status ${status}`);
    const oldPath = normalizePath(parts[index++], 'changed path');
    let path = oldPath;
    if (status[0] === 'R' || status[0] === 'C') {
      path = normalizePath(parts[index++], 'renamed path');
    }
    files.push({
      status, path, ...(path === oldPath ? {} : { oldPath }),
      domains: [...new Set([...classifyPath(oldPath), ...classifyPath(path)])].sort(),
    });
  }
  return files;
}

function stableJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

export function buildScope(baseSha, headSha, prNumber, relation, rawNameStatus, observedBaseSha = baseSha, mode = 'guide') {
  if (!SHA.test(baseSha) || !SHA.test(headSha) || !SHA.test(observedBaseSha)) {
    fail('base and head must be lowercase 40-character SHAs');
  }
  if (!Number.isInteger(prNumber) || prNumber < 1) fail('PR number must be positive');
  if (!['same-repo', 'fork'].includes(relation)) fail('repository relation is invalid');
  if (!['repair', 'guide'].includes(mode)) fail('review mode is invalid');
  const changedFiles = parseNameStatus(rawNameStatus);
  const domains = [...new Set(changedFiles.flatMap(file => file.domains))].sort();
  const scope = {
    version: 1,
    prNumber,
    baseSha,
    observedBaseSha,
    headSha,
    repositoryRelation: relation,
    mode,
    applicable: domains.length > 0,
    domains,
    changedFiles,
    reviewReferences: [
      'doc/security-model.md',
      'tools/wta/AGENTS.md',
      'src/cascadia/TerminalProtocol/TerminalProtocol.idl',
      'src/cascadia/WindowsTerminal/TerminalProtocolComServer.cpp',
      'tools/wta/src/master/mod.rs',
      'tools/wta/src/agent_tools/session_mcp.rs',
    ],
    analyzerLimits: {
      codeql: 'Current CodeQL supports C/C++ and Rust, but this workflow does not run PR code or a Windows build; consume separate CodeQL checks only when they match this head SHA.',
      cpp: 'CppCoreCheck/AuditMode and native TAEF validation require the Windows build pipeline.',
      rust: 'cargo audit covers dependency advisories, not WTA source trust-boundary behavior.',
    },
  };
  return { ...scope, scopeSha256: createHash('sha256').update(stableJson(scope)).digest('hex') };
}

function findingId(finding) {
  const anchor = `${finding.rule}\0${finding.category}\0${finding.file}\0${finding.startLine}`;
  return `ITSEC-${createHash('sha256').update(anchor).digest('hex').slice(0, 12).toUpperCase()}`;
}

export function createReportTemplate(scope) {
  if (!scope || scope.version !== 1 || !SHA.test(scope.baseSha) || !SHA.test(scope.headSha) ||
      !/^[0-9a-f]{64}$/.test(scope.scopeSha256 ?? '') ||
      !Number.isInteger(scope.prNumber) || scope.prNumber < 1 ||
      !['same-repo', 'fork'].includes(scope.repositoryRelation) ||
      !['guide', 'repair'].includes(scope.mode)) {
    fail('report template requires a valid immutable scope');
  }
  return {
    version: 1,
    prNumber: scope.prNumber,
    baseSha: scope.baseSha,
    headSha: scope.headSha,
    scopeSha256: scope.scopeSha256,
    repositoryRelation: scope.repositoryRelation,
    mode: scope.mode,
    summary: '',
    checks: [
      {
        name: 'deterministic-scope',
        status: 'pass',
        headSha: scope.headSha,
        evidence: 'Immutable diff classified by the trusted scope validator.',
      },
    ],
    review: { status: 'not-required', reviewer: 'none', evidence: 'No automatic repair was attempted.' },
    findings: [],
    patch: [],
  };
}

export function validateReport(report, scope, phase = 'final') {
  if (!['candidate', 'proposal', 'final'].includes(phase)) fail('unknown report validation phase');
  if (phase === 'candidate' && (scope.mode !== 'repair' || scope.repositoryRelation !== 'same-repo')) {
    fail('candidate validation is available only for same-repository repair submission');
  }
  if (!report || typeof report !== 'object' || Array.isArray(report) || report.version !== 1) {
    fail('report envelope is invalid');
  }
  for (const key of ['baseSha', 'headSha']) {
    if (report[key] !== scope[key]) fail(`report ${key} does not match immutable scope`);
  }
  if (report.prNumber !== scope.prNumber || report.scopeSha256 !== scope.scopeSha256 ||
      report.repositoryRelation !== scope.repositoryRelation || report.mode !== scope.mode) {
    fail('report identity does not match immutable scope');
  }
  const summary = text(report.summary, 'summary', 800);
  if (!Array.isArray(report.checks) || report.checks.length < 1 || report.checks.length > 12) {
    fail('checks must contain 1 to 12 entries');
  }
  const checks = report.checks.map((check, index) => {
    if (!check || typeof check !== 'object' || !CHECKS.has(check.name) ||
        !['pass', 'fail', 'skipped', 'blocked'].includes(check.status)) {
      fail(`check ${index + 1} is invalid`);
    }
    if (check.status === 'pass' && check.headSha !== scope.headSha) {
      fail(`passing check ${check.name} must identify the immutable head SHA`);
    }
    if (check.name !== 'deterministic-scope' && check.status === 'pass' &&
        !/^local command:/i.test(check.evidence ?? '')) {
      fail(`passing check ${check.name} needs local command evidence from this immutable workspace`);
    }
    return {
      name: check.name,
      status: check.status,
      evidence: text(check.evidence, `check ${index + 1} evidence`, 500),
      ...(check.status === 'pass' ? { headSha: check.headSha } : {}),
    };
  });
  if (!checks.some(check => check.name === 'deterministic-scope' && check.status === 'pass')) {
    fail('deterministic-scope PASS is required');
  }
  const reviewStatuses = phase === 'candidate'
    ? ['pending', 'not-required', 'fail']
    : ['source-pass', 'not-required', 'fail'];
  if (!report.review || !reviewStatuses.includes(report.review.status)) {
    fail('independent review result is invalid');
  }
  const review = {
    status: report.review.status,
    reviewer: text(report.review.reviewer, 'independent reviewer', 100),
    evidence: text(report.review.evidence, 'independent review evidence', 500),
    ...(report.review.status === 'source-pass' ? {
      headSha: report.review.headSha,
      patchSha256: report.review.patchSha256,
    } : {}),
  };
  if (review.status === 'source-pass' &&
      (review.headSha !== scope.headSha || !/^[0-9a-f]{64}$/.test(review.patchSha256 ?? ''))) {
    fail('independent SOURCE_PASS must bind the immutable head and final patch digest');
  }
  if (!Array.isArray(report.findings) || report.findings.length > 20) {
    fail('findings must be an array with at most 20 entries');
  }
  const changed = new Set(scope.changedFiles.flatMap(file => file.oldPath ? [file.oldPath, file.path] : [file.path]));
  const findings = report.findings.map((finding, index) => {
    if (!finding || typeof finding !== 'object' || !SAFE_RULE.test(finding.rule) ||
        !['high', 'medium', 'low'].includes(finding.severity) ||
        !['high', 'medium', 'low'].includes(finding.confidence) ||
        !CATEGORIES.has(finding.category)) {
      fail(`finding ${index + 1} has invalid classification`);
    }
    const file = normalizePath(finding.file, `finding ${index + 1} file`);
    if (!changed.has(file)) fail(`finding ${index + 1} is not anchored in a changed file`);
    if (!Number.isInteger(finding.startLine) || finding.startLine < 1 ||
        !Number.isInteger(finding.endLine) || finding.endLine < finding.startLine) {
      fail(`finding ${index + 1} has invalid lines`);
    }
    if (!Array.isArray(finding.evidence) || finding.evidence.length < 1 || finding.evidence.length > 6) {
      fail(`finding ${index + 1} evidence is invalid`);
    }
    const evidence = finding.evidence.map((item, evidenceIndex) => {
      if (!item || typeof item !== 'object' ||
          !['verified-reproducer', 'source-trace', 'analyzer', 'hypothesis'].includes(item.kind)) {
        fail(`finding ${index + 1} evidence ${evidenceIndex + 1} is invalid`);
      }
      return {
        kind: item.kind,
        reference: text(item.reference, `finding ${index + 1} evidence reference`, 300),
        detail: text(item.detail, `finding ${index + 1} evidence detail`, 500),
      };
    });
    if (finding.severity === 'high' && finding.confidence === 'high' &&
        evidence.every(item => item.kind === 'hypothesis')) {
      fail(`finding ${index + 1} cannot claim high-confidence HIGH from hypothesis-only evidence`);
    }
    const disposition = finding.fixDisposition?.state;
    const allowedDisposition = finding.severity === 'high'
      ? (scope.mode === 'repair'
        ? ['blocked', phase === 'final' ? 'fixed' : 'proposed']
        : ['blocked'])
      : ['advice-only'];
    if (!allowedDisposition.includes(disposition)) {
      fail(`finding ${index + 1} has an invalid fix disposition`);
    }
    if (disposition === 'fixed' || disposition === 'proposed') {
      if (finding.confidence !== 'high' || evidence.every(item => item.kind === 'hypothesis')) {
        fail(`finding ${index + 1} fixed disposition requires high confidence and strong evidence`);
      }
      if (phase === 'final' && !checks.some(check => ['wta-tests', 'native-windows', 'cpp-audit-mode'].includes(check.name) && check.status === 'pass')) {
        fail(`finding ${index + 1} fixed disposition requires applicable passing validation`);
      }
      if (checks.some(check => ['fail', 'blocked'].includes(check.status))) {
        fail(`finding ${index + 1} fixed disposition is incompatible with failed or blocked validation`);
      }
      if (review.status !== (phase === 'candidate' ? 'pending' : 'source-pass') ||
          review.reviewer !== 'ghaw-pr-security-reviewer') {
        fail(`finding ${index + 1} fixed disposition requires the independent security review gate`);
      }
    }
    return {
      id: findingId(finding),
      rule: finding.rule,
      severity: finding.severity,
      confidence: finding.confidence,
      category: finding.category,
      file,
      startLine: finding.startLine,
      endLine: finding.endLine,
      observed: text(finding.observed, `finding ${index + 1} observed`),
      expected: text(finding.expected, `finding ${index + 1} expected`),
      impact: text(finding.impact, `finding ${index + 1} impact`),
      evidence,
      proposedFix: text(finding.proposedFix, `finding ${index + 1} proposedFix`),
      validation: text(finding.validation, `finding ${index + 1} validation`),
      fixDisposition: {
        state: disposition,
        reason: text(finding.fixDisposition.reason, `finding ${index + 1} disposition reason`, 500),
      },
    };
  });
  if (!Array.isArray(report.patch) || report.patch.length > 20) {
    fail('patch must be an array with at most 20 entries');
  }
  const patch = report.patch.map((item, index) => {
    if (!item || typeof item !== 'object') fail(`patch item ${index + 1} is invalid`);
    const path = normalizePath(item.path, `patch item ${index + 1} path`);
    if (!/^tools\/wta\/src\/.*\.rs$/.test(path)) {
      fail(`patch item ${index + 1} is outside the automatic-fix allowlist`);
    }
    return { path, summary: text(item.summary, `patch item ${index + 1} summary`, 300) };
  });
  const fixed = findings.filter(finding => ['fixed', 'proposed'].includes(finding.fixDisposition.state));
  if (phase === 'candidate' && ((review.status === 'pending') !== (patch.length > 0))) {
    fail('pending independent review requires a patched repair candidate');
  }
  if (scope.mode === 'guide' && patch.length !== 0) fail('guide mode cannot contain a patch');
  if ((fixed.length === 0) !== (patch.length === 0)) {
    fail('fixed findings and patch entries must either both be present or both be absent');
  }
  const patchPaths = new Set(patch.map(item => item.path));
  for (const finding of fixed) {
    if (!patchPaths.has(finding.file)) fail(`fixed finding ${finding.id} has no patch entry for its source file`);
  }
  const fixedPaths = new Set(fixed.map(finding => finding.file));
  for (const item of patch) {
    if (!fixedPaths.has(item.path)) fail(`patch path ${item.path} has no fixed finding`);
  }
  return { ...report, summary, checks, review, findings, patch };
}

export function validateProposal(report, scope) {
  if (scope.mode !== 'repair' || scope.repositoryRelation !== 'same-repo') {
    fail('repair proposals require same-repository repair scope');
  }
  if (report.checks?.some(check => check.name !== 'deterministic-scope' && check.status === 'pass')) {
    fail('agent proposal cannot claim passing final-patch validation');
  }
  const validated = validateReport(report, scope, 'proposal');
  if (validated.patch.length > 0) validateRepairScope(scope);
  return validated;
}

export function validateCandidate(report, scope) {
  if (scope.mode !== 'repair' || scope.repositoryRelation !== 'same-repo') {
    fail('repair candidates require same-repository repair scope');
  }
  if (report.checks?.some(check => check.name !== 'deterministic-scope' && check.status === 'pass')) {
    fail('agent candidate cannot claim passing final-patch validation');
  }
  const validated = validateReport(report, scope, 'candidate');
  if (validated.patch.length > 0) validateRepairScope(scope);
  return validated;
}

export function submitSecurityReport(json, scope, outputPath) {
  if (typeof json !== 'string' || Buffer.byteLength(json, 'utf8') > 10 * 1024) {
    fail('report input must be JSON text of at most 10 KiB');
  }
  const input = JSON.parse(json);
  if (input?.review?.status === 'source-pass') {
    fail('model submission cannot claim trusted independent SOURCE_PASS');
  }
  const report = scope.mode === 'repair' ? validateCandidate(input, scope) : validateReport(input, scope);
  const serialized = serializeSecurityReport(report);
  const path = resolve(outputPath);
  const stat = lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink() || realpathSync(path) !== path) {
    fail('native report destination must remain a regular file without symlink ancestors');
  }
  const platformFlags = process.platform === 'win32' ? constants.O_CREAT : constants.O_NOFOLLOW;
  const fd = openSync(path, constants.O_WRONLY | constants.O_TRUNC | platformFlags);
  try {
    writeFileSync(fd, serialized);
  } finally {
    closeSync(fd);
  }
  return { accepted: true, mode: report.mode, findings: report.findings.length, patchEntries: report.patch.length };
}

export function serializeSecurityReport(report) {
  const serialized = `${JSON.stringify(report, null, 2)}\n`;
  if (Buffer.byteLength(serialized, 'utf8') > SECURITY_REPORT_MAX_BYTES) {
    fail('serialized security report exceeds the 64 KiB native output limit');
  }
  return serialized;
}

export function readSecurityDiff(scope, paths = [], workspace) {
  if (!SHA.test(scope?.baseSha ?? '') || !SHA.test(scope?.headSha ?? '') ||
      !Array.isArray(paths) || paths.length > 20) fail('immutable diff inputs are invalid');
  const selected = paths.map(path => normalizePath(path));
  return git(['--no-pager', 'diff', '--no-ext-diff', '--no-textconv', '--unified=20',
    scope.baseSha, scope.headSha, '--', ...selected], workspace);
}

export function readSecuritySource(scope, revision, path, startLine = 1, endLine = 400, workspace) {
  const sha = revision === 'base' ? scope?.baseSha : revision === 'head' ? scope?.headSha : '';
  if (!SHA.test(sha ?? '') || !Number.isInteger(startLine) || startLine < 1 ||
      !Number.isInteger(endLine) || endLine < startLine || endLine - startLine >= 800) {
    fail('source read needs immutable base/head and a range of at most 800 lines');
  }
  const content = git(['cat-file', 'blob', `${sha}:${normalizePath(path)}`], workspace);
  return content.split('\n').slice(startLine - 1, endLine)
    .map((line, index) => `${startLine + index}: ${line}`).join('\n');
}

export function inspectSecurityRepair(scope, workspace) {
  if (scope?.mode !== 'repair' || !SHA.test(scope.headSha ?? '')) fail('repair inspection is not available in guide mode');
  const patch = git(['--no-pager', 'diff', '--no-ext-diff', '--no-textconv', '--binary', scope.headSha, '--'], workspace);
  return { patch, patchSha256: createHash('sha256').update(patch).digest('hex'), headSha: scope.headSha };
}

function securityRepairTarget(scope, workspace, path) {
  if (scope?.repositoryRelation !== 'same-repo') fail('repair writer requires same-repository scope');
  validateRepairScope(scope);
  const relative = normalizePath(path);
  if (!scope.changedFiles.some(file => file.status === 'M' && file.path === relative) ||
      !/^tools\/wta\/src\/.*\.rs$/.test(relative)) {
    fail('repair writer accepts only existing modified WTA Rust source in the immutable scope');
  }
  const root = realpathSync(resolve(workspace));
  const target = resolve(root, relative);
  if (!target.startsWith(`${root}${sep}`) || realpathSync(target) !== target ||
      !lstatSync(target).isFile() || lstatSync(target).isSymbolicLink()) {
    fail('repair target must be a regular file without symlink ancestors');
  }
  const entry = git(['ls-tree', scope.headSha, '--', relative], root).trim();
  if (!/^100644 blob [0-9a-f]{40}\t/.test(entry)) fail('repair target must be an immutable non-executable Git blob');
  return { root, target };
}

export function writeSecurityRepair(scope, workspace, path, content) {
  const { root, target } = securityRepairTarget(scope, workspace, path);
  if (typeof content !== 'string' || Buffer.byteLength(content, 'utf8') > 512 * 1024 ||
      content.includes('\0')) fail('repair source must be text of at most 512 KiB without NUL');
  const platformFlags = process.platform === 'win32' ? constants.O_CREAT : constants.O_NOFOLLOW;
  const fd = openSync(target, constants.O_WRONLY | constants.O_TRUNC | platformFlags);
  try {
    writeFileSync(fd, content);
  } finally {
    closeSync(fd);
  }
  return inspectSecurityRepair(scope, root);
}

export function replaceSecurityRepairText(scope, workspace, path, editsJson) {
  const { target } = securityRepairTarget(scope, workspace, path);
  if (typeof editsJson !== 'string' || Buffer.byteLength(editsJson, 'utf8') > 8192) {
    fail('repair edits must be JSON text of at most 8 KiB');
  }
  const edits = JSON.parse(editsJson);
  if (!Array.isArray(edits) || edits.length < 1 || edits.length > 8) {
    fail('repair requires 1 to 8 exact text replacements');
  }
  const bytes = readFileSync(target);
  let content = bytes.toString('utf8');
  if (!Buffer.from(content, 'utf8').equals(bytes)) fail('repair target must contain valid UTF-8 source');
  for (const edit of edits) {
    if (!edit || typeof edit.oldText !== 'string' || edit.oldText.length === 0 ||
        typeof edit.newText !== 'string' || edit.oldText.includes('\0') || edit.newText.includes('\0')) {
      fail('each repair edit requires nonempty oldText and NUL-free source text');
    }
    if (content.split(edit.oldText).length !== 2) fail('repair oldText must match exactly once');
    content = content.replace(edit.oldText, () => edit.newText);
  }
  return writeSecurityRepair(scope, workspace, path, content);
}

export function verifyCredentialFree(workspace) {
  const query = pattern => {
    const result = spawnSync('git', [
      '-c', 'core.fsmonitor=false', 'config', '--get-regexp', pattern,
    ], { cwd: workspace, encoding: 'utf8', timeout: 30_000 });
    if (result.error || ![0, 1].includes(result.status)) fail('could not verify Git credential postcondition');
    return result.status === 0 ? result.stdout : '';
  };
  if (query('^(credential(\\..*)?\\.helper|http(\\..*)?\\.extraheader)$')) {
    fail('Git credential helper or authentication header remains after cleanup');
  }
  if (/https?:\/\/[^/\s]*@/i.test(query('^remote\\..*\\.url$'))) {
    fail('authenticated Git remote remains after cleanup');
  }
  return true;
}

export function validatePatch(report, actualPaths, patchText = '') {
  const expected = [...new Set(report.patch.map(item => item.path))].sort();
  const actual = [...new Set(actualPaths.map(path => normalizePath(path, 'working tree path')))].sort();
  if (JSON.stringify(expected) !== JSON.stringify(actual)) {
    fail(`reported patch paths do not match the working tree: expected [${expected}], actual [${actual}]`);
  }
  if (report.patch.length > 0) {
    const actualDigest = createHash('sha256').update(patchText).digest('hex');
    if (report.review.patchSha256 !== actualDigest) {
      fail('independent SOURCE_PASS does not match the final patch digest');
    }
  }
}

export function validateQueuedOutput(report, queuedOutput) {
  if (!queuedOutput || typeof queuedOutput !== 'object' || !Array.isArray(queuedOutput.items) ||
      (queuedOutput.errors !== undefined && !Array.isArray(queuedOutput.errors))) {
    fail('agent output envelope is invalid');
  }
  if (queuedOutput.errors?.length) fail('agent output ingestion reported errors');
  const types = queuedOutput.items.map(item => item?.type);
  if (types.some(type => typeof type !== 'string')) fail('queued output type is invalid');
  if (types.length !== 1 || types[0] !== 'noop') {
    fail(`${report.mode} analysis worker requires exactly one noop output`);
  }
}

export function attestChecks(report, headSha, wtaTestsPassed, patchText = '') {
  if (report.review?.status === 'pending') {
    fail('pending independent review cannot enter trusted validation attestation');
  }
  if (!SHA.test(headSha) || report.headSha !== headSha) {
    fail('trusted validation attestation does not match the immutable head');
  }
  if (!Array.isArray(report.findings) || report.findings.some(finding => finding.fixDisposition?.state === 'fixed')) {
    fail('agent reports must propose repairs, not claim trusted fixed results');
  }
  const proposed = report.findings.filter(finding => finding.fixDisposition?.state === 'proposed');
  if (proposed.length > 0) {
    if (!wtaTestsPassed) fail('proposed repair requires trusted final-patch validation');
    if (report.mode !== 'repair' || report.repositoryRelation !== 'same-repo' ||
        proposed.some(finding => finding.severity !== 'high' || finding.confidence !== 'high')) {
      fail('only same-repository HIGH/high-confidence repairs may be proposed');
    }
    if (report.review?.status !== 'source-pass' || report.review.reviewer !== 'ghaw-pr-security-reviewer' ||
        report.review.headSha !== headSha ||
        report.review.patchSha256 !== createHash('sha256').update(patchText).digest('hex')) {
      fail('proposed repair requires independent SOURCE_PASS for the exact final patch');
    }
  }
  const checks = report.checks
    .filter(check => check.name === 'deterministic-scope' || check.status !== 'pass')
    .map(check => check.name === 'deterministic-scope'
      ? check
      : { ...check, status: check.status });
  if (wtaTestsPassed) {
    const existing = checks.findIndex(check => check.name === 'wta-tests');
    const attested = {
      name: 'wta-tests',
      status: 'pass',
      headSha,
      evidence: 'local command: trusted isolated Windows container: cargo test --locked --offline --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml (exit 0) against the final patch',
    };
    if (existing >= 0) checks[existing] = attested;
    else checks.push(attested);
  }
  const findings = report.findings.map(finding => finding.fixDisposition?.state === 'proposed'
    ? {
      ...finding,
      fixDisposition: {
        state: 'fixed',
        reason: 'Independent source review approved the exact patch; trusted isolated final-patch validation passed.',
      },
    }
    : finding);
  return { ...report, checks, findings };
}

export function stageRepairFiles(report, sourceRoot, targetRoot) {
  if (!report || typeof report !== 'object' || !Array.isArray(report.patch) ||
      report.patch.length < 1 || report.patch.length > 20) {
    fail('repair staging requires 1 to 20 reported patch entries');
  }
  const sourceBase = realpathSync(resolve(sourceRoot));
  const targetBase = realpathSync(resolve(targetRoot));
  const seen = new Set();
  for (const [index, item] of report.patch.entries()) {
    const path = normalizePath(item?.path, `patch item ${index + 1} path`);
    if (!/^tools\/wta\/src\/.*\.rs$/.test(path) || seen.has(path)) {
      fail(`patch item ${index + 1} is not a unique WTA Rust source path`);
    }
    seen.add(path);
    const source = resolve(sourceBase, path);
    const target = resolve(targetBase, path);
    if (!source.startsWith(`${sourceBase}${sep}`) || !target.startsWith(`${targetBase}${sep}`)) {
      fail(`patch item ${index + 1} escapes its workspace`);
    }
    if (realpathSync(source) !== source || realpathSync(target) !== target) {
      fail(`patch item ${index + 1} traverses a symlink or reparse-point ancestor`);
    }
    const sourceStat = lstatSync(source);
    const targetStat = lstatSync(target);
    if (!sourceStat.isFile() || sourceStat.isSymbolicLink() ||
        !targetStat.isFile() || targetStat.isSymbolicLink()) {
      fail(`patch item ${index + 1} must remain a regular tracked file`);
    }
    if ((sourceStat.mode & 0o777) !== (targetStat.mode & 0o777)) {
      fail(`patch item ${index + 1} changes the tracked file mode`);
    }
    const treeEntry = git(['ls-tree', 'HEAD', '--', path]).trim().split(/\s+/);
    if (treeEntry.length < 3 || treeEntry[0] !== '100644' || treeEntry[1] !== 'blob') {
      fail(`patch item ${index + 1} must target a non-executable regular Git blob`);
    }
    copyFileSync(source, target);
    chmodSync(target, targetStat.mode & 0o777);
  }
  return [...seen].sort();
}

export function validateRepairScope(scope) {
  if (!scope || scope.mode !== 'repair' || !Array.isArray(scope.changedFiles) ||
      scope.changedFiles.length === 0) {
    fail('automatic repair requires a non-empty trusted repair scope');
  }
  const ineligible = scope.changedFiles
    .map(file => ({
      path: normalizePath(file?.path, 'repair scope path'),
      status: file?.status,
    }))
    .filter(file => file.status !== 'M' || !/^tools\/wta\/src\/.*\.rs$/.test(file.path))
    .map(file => `${file.status ?? 'missing'}:${file.path}`);
  if (ineligible.length > 0) {
    fail(`automatic repair scope contains non-WTA-source paths or non-modification entries: ${ineligible.join(', ')}`);
  }
}

function escapeMarkdown(value) {
  return value
    .replace(/\r?\n/g, ' ')
    .replace(/[\\`*_{}[\]()#+\-.!|<>:/@~]/g, '\\$&');
}

export function renderReport(report) {
  const fixed = report.findings.filter(finding => finding.fixDisposition.state === 'fixed');
  const highs = report.findings.filter(finding => finding.severity === 'high' && finding.fixDisposition.state !== 'fixed');
  const mediumLow = report.findings.filter(finding => finding.severity !== 'high');
  const lines = [
    '## Intelligent Terminal security review',
    '',
    escapeMarkdown(report.summary),
    '',
    `- Source/head: \`${report.baseSha.slice(0, 12)}\` / \`${report.headSha.slice(0, 12)}\``,
    `- HIGH (must fix/block): **${highs.length}**`,
    `- HIGH fixed and validated: **${fixed.length}**`,
    `- Medium/Low (consider): **${mediumLow.length}**`,
    `- Mode: **${report.mode}**`,
    '',
    '| Check | Status | Evidence |',
    '| --- | --- | --- |',
    ...report.checks.map(check => `| ${check.name} | **${check.status}** | ${escapeMarkdown(check.evidence)} |`),
  ];
  for (const [title, findings] of [['Fixed', fixed], ['Must fix / blocking', highs], ['Consider', mediumLow]]) {
    lines.push('', `### ${title}`);
    if (findings.length === 0) {
      lines.push('', 'None.');
      continue;
    }
    for (const finding of findings) {
      lines.push(
        '',
        `#### ${finding.id} — ${finding.severity.toUpperCase()} / ${finding.confidence} confidence`,
        '',
        `${escapeMarkdown(finding.file)}:${finding.startLine}-${finding.endLine} · \`${finding.category}\` · \`${finding.rule}\``,
        '',
        `**Observed:** ${escapeMarkdown(finding.observed)}`,
        '',
        `**Expected:** ${escapeMarkdown(finding.expected)}`,
        '',
        `**Impact:** ${escapeMarkdown(finding.impact)}`,
        '',
        `**Proposed fix:** ${escapeMarkdown(finding.proposedFix)}`,
        '',
        `**Validation:** ${escapeMarkdown(finding.validation)}`,
        '',
        `**Disposition:** ${finding.fixDisposition.state} — ${escapeMarkdown(finding.fixDisposition.reason)}`,
      );
    }
  }
  return `${lines.join('\n')}\n`;
}

function option(name) {
  const index = process.argv.indexOf(name);
  if (index < 0 || index + 1 >= process.argv.length) fail(`missing ${name}`);
  return process.argv[index + 1];
}

function main() {
  const command = process.argv[2];
  if (command === 'scope') {
    const observedBase = option('--base').toLowerCase();
    const head = option('--head').toLowerCase();
    git(['cat-file', '-e', `${observedBase}^{commit}`]);
    git(['cat-file', '-e', `${head}^{commit}`]);
    const base = git(['merge-base', observedBase, head]).trim().toLowerCase();
    if (!SHA.test(base)) fail('could not resolve a comparison merge base');
    const raw = git(['diff', '--no-ext-diff', '--no-textconv', '--name-status', '-z', '--find-renames', base, head]);
    const scope = buildScope(base, head, Number(option('--pr')), option('--relation'), raw, observedBase, option('--mode'));
    writeFileSync(option('--output'), `${JSON.stringify(scope, null, 2)}\n`, { flag: 'wx' });
    return;
  }
  if (command === 'validate') {
    const scope = JSON.parse(readFileSync(option('--scope'), 'utf8'));
    const report = validateReport(JSON.parse(readFileSync(option('--report'), 'utf8')), scope);
    if (scope.mode === 'repair') {
      const modified = git(['diff', '--name-only', '-z', 'HEAD']).split('\0').filter(Boolean);
      const untracked = git(['ls-files', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean);
      if (untracked.length > 0) {
        fail(`automatic repair cannot include untracked files: ${untracked.join(', ')}`);
      }
      const patchText = git(['diff', '--binary', 'HEAD']);
      validatePatch(report, modified, patchText);
    }
    writeFileSync(option('--validated'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' });
    writeFileSync(option('--summary'), renderReport(report), { flag: 'wx' });
    writeFileSync(option('--status'), report.findings.some(finding => finding.severity === 'high' && finding.fixDisposition.state !== 'fixed') ? 'blocking\n' : 'pass\n', { flag: 'wx' });
    return;
  }
  if (command === 'check-report') {
    const scope = JSON.parse(readFileSync(option('--scope'), 'utf8'));
    const report = JSON.parse(readFileSync(option('--report'), 'utf8'));
    if (scope.mode === 'repair') validateProposal(report, scope);
    else validateReport(report, scope);
    console.log('Report contract is valid; this is not a publication or test attestation.');
    return;
  }
  if (command === 'verify-credentials') {
    verifyCredentialFree(option('--workspace'));
    console.log('Git credential postcondition verified.');
    return;
  }
  if (command === 'attest') {
    const report = JSON.parse(readFileSync(option('--report'), 'utf8'));
    const passed = process.argv.includes('--wta-tests-passed');
    const attested = attestChecks(report, option('--head'), passed,
      passed ? git(['diff', '--binary', 'HEAD']) : '');
    writeFileSync(option('--output'), `${JSON.stringify(attested, null, 2)}\n`, { flag: 'wx' });
    return;
  }
  if (command === 'init-report') {
    const scope = JSON.parse(readFileSync(option('--scope'), 'utf8'));
    writeFileSync(option('--output'), `${JSON.stringify(createReportTemplate(scope), null, 2)}\n`, { flag: 'wx' });
    return;
  }
  if (command === 'stage-repair') {
    const report = JSON.parse(readFileSync(option('--report'), 'utf8'));
    stageRepairFiles(report, option('--source'), option('--target'));
    return;
  }
  if (command === 'validate-proposal') {
    const scope = JSON.parse(readFileSync(option('--scope'), 'utf8'));
    const report = validateProposal(JSON.parse(readFileSync(option('--report'), 'utf8')), scope);
    const modified = git(['diff', '--name-only', '-z', 'HEAD']).split('\0').filter(Boolean);
    const untracked = git(['ls-files', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean);
    if (untracked.length > 0) fail('repair proposal cannot include untracked files');
    validatePatch(report, modified, git(['diff', '--binary', 'HEAD']));
    writeFileSync(option('--output'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' });
    return;
  }
  if (command === 'validate-repair-scope') {
    validateRepairScope(JSON.parse(readFileSync(option('--scope'), 'utf8')));
    return;
  }
  if (command === 'validate-output') {
    const report = JSON.parse(readFileSync(option('--validated'), 'utf8'));
    validateQueuedOutput(report, JSON.parse(readFileSync(option('--agent-output'), 'utf8')));
    return;
  }
  if (command === 'enforce') {
    const status = readFileSync(option('--status'), 'utf8').trim();
    if (status === 'blocking') {
      console.error('One or more HIGH security findings remain blocking.');
      process.exitCode = 2;
    } else if (status !== 'pass') {
      fail('invalid enforcement status');
    }
    return;
  }
  fail('expected scope, init-report, check-report, validate-proposal, validate-repair-scope, stage-repair, attest, validate, validate-output, or enforce command');
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(`security-review: ${error.message}`);
    process.exitCode = 1;
  }
}
