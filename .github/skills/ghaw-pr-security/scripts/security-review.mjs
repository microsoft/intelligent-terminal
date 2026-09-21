#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

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
  /\b(?:token|secret|password|authorization)\s*[:=]\s*["']?[A-Za-z0-9+/_.=-]{16,}/i,
];

function fail(message) {
  throw new Error(message);
}

function text(value, name, max = 600) {
  if (typeof value !== 'string' || value.trim().length === 0 || value.length > max) {
    fail(`${name} must be a non-empty string of at most ${max} characters`);
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
  if (/^(?:\.github\/workflows\/|\.github\/actions\/|\.github\/scripts\/|\.github\/agents\/|\.github\/skills\/)/.test(path)) {
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

function git(args) {
  return execFileSync('git', args, {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    timeout: 30_000,
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
    files.push({ status, path, ...(path === oldPath ? {} : { oldPath }), domains: classifyPath(path) });
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

export function validateReport(report, scope) {
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
    if (check.name !== 'deterministic-scope' && check.status === 'pass' &&
        !/https:\/\/github\.com\/[^/\s]+\/[^/\s]+\/actions\/runs\/[0-9]+|local command:/i.test(check.evidence ?? '')) {
      fail(`passing check ${check.name} needs a run URL or local command evidence`);
    }
    return { name: check.name, status: check.status, evidence: text(check.evidence, `check ${index + 1} evidence`, 500) };
  });
  if (!checks.some(check => check.name === 'deterministic-scope' && check.status === 'pass')) {
    fail('deterministic-scope PASS is required');
  }
  if (!report.review || !['pass', 'not-required', 'fail'].includes(report.review.status)) {
    fail('independent review result is invalid');
  }
  const review = {
    status: report.review.status,
    reviewer: text(report.review.reviewer, 'independent reviewer', 100),
    evidence: text(report.review.evidence, 'independent review evidence', 500),
  };
  if (!Array.isArray(report.findings) || report.findings.length > 20) {
    fail('findings must be an array with at most 20 entries');
  }
  const changed = new Set(scope.changedFiles.map(file => file.path));
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
      ? (scope.mode === 'repair' ? ['blocked', 'fixed'] : ['blocked'])
      : ['advice-only'];
    if (!allowedDisposition.includes(disposition)) {
      fail(`finding ${index + 1} has an invalid fix disposition`);
    }
    if (disposition === 'fixed') {
      if (finding.confidence !== 'high' || evidence.every(item => item.kind === 'hypothesis')) {
        fail(`finding ${index + 1} fixed disposition requires high confidence and strong evidence`);
      }
      if (!checks.some(check => ['wta-tests', 'native-windows', 'cpp-audit-mode'].includes(check.name) && check.status === 'pass')) {
        fail(`finding ${index + 1} fixed disposition requires applicable passing validation`);
      }
      if (checks.some(check => ['fail', 'blocked'].includes(check.status))) {
        fail(`finding ${index + 1} fixed disposition is incompatible with failed or blocked validation`);
      }
      if (review.status !== 'pass' || review.reviewer !== 'ghaw-pr-security-reviewer') {
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
    if (!/^(?:src\/|tools\/wta\/src\/|test\/)/.test(path) ||
        /(?:^|\/)(?:Cargo\.(?:toml|lock)|cgmanifest\.json)$/.test(path)) {
      fail(`patch item ${index + 1} is outside the automatic-fix allowlist`);
    }
    return { path, summary: text(item.summary, `patch item ${index + 1} summary`, 300) };
  });
  const fixed = findings.filter(finding => finding.fixDisposition.state === 'fixed');
  if (scope.mode === 'guide' && patch.length !== 0) fail('guide mode cannot contain a patch');
  if ((fixed.length === 0) !== (patch.length === 0)) {
    fail('fixed findings and patch entries must either both be present or both be absent');
  }
  const patchPaths = new Set(patch.map(item => item.path));
  for (const finding of fixed) {
    if (!patchPaths.has(finding.file)) fail(`fixed finding ${finding.id} has no patch entry for its source file`);
  }
  return { ...report, summary, checks, review, findings, patch };
}

export function validatePatch(report, actualPaths) {
  const expected = [...new Set(report.patch.map(item => item.path))].sort();
  const actual = [...new Set(actualPaths.map(path => normalizePath(path, 'working tree path')))].sort();
  if (JSON.stringify(expected) !== JSON.stringify(actual)) {
    fail(`reported patch paths do not match the working tree: expected [${expected}], actual [${actual}]`);
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
  const expected = report.mode === 'repair'
    ? (report.patch.length > 0 ? 'push_to_pull_request_branch' : 'noop')
    : (report.findings.length > 0 ? 'add_comment' : 'noop');
  const allowed = report.mode === 'repair'
    ? new Set(['push_to_pull_request_branch', 'noop'])
    : new Set(['add_comment', 'noop']);
  if (types.some(type => !allowed.has(type)) || types.length !== 1 || types[0] !== expected) {
    fail(`${report.mode} mode requires exactly one ${expected} output`);
  }
}

function escapeCell(value) {
  return value.replace(/[<>]/g, '').replace(/\|/g, '\\|').replace(/\r?\n/g, ' ');
}

export function renderReport(report) {
  const fixed = report.findings.filter(finding => finding.fixDisposition.state === 'fixed');
  const highs = report.findings.filter(finding => finding.severity === 'high' && finding.fixDisposition.state !== 'fixed');
  const mediumLow = report.findings.filter(finding => finding.severity !== 'high');
  const lines = [
    '## Intelligent Terminal security review',
    '',
    escapeCell(report.summary),
    '',
    `- Source/head: \`${report.baseSha.slice(0, 12)}\` / \`${report.headSha.slice(0, 12)}\``,
    `- HIGH (must fix/block): **${highs.length}**`,
    `- HIGH fixed and validated: **${fixed.length}**`,
    `- Medium/Low (consider): **${mediumLow.length}**`,
    `- Mode: **${report.mode}**`,
    '',
    '| Check | Status | Evidence |',
    '| --- | --- | --- |',
    ...report.checks.map(check => `| ${check.name} | **${check.status}** | ${escapeCell(check.evidence)} |`),
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
        `\`${finding.file}:${finding.startLine}-${finding.endLine}\` · \`${finding.category}\` · \`${finding.rule}\``,
        '',
        `**Observed:** ${escapeCell(finding.observed)}`,
        '',
        `**Expected:** ${escapeCell(finding.expected)}`,
        '',
        `**Impact:** ${escapeCell(finding.impact)}`,
        '',
        `**Proposed fix:** ${escapeCell(finding.proposedFix)}`,
        '',
        `**Validation:** ${escapeCell(finding.validation)}`,
        '',
        `**Disposition:** ${finding.fixDisposition.state} — ${escapeCell(finding.fixDisposition.reason)}`,
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
    const raw = execFileSync('git', ['diff', '--name-status', '-z', '--find-renames', base, head], {
      encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, timeout: 30_000,
    });
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
      validatePatch(report, [...modified, ...untracked]);
    }
    writeFileSync(option('--validated'), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' });
    writeFileSync(option('--summary'), renderReport(report), { flag: 'wx' });
    writeFileSync(option('--status'), report.findings.some(finding => finding.severity === 'high' && finding.fixDisposition.state !== 'fixed') ? 'blocking\n' : 'pass\n', { flag: 'wx' });
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
  fail('expected scope, validate, or enforce command');
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(`security-review: ${error.message}`);
    process.exitCode = 1;
  }
}
