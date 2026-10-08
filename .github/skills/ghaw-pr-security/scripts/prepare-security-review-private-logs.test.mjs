import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
import { runInNewContext } from 'node:vm';
import test from 'node:test';
import { PRIVATE_LOG_ASSETS, prepareSecurityReviewPrivateLogs } from './prepare-security-review-private-logs.mjs';
import { buildScope, createReportTemplate, SECURITY_NOOP_MESSAGE, validateCandidate, validateQueuedOutput, validateReport } from './security-review.mjs';
import { projectSecurityReviewOutput } from './security-review-driver.mjs';

const PUBLIC_DIAGNOSTIC_READERS = {
  'mcp-logs/': 'empty-native-sinks',
  'mcp-scripts/logs/': 'empty-native-sinks',
  'sandbox/agent/logs/': 'trusted-two-event-projection',
  'agent-stdio.log': 'trusted-projection-and-framework-diagnostics',
};
const PUBLIC_COLLECTOR_INVENTORY = [
  'aw-prompts/prompt.txt', 'sandbox/agent/logs/', 'redacted-urls.log', 'mcp-logs/', 'mcp-scripts/logs/',
  'agent_usage.json', 'agent-stdio.log', 'pre-agent-audit.txt', 'agent/', 'github_rate_limits.jsonl',
  'otel.jsonl', 'otlp-export-errors.jsonl', 'safeoutputs.jsonl', 'agent_output.json', 'aw-*.patch',
  'aw-*.bundle', 'awf-config.json', 'sandbox/firewall/logs/', 'sandbox/firewall/audit/', 'sandbox/firewall/awf-reflect.json',
];
const TRUSTED_FRAMEWORK_DIAGNOSTICS = [
  '[security-review-driver] starting primary',
  '[security-review-driver] starting fixed independent reviewer',
  'security driver: failed; inspect metadata diagnostics and report status',
  '[copilot-harness] attempt 1: process exit event exitCode=0',
  '[copilot-harness] done: exitCode=0 totalDuration=1m 23s',
  '[INFO] Executing agent command...',
  '[SUCCESS] Command completed successfully',
  'Process exiting with code: 0',
];

function trustedProjection({ report, scope, result }) {
  if (scope.mode === 'repair') validateCandidate(report, scope);
  else validateReport(report, scope);
  const usage = {};
  for (const key of ['inputTokens', 'outputTokens', 'totalTokens', 'input_tokens', 'output_tokens',
    'total_tokens', 'premiumRequests', 'totalApiDurationMs']) {
    const value = result.usage?.[key];
    if (typeof value === 'number' && Number.isFinite(value) && value >= 0) usage[key] = value;
  }
  return [
    { type: 'assistant.message', data: { phase: 'final_answer', toolRequests: [], content: JSON.stringify({
      summary: report.summary, headSha: report.headSha, reviewStatus: report.review.status,
      findings: report.findings.map(({ rule, severity, confidence, file, startLine, endLine }) =>
        ({ rule, severity, confidence, file, startLine, endLine })),
      checks: report.checks.map(({ name, status }) => ({ name, status })),
    }) } },
    { type: 'result', exitCode: 0, sessionId: createHash('sha256').update(result.sessionId).digest('hex'), usage },
  ].map(event => JSON.stringify(event));
}

