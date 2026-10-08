import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { installTrustedRestore, restoreTrustedInputs } from './restore-security-review-trusted-inputs.mjs';

const SKILL = '.github/skills/ghaw-pr-security/SKILL.md';
const MESSAGE = /Trusted security input restoration failed closed: unsafe or malformed paths/;
const scratch = resolve('scratch');
mkdirSync(scratch, { recursive: true });

function fixture(t) {
  const root = mkdtempSync(join(scratch, 'trusted-restore-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const workspace = join(root, 'workspace');
  const outside = join(root, 'outside');
  mkdirSync(workspace);
  mkdirSync(outside);
  const git = (...args) => execFileSync('git', ['-c', 'core.fsmonitor=false', '-C', workspace, ...args]);
  const files = { 'AGENTS.md': Buffer.from('root\r\n'), '.agents/policy.md': Buffer.from('policy\n'),
    [SKILL]: execFileSync('git', ['-c', 'core.fsmonitor=false', 'show', `HEAD:${SKILL}`]),
    'tools/wta/src/test.rs': Buffer.from('fn original() {}\r\n') };
  for (const [path, bytes] of Object.entries(files)) {
    mkdirSync(join(workspace, path, '..'), { recursive: true });
    writeFileSync(join(workspace, path), bytes);
  }
  git('init', '--quiet');
  git('add', '.');
  git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'fixture');
  const trustedSha = git('rev-parse', 'HEAD').toString().trim();
  // Git's Windows checkout normalization is not authority: compare against blobs.
  for (const path of Object.keys(files)) files[path] = git('show', `${trustedSha}:${path}`);
  const options = { workspace, trustedSha };
  writeFileSync(join(outside, 'sentinel'), 'OUTSIDE UNCHANGED');
  return { root, workspace, outside, git, files, options };
}

function snapshot(f) {
  return { index: readFileSync(join(f.workspace, '.git', 'index')),
    source: readFileSync(join(f.workspace, 'tools/wta/src/test.rs')),
    agents: readFileSync(join(f.workspace, 'AGENTS.md')),
    sentinel: readFileSync(join(f.outside, 'sentinel')) };
}

function unchanged(f, before) {
  assert.deepEqual(snapshot(f), before);
}

test('restores trusted Git bytes, removes generated stamp, and leaves WTA and index untouched', t => {
  const f = fixture(t);
  const index = readFileSync(join(f.workspace, '.git', 'index'));
  const source = readFileSync(join(f.workspace, 'tools/wta/src/test.rs'));
  writeFileSync(join(f.workspace, SKILL), 'Generated stamp\nPR bytes\n');
  writeFileSync(join(f.workspace, 'AGENTS.md'), 'changed intentional guidance');
  assert.equal(restoreTrustedInputs(f.options), 3);
  for (const [path, bytes] of Object.entries(f.files).filter(([path]) => !path.startsWith('tools/'))) {
    assert.deepEqual(readFileSync(join(f.workspace, path)), bytes);
  }
  assert.deepEqual(readFileSync(join(f.workspace, '.git', 'index')), index);
  assert.deepEqual(readFileSync(join(f.workspace, 'tools/wta/src/test.rs')), source);
  writeFileSync(join(f.workspace, SKILL), 'stamp');
  restoreTrustedInputs({ ...f.options, paths: [SKILL] });
  assert.deepEqual(readFileSync(join(f.workspace, SKILL)), f.files[SKILL]);
});

for (const replaced of ['commit', 'blob']) {
  test(`trusted restoration ignores ambient ${replaced} replacement refs`, t => {
    const f = fixture(t);
    const original = replaced === 'commit' ? f.options.trustedSha : f.git('rev-parse', `${f.options.trustedSha}:AGENTS.md`).toString().trim();
    writeFileSync(join(f.workspace, 'AGENTS.md'), 'ATTACKER REPLACEMENT');
    f.git('add', 'AGENTS.md');
    f.git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'replacement');
    const substitute = f.git('rev-parse', replaced === 'commit' ? 'HEAD' : 'HEAD:AGENTS.md').toString().trim();
    const oldBase = process.env.GIT_REPLACE_REF_BASE;
    process.env.GIT_REPLACE_REF_BASE = 'refs/security-restore-replacements/';
    try {
      const ambient = { ...process.env };
      delete ambient.GIT_NO_REPLACE_OBJECTS;
      const unprotectedGit = (...args) => execFileSync('git', ['-C', f.workspace, ...args], { env: ambient });
      unprotectedGit('replace', original, substitute);
      assert.equal(unprotectedGit('show', `${f.options.trustedSha}:AGENTS.md`).toString(), 'ATTACKER REPLACEMENT');
      const index = readFileSync(join(f.workspace, '.git', 'index'));
      restoreTrustedInputs(f.options);
      assert.deepEqual(readFileSync(join(f.workspace, 'AGENTS.md')), f.files['AGENTS.md']);
      assert.deepEqual(readFileSync(join(f.workspace, '.git', 'index')), index);
      assert.equal(readFileSync(join(f.outside, 'sentinel'), 'utf8'), 'OUTSIDE UNCHANGED');
    } finally {
      if (oldBase === undefined) delete process.env.GIT_REPLACE_REF_BASE;
      else process.env.GIT_REPLACE_REF_BASE = oldBase;
    }
  });
}

