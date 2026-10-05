import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  attestChecks, buildScope, classifyPath, createReportTemplate, normalizePath, renderReport, validatePatch,
  validateQueuedOutput, validateReport, validateProposal, stageRepairFiles, validateRepairScope,
} from './security-review.mjs';

const BASE = '1'.repeat(40);
const HEAD = '2'.repeat(40);
const PATCH_TEXT = 'diff --git a/tools/wta/src/master/mod.rs b/tools/wta/src/master/mod.rs\n';
const PATCH_SHA256 = createHash('sha256').update(PATCH_TEXT).digest('hex');

function scope(relation = 'same-repo') {
  return buildScope(
    BASE,
    HEAD,
    17,
    relation,
    'M\0tools/wta/src/master/mod.rs\0M\0tools/wta/src/logging.rs\0M\0.github/workflows/build.yml\0',
  );
}

function repairScope() {
  return buildScope(
    BASE,
    HEAD,
    17,
    'same-repo',
    'M\0tools/wta/src/master/mod.rs\0M\0tools/wta/src/logging.rs\0',
    BASE,
    'repair',
  );
}

function repairScopeWithStatus(status) {
  return buildScope(
    BASE,
    HEAD,
    17,
    'same-repo',
    `${status}\0tools/wta/src/master/mod.rs\0`,
    BASE,
    'repair',
  );
}

function report(overrides = {}, relation = 'same-repo') {
  const current = scope(relation);
  return {
    version: 1,
    prNumber: 17,
    baseSha: BASE,
    headSha: HEAD,
    scopeSha256: current.scopeSha256,
    repositoryRelation: relation,
    mode: 'guide',
    summary: 'No security regression found.',
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'native-windows', status: 'skipped', evidence: 'Not available in Linux.' },
    ],
    review: { status: 'not-required', reviewer: 'none', evidence: 'No automatic repair was attempted.' },
    findings: [],
    patch: [],
    ...overrides,
  };
}

test('classifies project trust boundaries', () => {
  assert.deepEqual(classifyPath('tools/wta/src/master/mod.rs'), ['session-routing', 'wta', 'wta-rust']);
  assert(classifyPath('src/cascadia/TerminalProtocol/TerminalProtocol.idl').includes('com-protocol'));
  assert(classifyPath('src/cascadia/TerminalSettingsEditor/Settings.xaml').includes('product-source'));
  assert(classifyPath('src/cascadia/TerminalApp/TerminalApp.vcxproj').includes('product-source'));
  assert(classifyPath('test/e2e/tests/Feature.Agent.Tests.ps1').includes('product-tests'));
  assert(classifyPath('tools/wta/prompts/terminal-agent.md').includes('wta'));
  assert(classifyPath('.github/workflows/review.yml').includes('workflow-credentials'));
  assert(classifyPath('.github/skills/reviewer/SKILL.md').includes('workflow-credentials'));
});

test('rejects unsafe changed paths', () => {
  for (const path of ['../escape.rs', '/tmp/file', 'C:/temp/file', 'a\\b.rs', 'a//b.rs']) {
    assert.throws(() => normalizePath(path));
  }
});

test('accepts no-findings and medium advice-only reports', () => {
  assert.equal(validateReport(report(), scope()).findings.length, 0);
  const candidate = report({
    findings: [{
      rule: 'diagnostic-metadata-overcollection',
      severity: 'medium',
      confidence: 'high',
      category: 'secret-handling',
      file: 'tools/wta/src/logging.rs',
      startLine: 10,
      endLine: 12,
      observed: 'The changed diagnostic records complete provider configuration.',
      expected: 'Log only non-sensitive identifiers.',
      impact: 'Diagnostics can expose sensitive configuration.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/logging.rs:10-12', detail: 'The full structure reaches tracing.' }],
      proposedFix: 'Record a bounded identifier instead.',
      validation: 'Add a redaction test and run the focused WTA test.',
      fixDisposition: { state: 'advice-only', reason: 'Medium findings are not automatically fixed.' },
    }],
  });
  assert.match(validateReport(candidate, scope()).findings[0].id, /^ITSEC-[A-F0-9]{12}$/);
});

