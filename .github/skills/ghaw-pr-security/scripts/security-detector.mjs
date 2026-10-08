#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { appendFileSync, chmodSync, copyFileSync, lstatSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createDetectorPublicationProof, validateDetectorResult } from './security-review.mjs';

export const PREPARE_SHA256 = 'd825da5d300f2b5d0a03665614bd872a3e3f54a328ef188f4a7fb4ce0e507ae7';
export const INSTALL_SHA256 = '1f35329a8662e59ba28748c8d18bc5e0dd8f2b47cc1323be067b87bb6763bc6c';
const clean = { prompt_injection: false, secret_leak: false, malicious_patch: false, reasons: [], warnings: [] };
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
function directory(path) {
  for (let current = resolve(path); ; current = dirname(current)) {
    const stat = lstatSync(current);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('irregular detector directory');
    if (dirname(current) === current) return;
  }
}
function regular(path) {
  directory(dirname(path instanceof URL ? fileURLToPath(path) : path));
  if (!lstatSync(path).isFile() || lstatSync(path).isSymbolicLink()) throw new Error('irregular detector input');
  return readFileSync(path);
}

export function consumedBinding(environment, scope, report, patch) {
  const draft = {
    version: 2, repository: environment.GITHUB_REPOSITORY, runId: environment.GITHUB_RUN_ID,
    runAttempt: Number(environment.GITHUB_RUN_ATTEMPT), sourceJob: 'detection',
    trustedWorkflowSha: environment.TRUSTED_SHA, prNumber: scope.prNumber, headSha: scope.headSha,
    baseSha: scope.observedBaseSha, comparisonBaseSha: scope.baseSha, scopeSha256: scope.scopeSha256,
    mode: scope.mode, repositoryRelation: scope.repositoryRelation, detectionSuccess: true, detectionConclusion: 'success',
    reportSha256: digest(report), patchSha256: digest(patch), detectorVersion: 'v0.5.1',
    detectorSourceSha: '230b061fde8539a492c6c7d4cb9c96d1b33323d3', executionExitCode: 0,
  };
  createDetectorPublicationProof({ ...environment, DETECTION_SUCCESS: 'true', DETECTION_CONCLUSION: 'success' },
    scope, report, patch, { executionOutcome: 'success', conclusionOutcome: 'success', consumed: draft, result: clean });
  return draft;
}