for (const path of ['AGENTS.md', '.agents/policy.md', SKILL]) {
  test(`rejects real leaf symlink at ${path} without changing any target or index`, t => {
    const f = fixture(t);
    const target = join(f.workspace, path);
    rmSync(target);
    symlinkSync(join(f.outside, 'sentinel'), target, 'file');
    const before = snapshot(f);
    assert.throws(() => restoreTrustedInputs(f.options), MESSAGE);
    unchanged(f, before);
    assert.ok(lstatSync(target).isSymbolicLink());
  });
}

for (const path of ['.github', '.github/skills', '.github/skills/ghaw-pr-security', '.agents']) {
  test(`rejects directory ancestor ${path} before writing earlier safe files`, t => {
    const f = fixture(t);
    writeFileSync(join(f.workspace, 'AGENTS.md'), 'keep intentional modified bytes on rejection');
    const target = join(f.workspace, path);
    rmSync(target, { recursive: true });
    symlinkSync(f.outside, target, process.platform === 'win32' ? 'junction' : 'dir');
    const before = snapshot(f);
    assert.throws(() => restoreTrustedInputs(f.options), MESSAGE);
    unchanged(f, before);
    assert.ok(lstatSync(target).isSymbolicLink());
    if (path.startsWith('.github')) {
      assert.throws(() => restoreTrustedInputs({ ...f.options, paths: [SKILL] }), MESSAGE);
      unchanged(f, before);
    }
  });
}

test('malformed identity, target selection and non-root workspace fail before writes', t => {
  const f = fixture(t);
  const before = snapshot(f);
  for (const options of [
    { ...f.options, trustedSha: 'HEAD' }, { ...f.options, trustedSha: '0'.repeat(40) },
    { ...f.options, workspace: '.' }, { ...f.options, workspace: join(f.workspace, '.github') },
    { ...f.options, paths: [] }, { ...f.options, paths: ['../outside/sentinel'] },
    { ...f.options, paths: ['AGENTS.md', SKILL] }, { ...f.options, paths: [join(f.outside, 'sentinel')] },
  ]) {
    assert.throws(() => restoreTrustedInputs(options));
    unchanged(f, before);
  }
});