test('accepts wrong-session HIGH as blocking and renders it first', () => {
  const candidate = report({
    findings: [{
      rule: 'session-route-target-binding',
      severity: 'high',
      confidence: 'high',
      category: 'session-routing',
      file: 'tools/wta/src/master/mod.rs',
      startLine: 20,
      endLine: 24,
      observed: 'Changed routing bypasses the authoritative session owner map.',
      expected: 'Resolve every request through session_to_helper.',
      impact: 'An action can reach the wrong pane.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:20-24', detail: 'The new branch uses an unbound helper.' }],
      proposedFix: 'Restore the owner-bound lookup.',
      validation: 'Add a wrong-session test and run the WTA suite.',
      fixDisposition: { state: 'blocked', reason: 'No safe validated patch exists in the read-only workflow.' },
    }],
  });
  const validated = validateReport(candidate, scope());
  assert.match(renderReport(validated), /Must fix \/ blocking[\s\S]+ITSEC-/);
});

test('only native validation promotes independently source-reviewed repair proposals', () => {
  const current = repairScope();
  const candidate = {
    ...report(),
    scopeSha256: current.scopeSha256,
    mode: 'repair',
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'wta-tests', status: 'pass', headSha: HEAD, evidence: 'local command: cargo test focused-security-test (exit 0)' },
    ],
    review: {
      status: 'source-pass',
      reviewer: 'ghaw-pr-security-reviewer',
      headSha: HEAD,
      patchSha256: PATCH_SHA256,
      evidence: 'Independent exact-patch source review returned SOURCE_PASS.',
    },
    findings: [{
      rule: 'session-route-target-binding',
      severity: 'high',
      confidence: 'high',
      category: 'session-routing',
      file: 'tools/wta/src/master/mod.rs',
      startLine: 20,
      endLine: 24,
      observed: 'Changed routing bypasses owner binding.',
      expected: 'Preserve owner binding.',
      impact: 'Wrong-pane mutation.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:20', detail: 'Unbound route.' }],
      proposedFix: 'Restore lookup.',
      validation: 'Focused wrong-session test passed.',
      fixDisposition: { state: 'fixed', reason: 'Minimal source patch and focused regression test passed.' },
    }],
    patch: [{ path: 'tools/wta/src/master/mod.rs', summary: 'Restore owner-bound lookup.' }],
  };
  const validated = validateReport(candidate, current);
  validatePatch(validated, ['tools/wta/src/master/mod.rs'], PATCH_TEXT);
  validateQueuedOutput(validated, { items: [{ type: 'noop' }], errors: [] });
  assert.throws(() => validateQueuedOutput(validated, { items: [{ type: 'push_to_pull_request_branch' }] }), /noop/);

  const proposal = structuredClone(candidate);
  proposal.checks = [candidate.checks[0]];
  proposal.findings[0].fixDisposition = { state: 'proposed', reason: 'Source-reviewed candidate awaits trusted validation.' };
  assert.throws(() => validateReport(proposal, current), /fix disposition/);
  assert.equal(validateProposal(proposal, current).findings[0].fixDisposition.state, 'proposed');
  assert.throws(() => validateProposal(candidate, current), /cannot claim passing/);
  const prematurePass = structuredClone(proposal);
  prematurePass.checks.push(candidate.checks[1]);
  assert.throws(() => validateProposal(prematurePass, current), /cannot claim passing/);
  assert.throws(() => validateProposal(proposal, scope('fork')), /same-repository/);
  const mixedProposalScope = structuredClone(current);
  mixedProposalScope.changedFiles.push({ status: 'M', path: '.github/workflows/build.yml' });
  assert.throws(() => validateProposal(proposal, mixedProposalScope), /non-WTA-source/);
  const sourceRejected = structuredClone(proposal);
  sourceRejected.review.status = 'fail';
  assert.throws(() => validateProposal(sourceRejected, current), /independent security review/);
  assert.throws(() => attestChecks(proposal, HEAD, false, PATCH_TEXT), /trusted final-patch validation/);
  assert.throws(() => attestChecks(candidate, HEAD, true, PATCH_TEXT), /must propose repairs/);
  assert.throws(() => attestChecks(proposal, HEAD, true, `${PATCH_TEXT}changed`), /exact final patch/);
  const failedReview = structuredClone(proposal);
  failedReview.review.status = 'fail';
  assert.throws(() => attestChecks(failedReview, HEAD, true, PATCH_TEXT), /SOURCE_PASS/);
  const staleReview = structuredClone(proposal);
  staleReview.review.headSha = BASE;
  assert.throws(() => attestChecks(staleReview, HEAD, true, PATCH_TEXT), /SOURCE_PASS/);
  const mediumProposal = structuredClone(proposal);
  mediumProposal.findings[0].severity = 'medium';
  assert.throws(() => attestChecks(mediumProposal, HEAD, true, PATCH_TEXT), /HIGH\/high-confidence/);
  const attested = attestChecks(proposal, HEAD, true, PATCH_TEXT);
  assert.equal(attested.findings[0].fixDisposition.state, 'fixed');
  const finalReport = validateReport(attested, current);
  validatePatch(finalReport, ['tools/wta/src/master/mod.rs'], PATCH_TEXT);
  assert.equal(proposal.findings[0].fixDisposition.state, 'proposed');
});