export function assertNoPublicRawLogPayloads(collectedRoot, trusted) {
  const reject = () => { throw new Error('Public raw log payload or irregular node rejected'); };
  if (resolve(collectedRoot) !== realpathSync(collectedRoot) || !lstatSync(collectedRoot).isDirectory()) reject();
  let directories = 0;
  const inspect = path => {
    const stat = lstatSync(path);
    if (stat.isSymbolicLink() || !stat.isDirectory() || realpathSync(path) !== path) reject();
    directories++;
    for (const name of readdirSync(path)) inspect(join(path, name));
  };
  for (const parts of [['mcp-logs'], ['mcp-scripts', 'logs']]) {
    let path = resolve(collectedRoot);
    let absent = false;
    for (const part of parts) {
      path = join(path, part);
      let stat;
      try { stat = lstatSync(path); }
      catch (error) { if (error.code === 'ENOENT') { absent = true; break; } throw error; }
      if (stat.isSymbolicLink() || !stat.isDirectory() || realpathSync(path) !== path) reject();
    }
    if (!absent) inspect(path);
  }
  const expected = trusted ? trustedProjection(trusted) : null;
  const regular = path => {
    const stat = lstatSync(path);
    if (stat.isSymbolicLink() || realpathSync(path) !== path) reject();
    return stat;
  };
  let agentLogs = resolve(collectedRoot);
  let absent = false;
  for (const part of ['sandbox', 'agent', 'logs']) {
    agentLogs = join(agentLogs, part);
    try { if (!regular(agentLogs).isDirectory()) reject(); }
    catch (error) { if (error.code === 'ENOENT') { absent = true; break; } throw error; }
  }
  if (!absent) {
    const names = readdirSync(agentLogs);
    if (names.length) {
      if (!expected || names.length !== 1 || names[0] !== 'events.jsonl') reject();
      const events = join(agentLogs, names[0]);
      const stat = regular(events);
      if (!stat.isFile() || stat.size > 1024 * 1024 || readFileSync(events, 'utf8') !== `${expected.join('\n')}\n`) reject();
    }
  }
  const stdio = join(resolve(collectedRoot), 'agent-stdio.log');
  try {
    const stat = regular(stdio);
    if (!stat.isFile() || stat.size > 1024 * 1024) reject();
    const lines = readFileSync(stdio, 'utf8').split(/\r?\n/).filter(Boolean);
    const projected = [];
    for (const line of lines) {
      if (TRUSTED_FRAMEWORK_DIAGNOSTICS.includes(line)) continue;
      if (!expected || !expected.includes(line)) reject();
      projected.push(line);
    }
    if (projected.length && JSON.stringify(projected) !== JSON.stringify(expected)) reject();
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  return { publicRawLogFiles: 0, regularEmptyDirectories: directories };
}

test('privacy test adapter permits only regular empty directory trees, never any file or symlink', () => {
  fixture(options => {
    assert.deepEqual(assertNoPublicRawLogPayloads(options.collectedRoot), { publicRawLogFiles: 0, regularEmptyDirectories: 0 });
    const sink = join(options.collectedRoot, 'mcp-logs', 'safeoutputs');
    mkdirSync(sink, { recursive: true });
    assert.deepEqual(assertNoPublicRawLogPayloads(options.collectedRoot), { publicRawLogFiles: 0, regularEmptyDirectories: 2 });
    for (const bytes of ['', 'DUMMY_RAW_SOURCE_SENTINEL']) {
      const file = join(sink, 'server.log');
      writeFileSync(file, bytes);
      assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
      rmSync(file);
    }
    const nested = join(sink, 'nested');
    mkdirSync(nested);
    writeFileSync(join(nested, 'source.log'), '');
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
    rmSync(nested, { recursive: true });
    symlinkSync(options.workspace, nested, process.platform === 'win32' ? 'junction' : 'dir');
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
  });
  fixture(options => {
    symlinkSync(options.workspace, join(options.collectedRoot, 'mcp-scripts'), process.platform === 'win32' ? 'junction' : 'dir');
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
  });
});

function projectionFixture(mode = 'repair') {
  const base = '1'.repeat(40);
  const head = '2'.repeat(40);
  const path = 'tools/wta/src/master/mod.rs';
  const scope = buildScope(base, head, 17, mode === 'repair' ? 'same-repo' : 'fork',
    `M\0${path}\0`, base, mode, [{ path, headLineCount: 2,
      hunks: [{ baseStart: 1, baseCount: 1, headStart: 1, headCount: 1 }] }]);
  const report = { ...createReportTemplate(scope), summary: 'Changed source was reviewed; no automatic repair was attempted.' };
  const result = { type: 'result', exitCode: 0, sessionId: 'DUMMY_PRIVATE_SESSION_IDENTIFIER',
    usage: { premiumRequests: 0, totalApiDurationMs: 57430, inputTokens: 12, outputTokens: 3,
      ignored: 'DUMMY_RAW_SOURCE_SENTINEL', totalTokens: -1, input_tokens: '12', output_tokens: Infinity } };
  return { scope, report, result };
}

function writePublicProjection(options, trusted) {
  const projection = projectSecurityReviewOutput({ events: [
    { type: 'tool.execution_start', data: { arguments: 'DUMMY_RAW_SOURCE_SENTINEL' } },
    { type: 'tool.execution_complete', data: { result: 'DUMMY_RAW_SOURCE_SENTINEL' } },
    { type: 'assistant.message', data: { content: 'DUMMY_RAW_SOURCE_SENTINEL' } },
    trusted.result,
  ] }, trusted.report);
  const directory = join(options.collectedRoot, 'sandbox', 'agent', 'logs');
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, 'events.jsonl'), projection);
  writeFileSync(join(options.collectedRoot, 'agent-stdio.log'),
    `${TRUSTED_FRAMEWORK_DIAGNOSTICS[0]}\n${projection}${TRUSTED_FRAMEWORK_DIAGNOSTICS[3]}\n`);
  return projection;
}

for (const mode of ['repair', 'guide']) {
  test(`${mode}: collector accepts report-derived public metadata, hashed session and numeric usage, not CLI source`, () => {
    fixture(options => {
      const trusted = projectionFixture(mode);
      const projection = writePublicProjection(options, trusted);
      const rows = projection.trim().split('\n').map(JSON.parse);
      assert.equal(rows.length, 2);
      assert.match(rows[1].sessionId, /^[a-f0-9]{64}$/);
      assert.deepEqual(rows[1].usage, { inputTokens: 12, outputTokens: 3, premiumRequests: 0, totalApiDurationMs: 57430 });
      assert(!projection.includes('DUMMY_RAW_SOURCE_SENTINEL'));
      assert(!projection.includes(trusted.result.sessionId));
      assert.deepEqual(assertNoPublicRawLogPayloads(options.collectedRoot, trusted),
        { publicRawLogFiles: 0, regularEmptyDirectories: 0 });
      assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
    });
  });
}

