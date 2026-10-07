import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { PRIVATE_LOG_ASSETS, prepareSecurityReviewPrivateLogs } from './prepare-security-review-private-logs.mjs';

function fixture(run) {
  mkdirSync(resolve('scratch'), { recursive: true });
  const root = mkdtempSync(resolve('scratch', 'security-private-logs-'));
  try {
    const options = { actionsDir: join(root, 'actions'), privateRoot: join(root, 'private'),
      collectedRoot: join(root, 'collected'), workspace: join(root, 'candidate'), safeOutputsRoot: join(root, 'safeoutputs') };
    for (const name of ['actionsDir', 'collectedRoot', 'workspace', 'safeOutputsRoot']) mkdirSync(options[name]);
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
      assert(!existsSync(join(options.safeOutputsRoot, 'private-security-logs')));
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
    const canonical = ['agent_output.json', 'safeoutputs.jsonl', 'security-findings.json', 'security-repair.patch'];
    for (const name of canonical) writeFileSync(join(options.collectedRoot, name), `unchanged ${name}`);
    const result = prepareSecurityReviewPrivateLogs(options);
    assert.deepEqual(result, { assets: 2, retainedSinks: 1 });
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
          options.safeOutputsRoot = join(options[root], 'safeoutputs');
          mkdirSync(options.safeOutputsRoot, { recursive: true });
          const before = options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8'));
          assert.throws(() => prepareSecurityReviewPrivateLogs(options));
          assert.deepEqual(options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8')), before);
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
          for (const asset of PRIVATE_LOG_ASSETS) cpSync(join(pinned, 'setup', 'sh', asset.name), join(options.actionsDir, asset.name));
          cpSync(join(pinned, 'setup', 'js'), options.safeOutputsRoot, { recursive: true });
          const oldSink = join(options.collectedRoot, 'mcp-logs', 'safeoutputs');
          mkdirSync(oldSink, { recursive: true });
          writeFileSync(join(oldSink, 'server.log'), 'DUMMY_PREEXISTING_SOURCE_SENTINEL');
          const result = prepareSecurityReviewPrivateLogs(options);
          assert.equal(result.assets, 2);
          assert.equal(result.retainedSinks, 1);
          assert.equal(readFileSync(join(options.privateRoot, 'existing-evidence', 'sink-0', 'safeoutputs', 'server.log'), 'utf8'),
            'DUMMY_PREEXISTING_SOURCE_SENTINEL');
          const require = createRequire(import.meta.url);
          const { injectCustomGatewayEnvArgs } = require(join(options.safeOutputsRoot, 'start_mcp_gateway.cjs'));
          const serviceLogDir = join(options.safeOutputsRoot, 'private-security-logs');
          const dockerArgs = injectCustomGatewayEnvArgs(['-e', 'GH_AW_MCP_LOG_DIR', '__GH_AW_MCP_GATEWAY_CUSTOM_ENV__'], {
            GH_AW_MCP_GATEWAY_CUSTOM_ENV_NAMES: '["GH_AW_MCP_LOG_DIR","MCP_GATEWAY_LOG_DIR"]',
            GH_AW_MCP_GATEWAY_ENV_0: serviceLogDir,
            GH_AW_MCP_GATEWAY_ENV_1: join(options.privateRoot, 'mcp-gateway'),
          });
          assert.equal(dockerArgs[3], `GH_AW_MCP_LOG_DIR=${serviceLogDir}`);
          const config = join(options.safeOutputsRoot, 'config.json');
          const output = join(options.collectedRoot, 'safeoutputs.jsonl');
          writeFileSync(config, JSON.stringify({ noop: { 'report-as-issue': false }, staged: true }));
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
          const valid = await request('tools/call', { name: 'noop', arguments: { message: 'Review complete.' } });
          assert(valid.result && !valid.result.isError);
          const noop = readFileSync(output, 'utf8').trim().split('\n').map(JSON.parse);
          assert.equal(noop.length, 1);
          assert.equal(noop[0].type, 'noop');
          assert.equal(noop[0].message, 'Review complete.');
          assert(!readFileSync(output, 'utf8').includes(argument));
          const privateLog = readFileSync(join(serviceLogDir, 'server.log'), 'utf8');
          assert(privateLog.includes(metadata));
          assert(privateLog.includes(argument));
          assert(readFileSync(stderr, 'utf8').includes(argument));
          assert(!existsSync(join(options.collectedRoot, 'mcp-logs')));
          const service = 'safe_outputs_mcp_server.cjs';
          assert(readFileSync(join(options.safeOutputsRoot, service)).equals(readFileSync(join(pinned, 'setup', 'js', service))));
          if (process.env.SECURITY_PRIVATE_LOG_PROOF_DIR) {
            const proof = resolve(process.env.SECURITY_PRIVATE_LOG_PROOF_DIR);
            assert(proof.startsWith(`${resolve('scratch')}\\`) || proof.startsWith(`${resolve('scratch')}/`));
            mkdirSync(proof, { recursive: true });
            cpSync(output, join(proof, 'canonical-noop.jsonl'));
            cpSync(join(serviceLogDir, 'server.log'), join(proof, 'private-safeoutputs-server.log'));
            cpSync(stderr, join(proof, 'private-safeoutputs-stdio.log'));
            writeFileSync(join(proof, 'runtime-proof.json'), JSON.stringify({
              version: 1, pin: 'bc8c008a419c5b7a29df6f5641edd35fd1c6ea85',
              unchangedServiceSha256: createHash('sha256').update(readFileSync(join(options.safeOutputsRoot, service))).digest('hex'),
              verifiedAssets: result.assets, noopCount: noop.length, publicMcpRootAbsent: true,
              privateMetadataSentinel: true, privateArgumentSentinel: true, privateStderrSentinel: true,
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
        const before = options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8'));
        assert.throws(() => prepareSecurityReviewPrivateLogs(options));
        assert.deepEqual(options.assets.map(asset => readFileSync(join(options.actionsDir, asset.name), 'utf8')), before);
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