test('native report templates preserve immutable identity and cannot pass untouched', () => {
  for (const current of [scope('fork'), repairScope()]) {
    const template = createReportTemplate(current);
    for (const key of ['version', 'prNumber', 'baseSha', 'headSha', 'scopeSha256', 'repositoryRelation', 'mode']) {
      assert.equal(template[key], current[key]);
    }
    assert.throws(() => validateReport(template, current), /summary/);
    template.summary = 'Reviewed every applicable hunk; no introduced regression found.';
    assert.doesNotThrow(() => validateReport(template, current));
    assert.throws(
      () => validateReport({ ...template, mode: current.mode === 'guide' ? 'repair' : 'guide' }, current),
      /identity/,
    );
  }
  assert.throws(() => createReportTemplate({}), /immutable scope/);
});

test('only trusted post-step attestation can authorize a passing repair check', () => {
  const current = repairScope();
  const claimed = {
    ...createReportTemplate(current),
    summary: 'Reviewed immutable source and proposed validation evidence.',
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'wta-tests', status: 'pass', headSha: HEAD, evidence: 'local command: untrusted claim (exit 0)' },
    ],
  };

  const unattested = attestChecks(claimed, HEAD, false);
  assert.equal(unattested.checks.some(check => check.name === 'wta-tests' && check.status === 'pass'), false);
  const attested = attestChecks(claimed, HEAD, true);
  assert.doesNotThrow(() => validateReport(attested, current));
  assert.match(attested.checks.find(check => check.name === 'wta-tests').evidence, /trusted isolated container:/);
});

test('automatic repair accepts only modifications to existing WTA Rust source', () => {
  assert.doesNotThrow(() => validateRepairScope(repairScopeWithStatus('M')));
  for (const status of ['A', 'D', 'T', 'U', 'X', 'B']) {
    assert.throws(
      () => validateRepairScope(repairScopeWithStatus(status)),
      new RegExp(`${status}:tools/wta/src/master/mod.rs`),
    );
  }
  assert.throws(
    () => validateRepairScope(buildScope(
      BASE,
      HEAD,
      17,
      'same-repo',
      'R100\0tools/wta/src/old.rs\0tools/wta/src/master/mod.rs\0',
      BASE,
      'repair',
    )),
    /R100:tools\/wta\/src\/master\/mod.rs/,
  );
  assert.throws(
    () => validateRepairScope(buildScope(
      BASE, HEAD, 17, 'same-repo',
      'C100\0tools/wta/src/old.rs\0tools/wta/src/master/mod.rs\0',
      BASE, 'repair',
    )),
    /C100:tools\/wta\/src\/master\/mod.rs/,
  );
});