for (const [namespace, reader] of Object.entries(PUBLIC_DIAGNOSTIC_READERS)) {
  for (const marker of ['DUMMY_RAW_SOURCE_SENTINEL fn route_secret() {}',
    'github_pat_DUMMY_CREDENTIAL_SENTINEL_12345678901234567890']) {
    test(`${namespace}: ${reader} rejects injected ${marker.startsWith('github') ? 'credential' : 'source'} bytes`, () => {
      fixture(options => {
        const trusted = projectionFixture();
        writePublicProjection(options, trusted);
        assert.doesNotThrow(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
        const path = namespace.endsWith('/')
          ? join(options.collectedRoot, ...namespace.split('/').filter(Boolean), 'injected.log')
          : join(options.collectedRoot, namespace);
        mkdirSync(resolve(path, '..'), { recursive: true });
        writeFileSync(path, marker, { flag: 'a' });
        assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
      });
    });
  }
}

test('each raw MCP sink rejects even a zero-byte native payload', () => {
  for (const namespace of ['mcp-logs', join('mcp-scripts', 'logs')]) fixture(options => {
    const root = join(options.collectedRoot, namespace);
    mkdirSync(root, { recursive: true });
    assert.doesNotThrow(() => assertNoPublicRawLogPayloads(options.collectedRoot));
    writeFileSync(join(root, 'server.log'), '');
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
  });
});

test('approved report source descriptions remain public, while credential-like report text invalidates the authority', () => {
  fixture(options => {
    const trusted = projectionFixture();
    trusted.report.summary = 'Reviewed fn resolve() and session_to_helper lookup; no regression found.';
    const bytes = writePublicProjection(options, trusted);
    assert(bytes.includes('fn resolve()'));
    assert.doesNotThrow(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
    trusted.report.summary = 'github_pat_DUMMY_CREDENTIAL_SENTINEL_12345678901234567890';
    writePublicProjection(options, trusted);
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
  });
});

test('regular empty agent log trees and fixed failure diagnostics are not mistaken for raw CLI evidence', () => {
  fixture(options => {
    mkdirSync(join(options.collectedRoot, 'sandbox', 'agent', 'logs'), { recursive: true });
    writeFileSync(join(options.collectedRoot, 'agent-stdio.log'), `${TRUSTED_FRAMEWORK_DIAGNOSTICS[2]}\n`);
    assert.doesNotThrow(() => assertNoPublicRawLogPayloads(options.collectedRoot));
    writeFileSync(join(options.collectedRoot, 'agent-stdio.log'),
      'fatal: DUMMY_RAW_SOURCE_SENTINEL\n', { flag: 'a' });
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot));
  });
});

for (const [name, change] of [
  ['tool request', rows => { rows[0].data.toolRequests.push({ name: 'read', arguments: 'DUMMY_RAW_SOURCE_SENTINEL' }); }],
  ['call result', rows => { rows.splice(1, 0, { type: 'tool.execution_complete', data: { result: 'DUMMY_RAW_SOURCE_SENTINEL' } }); }],
  ['unknown field', rows => { rows[1].source = 'DUMMY_RAW_SOURCE_SENTINEL'; }],
  ['reserved prototype field', rows => { rows[1].usage = JSON.parse('{"__proto__":{"source":"DUMMY_RAW_SOURCE_SENTINEL"}}'); }],
  ['constructor field', rows => { rows[1].constructor = 'DUMMY_RAW_SOURCE_SENTINEL'; }],
  ['raw session identifier', rows => { rows[1].sessionId = 'DUMMY_PRIVATE_SESSION_IDENTIFIER'; }],
  ['negative usage', rows => { rows[1].usage.inputTokens = -1; }],
  ['string usage', rows => { rows[1].usage.inputTokens = '12'; }],
  ['unknown usage', rows => { rows[1].usage.Core_User = 'DUMMY_RAW_SOURCE_SENTINEL'; }],
  ['non-finite usage', rows => { rows[1].usage.inputTokens = Infinity; }],
  ['duplicated result', rows => { rows.push(rows[1]); }],
  ['out-of-order result', rows => { rows.reverse(); }],
  ['summary not derived from report', rows => { rows[0].data.content = '{"summary":"DUMMY_RAW_SOURCE_SENTINEL"}'; }],
]) {
  for (const target of ['events', 'stdio']) test(`${target} rejects ${name} rather than treating it as findings`, () => {
    fixture(options => {
      const trusted = projectionFixture();
      const rows = writePublicProjection(options, trusted).trim().split('\n').map(JSON.parse);
      change(rows);
      const path = target === 'events' ? join(options.collectedRoot, 'sandbox', 'agent', 'logs', 'events.jsonl')
        : join(options.collectedRoot, 'agent-stdio.log');
      writeFileSync(path, `${rows.map(row => JSON.stringify(row)).join('\n')}\n`);
      assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
    });
  });
}

test('framework prefixes are not an authorization to upload arbitrary diagnostics or source', () => {
  for (const prefix of ['[INFO]', '[copilot-harness]', '[security-review-driver]', '[entrypoint]', 'Error:']) {
    fixture(options => {
      const trusted = projectionFixture();
      writePublicProjection(options, trusted);
      writeFileSync(join(options.collectedRoot, 'agent-stdio.log'), `${prefix} DUMMY_RAW_SOURCE_SENTINEL\n`, { flag: 'a' });
      assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
    });
  }
});

