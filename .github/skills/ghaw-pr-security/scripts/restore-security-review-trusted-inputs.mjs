#!/usr/bin/env node

import { execFileSync } from 'node:child_process';
import { chmodSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, parse, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const SOURCE = fileURLToPath(import.meta.url);
const SKILL = '.github/skills/ghaw-pr-security/SKILL.md';
const ERROR = 'Trusted security input restoration failed closed: unsafe or malformed paths are not permitted.';

function reject() {
  throw new Error(ERROR);
}

function checkedPath(path, directory = false) {
  const absolute = resolve(path);
  const root = parse(absolute).root;
  let current = root;
  for (const part of absolute.slice(root.length).split(sep).filter(Boolean)) {
    current = join(current, part);
    let stat;
    try {
      stat = lstatSync(current);
    } catch (error) {
      if (error.code === 'ENOENT') continue;
      reject();
    }
    if (stat.isSymbolicLink() || realpathSync(current) !== current ||
        (current !== absolute || directory ? !stat.isDirectory() : !stat.isFile())) reject();
  }
  return absolute;
}

function git(workspace, ...args) {
  return execFileSync('git', ['-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=', '-C', workspace, ...args],
    { env: { ...process.env, GIT_NO_REPLACE_OBJECTS: '1' },
      maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });
}

function plan({ workspace, trustedSha, paths }) {
  if (typeof workspace !== 'string' || !isAbsolute(workspace) ||
      workspace !== resolve(workspace) || !/^[0-9a-f]{40}$/.test(trustedSha ?? '')) reject();
  checkedPath(workspace, true);
  if (realpathSync(workspace) !== workspace ||
      resolve(git(workspace, 'rev-parse', '--show-toplevel').toString().trim()) !== workspace) reject();
  if (paths !== undefined && (!Array.isArray(paths) || paths.length !== 1 || paths[0] !== SKILL)) reject();
  const entries = git(workspace, 'ls-tree', '-rz', '--full-tree', trustedSha, '--',
    ...(paths ?? ['AGENTS.md', '.agents', '.github'])).toString().split('\0').filter(Boolean);
  if (!entries.length) reject();
  const result = entries.map(entry => {
    const match = /^(100644|100755) blob ([0-9a-f]{40})\t(.+)$/.exec(entry);
    if (!match) reject();
    const [, mode, oid, path] = match;
    if (!(path === 'AGENTS.md' || path.startsWith('.agents/') || path.startsWith('.github/')) ||
        path.includes('\\') || /[\u0000-\u001f]/.test(path) ||
        path.split('/').some(part => !part || part === '.' || part === '..' || part.includes(':'))) reject();
    const destination = resolve(workspace, ...path.split('/'));
    if (!destination.startsWith(`${workspace}${sep}`)) reject();
    checkedPath(destination);
    return { destination, mode, bytes: git(workspace, 'cat-file', 'blob', oid) };
  });
  const removals = [];
  if (paths === undefined) {
    const trusted = new Set(result.map(entry => entry.destination));
    function inspect(path) {
      let stat;
      try {
        stat = lstatSync(path);
      } catch (error) {
        if (error.code === 'ENOENT') return;
        reject();
      }
      checkedPath(path, stat.isDirectory());
      if (stat.isDirectory()) {
        for (const name of readdirSync(path)) inspect(join(path, name));
      } else if (!trusted.has(path)) {
        removals.push(path);
      }
    }
    checkedPath(join(workspace, 'AGENTS.md'));
    checkedPath(join(workspace, '.mcp.json'));
    for (const path of ['AGENTS.md', '.agents', '.github', '.mcp.json']) inspect(join(workspace, path));
  }
  return { entries: result, removals };
}

// Resolve every immutable blob and check every destination before the first write.
export function restoreTrustedInputs(options) {
  const { entries, removals } = plan(options);
  for (const path of removals) rmSync(path);
  for (const { destination, mode, bytes } of entries) {
    mkdirSync(dirname(destination), { recursive: true });
    checkedPath(destination);
    // Rename a fresh regular file rather than truncate a potentially hard-linked leaf.
    const staged = `${destination}.security-restore-${process.pid}`;
    writeFileSync(staged, bytes, { flag: 'wx', mode: mode === '100755' ? 0o755 : 0o644 });
    try {
      chmodSync(staged, mode === '100755' ? 0o755 : 0o644);
      renameSync(staged, destination);
    } finally {
      rmSync(staged, { force: true });
    }
    if (!readFileSync(destination).equals(bytes)) reject();
  }
  return entries.length;
}

export function installTrustedRestore({ actionsDir, ...options }) {
  plan(options);
  if (typeof actionsDir !== 'string' || !isAbsolute(actionsDir)) reject();
  const actions = checkedPath(actionsDir, true);
  if (actions === options.workspace || actions.startsWith(`${options.workspace}${sep}`) ||
      SOURCE.startsWith(`${options.workspace}${sep}`)) reject();
  const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
  const script = `#!/bin/sh\nset -eu\nexec node ${quote(SOURCE)} restore --workspace ${quote(options.workspace)} --trusted-sha ${quote(options.trustedSha)}\n`;
  const targets = ['restore_base_github_folders.sh', 'restore_inline_sub_agents.sh', 'restore_inline_skills.sh']
    .map(name => checkedPath(join(actions, name)));
  for (const target of targets) writeFileSync(target, script);
}

if (process.argv[1] && resolve(process.argv[1]) === SOURCE) {
  try {
    const [command, ...args] = process.argv.slice(2);
    const values = {};
    for (let index = 0; index < args.length; index += 2) {
      const key = args[index];
      if (!['--workspace', '--trusted-sha', '--actions-dir', '--only'].includes(key) ||
          !args[index + 1] || values[key] !== undefined) reject();
      values[key] = args[index + 1];
    }
    if (values['--only'] && values['--only'] !== 'skill') reject();
    const options = { workspace: values['--workspace'], trustedSha: values['--trusted-sha'],
      ...(values['--only'] ? { paths: [SKILL] } : {}) };
    if (command === 'install') installTrustedRestore({ ...options, actionsDir: values['--actions-dir'] });
    else if (command === 'restore' && !values['--actions-dir']) restoreTrustedInputs(options);
    else reject();
  } catch {
    console.error(`::error::${ERROR}`);
    process.exitCode = 1;
  }
}
