import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  attestChecks, buildScope, classifyPath, createReportTemplate, normalizePath, renderReport, validatePatch,
  validateQueuedOutput, validateReport, validateProposal, validateCandidate, stageRepairFiles, validateRepairScope,
  submitSecurityReport, readSecurityDiff, readImmutableHunks, readSecuritySource, inspectSecurityRepair, writeSecurityRepair, replaceSecurityRepairText, verifyCredentialFree,
} from './security-review.mjs';

const BASE = '1'.repeat(40);
const tmpdir = () => process.cwd();
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
    ['tools/wta/src/master/mod.rs', 'tools/wta/src/logging.rs'].map(path => ({
      path, headLineCount: 120, hunks: [{ baseStart: 20, baseCount: 5, headStart: 20, headCount: 5 }],
    })),
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

test('unrelated unchanged line cannot authorize an automatic repair', () => {
  const current = buildScope(BASE, HEAD, 17, 'same-repo', 'M\0tools/wta/src/master/mod.rs\0',
    BASE, 'repair', [{
      path: 'tools/wta/src/master/mod.rs', headLineCount: 120,
      hunks: [{ baseStart: 10, baseCount: 1, headStart: 10, headCount: 1 }],
    }]);
  const candidate = {
    ...createReportTemplate(current), summary: 'Repair candidate.',
    review: { status: 'pending', reviewer: 'ghaw-pr-security-reviewer', evidence: 'Awaiting independent review.' },
    findings: [{
      rule: 'session-route-target-binding', severity: 'high', confidence: 'high', category: 'session-routing',
      file: 'tools/wta/src/master/mod.rs', startLine: 100, endLine: 100,
      observed: 'Unchanged pre-existing route.', expected: 'Owner binding.', impact: 'Wrong session.',
      evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:100', detail: 'Pre-existing route.' }],
      proposedFix: 'Restore binding.', validation: 'Run focused tests.',
      fixDisposition: { state: 'proposed', reason: 'Awaiting validation.' },
    }],
    patch: [{ path: 'tools/wta/src/master/mod.rs', summary: 'Restore binding.' }],
  };
  assert.throws(() => validateCandidate(candidate, current), /immutable HEAD diff hunk/);
  const proposal = structuredClone(candidate);
  proposal.review = {
    status: 'source-pass', reviewer: 'ghaw-pr-security-reviewer', headSha: HEAD,
    patchSha256: PATCH_SHA256, evidence: 'Independent exact-patch review.',
  };
  assert.throws(() => validateProposal(proposal, current), /immutable HEAD diff hunk/);
  const final = attestChecks(proposal, HEAD, true, PATCH_TEXT);
  assert.throws(() => validateReport(final, current), /immutable HEAD diff hunk/);
  for (const [input, validate] of [[candidate, validateCandidate], [proposal, validateProposal], [final, validateReport]]) {
    input.findings[0].startLine = 10;
    input.findings[0].endLine = 10;
    assert.doesNotThrow(() => validate(input, current));
    input.findings[0].endLine = 121;
    assert.throws(() => validate(input, current), /source EOF/);
    input.findings[0].endLine = 10;
  }
  const tampered = structuredClone(current);
  tampered.immutableHunks[0].hunks[0].headStart = 100;
  assert.throws(() => validateCandidate(candidate, tampered), /scope identity/);
  const legacy = buildScope(BASE, HEAD, 17, 'same-repo', 'M\0tools/wta/src/master/mod.rs\0', BASE, 'repair');
  assert.throws(() => validateCandidate({ ...candidate, scopeSha256: legacy.scopeSha256 }, legacy), /immutable HEAD diff hunk/);
  const blocked = structuredClone(candidate);
  blocked.findings[0].startLine = blocked.findings[0].endLine = 100;
  blocked.findings[0].fixDisposition.state = 'blocked';
  blocked.review = { status: 'not-required', reviewer: 'none', evidence: 'Guidance only.' };
  blocked.patch = [];
  assert.doesNotThrow(() => validateCandidate(blocked, current));
  const root = mkdtempSync(join(process.cwd(), '.hunk-submission-'));
  try {
    const path = join(root, 'report.json');
    writeFileSync(path, 'original');
    const unrelated = structuredClone(candidate);
    unrelated.findings[0].startLine = unrelated.findings[0].endLine = 100;
    assert.throws(() => submitSecurityReport(JSON.stringify(unrelated), current, path), /immutable HEAD diff hunk/);
    assert.equal(readFileSync(path, 'utf8'), 'original');
    assert.equal(submitSecurityReport(JSON.stringify(candidate), current, path).accepted, true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('trusted Git scope binds exact changes, deletion mapping, renames, modes and empty HEAD', () => {
  const root = mkdtempSync(join(process.cwd(), '.immutable-hunks-'));
  const path = 'tools/wta/src/routing.rs';
  const validator = fileURLToPath(new URL('./security-review.mjs', import.meta.url));
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', timeout: 30_000 }).trim();
  const source = join(root, path);
  const lines = Array.from({ length: 120 }, (_, index) => `line ${index + 1}\n`);
  try {
    git('init', '--quiet');
    git('config', 'user.name', 'Local contract fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    git('config', 'core.autocrlf', 'false');
    mkdirSync(join(root, 'tools', 'wta', 'src'), { recursive: true });
    writeFileSync(source, lines.join(''));
    git('add', '.');
    git('commit', '--quiet', '-m', 'Fixture base');
    const base = git('rev-parse', 'HEAD');
    const changed = [...lines];
    changed[9] = 'changed line 10\n';
    writeFileSync(source, changed.join(''));
    git('add', '.');
    git('commit', '--quiet', '-m', 'Fixture head');
    const head = git('rev-parse', 'HEAD');
    const scopePath = join(root, 'scope.json');
    const cli = spawnSync(process.execPath, [validator, 'scope', '--base', base, '--head', head,
      '--pr', '17', '--relation', 'same-repo', '--mode', 'repair', '--output', scopePath],
    { cwd: root, encoding: 'utf8', timeout: 30_000 });
    assert.equal(cli.status, 0, cli.stderr);
    const current = JSON.parse(readFileSync(scopePath, 'utf8'));
    assert.deepEqual(current.immutableHunks, [{
      path, headLineCount: 120, hunks: [{ baseStart: 10, baseCount: 1, headStart: 10, headCount: 1 }],
    }]);
    const candidate = {
      ...createReportTemplate(current), summary: 'A localized repair candidate.',
      review: { status: 'pending', reviewer: 'ghaw-pr-security-reviewer', evidence: 'Awaiting source review.' },
      findings: [{
        rule: 'session-route-target-binding', severity: 'high', confidence: 'high', category: 'session-routing',
        file: path, startLine: 100, endLine: 100, observed: 'Route.', expected: 'Owner binding.', impact: 'Wrong session.',
        evidence: [{ kind: 'source-trace', reference: `${path}:100`, detail: 'Route trace.' }],
        proposedFix: 'Bind owner.', validation: 'Focused tests.',
        fixDisposition: { state: 'proposed', reason: 'Awaiting trusted validation.' },
      }], patch: [{ path, summary: 'Bind owner.' }],
    };
    assert.throws(() => validateCandidate(candidate, current), /immutable HEAD diff hunk/);
    const proposal = structuredClone(candidate);
    proposal.review = {
      status: 'source-pass', reviewer: 'ghaw-pr-security-reviewer', headSha: head,
      patchSha256: PATCH_SHA256, evidence: 'Independent exact-patch source review.',
    };
    assert.throws(() => validateProposal(proposal, current), /immutable HEAD diff hunk/);
    assert.throws(() => validateReport(attestChecks(proposal, head, true, PATCH_TEXT), current), /immutable HEAD diff hunk/);
    const reportPath = join(root, 'report.json');
    writeFileSync(reportPath, JSON.stringify(proposal));
    const rejected = spawnSync(process.execPath, [validator, 'check-report', '--scope', scopePath, '--report', reportPath],
      { cwd: root, encoding: 'utf8', timeout: 30_000 });
    assert.equal(rejected.status, 1);
    assert.match(rejected.stderr, /immutable HEAD diff hunk/);
    unlinkSync(reportPath);
    const target = join(root, 'target');
    git('-c', 'core.autocrlf=false', 'clone', '--quiet', '--no-hardlinks', root, target);
    writeFileSync(source, changed.join('').replace('line 100\n', 'unrelated repaired line\n'));
    const regeneratedPath = join(root, 'regenerated-scope.json');
    const regenerated = spawnSync(process.execPath, [validator, 'scope', '--base', base, '--head', head,
      '--pr', '17', '--relation', 'same-repo', '--mode', 'repair', '--output', regeneratedPath],
    { cwd: root, encoding: 'utf8', timeout: 30_000 });
    assert.equal(regenerated.status, 0, regenerated.stderr);
    assert.deepEqual(JSON.parse(readFileSync(regeneratedPath, 'utf8')), current,
      'scope identity must remain identical after candidate workspace edits');
    unlinkSync(regeneratedPath);
    assert.throws(() => stageRepairFiles(candidate, root, target), /immutable HEAD diff hunk/);
    assert.equal(readFileSync(join(target, path), 'utf8'), changed.join(''));
    candidate.findings[0].startLine = candidate.findings[0].endLine = 10;
    proposal.findings[0].startLine = proposal.findings[0].endLine = 10;
    assert.doesNotThrow(() => validateCandidate(candidate, current));
    assert.doesNotThrow(() => validateProposal(proposal, current));
    assert.doesNotThrow(() => validateReport(attestChecks(proposal, head, true, PATCH_TEXT), current));
    assert.deepEqual(stageRepairFiles(candidate, root, target), [path]);
    assert.equal(readFileSync(join(target, path), 'utf8'), readFileSync(source, 'utf8'));
    rmSync(target, { recursive: true, force: true });
    unlinkSync(scopePath);
    writeFileSync(source, `inserted first\ninserted second\n${changed.join('')}`);
    git('add', '.');
    const shiftedTree = git('write-tree');
    assert.deepEqual(readImmutableHunks(buildScope(base, shiftedTree, 17, 'same-repo', `M\0${path}\0`), root)[0].hunks, [
      { baseStart: 1, baseCount: 0, headStart: 1, headCount: 2 },
      { baseStart: 10, baseCount: 1, headStart: 12, headCount: 1 },
    ]);
    for (const [content, expected] of [
      [lines.filter((_, index) => index !== 9).join(''), { baseStart: 10, baseCount: 1, headStart: 10, headCount: 0 }],
      [lines.slice(0, -1).join(''), { baseStart: 120, baseCount: 1, headStart: 120, headCount: 0 }],
      [lines.slice(1).join(''), { baseStart: 1, baseCount: 1, headStart: 1, headCount: 0 }],
      ['', { baseStart: 1, baseCount: 120, headStart: 1, headCount: 0 }],
    ]) {
      writeFileSync(source, content);
      git('add', '.');
      const tree = git('write-tree');
      const inputs = buildScope(base, tree, 17, 'same-repo', `M\0${path}\0`, base, 'repair');
      const metadata = readImmutableHunks(inputs, root);
      assert.deepEqual(metadata[0].hunks, [expected]);
      const deletion = buildScope(base, tree, 17, 'same-repo', `M\0${path}\0`, base, 'repair', metadata);
      const anchor = Math.min(expected.headStart, metadata[0].headLineCount) || 1;
      const report = structuredClone(candidate);
      Object.assign(report, createReportTemplate(deletion), {
        summary: candidate.summary, findings: structuredClone(candidate.findings), patch: candidate.patch, review: candidate.review,
      });
      report.findings[0].startLine = report.findings[0].endLine = anchor;
      if (content) assert.doesNotThrow(() => validateCandidate(report, deletion));
      else assert.throws(() => validateCandidate(report, deletion), /source EOF/);
    }
    writeFileSync(source, lines.join(''));
    git('add', '.');
    git('update-index', '--chmod=+x', path);
    const modeTree = git('write-tree');
    const modeGuide = buildScope(base, modeTree, 17, 'fork', `M\0${path}\0`);
    assert.deepEqual(readImmutableHunks(modeGuide, root)[0].hunks, []);
    const modeReport = { ...createReportTemplate(modeGuide), summary: 'Mode-only guidance.',
      findings: [{ ...candidate.findings[0], startLine: 100, endLine: 100,
        fixDisposition: { state: 'blocked', reason: 'Mode change needs guidance, not automatic source repair.' } }] };
    assert.doesNotThrow(() => validateReport(modeReport, modeGuide));
    git('update-index', '--chmod=-x', path);
    const renamed = 'tools/wta/src/renamed.rs';
    git('mv', path, renamed);
    const renameTree = git('write-tree');
    const guide = buildScope(base, renameTree, 17, 'fork', `R100\0${path}\0${renamed}\0`);
    assert.deepEqual(readImmutableHunks(guide, root)[0].hunks, []);
    const guidance = { ...createReportTemplate(guide), summary: 'Rename guidance.',
      findings: [{ ...candidate.findings[0], file: path, startLine: 100, endLine: 100,
        fixDisposition: { state: 'blocked', reason: 'Read-only unchanged context trace.' } }] };
    assert.doesNotThrow(() => validateReport(guidance, guide));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('classifies project trust boundaries', () => {
  assert.deepEqual(classifyPath('tools/wta/src/master/mod.rs'), ['build-tooling', 'session-routing', 'wta', 'wta-rust']);
  assert(classifyPath('src/cascadia/TerminalProtocol/TerminalProtocol.idl').includes('com-protocol'));
  assert(classifyPath('src/cascadia/TerminalSettingsEditor/Settings.xaml').includes('product-source'));
  assert(classifyPath('src/cascadia/TerminalApp/TerminalApp.vcxproj').includes('product-source'));
  assert(classifyPath('test/e2e/tests/Feature.Agent.Tests.ps1').includes('product-tests'));
  assert(classifyPath('tools/wta/prompts/terminal-agent.md').includes('wta'));
  assert(classifyPath('.github/workflows/review.yml').includes('workflow-credentials'));
  assert(classifyPath('.github/skills/reviewer/SKILL.md').includes('workflow-credentials'));
  assert(classifyPath('.github/policies/resourceManagement.yml').includes('workflow-credentials'));
  assert(classifyPath('.github/instructions/security.instructions.md').includes('workflow-credentials'));
  assert(classifyPath('tools/razzle.cmd').includes('build-tooling'));
  assert(classifyPath('.cargo/config.toml').includes('build-tooling'));
});

test('Cargo configuration triggers review but cannot enter automatic repair', () => {
  const controller = readFileSync(new URL('../../../workflows/ghaw-pr-security-controller.yml', import.meta.url), 'utf8');
  assert(controller.includes("- '.cargo/**'"));
  const cargoOnly = buildScope(BASE, HEAD, 17, 'same-repo', 'M\0.cargo/config.toml\0', BASE, 'repair');
  assert.equal(cargoOnly.applicable, true);
  assert(cargoOnly.domains.includes('build-tooling'));
  assert.throws(() => validateRepairScope(cargoOnly), /non-WTA-source paths/);
  const mixed = buildScope(BASE, HEAD, 17, 'same-repo',
    'M\0.cargo/config.toml\0M\0tools/wta/src/master/session_mcp.rs\0', BASE, 'repair');
  assert.equal(mixed.applicable, true);
  assert.throws(() => validateRepairScope(mixed), /non-WTA-source paths/);
});

test('loaded instruction roots have coherent triggers and classification without Rust repair eligibility', () => {
  const workflow = name => readFileSync(new URL(`../../../workflows/${name}`, import.meta.url), 'utf8');
  const controller = workflow('ghaw-pr-security-controller.yml');
  const pathsBlock = controller.match(/    paths:\r?\n((?:      - '[^']+'\r?\n)+)/)?.[1];
  assert(pathsBlock, 'controller must declare review path triggers');
  const triggers = [...pathsBlock.matchAll(/      - '([^']+)'/g)].map(match => match[1]);
  assert.deepEqual(triggers, [
    'AGENTS.md', '.agents/**', '.cargo/**', '.github/**', 'build/**',
    'src/**', 'tools/**', 'test/**', 'doc/security-model.md',
  ]);
  const triggered = path => triggers.some(pattern =>
    pattern.endsWith('/**') ? path.startsWith(pattern.slice(0, -2)) : path === pattern);
  for (const name of ['ghaw-pr-security.lock.yml', 'ghaw-pr-security-guide-fork.lock.yml']) {
    const worker = workflow(name);
    assert(worker.includes('GH_AW_AGENT_FILES: "AGENTS.md"'));
    assert(worker.includes('GH_AW_AGENT_FOLDERS: ".agents .github"'));
  }
  for (const path of [
    'AGENTS.md', '.agents/skills/reviewer/SKILL.md',
    '.github/instructions/security.instructions.md',
  ]) {
    assert.equal(triggered(path), true);
    assert.deepEqual(classifyPath(path), ['workflow-credentials']);
    const instructionOnly = buildScope(BASE, HEAD, 17, 'same-repo', `M\0${path}\0`, BASE, 'repair');
    assert.equal(instructionOnly.applicable, true);
    assert.throws(() => validateRepairScope(instructionOnly), /non-WTA-source paths/);
    const mixed = buildScope(BASE, HEAD, 17, 'same-repo',
      `M\0${path}\0M\0tools/wta/src/master/mod.rs\0`, BASE, 'repair');
    assert.equal(mixed.applicable, true);
    assert.throws(() => validateRepairScope(mixed), /non-WTA-source paths/);
  }
  for (const path of ['AGENTS.md.bak', 'docs/AGENTS.md', '.agents-backup/skills/reviewer/SKILL.md']) {
    assert.equal(triggered(path), false);
    assert.deepEqual(classifyPath(path), []);
  }
});

test('guide cannot delegate and repair uses trusted fixed reviewer orchestration', () => {
  const workflow = name => readFileSync(new URL(`../../../workflows/${name}`, import.meta.url), 'utf8');
  const guide = workflow('ghaw-pr-security-guide-fork.md');
  assert(guide.includes("args: ['--excluded-tools', 'task', 'read_agent', 'write_agent', 'list_agents']"));
  const repair = workflow('ghaw-pr-security.md');
  assert(repair.includes('security-review-native/security-review-driver.mjs'));
  assert(repair.includes('watchdog-timeout: 600'));
  assert(repair.includes('max-retries: 0'));
  assert(repair.includes('install_copilot_cli.sh" 1.0.90'));
  assert(repair.includes('git show "$TRUSTED_SHA:.github/skills/ghaw-pr-security/scripts/security-review-driver.mjs"'));
  assert(!repair.includes('## agent:'));
  for (const name of ['ghaw-pr-security', 'ghaw-pr-security-reviewer']) {
    const profile = readFileSync(new URL(`../../../agents/${name}.agent.md`, import.meta.url), 'utf8');
    const header = profile.split('---')[1];
    assert(header.includes('tools:'));
    assert(!/^\s*-\s*['"]?(?:agent|custom-agent|Task|execute|shell|bash|powershell|edit|\*)['"]?\s*$/im.test(header));
  }
});

test('credential postcondition rejects retained helpers and headers rather than trusting cleanup exit', () => {
  const root = mkdtempSync(join(tmpdir(), 'ghaw-credential-postcondition-'));
  const previousGlobal = process.env.GIT_CONFIG_GLOBAL;
  const previousSystem = process.env.GIT_CONFIG_NOSYSTEM;
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', timeout: 30_000 });
  try {
    process.env.GIT_CONFIG_GLOBAL = join(root, 'isolated.gitconfig');
    process.env.GIT_CONFIG_NOSYSTEM = '1';
    writeFileSync(process.env.GIT_CONFIG_GLOBAL, '');
    git('init', '--quiet');
    assert.equal(verifyCredentialFree(root), true);
    git('config', 'credential.helper', 'retained-test-helper');
    assert.throws(() => verifyCredentialFree(root), /remains after cleanup/);
    git('config', '--unset-all', 'credential.helper');
    git('config', 'http.https://github.com/.extraheader', 'synthetic-test-header');
    assert.throws(() => verifyCredentialFree(root), /remains after cleanup/);
    git('config', '--unset-all', 'http.https://github.com/.extraheader');
    assert.equal(verifyCredentialFree(root), true);
  } finally {
    if (previousGlobal === undefined) delete process.env.GIT_CONFIG_GLOBAL;
    else process.env.GIT_CONFIG_GLOBAL = previousGlobal;
    if (previousSystem === undefined) delete process.env.GIT_CONFIG_NOSYSTEM;
    else process.env.GIT_CONFIG_NOSYSTEM = previousSystem;
    rmSync(root, { recursive: true, force: true });
  }
});

test('sensitive source paths remain applicable after rename or copy outside their domain', () => {
  for (const status of ['R100', 'C100']) {
    const current = buildScope(
      BASE, HEAD, 17, 'fork',
      `${status}\0.github/workflows/review.yml\0archive/review.txt\0`,
    );
    assert.equal(current.applicable, true);
    assert(current.domains.includes('workflow-credentials'));
    assert.equal(current.changedFiles[0].oldPath, '.github/workflows/review.yml');
    assert.equal(current.changedFiles[0].path, 'archive/review.txt');
  }
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
  const wrongDomain = structuredClone(candidate);
  wrongDomain.findings[0].category = 'cpp-memory';
  assert.throws(() => validateReport(wrongDomain, scope()), /invalid classification/);
  const wrongDisposition = structuredClone(candidate);
  wrongDisposition.findings[0].fixDisposition.state = 'blocked';
  assert.throws(() => validateReport(wrongDisposition, scope()), /invalid fix disposition/);
  assert.throws(() => validateReport({ ...candidate, summary: 's'.repeat(801) }, scope()), /at most 800/);
  assert.doesNotThrow(() => validateReport({ ...candidate, summary: 's'.repeat(800) }, scope()));
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
  const nestedExtras = JSON.parse(JSON.stringify(candidate));
  for (const object of [
    ...nestedExtras.checks, nestedExtras.review, ...nestedExtras.findings,
    ...nestedExtras.findings.flatMap(finding => [...finding.evidence, finding.fixDisposition]),
    ...nestedExtras.patch,
  ]) {
    Object.assign(object, JSON.parse('{"extra":{"password":"synthetic_credential_material_123456"},"__proto__":{"shadow":true},"constructor":{"prototype":{"shadow":true}}}'));
  }
  assert.deepEqual(validateReport(nestedExtras, current), validated);
  assert.throws(() => validateReport({ ...candidate, extra: {} }, current), /unsupported fields/);
  validatePatch(validated, ['tools/wta/src/master/mod.rs'], PATCH_TEXT);
  validateQueuedOutput(validated, { items: [{ type: 'noop' }], errors: [] });
  assert.throws(() => validateQueuedOutput(validated, { items: [{ type: 'push_to_pull_request_branch' }] }), /noop/);

  const proposal = structuredClone(candidate);
  proposal.checks = [candidate.checks[0]];
  proposal.findings[0].fixDisposition = { state: 'proposed', reason: 'Source-reviewed candidate awaits trusted validation.' };
  assert.throws(() => validateReport(proposal, current), /fix disposition/);
  assert.equal(validateProposal(proposal, current).findings[0].fixDisposition.state, 'proposed');
  assert.throws(() => validateProposal({ ...proposal, extra: {} }, current), /unsupported fields/);
  const pending = structuredClone(proposal);
  pending.review = {
    status: 'pending', reviewer: 'ghaw-pr-security-reviewer',
    evidence: 'Trusted driver must launch the independent reviewer.',
  };
  assert.equal(validateCandidate(pending, current).review.status, 'pending');
  assert.throws(() => validateCandidate({ ...pending, extra: {} }, current), /unsupported fields/);
  assert.throws(() => validateCandidate(proposal, current), /review|pending|source-pass/i);
  assert.throws(() => validateCandidate(candidate, current), /passing|pending/i);
  assert.throws(() => validateCandidate(pending, scope('fork')), /same-repository|repair/i);
  assert.throws(() => validateProposal(pending, current), /review/i);
  assert.throws(() => validateReport(pending, current), /review|disposition/i);
  assert.throws(() => attestChecks(pending, HEAD, true, PATCH_TEXT), /pending independent review/);
  const pendingReportRoot = mkdtempSync(join(tmpdir(), 'ghaw-pending-review-'));
  try {
    const pendingReportPath = join(pendingReportRoot, 'report.json');
    writeFileSync(pendingReportPath, 'original');
    assert.throws(() => submitSecurityReport(JSON.stringify(proposal), current, pendingReportPath), /model submission cannot claim trusted independent SOURCE_PASS/);
    assert.equal(readFileSync(pendingReportPath, 'utf8'), 'original');
    assert.equal(submitSecurityReport(JSON.stringify(pending), current, pendingReportPath).accepted, true);
    assert.equal(JSON.parse(readFileSync(pendingReportPath, 'utf8')).review.status, 'pending');
  } finally {
    rmSync(pendingReportRoot, { recursive: true, force: true });
  }
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

test('report envelope rejects unknown JSON fields before text validation without reflecting input', () => {
  for (const input of [null, [], true, 1, 'report']) {
    assert.throws(() => validateReport(input, scope()), /report envelope is invalid/);
  }
  const payloads = [
    { extra: { nested: { authorization: 'Bearer synthetic_credential_material_123456' } } },
    { harmlessMetadata: 'innocuous' },
    JSON.parse('{"__proto__":{"prNumber":999}}'),
    { constructor: { prototype: { headSha: BASE } } },
    { prototype: { scopeSha256: '0'.repeat(64) } },
    { 'password=synthetic_credential_material_123456': 'untrusted' },
  ];
  for (const extra of payloads) {
    const input = JSON.parse(JSON.stringify({ ...report(), ...extra }));
    assert.throws(() => validateReport(input, scope()), {
      message: 'report envelope contains unsupported fields',
    });
    input.summary = '';
    assert.throws(() => validateReport(input, scope()), {
      message: 'report envelope contains unsupported fields',
    });
  }
  const inheritedIdentity = report();
  delete inheritedIdentity.headSha;
  Object.setPrototypeOf(inheritedIdentity, { headSha: HEAD });
  assert.throws(() => validateReport(inheritedIdentity, scope()), /report envelope is invalid/);
  for (const key of ['prNumber', 'baseSha', 'headSha', 'scopeSha256', 'repositoryRelation', 'mode']) {
    const shadowed = report();
    shadowed[key] = key === 'prNumber' ? 999 : 'shadowed';
    assert.throws(() => validateReport(JSON.parse(JSON.stringify(shadowed)), scope()), /immutable scope|identity/);
  }
});

test('native submission leaves destination bytes unchanged on rejected report envelopes', () => {
  const root = mkdtempSync(join(process.cwd(), '.security-report-envelope-'));
  try {
    const path = join(root, 'report.json');
    for (const current of [scope('fork'), repairScope()]) {
      const initialized = Buffer.from(`${JSON.stringify(createReportTemplate(current), null, 2)}\n`);
      writeFileSync(path, initialized);
      const valid = { ...createReportTemplate(current), summary: 'Reviewed immutable source.' };
      for (const extra of [
        { extra: { nested: { password: 'synthetic_credential_material_123456' } } },
        { harmlessMetadata: true },
        JSON.parse('{"__proto__":{"mode":"guide"}}'),
        { constructor: { prototype: { prNumber: 999 } } },
        { prototype: { headSha: BASE } },
      ]) {
        assert.throws(() => submitSecurityReport(JSON.stringify({ ...valid, ...extra }), current, path), {
          message: 'report envelope contains unsupported fields',
        });
        assert.deepEqual(readFileSync(path), initialized);
      }
      for (const key of ['prNumber', 'baseSha', 'headSha', 'scopeSha256', 'repositoryRelation', 'mode']) {
        const shadowed = { ...valid, [key]: key === 'prNumber' ? 999 : 'shadowed' };
        assert.throws(() => submitSecurityReport(JSON.stringify(shadowed), current, path), /immutable scope|identity/);
        assert.deepEqual(readFileSync(path), initialized);
      }
      assert.equal(submitSecurityReport(JSON.stringify(valid), current, path).accepted, true);
      const saved = JSON.parse(readFileSync(path, 'utf8'));
      assert.deepEqual(saved, validateReport(valid, current));
      assert.deepEqual(Object.keys(saved), Object.keys(createReportTemplate(current)));
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
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
  assert.match(attested.checks.find(check => check.name === 'wta-tests').evidence, /trusted isolated Windows container:/);
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
  test.after(() => rmSync(root, { recursive: true, force: true }));
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
  test.after(() => rmSync(root, { recursive: true, force: true }));
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
  const exoticPath = 'tools/wta/src/a`[click](https:evil.example).rs';
  const exoticScope = buildScope(BASE, HEAD, 17, 'fork', `M\0${exoticPath}\0`);
  const exoticReport = structuredClone(candidate);
  exoticReport.scopeSha256 = exoticScope.scopeSha256;
  exoticReport.findings[0].file = exoticPath;
  const rendered = renderReport(validateReport(exoticReport, exoticScope));
  assert(!rendered.includes('[click](https:evil.example)'));
  assert(rendered.includes('a\\`\\[click\\]\\(https\\:evil\\.example\\)\\.rs'));
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
  for (const summary of [
    'Bearer abcdefghijklmnopqrstuvwxyz123456',
    'session_capability=abcdefghijklmnopqrstuvwxyz123456',
    'mcp-token: abcdefghijklmnopqrstuvwxyz123456',
    '{"session_capability":"abcdefghijklmnopqrstuvwxyz123456"}',
    '{"authorization":"Bearer abcdefghijklmnopqrstuvwxyz123456"}',
  ]) {
    assert.throws(() => validateReport(report({ summary }), scope()), /secret material/);
  }
});

test('native CLI and bounded data-only report submission work end to end', () => {
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
    const readScope = buildScope(base, head, 17, 'fork', 'M\0tools/wta/src/routing.rs\0');
    assert.match(readSecurityDiff(readScope, ['tools/wta/src/routing.rs'], workspace), /changed route for review/);
    assert.match(readSecuritySource(readScope, 'head', 'tools/wta/src/routing.rs', 1, 1, workspace), /^1: fn route/);
    assert.throws(() => readSecuritySource(readScope, 'HEAD; arbitrary-command', 'tools/wta/src/routing.rs', 1, 1, workspace), /immutable base\/head/);
    assert.throws(() => readSecuritySource(readScope, 'head', '../escape', 1, 1, workspace), /normalized/);
    assert.throws(() => readSecuritySource(readScope, 'head', 'tools/wta/src/routing.rs', 1, 801, workspace), /at most 800/);
    assert.equal(readSecurityDiff(readScope, ['--ext-diff'], workspace), '');
    assert.throws(() => inspectSecurityRepair(readScope, workspace), /not available/);
    // Keep report artifacts outside the checkout, as on the hosted runner.
    const artifacts = join(root, 'artifacts');
    mkdirSync(artifacts);
    const writerScope = buildScope(base, head, 17, 'same-repo',
      'M\0tools/wta/src/routing.rs\0', base, 'repair');
    assert.throws(() => writeSecurityRepair(readScope, workspace, 'tools/wta/src/routing.rs', 'x'), /same-repository/);
    assert.throws(() => writeSecurityRepair(writerScope, workspace, '.github/workflows/review.yml', 'x'), /only existing/);
    assert.throws(() => writeSecurityRepair(writerScope, workspace, '../outside', 'x'), /normalized/);
    assert.throws(() => writeSecurityRepair(writerScope, workspace, 'tools/wta/src/routing.rs', '\0'), /without NUL/);
    const candidateText = 'fn route() { /* bounded repair candidate */ }\n';
    const repairResult = writeSecurityRepair(writerScope, workspace, 'tools/wta/src/routing.rs', candidateText);
    assert.match(repairResult.patch, /bounded repair candidate/);
    assert.equal(readFileSync(source, 'utf8'), candidateText);
    assert.throws(() => replaceSecurityRepairText(writerScope, workspace, 'tools/wta/src/routing.rs',
      JSON.stringify([{ oldText: 'not present', newText: 'x' }])), /exactly once/);
    assert.equal(readFileSync(source, 'utf8'), candidateText);
    const edits = [{ oldText: 'bounded repair candidate', newText: 'exact-text repaired candidate' }];
    const edited = replaceSecurityRepairText(writerScope, workspace, 'tools/wta/src/routing.rs', JSON.stringify(edits));
    assert.match(edited.patch, /exact-text repaired candidate/);
    assert.throws(() => replaceSecurityRepairText(writerScope, workspace, 'tools/wta/src/routing.rs',
      JSON.stringify([{ oldText: 'exact-text', newText: 'partial' }, { oldText: 'missing', newText: '' }])), /exactly once/);
    assert(readFileSync(source, 'utf8').includes('exact-text'));
    writeSecurityRepair(writerScope, workspace, 'tools/wta/src/routing.rs',
      readSecuritySource(writerScope, 'head', 'tools/wta/src/routing.rs', 1, 1, workspace).replace(/^1: /, '') + '\n');
    git('replace', head, base);
    for (const [relation, mode] of [['fork', 'guide'], ['same-repo', 'repair']]) {
      const scopePath = join(artifacts, `${mode}-scope.json`);
      const reportPath = join(artifacts, `${mode}-report.json`);
      const validated = join(artifacts, `${mode}-validated.json`);
      const summary = join(artifacts, `${mode}-summary.md`);
      const status = join(artifacts, `${mode}-status.txt`);
      let result = invoke('scope', '--base', base, '--head', head, '--pr', '17',
        '--relation', relation, '--mode', mode, '--output', scopePath);
      assert.equal(result.status, 0, result.stderr);
      assert.equal(JSON.parse(readFileSync(scopePath, 'utf8')).changedFiles.length, 1,
        'native immutable scope must ignore runner-configured replacement refs');
      result = invoke('init-report', '--scope', scopePath, '--output', reportPath);
      assert.equal(result.status, 0, result.stderr);
      const validate = () => invoke('validate', '--scope', scopePath, '--report', reportPath,
        '--validated', validated, '--summary', summary, '--status', status);
      result = validate();
      assert.equal(result.status, 1);
      assert.match(result.stderr, /summary/);
      result = invoke('check-report', '--scope', scopePath, '--report', reportPath);
      assert.equal(result.status, 1);
      assert.match(result.stderr, /summary/);
      const current = JSON.parse(readFileSync(scopePath, 'utf8'));
      const completed = JSON.parse(readFileSync(reportPath, 'utf8'));
      completed.summary = 'Reviewed the immutable patch; no regression found.';
      assert.equal(submitSecurityReport(JSON.stringify(completed), current, reportPath).accepted, true);
      result = validate();
      assert.equal(result.status, 0, result.stderr);
      result = invoke('check-report', '--scope', scopePath, '--report', reportPath);
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

test('bounded report submission validates data before writing and rejects symlink destinations', () => {
  const root = mkdtempSync(join(tmpdir(), 'ghaw-security-report-tool-'));
  const path = join(root, 'report.json');
  const current = scope('fork');
  const valid = createReportTemplate(current);
  valid.summary = 'Reviewed immutable source; no introduced security regression.';
  writeFileSync(path, 'original');
  try {
    assert.throws(() => submitSecurityReport(JSON.stringify({ ...valid, summary: 's'.repeat(801) }), current, path), /at most 800/);
    assert.equal(readFileSync(path, 'utf8'), 'original');
    assert.throws(() => submitSecurityReport('x'.repeat(10 * 1024 + 1), current, path), /10 KiB/);
    const result = submitSecurityReport(JSON.stringify(valid), current, path);
    assert.equal(result.accepted, true);
    assert.equal(JSON.parse(readFileSync(path, 'utf8')).headSha, HEAD);
    const target = join(root, 'outside.json');
    writeFileSync(target, 'unchanged');
    unlinkSync(path);
    symlinkSync(target, path);
    assert.throws(() => submitSecurityReport(JSON.stringify(valid), current, path), /regular file/);
    assert.equal(readFileSync(target, 'utf8'), 'unchanged');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('repair publication requires the exact reviewed head even after a branch rewind', () => {
  const root = mkdtempSync(join(tmpdir(), 'ghaw-security-publication-race-'));
  const checkout = join(root, 'checkout');
  const remote = join(root, 'remote.git');
  mkdirSync(checkout);
  const git = (...args) => execFileSync('git', args, {
    cwd: checkout, encoding: 'utf8', timeout: 30_000,
  }).trim();
  const updateRemote = sha => git('--git-dir', remote, 'update-ref', 'refs/heads/reviewed', sha);
  try {
    git('init', '--quiet');
    git('init', '--quiet', '--bare', remote);
    git('config', 'user.name', 'Local publication fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    git('remote', 'add', 'origin', remote);
    git('commit', '--quiet', '--allow-empty', '-m', 'Base');
    const base = git('rev-parse', 'HEAD');
    git('commit', '--quiet', '--allow-empty', '-m', 'Reviewed head');
    const reviewed = git('rev-parse', 'HEAD');
    git('push', '--quiet', 'origin', 'HEAD:refs/heads/reviewed');
    git('commit', '--quiet', '--allow-empty', '-m', 'Validated repair');
    const repair = git('rev-parse', 'HEAD');
    git('merge-base', '--is-ancestor', reviewed, repair);
    const publish = () => spawnSync('git', [
      'push', '--quiet', `--force-with-lease=refs/heads/reviewed:${reviewed}`,
      'origin', `${repair}:refs/heads/reviewed`,
    ], { cwd: checkout, encoding: 'utf8', timeout: 30_000 });
    let result = publish();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(git('--git-dir', remote, 'rev-parse', 'refs/heads/reviewed'), repair);
    updateRemote(base);
    // A non-force push alone accepts this race and restores removed history.
    git('push', '--quiet', 'origin', `${repair}:refs/heads/reviewed`);
    updateRemote(base);
    result = publish();
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /stale info|rejected/);
    assert.equal(git('--git-dir', remote, 'rev-parse', 'refs/heads/reviewed'), base);
    updateRemote(repair);
    result = publish();
    assert.equal(result.status, 0, result.stderr);
    git('commit', '--quiet', '--allow-empty', '-m', 'Concurrent branch advance');
    const advanced = git('rev-parse', 'HEAD');
    git('push', '--quiet', 'origin', `${advanced}:refs/heads/reviewed`);
    result = publish();
    assert.notEqual(result.status, 0);
    assert.equal(git('--git-dir', remote, 'rev-parse', 'refs/heads/reviewed'), advanced);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