test('public agent diagnostic directories and stdio reject irregular ancestors and extra files', () => {
  for (const namespace of ['sandbox', 'agent-stdio.log']) fixture(options => {
    symlinkSync(options.workspace, join(options.collectedRoot, namespace), process.platform === 'win32' ? 'junction' : 'dir');
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot, projectionFixture()));
  });
  fixture(options => {
    const trusted = projectionFixture();
    writePublicProjection(options, trusted);
    const log = join(options.collectedRoot, 'sandbox', 'agent', 'logs', 'events.jsonl');
    rmSync(log);
    symlinkSync(join(options.actionsDir, options.assets[0].name), log, 'file');
    assert.throws(() => assertNoPublicRawLogPayloads(options.collectedRoot, trusted));
  });
});

test('compiled collector inventory pins every uploaded namespace and all four diagnostic readers in both lanes', () => {
  for (const name of ['ghaw-pr-security', 'ghaw-pr-security-guide-fork']) {
    const lock = readFileSync(new URL(`../../../workflows/${name}.lock.yml`, import.meta.url), 'utf8');
    const upload = lock.slice(lock.indexOf('- name: Upload agent artifacts'), lock.indexOf('\n  conclusion:'));
    const paths = [...upload.matchAll(/^\s{12}\/tmp\/gh-aw\/([^\r\n]+)$/gm)].map(match => match[1]);
    assert.deepEqual(paths, PUBLIC_COLLECTOR_INVENTORY, `${name}: new collector paths need an explicit reader classification`);
    assert.deepEqual(paths.filter(path => Object.hasOwn(PUBLIC_DIAGNOSTIC_READERS, path)),
      ['sandbox/agent/logs/', 'mcp-logs/', 'mcp-scripts/logs/', 'agent-stdio.log']);
    assert(upload.includes('actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a'));
    assert(lock.includes('GH_AW_AWF_LOG_FILE=/tmp/gh-aw/agent-stdio.log'));
    assert(lock.includes('--log-dir /tmp/gh-aw/sandbox/agent/logs/'));
  }
});

