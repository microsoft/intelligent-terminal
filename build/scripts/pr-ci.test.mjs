import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const workflow = readFileSync(new URL('../../.github/workflows/pr-ci.yml', import.meta.url), 'utf8');
const script = workflow.split('          script: |')[1].split('\n  cpp:')[0];
const execute = new (Object.getPrototypeOf(async function () {}).constructor)('github', 'context', 'core', script);

async function select(paths, options = {}) {
  const files = paths.map(path => typeof path === 'string' ? { filename: path, status: 'modified' } : path);
  const pull = { number: 17, head: { sha: 'a'.repeat(40) }, base: { sha: 'b'.repeat(40) },
    changed_files: options.changedFiles ?? files.length };
  const outputs = {}, warnings = [];
  let summary = '';
  await execute({
    rest: { pulls: { get: async () => ({ data: options.stale ? { ...pull, head: { sha: 'c'.repeat(40) } } : pull }),
      listFiles: 'files' } },
    paginate: async () => { if (options.apiError) throw Error('API unavailable'); return files; },
  }, {
    repo: { owner: 'owner', repo: 'repo' },
    eventName: options.eventName ?? 'pull_request', payload: { pull_request: pull },
  }, {
    setOutput: (key, value) => { outputs[key] = value; },
    warning: message => warnings.push(message),
    summary: { addRaw: value => { summary += value; return { write: async () => {} }; } },
  });
  return { ...outputs, warnings, summary };
}

test('XAML and native source/header/resource changes select only C++', async () => {
  for (const path of ['src/cascadia/TerminalSettingsEditor/Profiles.xaml', 'src/host/screenInfo.cpp',
    'src/host/screenInfo.hpp', 'src/cascadia/TerminalApp/TerminalPage.idl']) {
    const result = await select([path]);
    assert.equal(result.cpp, 'true', path);
    assert.equal(result.rust, 'false', path);
  }
});

test('Rust sources and embedded locale/prompt/hook inputs select only Rust', async () => {
  for (const path of ['tools/wta/src/main.rs', 'installer/bootstrap/src/main.rs',
    'tools/wta/prompts/auto-fix.md',
    'tools/wta/wt-agent-hooks/opencode/plugin.json']) {
    const result = await select([path]);
    assert.equal(result.cpp, 'false', path);
    assert.equal(result.rust, 'true', path);
  }
});

test('plain documentation skips both builds; mixed changes accumulate required languages', async () => {
  const docs = await select(['doc/guide.md', 'README.md', 'tools/wta/README.md', 'installer/bootstrap/README.md']);
  assert.equal(docs.cpp, 'false');
  assert.equal(docs.rust, 'false');
  assert.match(docs.summary, /not required/);
  const mixed = await select(['doc/guide.md', 'src/host/example.cpp', 'tools/wta/src/main.rs']);
  assert.equal(mixed.cpp, 'true');
  assert.equal(mixed.rust, 'true');
});

test('Assets folders are excluded without maintaining an image-extension list', async () => {
  for (const path of ['src/tools/integrity/packageuwp/assets/Logo.scale-100.png',
    'src/cascadia/TerminalApp/Assets/Icon.svg', 'src/cascadia/TerminalApp/Assets/resources.json']) {
    const result = await select([path]);
    assert.equal(result.cpp, 'false');
    assert.equal(result.rust, 'false');
  }
});

test('localization-only changes skip builds without a locale allowlist; source changes still build', async () => {
  for (const locale of ['en-US', 'zh-CN', 'new-locale']) {
    const native = await select([`src/cascadia/TerminalSettingsEditor/Resources/${locale}/Resources.resw`]);
    assert.equal(native.cpp, 'false');
    assert.equal(native.rust, 'false');
    const rust = await select([`tools/wta/locales/${locale}.yml`]);
    assert.equal(rust.cpp, 'false');
    assert.equal(rust.rust, 'false');
  }
  const mixed = await select(['tools/wta/locales/en-US.yml', 'tools/wta/src/main.rs']);
  assert.equal(mixed.cpp, 'false');
  assert.equal(mixed.rust, 'true');
  const removed = await select([{ filename: 'tools/wta/locales/fr-FR.yml', status: 'removed' }]);
  assert.equal(removed.cpp, 'false');
  assert.equal(removed.rust, 'false');
});

test('shared build inputs, unknown paths, deletions and renames retain both jobs', async () => {
  for (const path of ['tools/wta/Cargo.toml', 'tools/wta/Cargo.lock', 'tools/wta/build.rs',
    'src/inc/telemetry/ProjectTelemetry.h', 'src/tools/wtcli/wtcli_functions.h',
    'src/cascadia/inc/AcpModelUtils.h', 'build/scripts/Run-Tests.ps1', '.github/workflows/pr-ci.yml',
    '.github/skills/example/SKILL.md', 'new-area/file.xyz',
    { filename: 'tools/wta/src/old.rs', status: 'removed' },
    { filename: 'doc/guide.md', previous_filename: 'tools/wta/src/main.rs', status: 'renamed' }]) {
    const result = await select([path]);
    assert.equal(result.cpp, 'true');
    assert.equal(result.rust, 'true');
  }
});

test('push/manual and incomplete file coverage run both; failed or stale selection never succeeds', async () => {
  for (const options of [{ eventName: 'push' }, { eventName: 'workflow_dispatch' }, { changedFiles: 3001 }]) {
    const result = await select(['doc/guide.md'], options);
    assert.equal(result.cpp, 'true');
    assert.equal(result.rust, 'true');
    if (options.changedFiles) assert.equal(result.warnings.length, 1);
  }
  const empty = await select([]);
  assert.equal(empty.cpp, 'true');
  assert.equal(empty.rust, 'true');
  await assert.rejects(select(['README.md'], { apiError: true }), /API unavailable/);
  await assert.rejects(select(['README.md'], { stale: true }), /revision changed/);
});
