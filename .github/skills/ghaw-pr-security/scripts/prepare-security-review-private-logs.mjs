#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { closeSync, constants, fstatSync, ftruncateSync, lstatSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { SECURITY_NOOP_MESSAGE } from './security-review.mjs';

export const PRIVATE_LOG_ASSETS = [
  { name: 'copy_copilot_session_state.sh', sha256: '38c7945c942309546f4c1c26403d14839dc841042f7f8792099caeaa9903810a',
    needle: 'cp -rv "$SESSION_STATE_DIR"/. "$LOGS_DIR/" 2>/dev/null || true', count: 1, sessionCopy: true },
  { name: 'start_mcp_scripts_server.sh', sha256: '6cbbcc463da20cfcbeb85b9a31b67b53adb7002976f73d4febdab24e458b53a3',
    needle: '/tmp/gh-aw/mcp-scripts/logs', count: 6 },
  { name: 'safe_outputs_handlers.cjs', sha256: '9b59060b83de9f1032f8a47fd028d63b619dbb31ec265a014f8ace08facc0f1c',
    needle: '  const defaultHandler = type => args => {\n    const entry = { ...(args || {}), type };', count: 1, fixedNoop: true },
];

function fail() {
  throw new Error('security private logs: trusted preparation rejected');
}

function within(path, root) {
  return path === root || path.startsWith(`${root}${sep}`);
}

function regular(path, directory = false) {
  if (!isAbsolute(path) || realpathSync(path) !== resolve(path)) fail();
  const stat = lstatSync(path);
  if (stat.isSymbolicLink() || (directory ? !stat.isDirectory() : !stat.isFile())) fail();
  return stat;
}

export function prepareSecurityReviewPrivateLogs({
  actionsDir, privateRoot, collectedRoot, workspace, safeOutputsRoot, assets = PRIVATE_LOG_ASSETS,
}) {
  for (const path of [actionsDir, privateRoot, collectedRoot, workspace, safeOutputsRoot]) if (!isAbsolute(path ?? '')) fail();
  const actions = resolve(actionsDir);
  const privateDirectory = resolve(privateRoot);
  const collected = resolve(collectedRoot);
  const candidate = resolve(workspace);
  const safeOutputs = resolve(safeOutputsRoot);
  const serviceLogs = resolve(safeOutputs, 'private-security-logs');
  regular(actions, true);
  regular(collected, true);
  regular(candidate, true);
  regular(safeOutputs, true);
  for (const root of [collected, candidate, actions, privateDirectory]) {
    if (within(safeOutputs, root) || within(root, safeOutputs)) fail();
  }
  try {
    regular(serviceLogs, true);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  regular(dirname(privateDirectory), true);
  if (within(privateDirectory, collected) || within(collected, privateDirectory) ||
      within(privateDirectory, candidate) || within(privateDirectory, actions) ||
      within(actions, candidate)) fail();
  try {
    regular(privateDirectory, true);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  for (const parts of [['mcp-scripts'], ['mcp-scripts', 'logs'], ['mcp-gateway'], ['existing-evidence']]) {
    try {
      regular(resolve(privateDirectory, ...parts), true);
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }
  const prepared = assets.map(asset => {
    if (basename(asset.name) !== asset.name) fail();
    const path = resolve(asset.fixedNoop ? safeOutputs : actions, asset.name);
    const stat = regular(path);
    if (stat.size > 1024 * 1024) fail();
    const bytes = readFileSync(path);
    if (createHash('sha256').update(bytes).digest('hex') !== asset.sha256) fail();
    const source = bytes.toString('utf8');
    if (source.split(asset.needle).length - 1 !== asset.count) fail();
    const replacement = asset.fixedNoop
      ? source.replace(asset.needle, `  const defaultHandler = type => args => {\n    const entry = type === "noop" ? { type, message: ${JSON.stringify(SECURITY_NOOP_MESSAGE)} } : { ...(args || {}), type };`)
      : asset.sessionCopy
      ? '#!/usr/bin/env bash\nset -euo pipefail\nprintf "%s\\n" "Security review: raw CLI session collection disabled; original evidence retained."\n'
      : source.replaceAll(asset.needle, resolve(privateDirectory, 'mcp-scripts', 'logs').replaceAll('\\', '/'));
    return { path, stat, bytes, replacement };
  });
  const oldLogs = ['sandbox/agent/logs', 'mcp-logs', 'mcp-scripts/logs', 'agent-stdio.log']
    .map(path => resolve(collected, ...path.split('/'))).filter(path => {
      try {
        const stat = lstatSync(path);
        regular(path, stat.isDirectory());
        return true;
      } catch (error) {
        if (error.code === 'ENOENT') return false;
        throw error;
      }
    });
  // All asset hashes, replacement counts, and existing sink paths are checked before any writes.
  mkdirSync(privateDirectory, { recursive: true, mode: 0o700 });
  regular(privateDirectory, true);
  if (process.platform !== 'win32' && (lstatSync(privateDirectory).mode & 0o077)) fail();
  // This directory is already mounted into the generated safeoutputs container.
  mkdirSync(serviceLogs, { recursive: true, mode: 0o700 });
  regular(serviceLogs, true);
  if (process.platform !== 'win32' && (lstatSync(serviceLogs).mode & 0o077)) fail();
  for (const parts of [['mcp-scripts', 'logs'], ['mcp-gateway']]) {
    const logs = resolve(privateDirectory, ...parts);
    mkdirSync(logs, { recursive: true, mode: 0o700 });
    regular(logs, true);
    if (process.platform !== 'win32' && (lstatSync(logs).mode & 0o077)) fail();
    const probe = resolve(logs, 'privacy-preparation.json');
    writeFileSync(probe, '{"version":1,"rawLogsCollected":false}\n', { flag: 'wx', mode: 0o600 });
  }
  const evidence = resolve(privateDirectory, 'existing-evidence');
  if (oldLogs.length) {
    mkdirSync(evidence, { mode: 0o700 });
    oldLogs.forEach((path, index) => renameSync(path, resolve(evidence, `sink-${index}`)));
  }
  for (const item of prepared) {
    const fd = openSync(item.path, constants.O_WRONLY | (process.platform === 'win32' ? 0 : constants.O_NOFOLLOW));
    try {
      const current = fstatSync(fd);
      if (current.dev !== item.stat.dev || current.ino !== item.stat.ino ||
          !readFileSync(item.path).equals(item.bytes)) fail();
      // Preserve installed executable modes; only the verified shell contents change.
      writeFileSync(fd, item.replacement);
      ftruncateSync(fd, Buffer.byteLength(item.replacement));
    } finally {
      closeSync(fd);
    }
  }
  return { assets: prepared.length, retainedSinks: oldLogs.length };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const options = {};
    const names = { '--actions-dir': 'actionsDir', '--private-root': 'privateRoot',
      '--collected-root': 'collectedRoot', '--workspace': 'workspace', '--safe-outputs-root': 'safeOutputsRoot' };
    for (let index = 2; index < process.argv.length; index += 2) {
      if (!names[process.argv[index]] || !process.argv[index + 1] || options[names[process.argv[index]]]) fail();
      options[names[process.argv[index]]] = process.argv[index + 1];
    }
    const result = prepareSecurityReviewPrivateLogs(options);
    process.stdout.write(`security private logs: prepared assets=${result.assets} retained-sinks=${result.retainedSinks}\n`);
  } catch {
    process.stderr.write('security private logs: preparation failed before inference\n');
    process.exitCode = 1;
  }
}
