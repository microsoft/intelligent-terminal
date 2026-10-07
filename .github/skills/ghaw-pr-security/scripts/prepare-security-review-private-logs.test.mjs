import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import test from 'node:test';
import { PRIVATE_LOG_ASSETS, prepareSecurityReviewPrivateLogs } from './prepare-security-review-private-logs.mjs';

function fixture(run) {
  mkdirSync(resolve('scratch'), { recursive: true });
  const root = mkdtempSync(resolve('scratch', 'security-private-logs-'));
  try {
    const options = { actionsDir: join(root, 'actions'), privateRoot: join(root, 'private'),
      collectedRoot: join(root, 'collected'), workspace: join(root, 'candidate') };
    for (const name of ['actionsDir', 'collectedRoot', 'workspace']) mkdirSync(options[name]);
    options.assets = PRIVATE_LOG_ASSETS.map(asset => {
      const source = `#!/usr/bin/env bash\n${Array(asset.count).fill(asset.needle).join('\n')}\n`;
      writeFileSync(join(options.actionsDir, asset.name), source);
      return { ...asset, sha256: createHash('sha256').update(source).digest('hex') };
    });
    return run(options);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test('all hashes and counts reject before any asset, log, or private-directory modification', () => {
  for (const mismatch of ['first-hash', 'second-hash', 'first-count', 'second-count']) {
    fixture(options => {
      const index = mismatch.startsWith('first') ? 0 : 1;
      if (mismatch.endsWith('hash')) options.assets[index].sha256 = '0'.repeat(64);
      else options.assets[index].count++;
      const before = options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8'));
      assert.throws(() => prepareSecurityReviewPrivateLogs(options));
      assert(!existsSync(options.privateRoot));
      assert.deepEqual(options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8')), before);
    });
  }
});

test('native source-bearing sink roots are routed privately and existing evidence is preserved', () => {
  fixture(options => {
    const sentinel = 'DUMMY_NATIVE_SOURCE_SECRET_SENTINEL';
    const logs = join(options.collectedRoot, 'mcp-scripts', 'logs');
    mkdirSync(logs, { recursive: true });
    writeFileSync(join(logs, 'server.log'), sentinel);
    const originalStore = join(options.collectedRoot, 'original-session-store');
    writeFileSync(originalStore, sentinel);
    const result = prepareSecurityReviewPrivateLogs(options);
    assert.deepEqual(result, { assets: 2, retainedSinks: 1 });
    assert(!existsSync(logs));
    assert.equal(readFileSync(join(options.privateRoot, 'existing-evidence', 'sink-0', 'server.log'), 'utf8'), sentinel);
    assert.equal(readFileSync(originalStore, 'utf8'), sentinel);
    const copy = readFileSync(join(options.actionsDir, 'copy_copilot_session_state.sh'), 'utf8');
    assert(!copy.includes('cp -rv'));
    assert(copy.includes('original evidence retained'));
    const server = readFileSync(join(options.actionsDir, 'start_mcp_scripts_server.sh'), 'utf8');
    assert(!server.includes('/tmp/gh-aw/mcp-scripts/logs'));
    assert.equal(server.split(join(options.privateRoot, 'mcp-scripts', 'logs').replaceAll('\\', '/')).length - 1, 6);
    assert(existsSync(join(options.privateRoot, 'mcp-gateway', 'privacy-preparation.json')));
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

test('a replay refuses already-modified assets without destroying retained evidence', () => {
  fixture(options => {
    prepareSecurityReviewPrivateLogs(options);
    const before = readFileSync(join(options.actionsDir, 'copy_copilot_session_state.sh'), 'utf8');
    assert.throws(() => prepareSecurityReviewPrivateLogs(options));
    assert.equal(readFileSync(join(options.actionsDir, 'copy_copilot_session_state.sh'), 'utf8'), before);
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
        const before = options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8'));
        assert.throws(() => prepareSecurityReviewPrivateLogs(options));
        assert.deepEqual(options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8')), before);
        assert(!existsSync(join(target, 'logs', 'privacy-preparation.json')));
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
          assert(lock.includes('GH_AW_MCP_GATEWAY_CUSTOM_ENV_NAMES: "[\\"MCP_GATEWAY_LOG_DIR\\"]"'));
          assert(lock.includes('GH_AW_MCP_GATEWAY_ENV_0: "/tmp/gh-aw-security-private/mcp-gateway"'));
          const upload = lock.slice(lock.indexOf('- name: Upload agent artifacts'), lock.indexOf('\n  conclusion:'));
          assert(!upload.includes('/tmp/gh-aw-security-private'));
          assert(upload.includes('/tmp/gh-aw/agent/'));
          assert(upload.includes('/tmp/gh-aw/agent_output.json'));
        }
      });
    }
  });
});
