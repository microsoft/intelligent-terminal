import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  SECURITY_NOOP_MESSAGE, attestChecks, buildScope, classifyPath, createReportTemplate, normalizePath, renderReport, validatePatch,
  validateQueuedOutput, validateReport, validateProposal, validateCandidate, stageRepairFiles, validateRepairScope,
  submitSecurityReport, readSecurityDiff, readImmutableHunks, readSecuritySource, inspectSecurityRepair, writeSecurityRepair, replaceSecurityRepairText, verifyCredentialFree,
  publicationDecision, validatePublicationRun, validateNativePublicationProof, preparePublication, validateRepairChanges,
  createDetectorPublicationProof, validateDetectorPublicationProof, validateDetectorExecutionJobs,
} from './security-review.mjs';
import { PREPARE_SHA256, INSTALL_SHA256, consumedBinding, stageDetectorInputs, verifyDetectorInputs,
  completeHostDetector, validateOriginalOutcomes, validateNativeConclusionOutput } from './security-detector.mjs';

const BASE = '1'.repeat(40);
const tmpdir = () => process.cwd();
const HEAD = '2'.repeat(40);
const PATCH_TEXT = 'diff --git a/tools/wta/src/master/mod.rs b/tools/wta/src/master/mod.rs\n--- a/tools/wta/src/master/mod.rs\n+++ b/tools/wta/src/master/mod.rs\n@@ -20 +20 @@\n-Changed source.\n+Bound owner.\n';
const PATCH_SHA256 = createHash('sha256').update(PATCH_TEXT).digest('hex');

function publicationJobs(sameRepo) {
  return ['agent', 'detection', 'safe_outputs', 'publication_gate', ...(sameRepo ? ['finalize', 'validate_windows / validate'] : [])]
    .map(name => ({
      name, run_id: 123, run_attempt: 1, status: 'completed', conclusion: 'success',
      ...(name === 'publication_gate' ? { steps: [
        'Attest successful generated detector outcome', 'Upload trusted detector publication proof',
      ].map(name => ({ name, status: 'completed', conclusion: 'success' })) } : {}),
      ...(name === 'detection' ? { steps: [
        'Conclude detector for publication', 'Attest original detector outcomes on host', 'Upload trusted host detector completion',
      ].map(name => ({ name, status: 'completed', conclusion: 'success' })) } : {}),
    }));
}

function detectorProof(current, candidate, patch = '') {
  const environment = {
    DETECTION_SUCCESS: 'true', DETECTION_CONCLUSION: 'success', GITHUB_REPOSITORY: 'owner/repo',
    GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1', TRUSTED_SHA: current.observedBaseSha,
    EXPECTED_BASE_SHA: current.observedBaseSha, EXPECTED_HEAD_SHA: current.headSha,
    COMPARISON_BASE_SHA: current.baseSha, PR_NUMBER: String(current.prNumber),
  };
  return createDetectorPublicationProof(environment, current, JSON.stringify(candidate), patch,
    detectorEvidence(environment, current, JSON.stringify(candidate), patch));
}

function detectorEvidence(environment, scope, bytes, patch = '') {
  return {
    executionOutcome: 'success', conclusionOutcome: 'success', consumed: consumedBinding(environment, scope, bytes, patch),
    result: { prompt_injection: false, secret_leak: false, malicious_patch: false, reasons: [], warnings: [] },
  };
}
function detectorArtifact(base = BASE) {
  return { name: 'ghaw-pr-security-detector-proof-123-1-17', expired: false, workflow_run: { id: 123, head_sha: base } };
}

test('queued noop rejects model content, missing message, unknown and prototype fields', () => {
  const fixed = { type: 'noop', message: SECURITY_NOOP_MESSAGE };
  for (const mode of ['guide', 'repair']) {
    validateQueuedOutput({ mode }, { items: [fixed], errors: [] });
    for (const item of [
      { type: 'noop' }, { ...fixed, message: 'DUMMY_DIFF_CODE_SENTINEL' },
      { ...fixed, message: 'github_pat_DUMMY_CREDENTIAL_SENTINEL_12345678901234567890' },
      { ...fixed, reason: 'DUMMY_REASON_SENTINEL' },
      { ...fixed, id: 'DUMMY_MODEL_ID_SENTINEL' },
      JSON.parse(`{"type":"noop","message":${JSON.stringify(SECURITY_NOOP_MESSAGE)},"__proto__":{"secret":"DUMMY"}}`),
      Object.assign(Object.create({ secret: 'DUMMY' }), fixed),
      Object.assign(Object.create(null), fixed),
      { ...fixed, [Symbol('secret')]: 'DUMMY' },
    ]) assert.throws(() => validateQueuedOutput({ mode }, { items: [item], errors: [] }), /fixed payload/);
  }
});