test('trusted repair staging rejects symlinks and mode changes', () => {
  const root = join(tmpdir(), `ghaw-security-${process.pid}-${Date.now()}`);
  const source = join(root, 'source');
  const target = join(root, 'target');
  const relative = 'tools/wta/src/master/mod.rs';
  mkdirSync(join(source, 'tools/wta/src/master'), { recursive: true });
  mkdirSync(join(target, 'tools/wta/src/master'), { recursive: true });
  writeFileSync(join(source, relative), 'safe\n');
  writeFileSync(join(target, relative), 'base\n');
  const candidate = { patch: [{ path: relative }] };
  if (process.platform !== 'win32') {
    chmodSync(join(source, relative), 0o755);
    assert.throws(() => stageRepairFiles(candidate, source, target), /file mode/);
    chmodSync(join(source, relative), 0o644);
  }
  writeFileSync(join(source, 'link-target'), 'unsafe\n');
  unlinkSync(join(source, relative));
  symlinkSync(join(source, 'link-target'), join(source, relative));
  assert.throws(() => stageRepairFiles(candidate, source, target), /symlink|regular tracked file/);
});

test('trusted repair staging rejects symlinked source ancestors', () => {
  const root = join(tmpdir(), `ghaw-security-parent-${process.pid}-${Date.now()}`);
  const source = join(root, 'source');
  const target = join(root, 'target');
  const outside = join(root, 'outside');
  const relative = 'tools/wta/src/master/mod.rs';
  mkdirSync(source, { recursive: true });
  mkdirSync(join(target, 'tools/wta/src/master'), { recursive: true });
  mkdirSync(join(outside, 'wta/src/master'), { recursive: true });
  writeFileSync(join(outside, 'wta/src/master/mod.rs'), 'outside\n');
  writeFileSync(join(target, relative), 'base\n');
  symlinkSync(outside, join(source, 'tools'), 'junction');
  assert.throws(
    () => stageRepairFiles({ patch: [{ path: relative }] }, source, target),
    /symlink or reparse-point ancestor/,
  );
});

test('rejects a fixed finding without applicable validation or independent PASS', () => {
  const current = repairScope();
  const candidate = {
    ...report(),
    scopeSha256: current.scopeSha256,
    mode: 'repair',
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'manual-review', status: 'pass', headSha: HEAD, evidence: 'local command: git diff (exit 0)' },
    ],
    findings: [{
      rule: 'session-route-target-binding',
      severity: 'high',
      confidence: 'high',
      category: 'session-routing',
      file: 'tools/wta/src/master/mod.rs',
      startLine: 20,
      endLine: 24,
      observed: 'Changed routing bypasses owner binding.',
      expected: 'Preserve owner binding.',
      impact: 'Wrong-pane mutation.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:20', detail: 'Unbound route.' }],
      proposedFix: 'Restore lookup.',
      validation: 'No applicable executable validation.',
      fixDisposition: { state: 'fixed', reason: 'Claimed fixed.' },
    }],
    patch: [{ path: 'tools/wta/src/master/mod.rs', summary: 'Restore lookup.' }],
  };
  assert.throws(() => validateReport(candidate, current), /applicable passing validation/);
});

test('rejects patch paths without a matching fixed finding', () => {
  const current = repairScope();
  const candidate = {
    ...report(),
    scopeSha256: current.scopeSha256,
    mode: 'repair',
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'wta-tests', status: 'pass', headSha: HEAD, evidence: 'local command: cargo test focused-security-test (exit 0)' },
    ],
    review: {
      status: 'source-pass',
      reviewer: 'ghaw-pr-security-reviewer',
      headSha: HEAD,
      patchSha256: PATCH_SHA256,
      evidence: 'Independent exact-patch source review returned SOURCE_PASS.',
    },
    findings: [{
      rule: 'session-route-target-binding',
      severity: 'high',
      confidence: 'high',
      category: 'session-routing',
      file: 'tools/wta/src/master/mod.rs',
      startLine: 20,
      endLine: 24,
      observed: 'Changed routing bypasses owner binding.',
      expected: 'Preserve owner binding.',
      impact: 'Wrong-pane mutation.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:20', detail: 'Unbound route.' }],
      proposedFix: 'Restore lookup.',
      validation: 'Focused wrong-session test passed.',
      fixDisposition: { state: 'fixed', reason: 'Minimal source patch and focused regression test passed.' },
    }],
    patch: [
      { path: 'tools/wta/src/master/mod.rs', summary: 'Restore owner-bound lookup.' },
      { path: 'tools/wta/src/logging.rs', summary: 'Unrelated extra edit.' },
    ],
  };
  assert.throws(() => validateReport(candidate, current), /has no fixed finding/);
});