test('workspace symlink and directory occupying trusted file fail closed', t => {
  const f = fixture(t);
  const alias = join(f.root, 'alias');
  symlinkSync(f.workspace, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const before = snapshot(f);
  assert.throws(() => restoreTrustedInputs({ ...f.options, workspace: alias }), MESSAGE);
  unchanged(f, before);
  rmSync(join(f.workspace, SKILL));
  mkdirSync(join(f.workspace, SKILL));
  assert.throws(() => restoreTrustedInputs(f.options), MESSAGE);
  unchanged(f, before);
});

test('all native restore surfaces bind to protected helper and immutable workflow revision', t => {
  const f = fixture(t);
  const actionsDir = join(f.root, 'runtime-actions');
  mkdirSync(actionsDir);
  const scripts = ['restore_base_github_folders.sh', 'restore_inline_sub_agents.sh', 'restore_inline_skills.sh'];
  for (const name of scripts) writeFileSync(join(actionsDir, name), 'original native cp');
  installTrustedRestore({ ...f.options, actionsDir });
  const contents = scripts.map(name => readFileSync(join(actionsDir, name), 'utf8'));
  assert.equal(new Set(contents).size, 1);
  assert.ok(contents[0].includes(f.options.trustedSha));
  assert.ok(contents[0].includes('restore-security-review-trusted-inputs.mjs'));
  assert.ok(!contents[0].includes('cp '));
  assert.throws(() => installTrustedRestore({ ...f.options, actionsDir: f.workspace }), MESSAGE);
  // Rejected destination preflight must not even rewrite the protected wrappers.
  rmSync(join(f.workspace, SKILL));
  symlinkSync(join(f.outside, 'sentinel'), join(f.workspace, SKILL), 'file');
  assert.throws(() => installTrustedRestore({ ...f.options, actionsDir }), MESSAGE);
  assert.deepEqual(scripts.map(name => readFileSync(join(actionsDir, name), 'utf8')), contents);
});

test('CLI rejects symlink destinations with one static explicit message and exit 1', t => {
  const f = fixture(t);
  rmSync(join(f.workspace, SKILL));
  symlinkSync(join(f.outside, 'sentinel'), join(f.workspace, SKILL), 'file');
  const before = snapshot(f);
  let error;
  try {
    execFileSync(process.execPath, [fileURLToPath(new URL('./restore-security-review-trusted-inputs.mjs', import.meta.url)),
      'restore', '--workspace', f.workspace, '--trusted-sha', f.options.trustedSha, '--only', 'skill'],
    { stdio: ['ignore', 'pipe', 'pipe'] });
  } catch (failure) {
    error = failure;
  }
  assert.equal(error?.status, 1);
  assert.equal(error.stderr.toString().trim(),
    '::error::Trusted security input restoration failed closed: unsafe or malformed paths are not permitted.');
  unchanged(f, before);
});

test('preserves native removal of PR-added guidance and repository MCP configuration', t => {
  const f = fixture(t);
  const extras = ['.github/agents/injected.agent.md', '.agents/injected.md', '.mcp.json'];
  for (const path of extras) {
    mkdirSync(join(f.workspace, path, '..'), { recursive: true });
    writeFileSync(join(f.workspace, path), 'PR-controlled extra');
  }
  const before = snapshot(f);
  restoreTrustedInputs(f.options);
  for (const path of extras) assert.throws(() => lstatSync(join(f.workspace, path)), { code: 'ENOENT' });
  assert.deepEqual(readFileSync(join(f.workspace, '.git', 'index')), before.index);
  assert.deepEqual(readFileSync(join(f.workspace, 'tools/wta/src/test.rs')), before.source);
  assert.deepEqual(readFileSync(join(f.outside, 'sentinel')), before.sentinel);
});

test('an injected extra symlink rejects the entire batch before writes or removals', t => {
  const f = fixture(t);
  writeFileSync(join(f.workspace, 'AGENTS.md'), 'preserve modified guidance');
  writeFileSync(join(f.workspace, '.mcp.json'), 'must not remove on rejection');
  symlinkSync(join(f.outside, 'sentinel'), join(f.workspace, '.github/injected.md'), 'file');
  const before = snapshot(f);
  assert.throws(() => restoreTrustedInputs(f.options), MESSAGE);
  unchanged(f, before);
  assert.equal(readFileSync(join(f.workspace, '.mcp.json'), 'utf8'), 'must not remove on rejection');
});