test('controller separates analysis from mutually exclusive narrow publication jobs', () => {
  const controller = readFileSync(new URL('../../../workflows/ghaw-pr-security-controller.yml', import.meta.url), 'utf8');
  assert(controller.includes('permissions: {}'));
  const analysis = controller.split('  security-review:')[1].split('  publish-repair:')[0];
  assert(analysis.includes('contents: read'));
  assert(analysis.includes('actions: write'));
  assert(!analysis.includes('contents: write'));
  assert(!analysis.includes('issues: write'));
  assert(!analysis.includes('git push'));
  assert(analysis.includes('prepare-publication'));
  assert(analysis.indexOf('Canonicalize worker') < analysis.indexOf('Upload canonical publication handoff'));
  const repair = controller.split('  publish-repair:')[1].split('  publish-guidance:')[0];
  assert(repair.includes("outputs.publication == 'push'"));
  assert(repair.includes('contents: write'));
  assert(!repair.includes('issues: write'));
  assert(!repair.includes('actions: write'));
  assert(repair.includes('ref: ${{ github.event.pull_request.base.sha }}'));
  assert(repair.includes('git apply --cached --binary'));
  assert(repair.includes('--force-with-lease="refs/heads/$HEAD_REF:$EXPECTED_HEAD_SHA"'));
  assert(repair.includes('git merge-base --is-ancestor'));
  const guidance = controller.split('  publish-guidance:')[1];
  assert(guidance.includes("outputs.publication == 'comment'"));
  assert(guidance.includes('issues: write'));
  assert(!guidance.includes('contents: write'));
  assert(!guidance.includes('actions: write'));
  assert(guidance.includes('reviewed-head: ${head}'));
  assert(guidance.includes('existing.body !== body'));
  for (const publisher of [repair, guidance]) assert(publisher.includes("outputs.detector_attested == 'true'"));
  assert(analysis.includes('--detector-proof'));
  assert(!/\$\{\{\s*github\.event\.pull_request\.(?:title|body|head\.ref)/.test(
    [...controller.matchAll(/        run: \|\r?\n([\s\S]*?)(?=\r?\n      -|\r?\n  [a-z-]+:|$)/g)].map(m => m[1]).join('\n')));
});

test('both compiled workers attest generated detector outputs only after detection in a native job', () => {
  for (const name of ['ghaw-pr-security', 'ghaw-pr-security-guide-fork']) {
    const markdown = readFileSync(new URL(`../../../workflows/${name}.md`, import.meta.url), 'utf8');
    const lock = readFileSync(new URL(`../../../workflows/${name}.lock.yml`, import.meta.url), 'utf8');
    for (const source of [markdown, lock]) {
      const gate = source.split('  publication_gate:')[1].split(/\r?\n(?:safe-outputs:|  [a-z_]+:)/)[0];
      assert(gate, 'native publication gate exists');
      assert.match(gate, /needs:[^\n]*(?:agent|\r?\n)/);
      assert(gate.includes('detection'));
      assert(source.includes('Install trusted exact-input detector binding'));
      assert(source.includes('security-detector.mjs prepare'));
      assert(gate.includes('DETECTION_SUCCESS: ${{ needs.detection.outputs.detection_success }}'));
      assert(gate.includes('DETECTION_CONCLUSION: ${{ needs.detection.outputs.detection_conclusion }}'));
      assert(gate.includes('ref: ${{ github.workflow_sha }}'));
      assert(gate.includes('attest-detector'));
      assert(gate.includes('ghaw-pr-security-detector-proof-${{ github.run_id }}-${{ github.run_attempt }}'));
      assert(!gate.includes('continue-on-error'));
      assert(!gate.includes('always()'));
      assert(!gate.includes('contents: write'));
    }
  }
});

test('typed detector proof rejects warning, cancelled, missing and self-reported success', () => {
  for (const relation of ['same-repo', 'fork']) {
    const current = buildScope(BASE, HEAD, 17, relation, 'M\0tools/wta/src/master/mod.rs\0', BASE,
      relation === 'same-repo' ? 'repair' : 'guide');
    const candidate = { ...createReportTemplate(current), summary: 'Detector binding fixture.' };
    const environment = {
      DETECTION_SUCCESS: 'true', DETECTION_CONCLUSION: 'success', GITHUB_REPOSITORY: 'owner/repo',
      GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1', TRUSTED_SHA: BASE, EXPECTED_BASE_SHA: BASE,
      EXPECTED_HEAD_SHA: HEAD, COMPARISON_BASE_SHA: BASE, PR_NUMBER: '17',
    };
    const bytes = JSON.stringify(candidate);
    const evidence = detectorEvidence(environment, current, bytes);
    for (const extra of [
      { DETECTION_SUCCESS: undefined }, { DETECTION_SUCCESS: '' }, { DETECTION_SUCCESS: 'false' },
      { DETECTION_SUCCESS: true }, { DETECTION_SUCCESS: 'TRUE' },
      ...['warning', 'failure', 'cancelled', 'skipped', '', undefined].map(DETECTION_CONCLUSION => ({ DETECTION_CONCLUSION })),
    ]) assert.throws(() => createDetectorPublicationProof({ ...environment, ...extra }, current, bytes), /explicit successful/);
    const proof = createDetectorPublicationProof(environment, current, bytes, '', evidence);
    for (const executionOutcome of ['failure', 'cancelled', 'skipped', undefined]) {
      assert.throws(() => createDetectorPublicationProof(environment, current, bytes, '',
        { ...evidence, executionOutcome }), /explicit successful/);
    }
    for (const conclusionOutcome of ['failure', 'cancelled', 'skipped', undefined]) {
      assert.throws(() => createDetectorPublicationProof(environment, current, bytes, '',
        { ...evidence, conclusionOutcome }), /explicit successful/);
    }
    for (const warnings of [
      [{ field: 'agent_output', code: 'ERR_VALIDATION', message: 'Missing structured output.' }],
      [{ field: 'patch', code: 'ERR_VALIDATION', message: 'No readable patch.' }],
      [{ field: 'prompt', code: 'ERR_VALIDATION', message: 'Missing prompt context.' }],
      [{ field: 'comment_memory', code: 'ERR_SYSTEM', message: 'Unreadable context.' }],
      null, 'warning', [{}],
    ]) assert.throws(() => createDetectorPublicationProof(environment, current, bytes, '',
      { ...evidence, result: { ...evidence.result, warnings } }), /no inspection warnings/);
    for (const consumed of [undefined, { ...evidence.consumed, executionExitCode: 1 },
      { ...evidence.consumed, runId: '124' }, { ...evidence.consumed, runAttempt: 2 },
      { ...evidence.consumed, sourceJob: 'agent' },
      { ...evidence.consumed, reportSha256: '0'.repeat(64) },
      { ...evidence.consumed, patchSha256: '0'.repeat(64) }]) {
      assert.throws(() => createDetectorPublicationProof(environment, current, bytes, '',
        { ...evidence, consumed }), /exact final publication inputs/);
    }
    const run = { id: 123, run_attempt: 1, head_sha: BASE };
    assert.doesNotThrow(() => validateDetectorPublicationProof(proof, run, current, bytes, '', 'owner/repo'));
    for (const [key, value] of Object.entries({
      version: 1, repository: 'wrong/repo', runId: '124', runAttempt: 2, sourceJob: 'agent',
      trustedWorkflowSha: HEAD, prNumber: 18, headSha: BASE, baseSha: HEAD, comparisonBaseSha: HEAD,
      scopeSha256: '0'.repeat(64), mode: 'other', repositoryRelation: 'other',
      detectionSuccess: 'true', detectionConclusion: 'warning', reportSha256: '0'.repeat(64),
      patchSha256: '0'.repeat(64), extraClaim: true,
    })) assert.throws(() => validateDetectorPublicationProof({ ...proof, [key]: value }, run, current, bytes, '', 'owner/repo'), /attestation/);
    assert.throws(() => validateDetectorPublicationProof(proof, run, current, `${bytes}\n`, '', 'owner/repo'), /attestation/);
    assert.throws(() => validateDetectorPublicationProof(proof, run, current, bytes, PATCH_TEXT, 'owner/repo'), /attestation/);
  }
});

test('native staging consumes exact final bytes and rejects missing or subsequently changed inputs', () => {
  const root = mkdtempSync(join(process.cwd(), '.detector-cli-'));
  try {
    const current = buildScope(BASE, HEAD, 17, 'fork', 'M\0tools/wta/src/master/mod.rs\0', BASE, 'guide');
    const candidate = { ...createReportTemplate(current), summary: 'Native detector CLI fixture.' };
    const scopePath = join(root, 'scope.json');
    const reportPath = join(root, 'report.json');
    const inputs = join(root, 'inputs');
    mkdirSync(inputs);
    writeFileSync(join(root, 'security-scope.json'), JSON.stringify(current));
    writeFileSync(join(root, 'security-findings.validated.json'), JSON.stringify(candidate));
    const environment = {
      ...process.env, DETECTION_SUCCESS: 'true', DETECTION_CONCLUSION: 'success',
      GITHUB_REPOSITORY: 'owner/repo', GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1',
      TRUSTED_SHA: BASE, EXPECTED_BASE_SHA: BASE, EXPECTED_HEAD_SHA: HEAD,
      COMPARISON_BASE_SHA: BASE, PR_NUMBER: '17',
      SECURITY_SCOPE_FILE: 'security-scope.json',
    };
    writeFileSync(join(inputs, 'agent_output.json'), '{"items":[{"type":"noop"}]}');
    const binding = stageDetectorInputs(root, inputs, environment);
    assert.equal(readFileSync(join(inputs, 'agent_output.json'), 'utf8'), JSON.stringify(candidate));
    assert.deepEqual(verifyDetectorInputs(root, inputs, environment), binding);
    writeFileSync(join(inputs, 'agent_output.json'), `${JSON.stringify(candidate)}\n`);
    assert.throws(() => verifyDetectorInputs(root, inputs, environment), /changed before consumption/);
    unlinkSync(join(inputs, 'agent_output.json'));
    assert.throws(() => verifyDetectorInputs(root, inputs, environment));
    writeFileSync(join(inputs, 'agent_output.json'), JSON.stringify(candidate));
    writeFileSync(join(inputs, 'aw-unexpected.patch'), PATCH_TEXT);
    assert.throws(() => verifyDetectorInputs(root, inputs, environment));
    unlinkSync(join(inputs, 'aw-unexpected.patch'));
    for (const extra of [{ GITHUB_RUN_ATTEMPT: '2' }, { TRUSTED_SHA: HEAD }, { EXPECTED_HEAD_SHA: BASE }]) {
      assert.throws(() => verifyDetectorInputs(root, inputs, { ...environment, ...extra }));
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('repair detector stages the actual patch bytes under the pinned aw patch consumer name', () => {
  const root = mkdtempSync(join(process.cwd(), '.detector-patch-'));
  try {
    const current = buildScope(BASE, HEAD, 17, 'same-repo', 'M\0tools/wta/src/master/mod.rs\0', BASE, 'repair');
    const candidate = { ...createReportTemplate(current), summary: 'Exact finalized patch binding.' };
    const environment = {
      GITHUB_REPOSITORY: 'owner/repo', GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '1',
      TRUSTED_SHA: BASE, EXPECTED_BASE_SHA: BASE, EXPECTED_HEAD_SHA: HEAD,
      COMPARISON_BASE_SHA: BASE, PR_NUMBER: '17', SECURITY_SCOPE_FILE: 'security-scope.validated.json',
    };
    const inputs = join(root, 'inputs');
    mkdirSync(inputs);
    writeFileSync(join(root, environment.SECURITY_SCOPE_FILE), JSON.stringify(current));
    writeFileSync(join(root, 'security-findings.validated.json'), JSON.stringify(candidate));
    writeFileSync(join(root, 'security-repair.patch'), PATCH_TEXT);
    writeFileSync(join(inputs, 'aw-previous.patch'), 'Unrelated transport.');
    stageDetectorInputs(root, inputs, environment);
    assert.equal(readFileSync(join(inputs, 'aw-security-repair.patch'), 'utf8'), PATCH_TEXT);
    assert.throws(() => readFileSync(join(inputs, 'aw-previous.patch')));
    assert.doesNotThrow(() => verifyDetectorInputs(root, inputs, environment));
    writeFileSync(join(inputs, 'aw-security-repair.patch'), `${PATCH_TEXT}\n`);
    assert.throws(() => verifyDetectorInputs(root, inputs, environment), /changed before consumption/);
    unlinkSync(join(inputs, 'aw-security-repair.patch'));
    assert.throws(() => verifyDetectorInputs(root, inputs, environment), /changed before consumption/);
    unlinkSync(join(root, 'security-repair.patch'));
    assert.throws(() => stageDetectorInputs(root, inputs, environment));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('all trusted materialization and reconstruction workflows disable ambient replacement refs', () => {
  for (const name of [
    'ghaw-pr-security.md', 'ghaw-pr-security-guide-fork.md',
    'ghaw-pr-security.lock.yml', 'ghaw-pr-security-guide-fork.lock.yml',
    'ghaw-pr-security-controller.yml',
  ]) {
    const text = readFileSync(new URL(`../../../workflows/${name}`, import.meta.url), 'utf8');
    assert.match(text, /^env:\r?\n  GIT_NO_REPLACE_OBJECTS: ['"]1['"]/m, name);
  }
  const windows = readFileSync(new URL('../../../workflows/ghaw-pr-security-validate-windows.yml', import.meta.url), 'utf8');
  assert.match(windows, /^    env:\r?\n      GIT_NO_REPLACE_OBJECTS: ['"]1['"]/m);
});

test('pinned actual preparation and installer assets match the trusted detector hook contract',
  { skip: !process.env.SECURITY_PINNED_GHAW_RUNTIME }, () => {
    const source = process.env.SECURITY_PINNED_GHAW_RUNTIME;
    for (const [name, expected] of [
      ['prepare_threat_detection_files.sh', PREPARE_SHA256], ['install_threat_detect_binary.sh', INSTALL_SHA256],
    ]) assert.equal(createHash('sha256').update(readFileSync(join(source, 'setup', 'sh', name))).digest('hex'), expected);
    const prepare = readFileSync(join(source, 'setup', 'sh', 'prepare_threat_detection_files.sh'), 'utf8');
    assert(prepare.includes('copy_optional_file "${SOURCE_DIR}/agent_output.json"'));
    assert(prepare.includes('for artifact_pattern in aw-*.patch aw-*.bundle'));
    for (const name of ['ghaw-pr-security', 'ghaw-pr-security-guide-fork']) {
      const lock = readFileSync(new URL(`../../../workflows/${name}.lock.yml`, import.meta.url), 'utf8');
      const detection = lock.split('  detection:')[1].split(/\r?\n  [a-z_]+:/)[0];
      assert(detection.indexOf('id: setup') < detection.indexOf('Install trusted exact-input detector binding'));
      assert(detection.indexOf('Install trusted exact-input detector binding') < detection.indexOf('Prepare threat detection files'));
      assert(detection.indexOf('Prepare threat detection files') < detection.indexOf('Install threat-detect binary'));
      assert(detection.indexOf('Install threat-detect binary') < detection.indexOf('Execute threat detection with AWF'));
      assert(detection.indexOf('Execute threat detection with AWF') < detection.indexOf('Conclude detector for publication'));
      assert(detection.indexOf('Conclude detector for publication') < detection.indexOf('Attest original detector outcomes on host'));
      assert(detection.indexOf('Attest original detector outcomes on host') < detection.indexOf('Upload trusted host detector completion'));
      assert(detection.includes('DETECTION_EXECUTION_OUTCOME: ${{ steps.detection_agentic_execution.outcome }}'));
      assert(detection.includes('DETECTION_CONCLUSION_OUTCOME: ${{ steps.security_detector_conclusion.outcome }}'));
      assert(detection.includes('path: ${{ runner.temp }}/security-detector-host/security-detector-host.json'));
      assert(!detection.includes('--mount "${RUNNER_TEMP}/security-detector-host'));
      const hostSteps = detection.split('      - name: Conclude detector for publication')[1]
        .split('      - name: Upload threat detection artifact')[0];
      assert(!hostSteps.includes('continue-on-error'), 'host conclusion, attestation and upload failures remain fatal');
      assert(!detection.includes('--mount "${RUNNER_TEMP}:${RUNNER_TEMP}'), 'detector cannot mount the parent containing host completion');
      if (name === 'ghaw-pr-security') assert(detection.split('    steps:')[0].includes('- finalize'));
    }
  });

test('clean pinned verdict cannot hide failed execution or failed conclusion in a green API job', () => {
  const jobs = publicationJobs(false);
  assert.doesNotThrow(() => validateDetectorExecutionJobs(jobs, 123, 1));
  const detection = jobs.find(job => job.name === 'detection');
  for (const index of [0, 1, 2]) {
    for (const conclusion of ['failure', 'cancelled', 'skipped', null]) {
      const changed = jobs.map(job => job === detection ? {
        ...job, steps: job.steps.map((step, i) => i === index ? { ...step, conclusion } : step),
      } : job);
      assert.throws(() => validateDetectorExecutionJobs(changed, 123, 1), /actual successful detector/);
    }
  }
  for (const steps of [undefined, [], detection.steps.slice(1), [...detection.steps, detection.steps[0]]]) {
    assert.throws(() => validateDetectorExecutionJobs(jobs.map(job => job === detection ? { ...job, steps } : job),
      123, 1), /actual successful detector/);
  }
});

test('native host conclusion rejects exit-zero skip or warning instead of attesting a successful verdict', () => {
  assert.doesNotThrow(() => validateNativeConclusionOutput('conclusion=success\nsuccess=true\nreason=\n'));
  assert.doesNotThrow(() => validateNativeConclusionOutput('conclusion=success\r\nsuccess=true\r\nreason=\r\n'));
  for (const output of [
    '',
    'conclusion=skipped\nsuccess=true\nreason=\n',
    'conclusion=warning\nsuccess=true\nreason=agent_failure\n',
    'conclusion=failure\nsuccess=false\nreason=threat_detected\n',
    'conclusion=success\nsuccess=true\nreason=agent_failure\n',
    'conclusion=success\nsuccess=true\nreason=\nconclusion=skipped\n',
  ]) {
    assert.throws(() => validateNativeConclusionOutput(output), /must evaluate a successful verdict/);
  }
  const helper = readFileSync(new URL('./security-detector.mjs', import.meta.url), 'utf8');
  assert(helper.includes("RUN_DETECTION: 'true'"));
  assert(helper.includes('DETECTION_AGENTIC_EXECUTION_OUTCOME: process.env.DETECTION_EXECUTION_OUTCOME'));
  assert(helper.includes("GH_AW_DETECTION_CONTINUE_ON_ERROR: 'false'"));
  assert(helper.includes('GITHUB_OUTPUT: output'));
});

test('runner original failure rejects clean verdict despite post-continue-on-error REST success and forged sandbox evidence', () => {
  const root = mkdtempSync(join(process.cwd(), '.detector-host-outcomes-'));
  try {
    const host = join(root, 'host');
    const sandbox = join(root, 'sandbox');
    mkdirSync(host);
    mkdirSync(sandbox);
    const checkpoint = {
      binding: { sourceJob: 'detection', executionExitCode: 0 },
      executionOutcome: 'success', conclusionExitCode: 0,
      result: { prompt_injection: false, secret_leak: false, malicious_patch: false, reasons: [], warnings: [] },
    };
    writeFileSync(join(sandbox, 'security-consumed.json'), JSON.stringify(checkpoint));
    writeFileSync(join(sandbox, 'detection_result.json'), JSON.stringify(checkpoint.result));
    const forgedApi = publicationJobs(false);
    assert.doesNotThrow(() => validateDetectorExecutionJobs(forgedApi, 123, 1),
      'REST success can be forged and is not the original outcome authority');
    assert.throws(() => completeHostDetector(host, 'success', 'success'), /ENOENT/,
      'sandbox evidence cannot substitute for the protected host checkpoint');
    writeFileSync(join(host, 'conclusion.json'), JSON.stringify(checkpoint));
    for (const [execution, conclusion] of [
      ['failure', 'success'], ['success', 'failure'], ['cancelled', 'success'],
      ['skipped', 'success'], ['success', 'skipped'], ['', 'success'],
    ]) {
      assert.throws(() => validateOriginalOutcomes(execution, conclusion), /original detector outcomes/);
      assert.throws(() => completeHostDetector(host, execution, conclusion), /original detector outcomes/);
      assert.throws(() => readFileSync(join(host, 'security-detector-host.json')));
    }
    const completion = completeHostDetector(host, 'success', 'success');
    assert.equal(completion.executionOutcome, 'success');
    assert.equal(completion.conclusionOutcome, 'success');
    assert.notEqual(join(host, 'security-detector-host.json'), join(sandbox, 'security-consumed.json'));
    assert.throws(() => completeHostDetector(host, 'success', 'success'), /EEXIST/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('pinned native hooks install before inference and reject changed assets before mutation',
  { skip: !process.env.SECURITY_PINNED_GHAW_RUNTIME }, () => {
    const root = mkdtempSync(join(process.cwd(), '.detector-native-'));
    try {
      const actions = join(root, 'gh-aw', 'actions');
      const payload = join(root, 'security-payload');
      mkdirSync(actions, { recursive: true });
      mkdirSync(payload);
      const source = process.env.SECURITY_PINNED_GHAW_RUNTIME;
      const names = ['prepare_threat_detection_files.sh', 'install_threat_detect_binary.sh'];
      for (const name of names) writeFileSync(join(actions, name), readFileSync(join(source, 'setup', 'sh', name)));
      writeFileSync(join(payload, 'security-scope.json'), '{}');
      writeFileSync(join(payload, 'security-findings.validated.json'), '{}');
      const invoke = () => spawnSync(process.execPath, [
        fileURLToPath(new URL('./security-detector.mjs', import.meta.url)), 'prepare',
      ], { env: { ...process.env, RUNNER_TEMP: root, GITHUB_PATH: join(root, 'github-path'),
        SECURITY_SCOPE_FILE: 'security-scope.json' }, encoding: 'utf8', timeout: 30_000 });
      const original = readFileSync(join(actions, names[1]));
      writeFileSync(join(actions, names[1]), `${original.toString()}\nChanged asset.`);
      assert.notEqual(invoke().status, 0);
      assert.equal(createHash('sha256').update(readFileSync(join(actions, names[0]))).digest('hex'), PREPARE_SHA256);
      assert.throws(() => readFileSync(join(root, 'gh-aw', 'security-detector-native', 'security-detector.mjs')));
      writeFileSync(join(actions, names[1]), original);
      const result = invoke();
      assert.equal(result.status, 0, result.stderr);
      assert(readFileSync(join(actions, names[0]), 'utf8').includes('security-detector.mjs" stage'));
      assert(readFileSync(join(actions, names[1]), 'utf8').includes('security-detector.mjs" install-wrapper'));
      assert(readFileSync(join(root, 'github-path'), 'utf8').includes(join(root, 'gh-aw', 'bin')));
      assert.equal(readFileSync(join(root, 'gh-aw', 'security-payload', 'security-findings.validated.json'), 'utf8'), '{}');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

test('publication branches preserve fork guidance, same-repo repairs and no-patch no-op', () => {
  const input = { repositoryRelation: 'same-repo', mode: 'repair', findings: [], patch: [] };
  assert.equal(publicationDecision(input), 'none');
  input.findings = [{ severity: 'high', fixDisposition: { state: 'fixed' } }];
  input.patch = [{ path: 'tools/wta/src/routing.rs' }];
  assert.equal(publicationDecision(input), 'push');
  input.findings[0].fixDisposition.state = 'blocked';
  assert.equal(publicationDecision(input), 'none');
  input.repositoryRelation = 'fork';
  input.mode = 'guide';
  input.patch = [];
  assert.equal(publicationDecision(input), 'comment');
  input.findings[0].severity = 'medium';
  assert.equal(publicationDecision(input), 'comment');
  input.findings = [];
  assert.equal(publicationDecision(input), 'none');
});

test('publication rejects wrong run, revision, source job, attempt, detector and native proof', () => {
  for (const sameRepo of [true, false]) {
    const identity = { repository: 'owner/repo', runId: '123', baseSha: BASE, sameRepo, dispatchId: 'fixed-dispatch' };
    const run = {
      id: 123, repository: { full_name: 'owner/repo' }, head_sha: BASE, run_attempt: 1,
      path: `.github/workflows/${sameRepo ? 'ghaw-pr-security.lock.yml' : 'ghaw-pr-security-guide-fork.lock.yml'}`,
      event: 'workflow_dispatch', display_title: `${sameRepo ? 'Security Repair' : 'Security Guide'} fixed-dispatch`,
      status: 'completed', conclusion: 'success',
    };
    const jobs = publicationJobs(sameRepo);
    assert.doesNotThrow(() => validatePublicationRun(run, jobs, identity));
    for (const [key, value] of Object.entries({
      id: 124, head_sha: HEAD, path: '.github/workflows/other.yml', event: 'push',
      display_title: 'forged fixed-dispatch', conclusion: 'failure', status: 'in_progress',
    })) assert.throws(() => validatePublicationRun({ ...run, [key]: value }, jobs, identity));
    for (const job of jobs) {
      for (const extra of [{ run_id: 124 }, { run_attempt: 2 }, { conclusion: 'skipped' }, { conclusion: 'failure' }]) {
        assert.throws(() => validatePublicationRun(run, jobs.map(item => item === job ? { ...item, ...extra } : item), identity));
      }
    }
    assert.throws(() => validatePublicationRun(run, jobs.slice(1), identity));
    assert.throws(() => validatePublicationRun(run, [...jobs, jobs[0]], identity));
    const gate = jobs.find(job => job.name === 'publication_gate');
    for (const steps of [undefined, [], gate.steps.slice(1), [...gate.steps, gate.steps[0]],
      gate.steps.map(step => ({ ...step, conclusion: 'skipped' })),
      gate.steps.map(step => ({ ...step, conclusion: 'failure' }))]) {
      assert.throws(() => validatePublicationRun(run, jobs.map(job => job === gate ? { ...job, steps } : job), identity), /native detector/);
    }
  }
  const current = repairScope();
  const input = { review: { headSha: HEAD, patchSha256: PATCH_SHA256, status: 'source-pass' } };
  const proof = {
    repository: 'owner/repo', trustedWorkflowSha: BASE, baseSha: BASE,
    headSha: HEAD, comparisonBaseSha: BASE, scopeSha256: current.scopeSha256,
    testsPassed: true, exitCode: 0, patchSha256: PATCH_SHA256, review: input.review,
  };
  assert.doesNotThrow(() => validateNativePublicationProof(proof, input, current, 'owner/repo', BASE, PATCH_TEXT));
  for (const [key, value] of Object.entries({
    repository: 'wrong/repo', trustedWorkflowSha: HEAD, baseSha: HEAD, headSha: BASE,
    comparisonBaseSha: HEAD, scopeSha256: '0'.repeat(64), testsPassed: false, exitCode: 1,
    patchSha256: '0'.repeat(64), review: { status: 'pending' },
  })) assert.throws(() => validateNativePublicationProof({ ...proof, [key]: value }, input, current, 'owner/repo', BASE, PATCH_TEXT));
  assert.throws(() => validateNativePublicationProof(proof, input, current, 'owner/repo', BASE, `${PATCH_TEXT}tampered`));
});

test('canonical handoff rematerializes fork and no-patch reports and fails closed before outputs', () => {
  const root = mkdtempSync(join(process.cwd(), '.canonical-publication-'));
  try {
    for (const sameRepo of [true, false]) {
      const directory = join(root, String(sameRepo));
      const artifacts = join(directory, 'artifacts');
      mkdirSync(artifacts, { recursive: true });
      const current = buildScope(BASE, HEAD, 17, sameRepo ? 'same-repo' : 'fork',
        'M\0tools/wta/src/master/mod.rs\0', BASE, sameRepo ? 'repair' : 'guide');
      const candidate = { ...createReportTemplate(current), summary: 'Trustedly rendered report.' };
      if (!sameRepo) candidate.findings = [{
        rule: 'session-route-target-binding', severity: 'high', confidence: 'high', category: 'session-routing',
        file: 'tools/wta/src/master/mod.rs', startLine: 20, endLine: 24,
        observed: 'Changed routing bypasses owner binding.', expected: 'Bind the owner.', impact: 'Wrong-pane mutation.',
        evidence: [{ kind: 'source-trace', reference: 'routing:20', detail: 'Owner bypass.' }],
        proposedFix: 'Restore lookup.', validation: 'Focused tests.',
        fixDisposition: { state: 'blocked', reason: 'Read-only fork guidance.' },
      }];
      const scopePath = join(directory, 'scope.json');
      writeFileSync(scopePath, JSON.stringify(current));
      writeFileSync(join(artifacts, 'security-findings.validated.json'), JSON.stringify(candidate));
      writeFileSync(join(artifacts, 'security-summary.md'), 'forged raw worker summary');
      writeFileSync(join(artifacts, 'security-status.txt'), 'forged status');
      writeFileSync(join(artifacts, 'security-repair.patch'), '');
      const detectorPath = join(directory, 'detector.json');
      writeFileSync(detectorPath, JSON.stringify(detectorProof(current, candidate)));
      const environment = {
        GITHUB_REPOSITORY: 'owner/repo', PR_NUMBER: '17', RUN_ID: '123',
        EXPECTED_HEAD_SHA: HEAD, EXPECTED_BASE_SHA: BASE, SAME_REPO: String(sameRepo),
        HEAD_REF: 'branch-with-$-safe-data', DISPATCH_ID: 'fixed-dispatch', GITHUB_OUTPUT: join(directory, 'outputs'),
      };
      const run = {
        id: 123, repository: { full_name: 'owner/repo' }, head_sha: BASE, run_attempt: 1,
        path: `.github/workflows/${sameRepo ? 'ghaw-pr-security.lock.yml' : 'ghaw-pr-security-guide-fork.lock.yml'}`,
        event: 'workflow_dispatch', display_title: `${sameRepo ? 'Security Repair' : 'Security Guide'} fixed-dispatch`,
        status: 'completed', conclusion: 'success',
      };
      const jobs = publicationJobs(sameRepo);
      let proofArtifacts = [detectorArtifact()];
      const pull = { head: { sha: HEAD, ref: environment.HEAD_REF, repo: { full_name: sameRepo ? 'owner/repo' : 'fork/repo' } } };
      const request = endpoint => {
        if (endpoint === 'pulls/17') return pull;
        if (endpoint === 'actions/runs/123') return run;
        if (endpoint === 'actions/runs/123/attempts/1/jobs?per_page=100&page=1') return { jobs };
        if (endpoint === 'actions/runs/123/artifacts?per_page=100&page=1') return { artifacts: proofArtifacts };
        throw new Error(`Unexpected endpoint: ${endpoint}`);
      };
      let output = join(directory, 'rejected');
      mkdirSync(output);
      const paths = name => ({
        '--scope': scopePath, '--artifacts': artifacts, '--output': output, '--detector-proof': detectorPath,
      })[name];
      pull.head.sha = BASE;
      assert.throws(() => preparePublication({ environment, request, paths }), /stale/);
      assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
      pull.head.sha = HEAD;
      jobs[1].conclusion = 'skipped';
      assert.throws(() => preparePublication({ environment, request, paths }), /source jobs/);
      assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
      jobs[1].conclusion = 'success';
      writeFileSync(scopePath, JSON.stringify({ ...current, headSha: BASE }));
      assert.throws(() => preparePublication({ environment, request, paths }), /controller identity/);
      writeFileSync(scopePath, JSON.stringify(current));
      writeFileSync(join(artifacts, 'security-findings.validated.json'), JSON.stringify({ ...candidate, headSha: BASE }));
      assert.throws(() => preparePublication({ environment, request, paths }), /immutable scope/);
      assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
      writeFileSync(join(artifacts, 'security-findings.validated.json'), JSON.stringify(candidate));
      for (const invalid of [
        [], [detectorArtifact(), detectorArtifact()], [{ ...detectorArtifact(), expired: true }],
        [{ ...detectorArtifact(), workflow_run: { id: 124, head_sha: BASE } }],
        [{ ...detectorArtifact(), workflow_run: { id: 123, head_sha: HEAD } }],
      ]) {
        proofArtifacts = invalid;
        assert.throws(() => preparePublication({ environment, request, paths }), /detector proof artifact/);
        assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
        assert.throws(() => readFileSync(join(output, 'security-summary.md')));
      }
      proofArtifacts = [detectorArtifact()];
      for (const extra of [
        { detectionSuccess: false }, { detectionConclusion: 'warning' }, { runAttempt: 2 },
        { reportSha256: '0'.repeat(64) }, { sourceJob: 'agent' },
      ]) {
        writeFileSync(detectorPath, JSON.stringify({ ...detectorProof(current, candidate), ...extra }));
        assert.throws(() => preparePublication({ environment, request, paths }), /attestation/);
        assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
        assert.throws(() => readFileSync(join(output, 'security-summary.md')));
      }
      writeFileSync(detectorPath, JSON.stringify(detectorProof(current, candidate)));
      for (const bytes of ['{malformed', 'null']) {
        writeFileSync(detectorPath, bytes);
        assert.throws(() => preparePublication({ environment, request, paths }));
        assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
        assert.throws(() => readFileSync(join(output, 'security-summary.md')));
      }
      unlinkSync(detectorPath);
      assert.throws(() => preparePublication({ environment, request, paths }), /ENOENT/);
      assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
      writeFileSync(detectorPath, JSON.stringify(detectorProof(current, candidate)));
      output = join(directory, 'accepted');
      mkdirSync(output);
      preparePublication({ environment, request, paths });
      assert.equal(readFileSync(join(output, 'security-summary.md'), 'utf8'), renderReport(validateReport(candidate, current)));
      const canonicalSummary = readFileSync(join(output, 'security-summary.md'), 'utf8');
      assert(canonicalSummary.includes('| Severity | Status/Fix | Finding | Location | Evidence/Validation/Reason | Confidence |'));
      assert(canonicalSummary.includes('### Validation'));
      assert(!canonicalSummary.includes('forged raw worker summary'));
      assert.equal(readFileSync(join(output, 'security-status.txt'), 'utf8'), sameRepo ? 'pass\n' : 'blocking\n');
      assert.match(readFileSync(environment.GITHUB_OUTPUT, 'utf8'), new RegExp(`^publication=${sameRepo ? 'none' : 'comment'}\\npatch_sha256=[0-9a-f]{64}\\ndetector_attested=true\\n$`));
      if (sameRepo) {
        output = join(directory, 'unreported-patch');
        mkdirSync(output);
        writeFileSync(join(artifacts, 'security-repair.patch'), PATCH_TEXT);
        writeFileSync(detectorPath, JSON.stringify(detectorProof(current, candidate, PATCH_TEXT)));
        assert.throws(() => preparePublication({ environment, request, paths }), /unreported/);
        assert.throws(() => readFileSync(join(output, 'security-summary.md')));
      }
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('canonical repair applies only a digest-bound native-attested patch to an isolated index', () => {
  const root = mkdtempSync(join(process.cwd(), '.canonical-repair-'));
  const workspace = join(root, 'checkout');
  const artifacts = join(root, 'artifacts');
  const output = join(root, 'canonical');
  const previousIndex = process.env.GIT_INDEX_FILE;
  mkdirSync(workspace);
  mkdirSync(artifacts);
  mkdirSync(output);
  const path = 'tools/wta/src/routing.rs';
  const source = join(workspace, path);
  const tail = Array.from({ length: 99 }, (_, i) => `// stable line ${i + 2}\n`).join('');
  const git = (...args) => execFileSync('git', args, { cwd: workspace, encoding: 'utf8', timeout: 30_000 }).trim();
  try {
    git('init', '--quiet');
    git('config', 'user.name', 'Local canonical fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    git('config', 'core.autocrlf', 'false');
    mkdirSync(join(workspace, 'tools', 'wta', 'src'), { recursive: true });
    writeFileSync(source, 'fn route() { /* bound */ }\n' + tail);
    git('add', '.');
    git('commit', '--quiet', '-m', 'Base');
    const base = git('rev-parse', 'HEAD');
    writeFileSync(source, 'fn route() { /* bypass */ }\n' + tail);
    git('add', '.');
    git('commit', '--quiet', '-m', 'Reviewed head');
    const head = git('rev-parse', 'HEAD');
    const inputs = buildScope(base, head, 17, 'same-repo', `M\0${path}\0`, base, 'repair');
    const current = buildScope(base, head, 17, 'same-repo', `M\0${path}\0`, base, 'repair', readImmutableHunks(inputs, workspace));
    writeFileSync(source, 'fn route() { /* bound repair */ }\n' + tail);
    const patch = execFileSync('git', ['diff', '--binary', 'HEAD'], { cwd: workspace, encoding: 'utf8' });
    const digest = createHash('sha256').update(patch).digest('hex');
    git('restore', '--', path);
    const candidate = {
      ...createReportTemplate(current), summary: 'Validated owner binding repair.',
      checks: [
        { name: 'deterministic-scope', status: 'pass', headSha: head, evidence: 'Immutable scope.' },
        { name: 'wta-tests', status: 'pass', headSha: head, evidence: 'local command: trusted isolated Windows tests (exit 0)' },
      ],
      review: { status: 'source-pass', reviewer: 'ghaw-pr-security-reviewer', headSha: head, patchSha256: digest, evidence: 'Independent exact-patch review.' },
      findings: [{
        rule: 'session-route-target-binding', severity: 'high', confidence: 'high', category: 'session-routing',
        file: path, startLine: 1, endLine: 1, observed: 'Bypassed binding.', expected: 'Bound owner.', impact: 'Wrong-pane mutation.',
        evidence: [{ kind: 'source-trace', reference: `${path}:1`, detail: 'New bypass.' }],
        proposedFix: 'Restore binding.', validation: 'Native focused tests.',
        fixDisposition: { state: 'fixed', reason: 'Independently reviewed and tested.' },
      }], patch: [{ path, summary: 'Restore binding.' }],
    };
    const scopePath = join(artifacts, 'scope.json');
    const proofPath = join(artifacts, 'result.json');
    writeFileSync(scopePath, JSON.stringify(current));
    writeFileSync(join(artifacts, 'security-findings.validated.json'), JSON.stringify(validateReport(candidate, current)));
    const detectorPath = join(artifacts, 'detector.json');
    writeFileSync(detectorPath, JSON.stringify(detectorProof(current, validateReport(candidate, current), patch)));
    writeFileSync(join(artifacts, 'security-repair.patch'), patch);
    writeFileSync(proofPath, JSON.stringify({
      repository: 'owner/repo', trustedWorkflowSha: base, baseSha: base, headSha: head,
      comparisonBaseSha: base, scopeSha256: current.scopeSha256, testsPassed: true, exitCode: 0,
      patchSha256: digest, review: candidate.review,
    }));
    const environment = {
      GITHUB_REPOSITORY: 'owner/repo', PR_NUMBER: '17', RUN_ID: '123', EXPECTED_HEAD_SHA: head,
      EXPECTED_BASE_SHA: base, SAME_REPO: 'true', HEAD_REF: 'reviewed', DISPATCH_ID: 'fixed',
      GITHUB_OUTPUT: join(root, 'outputs'),
    };
    const run = { id: 123, repository: { full_name: 'owner/repo' }, head_sha: base, run_attempt: 1,
      path: '.github/workflows/ghaw-pr-security.lock.yml', event: 'workflow_dispatch',
      display_title: 'Security Repair fixed', status: 'completed', conclusion: 'success' };
    const jobs = publicationJobs(true);
    const request = endpoint => {
      if (endpoint === 'pulls/17') return { head: { sha: head, ref: 'reviewed', repo: { full_name: 'owner/repo' } } };
      if (endpoint === 'actions/runs/123') return run;
      if (endpoint === 'actions/runs/123/attempts/1/jobs?per_page=100&page=1') return { jobs };
      if (endpoint === 'actions/runs/123/artifacts?per_page=100&page=1') return { artifacts: [
        detectorArtifact(base), { name: 'ghaw-pr-security-windows-proof-123-1-17', expired: false },
      ] };
      throw new Error('Unexpected endpoint');
    };
    process.env.GIT_INDEX_FILE = join(root, 'isolated.index');
    const paths = name => ({
      '--scope': scopePath, '--artifacts': artifacts, '--output': output, '--proof': proofPath, '--detector-proof': detectorPath,
    })[name];
    git('read-tree', head);
    const initialIndex = git('write-tree');
    for (const extra of [
      { detectionSuccess: false }, { detectionConclusion: 'warning' }, { runId: '124' },
      { runAttempt: 2 }, { patchSha256: '0'.repeat(64) }, { reportSha256: '0'.repeat(64) },
    ]) {
      writeFileSync(detectorPath, JSON.stringify({ ...detectorProof(current, validateReport(candidate, current), patch), ...extra }));
      assert.throws(() => preparePublication({ environment, request, workspace, paths }), /attestation/);
      assert.equal(git('write-tree'), initialIndex, 'untrusted detector outcome cannot mutate the repair index');
      assert.throws(() => readFileSync(environment.GITHUB_OUTPUT));
      assert.throws(() => readFileSync(join(output, 'security-repair.patch')));
    }
    writeFileSync(detectorPath, JSON.stringify(detectorProof(current, validateReport(candidate, current), patch)));
    preparePublication({ environment, request, workspace, paths: name => ({
      '--scope': scopePath, '--artifacts': artifacts, '--output': output, '--proof': proofPath, '--detector-proof': detectorPath,
    })[name] });
    assert.equal(readFileSync(source, 'utf8'), 'fn route() { /* bypass */ }\n' + tail);
    assert.equal(git('show', `:${path}`), ('fn route() { /* bound repair */ }\n' + tail).trim());
    assert.equal(readFileSync(join(output, 'security-repair.patch'), 'utf8'), patch);
    assert.match(readFileSync(environment.GITHUB_OUTPUT, 'utf8'), /^publication=push\n/);
    const authorizedIndex = git('write-tree');
    writeFileSync(source, 'fn route() { /* bound repair */ }\n' + tail.replace('stable line 100', 'unrelated rewrite'));
    const extraPatch = execFileSync('git', ['diff', '--binary', head], { cwd: workspace, encoding: 'utf8' });
    writeFileSync(source, 'fn route() { /* bypass */ }\n' + tail);
    const extraDigest = createHash('sha256').update(extraPatch).digest('hex');
    const extraReport = { ...candidate, review: { ...candidate.review, patchSha256: extraDigest } };
    writeFileSync(join(artifacts, 'security-findings.validated.json'), JSON.stringify(extraReport));
    writeFileSync(join(artifacts, 'security-repair.patch'), extraPatch);
    const proof = JSON.parse(readFileSync(proofPath, 'utf8'));
    writeFileSync(proofPath, JSON.stringify({ ...proof, patchSha256: extraDigest, review: extraReport.review }));
    writeFileSync(detectorPath, JSON.stringify(detectorProof(current, extraReport, extraPatch)));
    assert.throws(() => preparePublication({ environment, request, workspace, paths: name => ({
      '--scope': scopePath, '--artifacts': artifacts, '--output': output, '--proof': proofPath, '--detector-proof': detectorPath,
    })[name] }), /actual repair changes/);
    assert.equal(git('write-tree'), authorizedIndex, 'digest-bound extra edits cannot enter the publication index');
  } finally {
    if (previousIndex === undefined) delete process.env.GIT_INDEX_FILE;
    else process.env.GIT_INDEX_FILE = previousIndex;
    rmSync(root, { recursive: true, force: true });
  }
});

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

test('actual segments use immutable HEAD coordinates rather than hunk overlap or candidate offsets', () => {
  const path = 'tools/wta/src/master/mod.rs';
  const current = buildScope(BASE, HEAD, 17, 'same-repo', `M\0${path}\0`, BASE, 'repair', [{
    path, headLineCount: 120, hunks: [
      { baseStart: 10, baseCount: 1, headStart: 10, headCount: 1 },
      { baseStart: 20, baseCount: 2, headStart: 20, headCount: 0 },
      { baseStart: 32, baseCount: 3, headStart: 30, headCount: 3 },
      { baseStart: 122, baseCount: 1, headStart: 120, headCount: 1 },
    ],
  }]);
  const patch = text => `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n${text}`;
  for (const text of [
    '@@ -10 +10 @@\n-old\n+fixed\n',
    '@@ -9,0 +10,2 @@\n+insert\n+insert2\n',
    '@@ -10,0 +11 @@\n+insert\n',
    '@@ -10 +9,0 @@\n-removed\n',
    '@@ -20 +20 @@\n-surviving deletion context\n+restore\n',
    '@@ -19,0 +20 @@\n+restore removed original line\n',
    '@@ -10,0 +11,3 @@\n+a\n+b\n+c\n@@ -30 +33 @@\n-old\n+fixed\n',
    '@@ -30,3 +30,0 @@\n-a\n-b\n-c\n@@ -120 +117 @@\n-last\n+fixed last\n',
    '@@ -120,0 +121 @@\n+new EOF\n',
  ]) assert.doesNotThrow(() => validateRepairChanges(current, patch(text)), text);
  for (const text of [
    '@@ -10 +10 @@\n-old\n+fixed\n@@ -100 +100 @@\n-unrelated\n+changed\n',
    '@@ -9,2 +9,2 @@\n-unchanged context\n-changed line\n+rewritten context\n+fixed\n',
    '@@ -10,91 +10 @@\n' + Array.from({ length: 91 }, () => '-old\n').join('') + '+replacement\n',
    '@@ -8,0 +9 @@\n+before unrelated line\n',
    '@@ -11,0 +12 @@\n+after unrelated line\n',
    '@@ -10,0 +11,3 @@\n+a\n+b\n+c\n@@ -33 +36 @@\n-outside despite candidate offsets\n+fixed\n',
    '@@ -119 +119 @@\n-unchanged before EOF\n+fixed\n',
    '@@ -121,0 +122 @@\n+past EOF\n',
  ]) assert.throws(() => validateRepairChanges(current, patch(text)), /actual repair changes|EOF/, text);
  const contextPatch = patch('@@ -9,3 +9,3 @@\n unchanged\n-old\n+fixed\n unchanged\n');
  assert.doesNotThrow(() => validateRepairChanges(current, contextPatch));
  assert.throws(() => validateRepairChanges(current, contextPatch.replace(' unchanged\n', '-unchanged\n+rewritten\n')),
    /actual repair changes/);
  const tampered = structuredClone(current);
  tampered.immutableHunks[0].hunks[0].headCount = 100;
  assert.throws(() => validateRepairChanges(tampered, contextPatch), /scope identity/);
});

test('nonempty patch API requires explicit immutable authority even with a matching digest', () => {
  const path = 'tools/wta/src/master/mod.rs';
  const current = buildScope(BASE, HEAD, 17, 'same-repo', `M\0${path}\0`, BASE, 'repair', [{
    path, headLineCount: 1000, hunks: [{ baseStart: 10, baseCount: 1, headStart: 10, headCount: 1 }],
  }]);
  const valid = `diff --git a/${path} b/${path}\n--- a/${path}\n+++ b/${path}\n@@ -10 +10 @@\n-old\n+fixed\n`;
  const extra = valid + '@@ -1000 +1000 @@\n-unchanged\n+unrelated\n';
  const report = {
    ...createReportTemplate(current), patch: [{ path }],
    review: { patchSha256: createHash('sha256').update(extra).digest('hex') },
  };
  assert.throws(() => validatePatch(report, [path], extra), /authoritative immutable scope/);
  assert.throws(() => validatePatch(report, [path], extra, current), /actual repair changes/);
  for (const invalid of [null, {}, [], { ...current, immutableHunks: undefined }]) {
    assert.throws(() => validatePatch(report, [path], extra, invalid), /scope|identity/);
  }
  const bounded = { ...report, review: { patchSha256: createHash('sha256').update(valid).digest('hex') } };
  assert.doesNotThrow(() => validatePatch(bounded, [path], valid, current));
  assert.throws(() => validatePatch(bounded, [path], valid), /authoritative immutable scope/);
  assert.throws(() => validatePatch({ ...bounded, headSha: BASE }, [path], valid, current), /report identity/);
  assert.throws(() => validatePatch(bounded, [path], '', current), /nonempty actual patch/);
  const headerOnly = `diff --git a/${path} b/${path}\n`;
  assert.throws(() => validatePatch(bounded, [path], headerOnly, current), /actual change hunks/);
  assert.throws(() => validatePatch({ patch: [] }, [], valid), /authoritative immutable scope/);
  assert.doesNotThrow(() => validatePatch({ patch: [] }, [], ''));
});

test('native writer rejects same-file extra edits and formatter rewrites before touching bytes or index', () => {
  const root = mkdtempSync(join(process.cwd(), '.actual-repair-bounds-'));
  const path = 'tools/wta/src/master/mod.rs';
  const other = 'tools/wta/src/logging.rs';
  const source = join(root, path);
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8', timeout: 30_000 }).trim();
  try {
    git('init', '--quiet');
    git('config', 'core.autocrlf', 'false');
    git('config', 'user.name', 'Fixture');
    git('config', 'user.email', 'fixture@example.invalid');
    mkdirSync(join(root, 'tools', 'wta', 'src', 'master'), { recursive: true });
    const lines = Array.from({ length: 1000 }, (_, i) => `original line ${i + 1}\n`);
    lines[440] = 'let owner = owners.get(&secret);\n';
    writeFileSync(source, lines.join(''));
    writeFileSync(join(root, other), 'original\n');
    git('add', '.');
    git('commit', '--quiet', '-m', 'Base');
    const base = git('rev-parse', 'HEAD');
    lines[440] = 'let owner = owners.values().next();\n';
    writeFileSync(source, lines.join(''));
    writeFileSync(join(root, other), 'changed\n');
    git('add', '.');
    git('commit', '--quiet', '-m', 'Head');
    const head = git('rev-parse', 'HEAD');
    const raw = `M\0${path}\0M\0${other}\0`;
    const inputs = buildScope(base, head, 17, 'same-repo', raw, base, 'repair');
    const current = buildScope(base, head, 17, 'same-repo', raw, base, 'repair', readImmutableHunks(inputs, root));
    const original = readFileSync(source);
    const index = git('write-tree');
    const valid = original.toString().replace('.values().next()', '.get(&secret)');
    for (const content of [
      valid.replace('original line 1000', 'unrelated edited line 1000'),
      valid.replaceAll('original line', 'formatter rewrote original line'),
    ]) {
      assert.throws(() => writeSecurityRepair(current, root, path, content), /actual repair changes/);
      assert.deepEqual(readFileSync(source), original);
      assert.equal(git('write-tree'), index);
    }
    assert.throws(() => replaceSecurityRepairText(current, root, path, JSON.stringify([
      { oldText: '.values().next()', newText: '.get(&secret)' },
      { oldText: 'original line 1000', newText: 'extra change' },
    ])), /actual repair changes/);
    assert.deepEqual(readFileSync(source), original);
    const validInspection = writeSecurityRepair(current, root, path, valid);
    assert.match(validInspection.patch, /\.get\(&secret\)/);
    assert.equal(git('write-tree'), index);
    writeFileSync(join(root, other), 'bounded second file repair\n');
    assert.doesNotThrow(() => inspectSecurityRepair(current, root));
    writeFileSync(source, valid.replace('original line 1000', 'unrelated extra'));
    assert.throws(() => inspectSecurityRepair(current, root), /actual repair changes/);
    const report = { patch: [{ path }, { path: other }], review: { patchSha256: createHash('sha256').update(
      execFileSync('git', ['diff', '--binary', head], { cwd: root, encoding: 'utf8' })).digest('hex') } };
    assert.throws(() => validatePatch(report, [path, other],
      execFileSync('git', ['diff', '--binary', head], { cwd: root, encoding: 'utf8' }), current), /actual repair changes/);
    const target = join(root, 'staging-target');
    git('-c', 'core.autocrlf=false', 'clone', '--quiet', '--no-hardlinks', root, target);
    const stageReport = {
      ...createReportTemplate(current), patch: [{ path: other }, { path }],
      findings: [
        { file: other, startLine: 1, endLine: 1, fixDisposition: { state: 'proposed' } },
        { file: path, startLine: 441, endLine: 441, fixDisposition: { state: 'proposed' } },
      ],
    };
    assert.throws(() => stageRepairFiles(stageReport, root, target), /actual repair changes/);
    assert.equal(readFileSync(join(target, other), 'utf8'), 'changed\n',
      'no earlier valid path may be copied before every candidate path passes');
    assert.deepEqual(readFileSync(join(target, path)), original);
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
    assert.throws(() => stageRepairFiles(candidate, root, target), /actual repair changes/);
    assert.equal(readFileSync(join(target, path), 'utf8'), changed.join(''));
    writeFileSync(source, changed.join('').replace('changed line 10\n', 'bounded repaired change\n'));
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

test('installer scripts and bootstrap packaging have synchronized review coverage without automatic repair', () => {
  const controller = readFileSync(new URL('../../../workflows/ghaw-pr-security-controller.yml', import.meta.url), 'utf8');
  assert(controller.includes("- 'installer/**'"));
  for (const path of [
    'installer/Install-Msix.ps1', 'installer/install.cmd',
    'installer/install-local-terminal.ps1', 'installer/uninstall-local-terminal.ps1',
    'installer/bootstrap/src/main.rs', 'installer/bootstrap/Cargo.toml',
    'installer/bootstrap/Cargo.lock',
  ]) {
    const domains = classifyPath(path);
    assert(domains.includes('packaging-paths-diagnostics'));
    assert(domains.includes('build-tooling'));
    assert(!domains.includes('wta-rust'));
    if (/Cargo\.(?:toml|lock)$/.test(path)) assert(domains.includes('dependency-supply-chain'));
    const current = buildScope(BASE, HEAD, 17, 'same-repo', `M\0${path}\0`, BASE, 'repair');
    assert.equal(current.applicable, true);
    assert.throws(() => validateRepairScope(current), /non-WTA-source paths/);
    const mixed = buildScope(BASE, HEAD, 17, 'same-repo',
      `M\0${path}\0M\0tools/wta/src/master/session_mcp.rs\0`, BASE, 'repair');
    assert.throws(() => validateRepairScope(mixed), /non-WTA-source paths/);
  }
});

test('loaded instruction roots have coherent triggers and classification without Rust repair eligibility', () => {
  const workflow = name => readFileSync(new URL(`../../../workflows/${name}`, import.meta.url), 'utf8');
  const controller = workflow('ghaw-pr-security-controller.yml');
  const pathsBlock = controller.match(/    paths:\r?\n((?:      - '[^']+'\r?\n)+)/)?.[1];
  assert(pathsBlock, 'controller must declare review path triggers');
  const triggers = [...pathsBlock.matchAll(/      - '([^']+)'/g)].map(match => match[1]);
  assert.deepEqual(triggers, [
    'AGENTS.md', '.agents/**', '.cargo/**', '.github/**', 'build/**',
    'installer/**', 'src/**', 'tools/**', 'test/**', 'doc/security-model.md',
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
  validatePatch(validated, ['tools/wta/src/master/mod.rs'], PATCH_TEXT, current);
  validateQueuedOutput(validated, { items: [{ type: 'noop', message: SECURITY_NOOP_MESSAGE }], errors: [] });
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
  validatePatch(finalReport, ['tools/wta/src/master/mod.rs'], PATCH_TEXT, current);
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

function tableFinding(rule, severity = 'high', state = 'blocked', overrides = {}) {
  return {
    rule, severity, confidence: 'high', category: 'session-routing',
    file: 'tools/wta/src/master/mod.rs', startLine: 20, endLine: 24,
    observed: 'Changed route bypasses owner binding.', expected: 'Bind requests to the owning session.',
    impact: 'Wrong-session mutation.', proposedFix: 'Restore owner binding.',
    evidence: [{ kind: 'source-trace', reference: 'tools/wta/src/master/mod.rs:20-24', detail: 'Changed route uses an unbound target.' }],
    validation: 'Run the focused owner-binding test.',
    fixDisposition: { state, reason: state === 'blocked' ? 'No validated repair is available.' : 'Native validation is required.' },
    ...overrides,
  };
}

test('required findings table orders blocking HIGH, native-fixed HIGH, MEDIUM, LOW with stable ties', () => {
  const current = repairScope();
  const proposal = {
    ...report(), mode: 'repair', scopeSha256: current.scopeSha256,
    review: { status: 'source-pass', reviewer: 'ghaw-pr-security-reviewer', headSha: HEAD,
      patchSha256: PATCH_SHA256, evidence: 'Independent exact-patch source review.' },
    findings: [
      tableFinding('low-advice', 'low', 'advice-only', { confidence: 'low' }),
      tableFinding('fixed-owner-binding', 'high', 'proposed'),
      tableFinding('medium-advice', 'medium', 'advice-only', { confidence: 'medium' }),
      tableFinding('z-blocking'),
      tableFinding('a-blocking'),
      tableFinding('earlier-line', 'high', 'blocked', { startLine: 19 }),
      tableFinding('earlier-path', 'high', 'blocked', { file: 'tools/wta/src/logging.rs' }),
    ],
    patch: [{ path: 'tools/wta/src/master/mod.rs', summary: 'Restore binding.' }],
  };
  const validated = validateReport(attestChecks(proposal, HEAD, true, PATCH_TEXT), current);
  const originalOrder = validated.findings.map(finding => finding.rule);
  const rendered = renderReport(validated);
  const rows = rendered.split('\n').filter(line => /^\| \*\*(HIGH|MEDIUM|LOW)\*\*/.test(line));
  assert.equal(rows.length, 7);
  for (const [index, rule] of ['earlier-path', 'earlier-line', 'a-blocking', 'z-blocking',
    'fixed-owner-binding', 'medium-advice', 'low-advice'].entries()) assert(rows[index].includes(rule));
  assert(rows[0].includes('Must fix / blocking'));
  assert(rows[4].includes('Fixed and validated'));
  assert(rows[5].endsWith('| medium |'));
  assert(rows[6].endsWith('| low |'));
  assert.match(rendered, /\| Severity \| Status\/Fix \| Finding \| Location \| Evidence\/Validation\/Reason \| Confidence \|/);
  assert.match(rendered, /HIGH \(must fix\/block\): \*\*4\*\*/);
  assert.match(rendered, /HIGH fixed and validated: \*\*1\*\*/);
  assert.match(rendered, /Medium\/Low \(consider\): \*\*2\*\*/);
  assert.deepEqual(validated.findings.map(finding => finding.rule), originalOrder);
  const reordered = { ...validated, findings: [...validated.findings].reverse() };
  assert.equal(renderReport(reordered), rendered);
});

test('empty findings still have a results row and explicit skipped, failed, and blocked validation', () => {
  const input = report({
    checks: [
      ...report().checks,
      { name: 'wta-tests', status: 'fail', evidence: 'Focused test failed (exit 1).' },
      { name: 'cpp-audit-mode', status: 'blocked', evidence: 'Required toolchain is unavailable.' },
    ],
  });
  const rendered = renderReport(validateReport(input, scope()));
  assert.match(rendered, /\| — \| No findings \| No introduced security regression found\./);
  assert.equal(rendered.split('\n').filter(line => line.startsWith('| — | No findings')).length, 1);
  assert.match(rendered, /### Validation[\s\S]+\| native-windows \| \*\*skipped\*\*/);
  assert.match(rendered, /\| wta-tests \| \*\*fail\*\*/);
  assert.match(rendered, /\| cpp-audit-mode \| \*\*blocked\*\*/);
  assert(rendered.includes(`Head \`${HEAD.slice(0, 12)}\``));
  assert(rendered.includes(`Source/head: \`${BASE.slice(0, 12)}\` / \`${HEAD.slice(0, 12)}\``));
  assert(!rendered.includes('Fixed and validated'));
});

test('candidate table does not claim Fixed or turn proposed source review into test validation', () => {
  const current = repairScope();
  const input = {
    ...report(), scopeSha256: current.scopeSha256, mode: 'repair',
    checks: report().checks,
    review: { status: 'pending', reviewer: 'ghaw-pr-security-reviewer', evidence: 'Independent source review has not run.' },
    findings: [tableFinding('proposed-owner-binding', 'high', 'proposed')],
    patch: [{ path: 'tools/wta/src/master/mod.rs', summary: 'Restore binding.' }],
  };
  const rendered = renderReport(validateCandidate(input, current));
  assert(rendered.includes('(proposed; validation pending)'));
  assert(!rendered.includes('Fixed and validated'));
  assert.match(rendered, /Independent source review:\*\* pending/);
  input.findings[0].fixDisposition.state = 'fixed';
  assert.throws(() => validateCandidate(input, current), /fix disposition/);
  const stale = report({ findings: [tableFinding('stale-fixed', 'high', 'fixed')] });
  assert.throws(() => validateReport(stale, scope()), /fix disposition/);
});

test('findings table escapes every attacker-controlled cell and preserves evidence without multiline rows', () => {
  const hostile = 'data | <img> [link](https://attacker.example) `code`\r\nnext\rpart\u2028last\u2029end';
  const input = report({
    summary: hostile,
    checks: [{ name: 'deterministic-scope', status: 'pass', headSha: HEAD, evidence: hostile }],
    review: { status: 'not-required', reviewer: 'none', evidence: hostile },
    findings: [tableFinding('table-escaping', 'medium', 'advice-only', {
      observed: hostile, expected: hostile, impact: hostile, proposedFix: hostile, validation: hostile,
      evidence: [{ kind: 'source-trace', reference: hostile, detail: hostile }],
      fixDisposition: { state: 'advice-only', reason: hostile },
    })],
  });
  const rendered = renderReport(validateReport(input, scope()));
  const row = rendered.split('\n').find(line => line.startsWith('| **MEDIUM**'));
  assert.equal(row.split(/(?<!\\)\|/).length, 8);
  assert(row.includes('data \\| \\<img\\>'));
  assert(row.includes('**source\\-trace:**'));
  for (const label of ['Observed', 'Expected', 'Impact', 'Proposed fix', 'Validation', 'Reason']) {
    assert(row.includes(`**${label}:**`));
  }
  assert(!rendered.includes('<img>'));
  assert(!rendered.includes('[link](https://attacker.example)'));
  assert(!/[\r\u2028\u2029]/.test(rendered));
  input.findings[0].evidence[0].detail = 'token=abcdefghijklmnopqrstuvwxyz123456';
  assert.throws(() => validateReport(input, scope()), /secret material/);
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
  validateQueuedOutput(noFindings, { items: [{ type: 'noop', message: SECURITY_NOOP_MESSAGE }], errors: [] });
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
  validateQueuedOutput(candidate, { items: [{ type: 'noop', message: SECURITY_NOOP_MESSAGE }], errors: [] });
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
    const writerInputs = buildScope(base, head, 17, 'same-repo',
      'M\0tools/wta/src/routing.rs\0', base, 'repair');
    const writerScope = buildScope(base, head, 17, 'same-repo',
      'M\0tools/wta/src/routing.rs\0', base, 'repair', readImmutableHunks(writerInputs, workspace));
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
      const nativeSummary = readFileSync(summary, 'utf8');
      assert.equal(nativeSummary, renderReport(validateReport(completed, current)));
      assert(nativeSummary.includes('| Severity | Status/Fix | Finding | Location | Evidence/Validation/Reason | Confidence |'));
      assert(nativeSummary.includes('| — | No findings |'));
      assert(nativeSummary.includes('### Validation'));
      result = invoke('check-report', '--scope', scopePath, '--report', reportPath);
      assert.equal(result.status, 0, result.stderr);
      assert.equal(readFileSync(status, 'utf8'), 'pass\n');
      const queue = join(artifacts, `${mode}-queue.json`);
      writeFileSync(queue, JSON.stringify({ items: [{ type: 'noop', message: SECURITY_NOOP_MESSAGE }], errors: [] }));
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