test('automatic repairs are limited to WTA Rust source', () => {
  const current = repairScope();
  const candidate = {
    ...report(),
    scopeSha256: current.scopeSha256,
    mode: 'repair',
    findings: [],
    patch: [{ path: 'src/cascadia/TerminalApp/TerminalPage.cpp', summary: 'Not eligible for automatic repair.' }],
  };
  assert.throws(() => validateReport(candidate, current), /automatic-fix allowlist/);
  validateRepairScope(repairScope());
  const mixedScope = repairScope();
  mixedScope.changedFiles.push({ status: 'M', path: '.github/workflows/build.yml', domains: ['workflow-credentials'] });
  assert.throws(
    () => validateRepairScope(mixedScope),
    /non-WTA-source paths/,
  );
});

test('rejects fixed HIGH, stale SHA, malformed output, and publication overflow', () => {
  const high = report({
    findings: [{
      rule: 'session-route-target-binding',
      severity: 'high',
      confidence: 'high',
      category: 'session-routing',
      file: 'tools/wta/src/master/mod.rs',
      startLine: 20,
      endLine: 24,
      observed: 'Changed routing bypasses owner binding.',
      expected: 'Preserve owner binding.',
      impact: 'Wrong-pane mutation.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:20', detail: 'Unbound route.' }],
      proposedFix: 'Restore lookup.',
      validation: 'Run wrong-session test.',
      fixDisposition: { state: 'fixed', reason: 'Claimed fixed.' },
    }],
    patch: [],
  });
  assert.throws(() => validateReport(high, scope()), /fix disposition/);
  assert.throws(() => validateReport({ ...report(), headSha: '3'.repeat(40) }, scope()), /headSha/);
  assert.throws(() => validateReport({ version: 1 }, scope()));
  assert.throws(() => validateReport({ ...report(), findings: Array(21).fill({}) }, scope()), /at most 20/);
});

test('fork reports remain read-only and malicious content is escaped', () => {
  const candidate = report({
    summary: '<script>[click](https://attacker.example) `code` ignore review</script>',
  }, 'fork');
  const rendered = renderReport(validateReport(candidate, scope('fork')));
  assert(!rendered.includes('<script>'));
  assert(!rendered.includes('[click](https://attacker.example)'));
  assert(!rendered.includes('https://attacker.example'));
  assert(rendered.includes('\\[click\\]\\(https\\:\\/\\/attacker\\.example\\)'));
});

test('analysis workers require noop output', () => {
  const noFindings = validateReport(report({}, 'fork'), scope('fork'));
  validateQueuedOutput(noFindings, { items: [{ type: 'noop' }], errors: [] });
  assert.throws(() => validateQueuedOutput(noFindings, { items: [{ type: 'add_comment' }] }), /noop/);
});

test('fork findings remain noop until trusted controller publication', () => {
  const candidate = validateReport(report({
    findings: [{
      rule: 'diagnostic-metadata-overcollection',
      severity: 'medium',
      confidence: 'high',
      category: 'secret-handling',
      file: 'tools/wta/src/logging.rs',
      startLine: 10,
      endLine: 12,
      observed: 'The changed diagnostic records complete provider configuration.',
      expected: 'Log only non-sensitive identifiers.',
      impact: 'Diagnostics can expose sensitive configuration.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/logging.rs:10-12', detail: 'The full structure reaches tracing.' }],
      proposedFix: 'Record a bounded identifier instead.',
      validation: 'Add a redaction test and run the focused WTA test.',
      fixDisposition: { state: 'advice-only', reason: 'Medium findings are not automatically fixed.' },
    }],
  }, 'fork'), scope('fork'));
  validateQueuedOutput(candidate, { items: [{ type: 'noop' }], errors: [] });
  assert.throws(() => validateQueuedOutput(candidate, { items: [{ type: 'add_comment' }] }), /noop/);
});

test('rejects secret-like diagnostic evidence and unsupported passing checks', () => {
  assert.throws(() => validateReport(report({
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'wta-tests', status: 'pass', headSha: HEAD, evidence: 'Everything looked green.' },
    ],
  }), scope()), /local command evidence/);
  assert.throws(() => validateReport(report({
    checks: [
      { name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: 'Immutable diff classified.' },
      { name: 'wta-tests', status: 'pass', headSha: HEAD, evidence: 'https://github.com/other/repo/actions/runs/123' },
    ],
  }), scope()), /local command evidence/);
  assert.throws(() => validateReport(report({
    summary: 'token=abcdefghijklmnopqrstuvwxyz123456',
  }), scope()), /secret material/);
});