function fixture(run) {
  mkdirSync(resolve('scratch'), { recursive: true });
  const root = mkdtempSync(resolve('scratch', 'security-private-logs-'));
  try {
    const options = { actionsDir: join(root, 'actions'), privateRoot: join(root, 'private'),
      collectedRoot: join(root, 'collected'), workspace: join(root, 'candidate'), safeOutputsRoot: join(root, 'safeoutputs') };
    for (const name of ['actionsDir', 'collectedRoot', 'workspace', 'safeOutputsRoot']) mkdirSync(options[name]);
    options.assets = PRIVATE_LOG_ASSETS.map(asset => {
      const source = `#!/usr/bin/env bash\n${Array(asset.count).fill(asset.needle).join('\n')}\n`;
      writeFileSync(join(asset.fixedNoop ? options.safeOutputsRoot : options.actionsDir, asset.name), source);
      return { ...asset, sha256: createHash('sha256').update(source).digest('hex') };
    });
    return run(options);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test('all hashes and counts reject before any asset, log, or private-directory modification', () => {
  for (const mismatch of ['first-hash', 'second-hash', 'third-hash', 'first-count', 'second-count', 'third-count']) {
    fixture(options => {
      const index = mismatch.startsWith('first') ? 0 : mismatch.startsWith('second') ? 1 : 2;
      if (mismatch.endsWith('hash')) options.assets[index].sha256 = '0'.repeat(64);
      else options.assets[index].count++;
      const oldLog = join(options.collectedRoot, 'agent-stdio.log');
      writeFileSync(oldLog, 'DUMMY_RETAINED_PRIVATE_EVIDENCE');
      const before = options.assets.map(asset => readFileSync(join(asset.fixedNoop ? options.safeOutputsRoot : options.actionsDir, asset.name), 'utf8'));
      assert.throws(() => prepareSecurityReviewPrivateLogs(options));
      assert(!existsSync(options.privateRoot));
      assert(!existsSync(join(options.safeOutputsRoot, 'private-security-logs')));
      assert.equal(readFileSync(oldLog, 'utf8'), 'DUMMY_RETAINED_PRIVATE_EVIDENCE');
      assert.deepEqual(options.assets.map(asset => readFileSync(join(asset.fixedNoop ? options.safeOutputsRoot : options.actionsDir, asset.name), 'utf8')), before);
    });
  }
});

test('canonical noop construction drops all model fields before persistence', () => {
  const asset = PRIVATE_LOG_ASSETS.find(item => item.fixedNoop);
  fixture(options => {
    prepareSecurityReviewPrivateLogs(options);
    const normalized = readFileSync(join(options.safeOutputsRoot, asset.name), 'utf8');
    const construction = normalized.slice(normalized.indexOf('    const entry = ')).trim();
    const inputs = [
      { message: 'DUMMY_DIFF_CODE_SENTINEL', reason: 'DUMMY_REASON' },
      JSON.parse('{"message":"github_pat_DUMMY_SENTINEL_12345678901234567890","type":"create_issue","__proto__":{"secret":"DUMMY"},"constructor":"DUMMY","id":"DUMMY"}'),
      Object.create({ message: 'DUMMY_INHERITED', secret: 'DUMMY' }),
    ];
    for (const args of inputs) {
      const entry = runInNewContext(`${construction}\nentry`, { type: 'noop', args });
      assert.deepEqual(JSON.parse(JSON.stringify(entry)), { type: 'noop', message: SECURITY_NOOP_MESSAGE });
    }
  });
});

test('native source-bearing sink roots are routed privately and existing evidence is preserved', () => {
  fixture(options => {
    const sentinel = 'DUMMY_NATIVE_SOURCE_SECRET_SENTINEL';
    const logs = join(options.collectedRoot, 'mcp-scripts', 'logs');
    mkdirSync(logs, { recursive: true });
    writeFileSync(join(logs, 'server.log'), sentinel);
    const originalStore = join(options.collectedRoot, 'original-session-store');
    writeFileSync(originalStore, sentinel);
    const canonical = ['agent_output.json', 'safeoutputs.jsonl', 'security-findings.json', 'security-repair.patch'];
    for (const name of canonical) writeFileSync(join(options.collectedRoot, name), `unchanged ${name}`);
    const result = prepareSecurityReviewPrivateLogs(options);
    assert.deepEqual(result, { assets: 3, retainedSinks: 1 });
    assert(!existsSync(logs));
    assert.equal(readFileSync(join(options.privateRoot, 'existing-evidence', 'sink-0', 'server.log'), 'utf8'), sentinel);
    assert.equal(readFileSync(originalStore, 'utf8'), sentinel);
    for (const name of canonical) assert.equal(readFileSync(join(options.collectedRoot, name), 'utf8'), `unchanged ${name}`);
    const copy = readFileSync(join(options.actionsDir, 'copy_copilot_session_state.sh'), 'utf8');
    assert(!copy.includes('cp -rv'));
    assert(copy.includes('original evidence retained'));
    const server = readFileSync(join(options.actionsDir, 'start_mcp_scripts_server.sh'), 'utf8');
    assert(!server.includes('/tmp/gh-aw/mcp-scripts/logs'));
    assert.equal(server.split(join(options.privateRoot, 'mcp-scripts', 'logs').replaceAll('\\', '/')).length - 1, 6);
    assert(existsSync(join(options.privateRoot, 'mcp-gateway', 'privacy-preparation.json')));
    assert(existsSync(join(options.safeOutputsRoot, 'private-security-logs')));
  });
});

test('uploaded, candidate, and symlinked private roots cannot receive native raw logs', () => {
  for (const root of ['collectedRoot', 'workspace']) {
    fixture(options => {
      options.privateRoot = join(options[root], 'private');
      assert.throws(() => prepareSecurityReviewPrivateLogs(options));
      assert(!existsSync(options.privateRoot));
    });
  }
  fixture(options => {
    const target = join(options.actionsDir, 'target');
    mkdirSync(target);
    symlinkSync(target, options.privateRoot, process.platform === 'win32' ? 'junction' : 'dir');
    assert.throws(() => prepareSecurityReviewPrivateLogs(options));
    assert(!existsSync(join(target, 'mcp-gateway')));
  });
});
    test('safeoutputs logs reject collected, candidate, or symlinked mounts before any writes', () => {
      for (const root of ['collectedRoot', 'workspace', 'actionsDir', 'privateRoot']) {
        fixture(options => {
          const paths = options.assets.map(asset => join(asset.fixedNoop ? options.safeOutputsRoot : options.actionsDir, asset.name));
          options.safeOutputsRoot = join(options[root], 'safeoutputs');
          mkdirSync(options.safeOutputsRoot, { recursive: true });
          const before = paths.map(path => readFileSync(path, 'utf8'));
          assert.throws(() => prepareSecurityReviewPrivateLogs(options));
          assert.deepEqual(paths.map(path => readFileSync(path, 'utf8')), before);
          assert(!existsSync(join(options.safeOutputsRoot, 'private-security-logs')));
        });
      }
      fixture(options => {
        symlinkSync(options.workspace, join(options.safeOutputsRoot, 'private-security-logs'),
          process.platform === 'win32' ? 'junction' : 'dir');
        assert.throws(() => prepareSecurityReviewPrivateLogs(options));
        assert(!existsSync(options.privateRoot));
      });
    });

    test('pinned generated stdio service privately logs source arguments/metadata and retains one canonical noop',
      { skip: !process.env.SECURITY_PINNED_GHAW_RUNTIME, timeout: 30000 }, async () => {
        mkdirSync(resolve('scratch'), { recursive: true });
        const root = mkdtempSync(resolve('scratch', 'security-safeoutputs-runtime-'));
        let child;
        let lines;
        try {
          const pinned = resolve(process.env.SECURITY_PINNED_GHAW_RUNTIME);
          const options = { actionsDir: join(root, 'actions'), privateRoot: join(root, 'private'),
            collectedRoot: join(root, 'collected'), workspace: join(root, 'candidate'), safeOutputsRoot: join(root, 'safeoutputs') };
          for (const name of ['actionsDir', 'collectedRoot', 'workspace', 'safeOutputsRoot']) mkdirSync(options[name]);
          for (const asset of PRIVATE_LOG_ASSETS.filter(asset => !asset.fixedNoop)) cpSync(join(pinned, 'setup', 'sh', asset.name), join(options.actionsDir, asset.name));
          cpSync(join(pinned, 'setup', 'js'), options.safeOutputsRoot, { recursive: true });
          const oldSink = join(options.collectedRoot, 'mcp-logs', 'safeoutputs');
          mkdirSync(oldSink, { recursive: true });
          writeFileSync(join(oldSink, 'server.log'), 'DUMMY_PREEXISTING_SOURCE_SENTINEL');
          const result = prepareSecurityReviewPrivateLogs(options);
          assert.equal(result.assets, 3);
          assert.equal(result.retainedSinks, 1);
          assert.equal(readFileSync(join(options.privateRoot, 'existing-evidence', 'sink-0', 'safeoutputs', 'server.log'), 'utf8'),
            'DUMMY_PREEXISTING_SOURCE_SENTINEL');
          const lock = readFileSync(new URL('../../../workflows/ghaw-pr-security.lock.yml', import.meta.url), 'utf8');
          assert(lock.indexOf('name: Prepare Safe Outputs Directories') > lock.indexOf('name: Prepare private security review log sinks'));
          assert(lock.includes('mkdir -p /tmp/gh-aw/mcp-logs/safeoutputs'));
          const bash = process.platform === 'win32' ? 'C:\\Program Files\\Git\\bin\\bash.exe' : 'bash';
          const mkdir = spawnSync(bash, ['-c', 'mkdir -p "$PUBLIC_ROOT/mcp-logs/safeoutputs"'], {
            env: { ...process.env, PUBLIC_ROOT: options.collectedRoot }, encoding: 'utf8',
          });
          assert.equal(mkdir.status, 0);
          const gatewaySource = readFileSync(join(options.safeOutputsRoot, 'start_mcp_gateway.cjs'), 'utf8');
          const startupMkdir = 'fs.mkdirSync("/tmp/gh-aw/mcp-logs", { recursive: true });';
          assert.equal(gatewaySource.split(startupMkdir).length - 1, 1);
          runInNewContext(startupMkdir.replace('"/tmp/gh-aw/mcp-logs"', JSON.stringify(join(options.collectedRoot, 'mcp-logs'))),
            { fs: { mkdirSync } });
          assert(existsSync(join(options.collectedRoot, 'mcp-logs')));
          assert.deepEqual(assertNoPublicRawLogPayloads(options.collectedRoot), { publicRawLogFiles: 0, regularEmptyDirectories: 2 });
          const require = createRequire(import.meta.url);
          const { injectCustomGatewayEnvArgs } = require(join(options.safeOutputsRoot, 'start_mcp_gateway.cjs'));
          const { createHandlers } = require(join(options.safeOutputsRoot, 'safe_outputs_handlers.cjs'));
          for (const args of [
            { message: 'DUMMY_SOURCE', reason: 'DUMMY_REASON', id: 'DUMMY_MODEL_ID' },
            JSON.parse('{"message":"DUMMY","type":"create_issue","data":{"secret":"DUMMY"},"__proto__":{"secret":"DUMMY"},"constructor":"DUMMY"}'),
            Object.create({ message: 'DUMMY_INHERITED', secret: 'DUMMY' }),
          ]) {
            const rows = [];
            const handler = createHandlers({ debug() {} }, row => rows.push(row), { noop: { max: 1 } }).defaultHandler('noop');
            const response = handler(args);
            assert(!response.isError);
            assert.deepEqual(rows, [{ type: 'noop', message: SECURITY_NOOP_MESSAGE }]);
            assert.throws(() => handler(args), error => error.code === -32602 &&
              error.data?.constraint === 'max' && error.data?.type === 'noop' && error.data?.limit === 1);
          }
          const serviceLogDir = join(options.safeOutputsRoot, 'private-security-logs');
          const dockerArgs = injectCustomGatewayEnvArgs(['-e', 'GH_AW_MCP_LOG_DIR', '__GH_AW_MCP_GATEWAY_CUSTOM_ENV__'], {
            GH_AW_MCP_GATEWAY_CUSTOM_ENV_NAMES: '["GH_AW_MCP_LOG_DIR","MCP_GATEWAY_LOG_DIR"]',
            GH_AW_MCP_GATEWAY_ENV_0: serviceLogDir,
            GH_AW_MCP_GATEWAY_ENV_1: join(options.privateRoot, 'mcp-gateway'),
          });
          assert.equal(dockerArgs[3], `GH_AW_MCP_LOG_DIR=${serviceLogDir}`);
          const config = join(options.safeOutputsRoot, 'config.json');
          const output = join(options.collectedRoot, 'safeoutputs.jsonl');
          writeFileSync(config, JSON.stringify({ noop: { max: 1, 'report-as-issue': false }, staged: true }));
          const stderr = join(serviceLogDir, 'stdio.log');
          child = spawn(process.execPath, [join(options.safeOutputsRoot, 'safe_outputs_mcp_server.cjs')], {
            cwd: options.workspace,
            env: { SystemRoot: process.env.SystemRoot, GH_AW_MCP_LOG_DIR: dockerArgs[3].slice('GH_AW_MCP_LOG_DIR='.length),
              GH_AW_SAFE_OUTPUTS_CONFIG_PATH: config, GH_AW_SAFE_OUTPUTS: output,
              GH_AW_SAFE_OUTPUTS_TOOLS_PATH: join(options.safeOutputsRoot, 'safe_outputs_tools.json') },
            stdio: ['pipe', 'pipe', 'pipe'],
          });
          child.stderr.on('data', data => writeFileSync(stderr, data, { flag: 'a' }));
          lines = createInterface({ input: child.stdout });
          const replies = new Map();
          lines.on('line', line => {
            const reply = JSON.parse(line);
            replies.get(reply.id)?.(reply);
          });
          let id = 0;
          const request = (method, params) => new Promise((resolveReply, reject) => {
            const next = ++id;
            const timeout = setTimeout(() => reject(new Error('pinned service response timeout')), 5000);
            replies.set(next, reply => { clearTimeout(timeout); replies.delete(next); resolveReply(reply); });
            child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: next, method, params })}\n`);
          });
          const metadata = 'DUMMY_SOURCE_METADATA_SENTINEL';
          const argument = 'DUMMY_SOURCE_ARGUMENT_SENTINEL';
          assert((await request('initialize', { protocolVersion: '2024-11-05', capabilities: {},
            clientInfo: { name: metadata, version: '1' } })).result);
          assert((await request('tools/list', {})).result.tools.some(tool => tool.name === 'noop'));
          const invalid = await request('tools/call', { name: 'noop', arguments: { message: 'Review complete.', sourceFixture: argument } });
          assert(invalid.error || invalid.result?.isError);
          const secret = 'github_pat_DUMMY_CREDENTIAL_SENTINEL_12345678901234567890';
          const source = 'DUMMY_DIFF_CODE_SENTINEL fn route_secret() {}';
          const valid = await request('tools/call', { name: 'noop', arguments: { message: `${secret}\n${source}` } });
          assert(valid.result && !valid.result.isError);
          const noop = readFileSync(output, 'utf8').trim().split('\n').map(JSON.parse);
          assert.equal(noop.length, 1);
          assert.equal(noop[0].type, 'noop');
          assert.deepEqual(noop[0], { type: 'noop', message: SECURITY_NOOP_MESSAGE });
          validateQueuedOutput({ mode: 'repair' }, { items: noop, errors: [] });
          const duplicate = await request('tools/call', { name: 'noop', arguments: { message: source } });
          assert(duplicate.error || duplicate.result?.isError);
          assert.equal(readFileSync(output, 'utf8').trim().split('\n').length, 1);
          assert(!readFileSync(output, 'utf8').includes(secret));
          assert(!readFileSync(output, 'utf8').includes(source));
          const summaryPath = join(options.collectedRoot, 'staged-noop-summary.md');
          const nativeNoop = readFileSync(join(pinned, 'setup', 'js', 'noop.cjs'), 'utf8');
          const nativeModule = { exports: {} };
          runInNewContext(nativeNoop, {
            module: nativeModule,
            require: name => {
              if (name === './load_agent_output.cjs') return { loadAgentOutput: () => ({ success: true, items: noop }) };
              if (name === './safe_output_helpers.cjs') return { isStagedMode: () => true };
              throw new Error('unexpected native noop dependency');
            },
            core: { info() {}, summary: { addRaw(text) {
              return { async write() { writeFileSync(summaryPath, text); } };
            } } },
          });
          await nativeModule.exports.main();
          const summary = readFileSync(summaryPath, 'utf8');
          assert(summary.includes(SECURITY_NOOP_MESSAGE));
          assert(!summary.includes(secret));
          assert(!summary.includes(source));
          const inspectPublic = directory => {
            for (const name of readdirSync(directory)) {
              const path = join(directory, name);
              const stat = lstatSync(path);
              assert(!stat.isSymbolicLink());
              if (stat.isDirectory()) inspectPublic(path);
              else {
                assert(stat.isFile());
                const bytes = readFileSync(path, 'utf8');
                for (const sentinel of [secret, source, argument, metadata]) assert(!bytes.includes(sentinel));
              }
            }
          };
          inspectPublic(options.collectedRoot);
          assert(!readFileSync(output, 'utf8').includes(argument));
          const privateLog = readFileSync(join(serviceLogDir, 'server.log'), 'utf8');
          assert(privateLog.includes(metadata));
          assert(privateLog.includes(argument));
          assert(privateLog.includes(secret));
          assert(privateLog.includes(source));
          assert(readFileSync(stderr, 'utf8').includes(argument));
          assert.deepEqual(assertNoPublicRawLogPayloads(options.collectedRoot), { publicRawLogFiles: 0, regularEmptyDirectories: 2 });
          const service = 'safe_outputs_mcp_server.cjs';
          assert(readFileSync(join(options.safeOutputsRoot, service)).equals(readFileSync(join(pinned, 'setup', 'js', service))));
          if (process.env.SECURITY_PRIVATE_LOG_PROOF_DIR) {
            const proof = resolve(process.env.SECURITY_PRIVATE_LOG_PROOF_DIR);
            assert(proof.startsWith(`${resolve('scratch')}\\`) || proof.startsWith(`${resolve('scratch')}/`));
            mkdirSync(proof, { recursive: true });
            cpSync(output, join(proof, 'canonical-noop.jsonl'));
            cpSync(summaryPath, join(proof, 'native-staged-noop-summary.md'));
            cpSync(join(serviceLogDir, 'server.log'), join(proof, 'private-safeoutputs-server.log'));
            cpSync(stderr, join(proof, 'private-safeoutputs-stdio.log'));
            writeFileSync(join(proof, 'runtime-proof.json'), JSON.stringify({
              version: 1, pin: 'bc8c008a419c5b7a29df6f5641edd35fd1c6ea85',
              unchangedServiceSha256: createHash('sha256').update(readFileSync(join(options.safeOutputsRoot, service))).digest('hex'),
              pinnedOriginalHandlerSha256: PRIVATE_LOG_ASSETS.find(asset => asset.fixedNoop).sha256,
              normalizedHandlerSha256: createHash('sha256').update(readFileSync(join(options.safeOutputsRoot, 'safe_outputs_handlers.cjs'))).digest('hex'),
              verifiedAssets: result.assets, noopCount: noop.length, publicMcpRootAbsent: false,
              publicRawLogFiles: 0, regularEmptyDirectories: 2,
              bootstrapDirectoryRecreationReproduced: true,
              privateMetadataSentinel: true, privateArgumentSentinel: true, privateStderrSentinel: true,
              fixedNoopMessage: SECURITY_NOOP_MESSAGE, credentialAndDiffAbsentFromPublicArtifacts: true,
              nativeMaxOneEnforced: true, unchangedNativeStagedNoopSummary: true,
              gatewayEnvInjection: true, credentials: 'none', models: 'none',
              gap: 'Local Windows stdio execution; Docker/AWF host-mount persistence requires hosted validation.',
            }, null, 2));
          }
        } finally {
          lines?.close();
          if (child && child.exitCode === null) {
            child.kill();
            await new Promise(resolveExit => child.once('close', resolveExit));
          }
          rmSync(root, { recursive: true, force: true });
        }
      });
test('a replay refuses already-modified assets without destroying retained evidence', () => {
  fixture(options => {
    prepareSecurityReviewPrivateLogs(options);
    const before = readFileSync(join(options.actionsDir, 'copy_copilot_session_state.sh'), 'utf8');
    assert.throws(() => prepareSecurityReviewPrivateLogs(options));
    assert.equal(readFileSync(join(options.actionsDir, 'copy_copilot_session_state.sh'), 'utf8'), before);
  });
});

test('nested private or collected symlink sinks reject before either runtime asset changes', () => {
    for (const location of ['private', 'collected']) {
      fixture(options => {
        const target = join(options.actionsDir, 'target');
        mkdirSync(target);
        const parent = location === 'private' ? options.privateRoot : options.collectedRoot;
        mkdirSync(parent, { recursive: true });
        symlinkSync(target, join(parent, 'mcp-scripts'), process.platform === 'win32' ? 'junction' : 'dir');
        if (location === 'collected') mkdirSync(join(target, 'logs'));
        const before = options.assets.map(asset => readFileSync(join(asset.fixedNoop ? options.safeOutputsRoot : options.actionsDir, asset.name), 'utf8'));
        assert.throws(() => prepareSecurityReviewPrivateLogs(options));
        assert.deepEqual(options.assets.map(asset => readFileSync(join(asset.fixedNoop ? options.safeOutputsRoot : options.actionsDir, asset.name), 'utf8')), before);
        assert(!existsSync(join(target, 'logs', 'privacy-preparation.json')));
      });
  }
});

test('compiled workers prepare private sinks before native startup and use literal gateway transport', () => {
        for (const name of ['ghaw-pr-security', 'ghaw-pr-security-guide-fork']) {
          const lock = readFileSync(new URL(`../../../workflows/${name}.lock.yml`, import.meta.url), 'utf8');
          const prepare = lock.indexOf('name: Prepare private security review log sinks before native services');
          assert(prepare > 0);
          assert(prepare < lock.indexOf('name: Write MCP Scripts Config'));
          assert(prepare < lock.indexOf('name: Start MCP Scripts HTTP Server'));
          assert(prepare < lock.indexOf('name: Start MCP Gateway'));
          assert(prepare < lock.indexOf('name: Execute GitHub Copilot CLI'));
          assert(lock.includes('GH_AW_MCP_GATEWAY_CUSTOM_ENV_NAMES: "[\\"GH_AW_MCP_LOG_DIR\\",\\"MCP_GATEWAY_LOG_DIR\\"]"'));
          assert(lock.includes('GH_AW_MCP_GATEWAY_ENV_0: "${{ runner.temp }}/gh-aw/safeoutputs/private-security-logs"'));
          assert(lock.includes('GH_AW_MCP_GATEWAY_ENV_1: "/tmp/gh-aw-security-private/mcp-gateway"'));
          const upload = lock.slice(lock.indexOf('- name: Upload agent artifacts'), lock.indexOf('\n  conclusion:'));
          assert(!upload.includes('/tmp/gh-aw-security-private'));
          assert(!upload.includes('private-security-logs'));
          assert(lock.includes('"GH_AW_MCP_LOG_DIR": "\\${GH_AW_MCP_LOG_DIR}"'));
          assert(lock.includes('${RUNNER_TEMP}/gh-aw/safeoutputs:${RUNNER_TEMP}/gh-aw/safeoutputs:rw'));
          assert(upload.includes('/tmp/gh-aw/agent/'));
          assert(upload.includes('/tmp/gh-aw/agent_output.json'));
        }
});