export function stageDetectorInputs(payload, inputs, environment = process.env) {
  directory(payload);
  directory(inputs);
  for (const name of ['detection_result.json', 'detection_result_full.json',
    join('sandbox', 'firewall', 'audit', 'security-consumed.json')]) {
    try {
      regular(join(inputs, name));
      rmSync(join(inputs, name));
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
  }
  const scope = JSON.parse(regular(join(payload, environment.SECURITY_SCOPE_FILE)));
  const report = regular(join(payload, 'security-findings.validated.json'));
  const patch = scope.mode === 'repair' ? regular(join(payload, 'security-repair.patch')) : Buffer.alloc(0);
  const binding = consumedBinding(environment, scope, report, patch);
  for (const name of readdirSync(inputs)) {
    if (/^aw-.*\.(patch|bundle)$/.test(name)) {
      regular(join(inputs, name));
      rmSync(join(inputs, name));
    }
  }
  const output = join(inputs, 'agent_output.json');
  try {
    regular(output);
    rmSync(output);
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  writeFileSync(output, report, { flag: 'wx', mode: 0o644 });
  if (patch.length) writeFileSync(join(inputs, 'aw-security-repair.patch'), patch, { flag: 'wx', mode: 0o644 });
  writeFileSync(join(payload, 'input-binding.json'), JSON.stringify(binding), { flag: 'wx', mode: 0o644 });
  return binding;
}

export function verifyDetectorInputs(payload, inputs, environment = process.env) {
  const scope = JSON.parse(regular(join(payload, environment.SECURITY_SCOPE_FILE)));
  const report = regular(join(inputs, 'agent_output.json'));
  const patchFiles = readdirSync(inputs).filter(name => /^aw-.*\.(patch|bundle)$/.test(name));
  const patch = patchFiles.length ? regular(join(inputs, 'aw-security-repair.patch')) : Buffer.alloc(0);
  if (patchFiles.some(name => name !== 'aw-security-repair.patch') ||
      (scope.mode === 'guide' && patchFiles.length)) throw new Error('unexpected detector patch input');
  const binding = consumedBinding(environment, scope, report, patch);
  if (JSON.stringify(binding) !== regular(join(payload, 'input-binding.json')).toString()) {
    throw new Error('detector inputs changed before consumption');
  }
  return binding;
}

function prepare() {
  const root = process.env.RUNNER_TEMP;
  const asset = join(root, 'gh-aw', 'actions', 'prepare_threat_detection_files.sh');
  const source = regular(asset);
  const install = join(root, 'gh-aw', 'actions', 'install_threat_detect_binary.sh');
  if (digest(source) !== PREPARE_SHA256 || digest(regular(install)) !== INSTALL_SHA256) throw new Error('pinned detector asset changed');
  const native = join(root, 'gh-aw', 'security-detector-native');
  const payload = join(root, 'gh-aw', 'security-payload');
  // These mounts are read-only inside AWF, whose container identity can differ.
  mkdirSync(native, { mode: 0o755 });
  mkdirSync(payload, { mode: 0o755 });
  for (const name of [process.env.SECURITY_SCOPE_FILE, 'security-findings.validated.json',
    ...(process.env.SECURITY_SCOPE_FILE === 'security-scope.validated.json' ? ['security-repair.patch'] : [])]) {
    writeFileSync(join(payload, name), regular(join(root, 'security-payload', name)), { flag: 'wx', mode: 0o644 });
  }
  for (const name of ['security-review.mjs', 'security-detector.mjs']) {
    writeFileSync(join(native, name), regular(new URL(name, import.meta.url)), { flag: 'wx', mode: 0o644 });
  }
  appendFileSync(process.env.GITHUB_PATH, `${join(root, 'gh-aw', 'bin')}\n`);
  appendFileSync(asset, '\nnode "$RUNNER_TEMP/gh-aw/security-detector-native/security-detector.mjs" stage\n');
  appendFileSync(install, '\nnode "$RUNNER_TEMP/gh-aw/security-detector-native/security-detector.mjs" install-wrapper\n');
  regular(join(payload, process.env.SECURITY_SCOPE_FILE));
}

function execute(args, captureConclusion = false) {
  const payload = join(process.env.RUNNER_TEMP, 'gh-aw', 'security-payload');
  const inputs = '/tmp/gh-aw/threat-detection';
  const concluding = args[0] === 'conclude';
  if (!concluding && JSON.stringify(args) !== JSON.stringify([
    '--engine', 'copilot', '--output', `${inputs}/detection_result.json`, inputs,
  ])) throw new Error('unexpected detector invocation');
  const binding = concluding ? undefined : verifyDetectorInputs(payload, inputs);
  const privateDir = concluding
    ? join(process.env.RUNNER_TEMP, 'gh-aw', 'security-detector-private-host')
    : '/tmp/gh-aw/security-detector-private';
  mkdirSync(privateDir, { recursive: true, mode: 0o700 });
  directory(privateDir);
  const output = join(privateDir, 'native-conclusion.outputs');
  const environmentFile = join(privateDir, 'native-conclusion.env');
  if (captureConclusion) {
    writeFileSync(output, '', { flag: 'wx', mode: 0o600 });
    writeFileSync(environmentFile, '', { flag: 'wx', mode: 0o600 });
  }
  const result = spawnSync(join(process.env.RUNNER_TEMP, 'gh-aw', 'bin', 'threat-detect-real'), args, {
    env: { ...process.env, GITHUB_STEP_SUMMARY: join(privateDir, 'summary.md'),
      HAS_PATCH: binding ? String(binding.patchSha256 !== digest('')) : process.env.HAS_PATCH,
      ...(captureConclusion ? {
        RUN_DETECTION: 'true',
        DETECTION_AGENTIC_EXECUTION_OUTCOME: process.env.DETECTION_EXECUTION_OUTCOME,
        GH_AW_DETECTION_CONTINUE_ON_ERROR: 'false',
        GITHUB_OUTPUT: output,
        GITHUB_ENV: environmentFile,
      } : {}) },
    timeout: concluding ? 30_000 : 540_000, maxBuffer: 16 * 1024 * 1024,
  });
  writeFileSync(join(privateDir, concluding ? 'conclusion.log' : 'execution.log'),
    Buffer.concat([result.stdout ?? Buffer.alloc(0), result.stderr ?? Buffer.alloc(0)]), { mode: 0o600 });
  if (result.error || result.status !== 0) throw new Error('detector execution failed');
  if (!concluding) {
    if (JSON.stringify(binding) !== JSON.stringify(verifyDetectorInputs(payload, inputs))) throw new Error('consumed inputs changed');
    regular(join(inputs, 'detection_result.json'));
  }
  if (captureConclusion) validateNativeConclusionOutput(regular(output).toString('utf8'));
  console.log('Trusted detector phase completed; raw diagnostics remain private.');
}

export function validateNativeConclusionOutput(output) {
  if (output.replace(/\r\n/g, '\n') !== 'conclusion=success\nsuccess=true\nreason=\n') {
    throw new Error('native detector conclusion must evaluate a successful verdict');
  }
}

export function validateOriginalOutcomes(execution, conclusion) {
  if (execution !== 'success' || conclusion !== 'success') throw new Error('original detector outcomes must both succeed');
}

function hostConclude() {
  if (process.env.DETECTION_EXECUTION_OUTCOME !== 'success') throw new Error('original detector execution failed');
  const payload = join(process.env.RUNNER_TEMP, 'gh-aw', 'security-payload');
  const inputs = '/tmp/gh-aw/threat-detection';
  const binding = verifyDetectorInputs(payload, inputs);
  validateDetectorResult(JSON.parse(regular(join(inputs, 'detection_result.json'))));
  execute(['conclude', '--result-file', join(inputs, 'detection_result.json'),
    '--detection-log', join(inputs, 'detection.log'), '--full-result-file', ''], true);
  const host = join(process.env.RUNNER_TEMP, 'security-detector-host');
  mkdirSync(host, { mode: 0o700 });
  directory(host);
  writeFileSync(join(host, 'conclusion.json'), JSON.stringify({
    binding, executionOutcome: process.env.DETECTION_EXECUTION_OUTCOME, conclusionExitCode: 0,
    result: JSON.parse(regular(join(inputs, 'detection_result.json'))),
  }), { flag: 'wx', mode: 0o600 });
}

export function completeHostDetector(host, execution, conclusion) {
  validateOriginalOutcomes(execution, conclusion);
  const checkpoint = JSON.parse(regular(join(host, 'conclusion.json')));
  if (checkpoint.executionOutcome !== execution || checkpoint.conclusionExitCode !== 0) throw new Error('host detector conclusion is missing');
  validateDetectorResult(checkpoint.result);
  const completion = { ...checkpoint, conclusionOutcome: conclusion };
  writeFileSync(join(host, 'security-detector-host.json'), JSON.stringify(completion), { flag: 'wx', mode: 0o600 });
  return completion;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv[2] === 'prepare') prepare();
    else if (process.argv[2] === 'stage') stageDetectorInputs(
      join(process.env.RUNNER_TEMP, 'gh-aw', 'security-payload'), '/tmp/gh-aw/threat-detection');
    else if (process.argv[2] === 'install-wrapper') {
      const bin = join(process.env.RUNNER_TEMP, 'gh-aw', 'bin');
      mkdirSync(bin, { recursive: true });
      copyFileSync('/usr/local/bin/threat-detect', join(bin, 'threat-detect-real'));
      chmodSync(join(bin, 'threat-detect-real'), 0o755);
      writeFileSync(join(bin, 'threat-detect'),
        '#!/usr/bin/env bash\nexec node "$RUNNER_TEMP/gh-aw/security-detector-native/security-detector.mjs" execute "$@"\n',
        { flag: 'wx', mode: 0o755 });
      chmodSync(join(bin, 'threat-detect'), 0o755);
    }
    else if (process.argv[2] === 'execute') execute(process.argv.slice(3));
    else if (process.argv[2] === 'host-conclude') hostConclude();
    else if (process.argv[2] === 'host-complete') completeHostDetector(
      join(process.env.RUNNER_TEMP, 'security-detector-host'),
      process.env.DETECTION_EXECUTION_OUTCOME, process.env.DETECTION_CONCLUSION_OUTCOME);
    else throw new Error('unknown detector operation');
  } catch {
    console.error('Trusted detector binding failed.');
    process.exitCode = 1;
  }
}