test('native CLI and permitted PowerShell report writes work end to end', () => {
  const root = mkdtempSync(join(tmpdir(), 'ghaw-security-native-replay-'));
  const workspace = join(root, 'checkout');
  mkdirSync(workspace);
  const validator = fileURLToPath(new URL('./security-review.mjs', import.meta.url));
  const invoke = (...args) => spawnSync(process.execPath, [validator, ...args], {
    cwd: workspace, encoding: 'utf8', timeout: 30_000,
  });
  const git = (...args) => execFileSync('git', args, {
    cwd: workspace, encoding: 'utf8', timeout: 30_000,
  }).trim();
  try {
    git('init', '--quiet');
    git('config', 'user.name', 'Local contract fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    mkdirSync(join(workspace, 'tools', 'wta', 'src'), { recursive: true });
    const source = join(workspace, 'tools', 'wta', 'src', 'routing.rs');
    writeFileSync(source, 'fn route() { /* owner-bound base */ }\n');
    git('add', '.');
    git('commit', '--quiet', '-m', 'Local fixture base');
    const base = git('rev-parse', 'HEAD');
    writeFileSync(source, 'fn route() { /* changed route for review */ }\n');
    git('add', '.');
    git('commit', '--quiet', '-m', 'Local fixture head');
    const head = git('rev-parse', 'HEAD');
    // Keep report artifacts outside the checkout, as on the hosted runner.
    const artifacts = join(root, 'artifacts');
    mkdirSync(artifacts);
    for (const [relation, mode] of [['fork', 'guide'], ['same-repo', 'repair']]) {
      const scopePath = join(artifacts, `${mode}-scope.json`);
      const reportPath = join(artifacts, `${mode}-report.json`);
      const validated = join(artifacts, `${mode}-validated.json`);
      const summary = join(artifacts, `${mode}-summary.md`);
      const status = join(artifacts, `${mode}-status.txt`);
      let result = invoke('scope', '--base', base, '--head', head, '--pr', '17',
        '--relation', relation, '--mode', mode, '--output', scopePath);
      assert.equal(result.status, 0, result.stderr);
      result = invoke('init-report', '--scope', scopePath, '--output', reportPath);
      assert.equal(result.status, 0, result.stderr);
      const validate = () => invoke('validate', '--scope', scopePath, '--report', reportPath,
        '--validated', validated, '--summary', summary, '--status', status);
      result = validate();
      assert.equal(result.status, 1);
      assert.match(result.stderr, /summary/);
      result = spawnSync('pwsh', ['-NoProfile', '-NonInteractive', '-Command', `
        $ErrorActionPreference = 'Stop'
        $path = $env:SECURITY_REPORT_PATH
        $report = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
        $report.summary = 'Reviewed the immutable patch; no regression found.'
        $json = ConvertTo-Json -InputObject $report -Depth 12
        [System.IO.File]::WriteAllText($path, $json, [System.Text.UTF8Encoding]::new($false))
      `], {
        cwd: workspace, encoding: 'utf8', timeout: 30_000,
        env: { ...process.env, SECURITY_REPORT_PATH: reportPath },
      });
      assert.equal(result.status, 0, result.stderr || result.error?.message);
      result = validate();
      assert.equal(result.status, 0, result.stderr);
      assert.equal(readFileSync(status, 'utf8'), 'pass\n');
      const queue = join(artifacts, `${mode}-queue.json`);
      writeFileSync(queue, JSON.stringify({ items: [{ type: 'noop' }], errors: [] }));
      result = invoke('validate-output', '--validated', validated, '--agent-output', queue);
      assert.equal(result.status, 0, result.stderr);
      writeFileSync(queue, JSON.stringify({ items: [{ type: 'add_comment' }], errors: [] }));
      result = invoke('validate-output', '--validated', validated, '--agent-output', queue);
      assert.equal(result.status, 1);
      assert.match(result.stderr, /exactly one noop/);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
